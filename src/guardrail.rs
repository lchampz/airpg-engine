use crate::llm::LlmClient;
use serde::Deserialize;

/// Guardrail de Saída: filtra o conteúdo narrativo gerado antes de ele chegar
/// ao jogador. Ver Guardrail-Narrativo / Decisoes-Resolvidas — no MVP, no
/// máximo 2 tentativas de reprocessamento antes de cair para um fallback
/// neutro.
pub struct GuardrailSaida {
    llm: LlmClient,
}

#[derive(Debug, Deserialize)]
struct Veredito {
    aprovado: bool,
    #[serde(default)]
    motivo: Option<String>,
}

const SYSTEM_PROMPT: &str = r#"Você é um revisor de conteúdo narrativo de um RPG de fantasia medieval.
Responda APENAS com um JSON no formato {"aprovado": true|false, "motivo": "..."}.
Reprove se houver: anacronismo (referências fora da época medieval/fantasia), antijogo (resolver o desafio pelo jogador), ou alucinação de estado (personagens mortos falando, itens inexistentes).
Aprove qualquer diálogo normal de fantasia medieval."#;

const MAX_TENTATIVAS: u32 = 2;

impl GuardrailSaida {
    pub fn new(llm: LlmClient) -> Self {
        Self { llm }
    }

    /// Retorna o texto aprovado (o original, se aprovado; um fallback neutro
    /// caso reprovado nas MAX_TENTATIVAS tentativas).
    pub async fn revisar(&self, texto_gerado: &str) -> String {
        for tentativa in 1..=MAX_TENTATIVAS {
            match self.llm.complete(SYSTEM_PROMPT, texto_gerado).await {
                Ok(resposta) => match parse_veredito(&resposta) {
                    Some(v) if v.aprovado => return texto_gerado.to_string(),
                    Some(v) => {
                        let motivo = v.motivo.unwrap_or_else(|| "sem motivo informado".into());
                        tracing::warn!(tentativa, %motivo, "guardrail de saida reprovou o conteudo");
                    }
                    None => {
                        tracing::warn!(tentativa, resposta = %resposta, "guardrail de saida: resposta nao parseavel, tratando como aprovado");
                        return texto_gerado.to_string();
                    }
                },
                Err(err) => {
                    tracing::error!(%err, "falha ao chamar o guardrail de saida");
                    return texto_gerado.to_string();
                }
            }
        }

        "O narrador hesita por um momento, incapaz de descrever o que aconteceu com clareza.".to_string()
    }
}

fn parse_veredito(resposta: &str) -> Option<Veredito> {
    let inicio = resposta.find('{')?;
    let fim = resposta.rfind('}')?;
    serde_json::from_str(&resposta[inicio..=fim]).ok()
}
