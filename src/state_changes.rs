use crate::events::{Event, EventType};
use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::state::Player;
use serde::Deserialize;

/// Persistência de mudança de estado: o LLM só **sugere**, nunca escreve
/// direto (ver Estado-Rigido / Pool-de-Agentes — "único ponto de escrita").
/// Este módulo valida cada sugestão contra uma whitelist de campos/operações
/// antes de aplicar. Sugestão fora da whitelist, ou que viole uma invariante
/// (ex: remover item que não existe), é rejeitada e logada — nunca aplicada
/// "no escuro".
#[derive(Debug, Deserialize)]
struct MudancasPropostas {
    #[serde(default)]
    mudancas: Vec<MudancaProposta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MudancaProposta {
    campo: String,
    operacao: String,
    valor: serde_json::Value,
}

const SYSTEM_PROMPT: &str = r#"Você é o sistema de regras de um RPG de fantasia medieval. Dado o que aconteceu no turno, decida se algo do ESTADO DO JOGO deve mudar de fato.
Responda APENAS com um JSON no formato {"mudancas": [{"campo": "...", "operacao": "...", "valor": ...}]}.
Se nada mudou mecanicamente, responda {"mudancas": []} — a maioria dos turnos não muda nada, diálogo comum NUNCA é motivo de mudança.
Campos permitidos e suas operações:
- "player.hp" com operacao "somar" e valor um número inteiro (negativo para dano, positivo para cura)
- "player.inventario" com operacao "adicionar" ou "remover" e valor uma string (nome do item)
- "player.location_id" com operacao "definir" e valor uma string curta em snake_case identificando o novo local (ex: "floresta_negra"), só se o jogador CLARAMENTE se deslocou para outro lugar (andou até, viajou para, entrou em)
Só proponha uma mudança se o texto deixar EXPLÍCITO que algo foi ganho, perdido, causou dano, curou, ou que o jogador se moveu de local. Nunca invente itens, dano ou destinos que não foram mencionados."#;

pub async fn propor_mudancas(llm: &LlmClient, contexto: &str) -> Vec<MudancaProposta> {
    match llm.complete(SYSTEM_PROMPT, contexto).await {
        Ok(resposta) => extrair_json::<MudancasPropostas>(&resposta)
            .map(|p| p.mudancas)
            .unwrap_or_else(|| {
                tracing::warn!(resposta = %resposta, "propostas de mudanca de estado nao parseaveis, ignorando");
                vec![]
            }),
        Err(err) => {
            tracing::error!(%err, "falha ao consultar propostas de mudanca de estado");
            vec![]
        }
    }
}

/// Aplica uma proposta ao Player em memória (o chamador é responsável por
/// persistir depois). Retorna o evento `mudanca_estado` se aplicada, ou uma
/// razão de rejeição em texto.
pub fn aplicar(player: &mut Player, turno: u64, proposta: &MudancaProposta) -> Result<Event, String> {
    match (proposta.campo.as_str(), proposta.operacao.as_str()) {
        ("player.hp", "somar") => {
            let delta = proposta
                .valor
                .as_i64()
                .ok_or_else(|| "valor de player.hp nao e um inteiro".to_string())?;
            let novo = (player.hp.atual as i64 + delta).clamp(0, player.hp.maximo as i64);
            let anterior = player.hp.atual;
            player.hp.atual = novo as i32;
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.hp", "operacao": "somar", "valor": delta, "anterior": anterior, "atual": player.hp.atual }),
            ))
        }
        ("player.inventario", "adicionar") => {
            let item = proposta
                .valor
                .as_str()
                .ok_or_else(|| "valor de player.inventario nao e uma string".to_string())?
                .to_string();
            if player.inventario.contains(&item) {
                return Err(format!("item '{item}' ja esta no inventario, ignorando duplicata"));
            }
            player.inventario.push(item.clone());
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.inventario", "operacao": "adicionar", "valor": item }),
            ))
        }
        ("player.inventario", "remover") => {
            let item = proposta
                .valor
                .as_str()
                .ok_or_else(|| "valor de player.inventario nao e uma string".to_string())?
                .to_string();
            let pos = player
                .inventario
                .iter()
                .position(|i| i == &item)
                .ok_or_else(|| format!("item '{item}' nao esta no inventario, rejeitando remocao"))?;
            player.inventario.remove(pos);
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.inventario", "operacao": "remover", "valor": item }),
            ))
        }
        ("player.location_id", "definir") => {
            let destino = proposta
                .valor
                .as_str()
                .ok_or_else(|| "valor de player.location_id nao e uma string".to_string())?
                .to_string();
            if destino.trim().is_empty() {
                return Err("destino vazio, rejeitando".to_string());
            }
            let anterior = player.location_id.clone();
            player.location_id = destino.clone();
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.location_id", "operacao": "definir", "valor": destino, "anterior": anterior }),
            ))
        }
        (campo, operacao) => Err(format!("campo/operacao fora da whitelist: {campo} / {operacao}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Hp;

    fn jogador() -> Player {
        Player {
            id: "player_01".into(),
            hp: Hp { atual: 10, maximo: 20 },
            atributos: Default::default(),
            inventario: vec!["chave_enferrujada".into()],
            location_id: "taverna".into(),
            nivel: 1,
            classe: "guerreiro".into(),
            xp: 0,
        }
    }

    #[test]
    fn aplica_dano_e_clampa_no_minimo_zero() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.hp".into(), operacao: "somar".into(), valor: serde_json::json!(-100) };
        aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(p.hp.atual, 0);
    }

    #[test]
    fn aplica_cura_e_clampa_no_maximo() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.hp".into(), operacao: "somar".into(), valor: serde_json::json!(100) };
        aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(p.hp.atual, 20);
    }

    #[test]
    fn rejeita_remover_item_inexistente() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.inventario".into(), operacao: "remover".into(), valor: serde_json::json!("espada_lendaria") };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
        assert_eq!(p.inventario.len(), 1);
    }

    #[test]
    fn rejeita_campo_fora_da_whitelist() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.nivel".into(), operacao: "somar".into(), valor: serde_json::json!(1) };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
    }

    #[test]
    fn adiciona_item_novo_e_rejeita_duplicata() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.inventario".into(), operacao: "adicionar".into(), valor: serde_json::json!("mapa") };
        assert!(aplicar(&mut p, 0, &proposta).is_ok());
        assert!(aplicar(&mut p, 0, &proposta).is_err());
        assert_eq!(p.inventario.len(), 2);
    }

    #[test]
    fn move_jogador_para_novo_local() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.location_id".into(), operacao: "definir".into(), valor: serde_json::json!("floresta_negra") };
        assert!(aplicar(&mut p, 0, &proposta).is_ok());
        assert_eq!(p.location_id, "floresta_negra");
    }

    #[test]
    fn rejeita_destino_vazio() {
        let mut p = jogador();
        let proposta = MudancaProposta { campo: "player.location_id".into(), operacao: "definir".into(), valor: serde_json::json!("") };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
    }
}
