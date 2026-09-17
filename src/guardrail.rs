use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::skills::ResultadoDados;
use serde::Deserialize;

/// Guardrail de Saída: filtra o conteúdo narrativo gerado antes de ele chegar
/// ao jogador. Ver Guardrail-Narrativo / Decisoes-Resolvidas — no MVP, no
/// máximo 2 tentativas de reprocessamento antes de cair para um fallback
/// neutro.
///
/// Quando um teste de dados foi rolado no turno (ver `mestre::avaliar_verificacao`
/// / `skills::skill_dados`), a mesma chamada também valida que a narração não
/// contradiz o resultado numérico — sem isso, nada impede a IA de narrar um
/// sucesso mesmo quando o dado disse fracasso.
pub struct GuardrailSaida {
    llm: LlmClient,
}

#[derive(Debug, Deserialize)]
struct Veredito {
    aprovado: bool,
    #[serde(default)]
    motivo: Option<String>,
}

const SYSTEM_PROMPT_BASE: &str = r#"Você é um revisor de conteúdo narrativo de um RPG de fantasia medieval. Sua tendência padrão deve ser APROVAR: a esmagadora maioria das falas de NPC é válida. Só reprove citando uma frase EXATA do texto revisado que viole um critério abaixo — se não conseguir citar a frase, aprove.
Responda APENAS com um JSON no formato {"aprovado": true|false, "motivo": "..."}.

Critérios de reprovação (todos exigem violação explícita e literal no texto, nunca suposição):
- anacronismo: menção clara a tecnologia, objeto ou termo do mundo real moderno (ex: celular, carro, internet, uma marca comercial real). Palavras genéricas como "disputa", "reunião", "negócio", "comércio" NÃO são anacronismo, mesmo que soem administrativas.
- antijogo: o texto narra o RESULTADO de uma ação de risco do jogador em vez de reagir a ela (ex.: "-Você derrota o bandido", "-Você consegue arrombar a porta") — decidir resultados de risco é papel do sistema de regras, não do NPC. Responder uma pergunta direta (preço, direção, nome) não é antijogo.
- alucinação de estado: o texto contradiz um fato listado em "fatos estabelecidos" ou nos itens do jogador informados no prompt. IMPORTANTE: todo personagem que fala nesta revisão já está vivo por definição do sistema (personagens mortos nunca chegam a esta etapa) — NUNCA reprove por "personagem morto falando" nesta revisão, essa categoria não se aplica aqui.

Diálogo comum, opiniões de NPC, menções a conflitos/política/preços do mundo do jogo, e formatação com *ação*/-fala são sempre válidos e devem ser aprovados."#;

const MAX_TENTATIVAS: u32 = 2;

impl GuardrailSaida {
    pub fn new(llm: LlmClient) -> Self {
        Self { llm }
    }

    /// Retorna o texto aprovado (o original, se aprovado; um fallback neutro
    /// caso reprovado nas MAX_TENTATIVAS tentativas). `resultado_dados`, se
    /// presente, é o resultado (já rolado, já determinístico) que a narração
    /// não pode contradizer.
    pub async fn revisar(
        &self,
        texto_gerado: &str,
        resultado_dados: Option<&ResultadoDados>,
    ) -> String {
        let system = montar_system_prompt(resultado_dados);

        for tentativa in 1..=MAX_TENTATIVAS {
            match self.llm.complete(&system, texto_gerado).await {
                Ok(resposta) => match extrair_json::<Veredito>(&resposta) {
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

        "*O narrador hesita por um momento, incapaz de descrever o que aconteceu com clareza.*"
            .to_string()
    }
}

fn montar_system_prompt(resultado_dados: Option<&ResultadoDados>) -> String {
    match resultado_dados {
        None => SYSTEM_PROMPT_BASE.to_string(),
        Some(r) => format!(
            "{SYSTEM_PROMPT_BASE}\nUm teste de dados foi rolado para esta ação: rolagem {} contra dificuldade {} → resultado {}. \
             Reprove também se a narração contradisser esse resultado (ex: descrever sucesso quando o resultado foi fracasso, ou vice-versa).",
            r.rolagem,
            r.dificuldade,
            if r.sucesso { "SUCESSO" } else { "FRACASSO" }
        ),
    }
}
