use crate::guardrail::GuardrailSaida;
use crate::llm::LlmClient;
use crate::state::Npc;

/// Ponto único de geração de reação de um agente reativo: chama o LLM com a
/// identidade do NPC, depois filtra pelo Guardrail de Saída. Compartilhado
/// entre o roteamento normal de turno (`/turn`) e o handoff do Mundo Vivo
/// (colisão vinda do Elixir) — ver Pool-de-Agentes / Mundo-Vivo.
pub async fn dialogar(llm: &LlmClient, guardrail: &GuardrailSaida, npc: &Npc, entrada: &str) -> String {
    let system = format!(
        "Você é {}, um NPC num RPG de fantasia medieval. Sua atitude atual com o jogador é: {}. \
         Responda em 1-2 frases curtas, em português, sempre em personagem, nunca saindo do papel.",
        npc.nome, npc.atitude_com_jogador
    );

    let bruto = match llm.complete(&system, entrada).await {
        Ok(texto) => texto,
        Err(err) => {
            tracing::error!(%err, npc = %npc.id, "falha ao chamar o LLM para dialogo do agente");
            return format!("{} hesita, sem saber o que responder.", npc.nome);
        }
    };

    guardrail.revisar(&bruto).await
}
