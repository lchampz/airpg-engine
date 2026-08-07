use crate::guardrail::GuardrailSaida;
use crate::llm::LlmClient;
use crate::state::Npc;

/// Ponto único de geração de reação de um agente reativo: chama o LLM com a
/// identidade do NPC e sua memória de curto prazo (ver Memoria-Narrativa no
/// vault), depois filtra pelo Guardrail de Saída. Compartilhado entre o
/// roteamento normal de turno (`/turn`) e o handoff do Mundo Vivo (colisão
/// vinda do Elixir) — ver Pool-de-Agentes / Mundo-Vivo.
///
/// Sem o histórico, o mesmo NPC "esquece" o que ele mesmo disse na mensagem
/// anterior e passa a inventar detalhes inconsistentes turno a turno — foi
/// exatamente esse bug que motivou este parâmetro.
pub async fn dialogar(
    llm: &LlmClient,
    guardrail: &GuardrailSaida,
    npc: &Npc,
    historico: &[(String, String)],
    entrada: &str,
) -> String {
    let system = format!(
        "Você é {}, um NPC num RPG de fantasia medieval. Sua atitude atual com o jogador é: {}. \
         Mantenha consistência com o que você mesmo disse antes (nome de lugares, promessas, fatos já estabelecidos). \
         Responda em 1-2 frases curtas, em português, sempre em personagem, nunca saindo do papel.",
        npc.nome, npc.atitude_com_jogador
    );

    let bruto = match llm.complete_with_history(&system, historico, entrada).await {
        Ok(texto) => texto,
        Err(err) => {
            tracing::error!(%err, npc = %npc.id, "falha ao chamar o LLM para dialogo do agente");
            return format!("{} hesita, sem saber o que responder.", npc.nome);
        }
    };

    guardrail.revisar(&bruto).await
}
