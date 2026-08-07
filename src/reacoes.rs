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

    let texto = dialogar(pool, llm, guardrail, npc, cena, &memoria, resultado_dados, entrada).await;

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
    pool: &SqlitePool,
    llm: &LlmClient,
    guardrail: &GuardrailSaida,
    npc: &Npc,
    cena: &Cena,
    memoria: &MemoriaNpc,
    resultado_dados: Option<&ResultadoDados>,
    entrada: &str,
) -> String {
    let texto = gerar_e_revisar(llm, guardrail, npc, cena, memoria, resultado_dados, entrada, None).await;

    // Anti-repetição (ver Change-Economia-Viva-e-Consistencia): NPCs
    // diferentes (ou o mesmo NPC em sessões diferentes) tendiam a abrir com
    // a mesma saudação genérica. Uma colisão só pede UMA nova geração — não
    // vale a pena um loop, e "quase igual de novo" ainda é melhor que nada.
    let texto = match db::frase_repetida(pool, &npc.location_id, &texto).await {
        Ok(Some(frase_antiga)) => {
            tracing::info!(npc = %npc.id, "reacoes: fala colidiu com frase recente, pedindo nova geracao");
            gerar_e_revisar(llm, guardrail, npc, cena, memoria, resultado_dados, entrada, Some(&frase_antiga)).await
        }
        _ => texto,
    };

    if let Err(err) = db::registrar_frase_recente(pool, &npc.location_id, &texto).await {
        tracing::warn!(%err, npc = %npc.id, "falha ao registrar frase recente");
    }

    texto
}

async fn gerar_e_revisar(
    llm: &LlmClient,
    guardrail: &GuardrailSaida,
    npc: &Npc,
    cena: &Cena,
    memoria: &MemoriaNpc,
    resultado_dados: Option<&ResultadoDados>,
    entrada: &str,
    evitar_frase: Option<&str>,
) -> String {
    let system = montar_system_prompt(npc, cena, memoria, resultado_dados, evitar_frase);

    let bruto = match llm.complete_with_history(&system, &memoria.ctx, entrada).await {
        Ok(texto) => texto,
        Err(err) => {
            tracing::error!(%err, npc = %npc.id, "falha ao chamar o LLM para dialogo do agente");
            return format!("*{} hesita, sem saber o que responder.*", npc.nome);
        }
    };

    // Checagem de formato (*ação*/-fala) é só observabilidade por ora, não
    // reprovação — o guardrail de saída (llama3.2 local) já rejeita respostas
    // válidas com frequência alta por outros critérios (ver Memoria-Narrativa
    // no vault); adicionar mais um motivo de reprovação numa chamada que já é
    // frágil arriscava piorar a taxa de fallback em vez de melhorar a
    // formatação. O parser do frontend já trata texto sem marcadores como
    // fala simples, então a UI não quebra de qualquer forma.
    if !bruto.contains('*') && !bruto.contains('-') {
        tracing::debug!(npc = %npc.id, "resposta sem marcadores de acao/fala (*acao*/-fala)");
    }

    guardrail.revisar(&bruto, resultado_dados).await
}

fn montar_system_prompt(npc: &Npc, cena: &Cena, memoria: &MemoriaNpc, resultado_dados: Option<&ResultadoDados>, evitar_frase: Option<&str>) -> String {
    let mut partes = vec![format!(
        "Você é {}, um NPC num RPG de fantasia medieval. Sua atitude de base com o jogador é: {}.{}",
        npc.nome,
        npc.atitude_com_jogador,
        if npc.descricao.is_empty() {
            String::new()
        } else {
            format!(" {}", npc.descricao)
        }
    )];

    if !npc.interesses.is_empty() {
        partes.push(format!("Suas motivações/necessidades concretas: {}.", npc.interesses.join("; ")));
    }

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

    partes.push(
        "FORMATO OBRIGATÓRIO: use *ação* para narrar gesto/expressão (sem aspas), e -fala para diálogo direto (traço no início da frase, sem aspas). \
         Pode misturar os dois. Exemplos: '*cruza os braços* -Não recebo estranhos de bom grado.' ou '-Saia daqui. *aponta para a porta*'."
            .to_string(),
    );

    if let Some(frase) = evitar_frase {
        partes.push(format!(
            "IMPORTANTE: você (ou outro personagem por aqui) já disse algo muito parecido com isto recentemente — não repita: \"{frase}\". Responda de um jeito genuinamente diferente."
        ));
    }

    partes.join("\n\n")
}
