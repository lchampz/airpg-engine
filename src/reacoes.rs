use crate::consolidacao;
use crate::db;
use crate::guardrail::GuardrailSaida;
use crate::llm::LlmClient;
use crate::skills::ResultadoDados;
use crate::state::{Cena, MemoriaNpc, Npc};
use sqlx::sqlite::SqlitePool;

/// Ponto único de interação de um agente reativo com um jogador: carrega a
/// memória do par (NPC, jogador), monta o prompt com a Cena (fatos do Mestre
/// de Jogo, read-only) e o estado emocional/resumo/janela verbatim, gera a
/// resposta, filtra pelo Guardrail de Saída, registra a troca, consolida a
/// memória quando chega a hora, e persiste tudo. Compartilhado entre o
/// roteamento normal de turno (`/turn`) e o handoff do Mundo Vivo (colisão
/// vinda do Elixir) — ver Pool-de-Agentes / Mundo-Vivo.
pub async fn processar_interacao(
    pool: &SqlitePool,
    llm: &LlmClient,
    guardrail: &GuardrailSaida,
    npc: &Npc,
    player_id: &str,
    cena: &Cena,
    resultado_dados: Option<&ResultadoDados>,
    entrada: &str,
) -> String {
    let mut memoria = db::get_memoria(pool, &npc.id, player_id).await.unwrap_or_default();

    let texto = dialogar(llm, guardrail, npc, cena, &memoria, resultado_dados, entrada).await;

    db::registrar_troca(&mut memoria, entrada.to_string(), texto.clone());

    if memoria.turnos_desde_consolidacao >= db::TURNOS_POR_CONSOLIDACAO {
        if let Some(resultado) = consolidacao::consolidar(llm, &npc.nome, &memoria.resumo, &memoria.ctx).await {
            memoria.resumo = resultado.resumo;
            memoria.estado_emocional = resultado.estado_emocional;
            memoria.turnos_desde_consolidacao = 0;
        }
    }

    if let Err(err) = db::salvar_memoria(pool, &npc.id, player_id, &memoria).await {
        tracing::error!(%err, npc = %npc.id, %player_id, "falha ao persistir memoria do agente");
    }

    texto
}

async fn dialogar(
    llm: &LlmClient,
    guardrail: &GuardrailSaida,
    npc: &Npc,
    cena: &Cena,
    memoria: &MemoriaNpc,
    resultado_dados: Option<&ResultadoDados>,
    entrada: &str,
) -> String {
    let system = montar_system_prompt(npc, cena, memoria, resultado_dados);

    let bruto = match llm.complete_with_history(&system, &memoria.ctx, entrada).await {
        Ok(texto) => texto,
        Err(err) => {
            tracing::error!(%err, npc = %npc.id, "falha ao chamar o LLM para dialogo do agente");
            return format!("{} hesita, sem saber o que responder.", npc.nome);
        }
    };

    guardrail.revisar(&bruto, resultado_dados).await
}

fn montar_system_prompt(npc: &Npc, cena: &Cena, memoria: &MemoriaNpc, resultado_dados: Option<&ResultadoDados>) -> String {
    let mut partes = vec![format!(
        "Você é {}, um NPC num RPG de fantasia medieval. Sua atitude de base com o jogador é: {}.",
        npc.nome, npc.atitude_com_jogador
    )];

    partes.push(format!(
        "Você está em {} ({}). Fatos estabelecidos sobre este lugar — você NÃO pode contradizer ou inventar por cima deles: {}",
        cena.nome,
        cena.descricao,
        if cena.fatos_estabelecidos.is_empty() {
            "nenhum fato adicional registrado ainda.".to_string()
        } else {
            cena.fatos_estabelecidos.join("; ")
        }
    ));

    let e = &memoria.estado_emocional;
    let emocional_relevante = e.prazer.abs() > 0.1 || e.ativacao.abs() > 0.1 || e.dominancia.abs() > 0.1;
    if emocional_relevante {
        partes.push(format!(
            "Seu estado emocional atual em relação a este jogador: prazer={:.1}, ativação={:.1}, dominância={:.1}, confiança={:.1}, respeito={:.1}, afinidade={:.1}. {}{}",
            e.prazer, e.ativacao, e.dominancia, e.confianca, e.respeito, e.afinidade,
            if !e.lente_perceptiva.is_empty() { format!("Você está enxergando o jogador através de: {}. ", e.lente_perceptiva) } else { String::new() },
            if !e.motivacao_imediata.is_empty() { format!("Sua motivação imediata agora: {}.", e.motivacao_imediata) } else { String::new() },
        ));
    }

    if !memoria.resumo.is_empty() {
        partes.push(format!("Resumo do que já aconteceu entre vocês: {}", memoria.resumo));
    }

    if let Some(r) = resultado_dados {
        partes.push(format!(
            "O jogador tentou uma ação que exigiu um teste de dados. Resultado JÁ DECIDIDO, você não pode contradizer: rolagem {} contra dificuldade {} → {}. Narre sua reação de forma consistente com esse resultado.",
            r.rolagem,
            r.dificuldade,
            if r.sucesso { "SUCESSO" } else { "FRACASSO" }
        ));
    }

    partes.push(
        "Mantenha consistência com o que você mesmo disse antes e com os fatos estabelecidos. Responda em 1-2 frases curtas, em português, sempre em personagem, nunca saindo do papel.".to_string(),
    );

    partes.join("\n\n")
}
