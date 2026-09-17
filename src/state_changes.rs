use crate::events::{Event, EventType};
use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::state::{ItemInventario, Npc, Player};
use serde::Deserialize;
use std::collections::HashMap;

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
    /// Só usado por ("player.inventario", "adicionar") — categoria livre do
    /// item novo (ex: "arma", "consumivel"). Ausente em propostas antigas ou
    /// em operações que não a usam.
    #[serde(default)]
    categoria: Option<String>,
}

const SYSTEM_PROMPT: &str = r#"Você é o sistema de regras de um RPG de fantasia medieval. Dado o que aconteceu no turno, decida se algo do ESTADO DO JOGO deve mudar de fato.
Responda APENAS com um JSON no formato {"mudancas": [{"campo": "...", "operacao": "...", "valor": ..., "categoria": "..."}]}.
Se nada mudou mecanicamente, responda {"mudancas": []} — a maioria dos turnos não muda nada, diálogo comum NUNCA é motivo de mudança.
Campos permitidos e suas operações:
- "player.hp" com operacao "somar" e valor um número inteiro (negativo para dano, positivo para cura)
- "player.moedas" com operacao "somar" e valor um número inteiro (negativo para gastar/pagar, positivo para ganhar/receber)
- "player.inventario" com operacao "adicionar" ou "remover" e valor uma string (nome do item). Ao "adicionar" um item, informe também "categoria": uma string curta e livre classificando o item (ex: "arma", "consumivel", "material", "missao") — se não souber classificar, use uma string vazia. Se o item foi COMPRADO de um vendedor, inclua TAMBÉM uma mudança separada em "player.moedas" com o valor pago (negativo) — ganhar um item sem essa contrapartida só é válido se o texto deixar claro que foi de graça, recompensa, achado ou roubado
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

/// Ver Change-Economia-Viva-e-Consistencia: quando o LLM propõe ganhar um
/// item cujo preço é conhecido (`Npc.precos` de algum NPC presente no
/// turno), mas não propõe a dedução simétrica de `player.moedas`, o Mestre
/// de Jogo é chamado de novo pra reescrever a própria lista de mudanças —
/// nunca aplicamos a proposta original inconsistente, e nunca inventamos a
/// correção no código (decisão do usuário: "o mestre de jogo reescreve a
/// cena", não um desconto silencioso).
pub async fn propor_e_validar(
    llm: &LlmClient,
    contexto: &str,
    npcs_presentes: &[&Npc],
) -> Vec<MudancaProposta> {
    let precos_conhecidos: HashMap<&str, u32> = npcs_presentes
        .iter()
        .flat_map(|n| n.precos.iter().map(|(item, preco)| (item.as_str(), *preco)))
        .collect();

    let propostas = propor_mudancas(llm, contexto).await;
    if precos_conhecidos.is_empty() {
        return propostas;
    }

    let moeda_proposta_valor = propostas
        .iter()
        .find(|p| p.campo == "player.moedas" && p.operacao == "somar")
        .and_then(|p| p.valor.as_i64());

    let item_com_preco_sem_pagamento = propostas.iter().find_map(|p| {
        if p.campo != "player.inventario" || p.operacao != "adicionar" {
            return None;
        }
        let item = p.valor.as_str()?;
        precos_conhecidos
            .get(item)
            .map(|&preco| (item.to_string(), preco))
    });

    if let Some((item, preco)) = &item_com_preco_sem_pagamento {
        let vendedor = npcs_presentes
            .iter()
            .find(|n| n.precos.contains_key(item))
            .map(|n| n.nome.as_str())
            .unwrap_or("o vendedor");
        match moeda_proposta_valor {
            None => {
                let entrada_corrigida = format!(
                    "{contexto}\n\nATENÇÃO: você propôs que o jogador ganhasse \"{item}\", que custa {preco} moedas na loja de {vendedor}. \
                     Isso só é válido se o texto acima deixa claro que foi de graça, recompensa, achado ou roubado. Reescreva a lista de mudanças: \
                     se foi uma compra normal, inclua também {{\"campo\": \"player.moedas\", \"operacao\": \"somar\", \"valor\": -{preco}}}. \
                     Se não houve pagamento nem justificativa clara no texto original, remova a proposta de ganhar \"{item}\"."
                );
                tracing::info!(%item, preco, %vendedor, "state_changes: item ganho sem contrapartida, pedindo correcao ao mestre de jogo");
                return propor_mudancas(llm, &entrada_corrigida).await;
            }
            // Cobrou algo, mas não o preço certo — o preço é FIXO por NPC
            // (decisão do usuário), então a narrativa não decide o valor.
            Some(v) if v != -(*preco as i64) => {
                let entrada_corrigida = format!(
                    "{contexto}\n\nATENÇÃO: \"{item}\" tem preço FIXO de {preco} moedas na loja de {vendedor} — o preço não é negociável nem decidido pela narração. \
                     Reescreva a lista de mudanças usando exatamente {{\"campo\": \"player.moedas\", \"operacao\": \"somar\", \"valor\": -{preco}}} para o pagamento."
                );
                tracing::info!(%item, preco, valor_proposto = v, %vendedor, "state_changes: valor pago nao bate com preco fixo, corrigindo");
                return propor_mudancas(llm, &entrada_corrigida).await;
            }
            Some(_) => {}
        }
    }

    // Caso simétrico: o jogador pagou um valor que bate exatamente com o
    // preço de algum item conhecido, mas o item nunca foi proposto pro
    // inventário — pagamento "no vazio". Mesma filosofia: o Mestre de Jogo
    // reescreve, o código nunca insere o item sozinho.
    let valor_pago = moeda_proposta_valor.filter(|v| *v < 0).map(|v| (-v) as u32);

    if let Some(preco_pago) = valor_pago {
        let item_correspondente = precos_conhecidos
            .iter()
            .find(|(_, &preco)| preco == preco_pago)
            .map(|(&item, _)| item);
        if let Some(item) = item_correspondente {
            let ja_ganha_este_item = propostas.iter().any(|p| {
                p.campo == "player.inventario"
                    && p.operacao == "adicionar"
                    && p.valor.as_str() == Some(item)
            });
            if !ja_ganha_este_item {
                let vendedor = npcs_presentes
                    .iter()
                    .find(|n| n.precos.get(item) == Some(&preco_pago))
                    .map(|n| n.nome.as_str())
                    .unwrap_or("o vendedor");
                let entrada_corrigida = format!(
                    "{contexto}\n\nATENÇÃO: você propôs debitar {preco_pago} moedas do jogador — valor que corresponde exatamente ao preço de \"{item}\" \
                     cobrado por {vendedor} — mas não incluiu a proposta de adicionar \"{item}\" ao inventário do jogador. Reescreva a lista de mudanças \
                     incluindo também {{\"campo\": \"player.inventario\", \"operacao\": \"adicionar\", \"valor\": \"{item}\", \"categoria\": \"consumivel\"}}, \
                     a menos que o texto deixe claro que o pagamento foi por outro motivo (ex: multa, aluguel, gorjeta) e não por esse item."
                );
                tracing::info!(%item, preco_pago, %vendedor, "state_changes: pagamento sem item correspondente, pedindo correcao ao mestre de jogo");
                return propor_mudancas(llm, &entrada_corrigida).await;
            }
        }
    }

    propostas
}

/// Aplica uma proposta ao Player em memória (o chamador é responsável por
/// persistir depois). Retorna o evento `mudanca_estado` se aplicada, ou uma
/// razão de rejeição em texto.
pub fn aplicar(
    player: &mut Player,
    turno: u64,
    proposta: &MudancaProposta,
) -> Result<Event, String> {
    match (proposta.campo.as_str(), proposta.operacao.as_str()) {
        ("player.hp", "somar") => {
            let delta = proposta
                .valor
                .as_i64()
                .ok_or_else(|| "valor de player.hp nao e um inteiro".to_string())?;
            let novo = (player.hp.atual as i64 + delta).clamp(0, player.hp.maximo as i64);
            let anterior = player.hp.atual;
            if novo as i32 == anterior {
                return Err(
                    "player.hp: proposta nao muda o valor atual (no-op), descartando".to_string(),
                );
            }
            player.hp.atual = novo as i32;
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.hp", "operacao": "somar", "valor": delta, "anterior": anterior, "atual": player.hp.atual }),
            ))
        }
        ("player.moedas", "somar") => {
            let delta = proposta
                .valor
                .as_i64()
                .ok_or_else(|| "valor de player.moedas nao e um inteiro".to_string())?;
            let novo = (player.moedas as i64 + delta).max(0);
            let anterior = player.moedas;
            if novo as u32 == anterior {
                return Err(
                    "player.moedas: proposta nao muda o valor atual (no-op), descartando"
                        .to_string(),
                );
            }
            player.moedas = novo as u32;
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.moedas", "operacao": "somar", "valor": delta, "anterior": anterior, "atual": player.moedas }),
            ))
        }
        ("player.inventario", "adicionar") => {
            let item = proposta
                .valor
                .as_str()
                .ok_or_else(|| "valor de player.inventario nao e uma string".to_string())?
                .to_string();
            let quantidade = match player.inventario.iter_mut().find(|i| i.nome == item) {
                Some(existente) => {
                    existente.quantidade += 1;
                    existente.quantidade
                }
                None => {
                    player.inventario.push(ItemInventario {
                        nome: item.clone(),
                        quantidade: 1,
                        categoria: proposta.categoria.clone().unwrap_or_default(),
                    });
                    1
                }
            };
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.inventario", "operacao": "adicionar", "valor": item, "quantidade": quantidade }),
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
                .position(|i| i.nome == item)
                .ok_or_else(|| {
                    format!("item '{item}' nao esta no inventario, rejeitando remocao")
                })?;
            player.inventario[pos].quantidade -= 1;
            let quantidade = player.inventario[pos].quantidade;
            if quantidade == 0 {
                player.inventario.remove(pos);
            }
            Ok(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.inventario", "operacao": "remover", "valor": item, "quantidade": quantidade }),
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
        (campo, operacao) => Err(format!(
            "campo/operacao fora da whitelist: {campo} / {operacao}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Hp;

    fn jogador() -> Player {
        Player {
            id: "player_01".into(),
            hp: Hp {
                atual: 10,
                maximo: 20,
            },
            atributos: Default::default(),
            inventario: vec![ItemInventario {
                nome: "chave_enferrujada".into(),
                quantidade: 1,
                categoria: String::new(),
            }],
            location_id: "taverna".into(),
            nivel: 1,
            classe: "guerreiro".into(),
            xp: 0,
            nome_personagem: None,
            moedas: 15,
        }
    }

    #[test]
    fn rejeita_hp_no_op_quando_ja_esta_no_maximo() {
        let mut p = jogador();
        p.hp.atual = p.hp.maximo;
        let proposta = MudancaProposta {
            campo: "player.hp".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(5),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
        assert_eq!(p.hp.atual, p.hp.maximo);
    }

    #[test]
    fn aplica_gasto_de_moedas_e_nao_deixa_negativo() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.moedas".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(-5),
            categoria: None,
        };
        aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(p.moedas, 10);

        let proposta_excede = MudancaProposta {
            campo: "player.moedas".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(-1000),
            categoria: None,
        };
        aplicar(&mut p, 0, &proposta_excede).unwrap();
        assert_eq!(p.moedas, 0);
    }

    #[test]
    fn rejeita_moedas_no_op() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.moedas".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(0),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
    }

    #[test]
    fn evento_de_inventario_carrega_quantidade() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.inventario".into(),
            operacao: "adicionar".into(),
            valor: serde_json::json!("cerveja"),
            categoria: Some("consumivel".into()),
        };
        let evento = aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(evento.payload["quantidade"], 1);

        let evento2 = aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(evento2.payload["quantidade"], 2);
    }

    fn npc_vendedor(nome: &str, item: &str, preco: u32) -> Npc {
        use crate::state::NpcStatus;
        Npc {
            id: format!("npc_{nome}"),
            nome: nome.into(),
            status: NpcStatus::Vivo,
            atitude_com_jogador: "neutro".into(),
            location_id: "taverna".into(),
            autonomo: true,
            hp: None,
            classe_armadura: None,
            dano_dado_faces: None,
            xp_recompensa: None,
            loot: vec![],
            descricao: String::new(),
            deslocamento: None,
            imunidades: vec![],
            resistencias: vec![],
            moedas: Some(50),
            precos: [(item.to_string(), preco)].into_iter().collect(),
            interesses: vec![],
            temperamento_base: Default::default(),
        }
    }

    #[test]
    fn npc_vendedor_tem_preco_consultavel() {
        let bram = npc_vendedor("bram", "cerveja", 5);
        assert_eq!(bram.precos.get("cerveja"), Some(&5));
    }

    #[test]
    fn aplica_dano_e_clampa_no_minimo_zero() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.hp".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(-100),
            categoria: None,
        };
        aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(p.hp.atual, 0);
    }

    #[test]
    fn aplica_cura_e_clampa_no_maximo() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.hp".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(100),
            categoria: None,
        };
        aplicar(&mut p, 0, &proposta).unwrap();
        assert_eq!(p.hp.atual, 20);
    }

    #[test]
    fn rejeita_remover_item_inexistente() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.inventario".into(),
            operacao: "remover".into(),
            valor: serde_json::json!("espada_lendaria"),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
        assert_eq!(p.inventario.len(), 1);
    }

    #[test]
    fn rejeita_campo_fora_da_whitelist() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.nivel".into(),
            operacao: "somar".into(),
            valor: serde_json::json!(1),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
    }

    #[test]
    fn adiciona_item_novo_e_incrementa_quantidade_em_repeticao() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.inventario".into(),
            operacao: "adicionar".into(),
            valor: serde_json::json!("mapa"),
            categoria: Some("material".into()),
        };
        assert!(aplicar(&mut p, 0, &proposta).is_ok());
        assert!(aplicar(&mut p, 0, &proposta).is_ok());
        assert_eq!(p.inventario.len(), 2);
        let mapa = p.inventario.iter().find(|i| i.nome == "mapa").unwrap();
        assert_eq!(mapa.quantidade, 2);
        assert_eq!(mapa.categoria, "material");
    }

    #[test]
    fn remove_item_decrementa_e_so_apaga_quando_zera() {
        let mut p = jogador();
        let proposta_add = MudancaProposta {
            campo: "player.inventario".into(),
            operacao: "adicionar".into(),
            valor: serde_json::json!("chave_enferrujada"),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta_add).is_ok());
        assert_eq!(
            p.inventario
                .iter()
                .find(|i| i.nome == "chave_enferrujada")
                .unwrap()
                .quantidade,
            2
        );

        let proposta_remover = MudancaProposta {
            campo: "player.inventario".into(),
            operacao: "remover".into(),
            valor: serde_json::json!("chave_enferrujada"),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta_remover).is_ok());
        assert_eq!(
            p.inventario
                .iter()
                .find(|i| i.nome == "chave_enferrujada")
                .unwrap()
                .quantidade,
            1
        );

        assert!(aplicar(&mut p, 0, &proposta_remover).is_ok());
        assert!(p
            .inventario
            .iter()
            .find(|i| i.nome == "chave_enferrujada")
            .is_none());
    }

    #[test]
    fn move_jogador_para_novo_local() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.location_id".into(),
            operacao: "definir".into(),
            valor: serde_json::json!("floresta_negra"),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta).is_ok());
        assert_eq!(p.location_id, "floresta_negra");
    }

    #[test]
    fn rejeita_destino_vazio() {
        let mut p = jogador();
        let proposta = MudancaProposta {
            campo: "player.location_id".into(),
            operacao: "definir".into(),
            valor: serde_json::json!(""),
            categoria: None,
        };
        assert!(aplicar(&mut p, 0, &proposta).is_err());
    }
}
