use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::state::EstadoEmocional;
use serde::Deserialize;

/// Consolidação de memória (ver Memoria-em-Camadas no vault): a cada
/// `db::TURNOS_POR_CONSOLIDACAO` turnos de um par (NPC, jogador), uma única
/// chamada de LLM resume o que saiu da janela verbatim e atualiza o estado
/// emocional — em vez de deixar a janela crua crescer sem limite.
const SYSTEM_PROMPT: &str = r#"Você analisa uma conversa entre um NPC de RPG e um jogador para atualizar a memória de longo prazo do NPC.
Responda APENAS com um JSON no formato:
{
  "resumo": "1-3 frases resumindo o relacionamento e eventos importantes até agora",
  "estado_emocional": {
    "prazer": -1.0 a 1.0 (quão agradável o NPC sente a interação, -1 sofrimento, 1 êxtase),
    "ativacao": -1.0 a 1.0 (nível de energia/alerta, -1 letárgico, 1 agitado/furioso),
    "dominancia": -1.0 a 1.0 (sentimento de controle, -1 submisso/impotente, 1 dominante/protetor),
    "confianca": -1.0 a 1.0 (quanto o NPC acredita no jogador),
    "respeito": -1.0 a 1.0,
    "afinidade": -1.0 a 1.0 (calor/frieza),
    "tags": ["rótulos curtos de eventos marcantes, ex: traidor_recente, divida_nao_paga"],
    "lente_perceptiva": "breve descrição de como o NPC está enquadrando o jogador agora, ex: Paranoia Defensiva",
    "motivacao_imediata": "o que o NPC quer fazer agora nesta conversa, em poucas palavras"
  }
}
Se nada de emocionalmente relevante aconteceu, ainda assim preencha os campos refletindo o estado atual (não invente eventos que não ocorreram)."#;

#[derive(Debug, Deserialize)]
struct ConsolidacaoGerada {
    resumo: String,
    estado_emocional: EstadoEmocional,
}

pub struct ResultadoConsolidacao {
    pub resumo: String,
    pub estado_emocional: EstadoEmocional,
}

/// Roda a consolidação. `resumo_atual` entra no prompt para a nova versão
/// substituir (não empilhar) a antiga — o tamanho fica aproximadamente
/// constante ao longo do jogo.
pub async fn consolidar(
    llm: &LlmClient,
    npc_nome: &str,
    resumo_atual: &str,
    trocas: &[(String, String)],
) -> Option<ResultadoConsolidacao> {
    let historico = trocas
        .iter()
        .map(|(p, r)| format!("Jogador: {p}\n{npc_nome}: {r}"))
        .collect::<Vec<_>>()
        .join("\n");
    let entrada = format!("Resumo anterior: {resumo_atual}\n\nTrocas recentes:\n{historico}");

    let resposta = match llm.complete(SYSTEM_PROMPT, &entrada).await {
        Ok(r) => r,
        Err(err) => {
            tracing::error!(%err, npc = %npc_nome, "falha ao consolidar memoria");
            return None;
        }
    };

    let gerada = match extrair_json::<ConsolidacaoGerada>(&resposta) {
        Some(g) => g,
        None => {
            tracing::warn!(resposta = %resposta, "consolidacao de memoria nao parseavel, mantendo estado anterior");
            return None;
        }
    };

    Some(ResultadoConsolidacao { resumo: gerada.resumo, estado_emocional: limitar_bounds(gerada.estado_emocional) })
}

/// Só os eixos numéricos são limitados — são os únicos usados por decisão de
/// código. Campos de texto livre (tags/lente/motivação) não são whitelisted,
/// servem só de contexto rico pro prompt (ver doc do tipo `EstadoEmocional`).
fn limitar_bounds(mut e: EstadoEmocional) -> EstadoEmocional {
    e.prazer = e.prazer.clamp(-1.0, 1.0);
    e.ativacao = e.ativacao.clamp(-1.0, 1.0);
    e.dominancia = e.dominancia.clamp(-1.0, 1.0);
    e.confianca = e.confianca.clamp(-1.0, 1.0);
    e.respeito = e.respeito.clamp(-1.0, 1.0);
    e.afinidade = e.afinidade.clamp(-1.0, 1.0);
    e.tags.truncate(5);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limita_eixos_fora_do_intervalo() {
        let e = EstadoEmocional { prazer: 5.0, ativacao: -5.0, dominancia: 2.0, confianca: -2.0, respeito: 0.5, afinidade: 0.0, tags: vec![], lente_perceptiva: String::new(), motivacao_imediata: String::new() };
        let limitado = limitar_bounds(e);
        assert_eq!(limitado.prazer, 1.0);
        assert_eq!(limitado.ativacao, -1.0);
        assert_eq!(limitado.dominancia, 1.0);
        assert_eq!(limitado.confianca, -1.0);
    }

    #[test]
    fn limita_quantidade_de_tags() {
        let e = EstadoEmocional { tags: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into(), "f".into()], ..Default::default() };
        let limitado = limitar_bounds(e);
        assert_eq!(limitado.tags.len(), 5);
    }
}
