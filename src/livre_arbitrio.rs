use crate::db;
use crate::events::{Event, EventType};
use crate::guardrail::GuardrailSaida;
use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::state::{Npc, NpcStatus};
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;

/// Ver Módulos 2+3+6 em Tarefas-Pendentes no vault: NPCs autônomos agem por
/// conta própria mesmo sem jogador interagindo com eles — o jogador é "mais
/// um personagem", não o centro do mundo. Deliberadamente conservador nesta
/// primeira versão: "mover" e "ambiente" só geram uma linha no Log Global
/// (sem mutar `location_id` de NPC — mover NPC de fato é um passo futuro,
/// arriscado o suficiente pra merecer sua própria rodada de validação). Só
/// "conversar" produz efeito visível ao jogador (Módulo 6).
const SYSTEM_PROMPT_ACAO_AUTONOMA: &str = r#"Você é o Mestre de Jogo de um RPG de fantasia medieval. Um personagem autônomo (NPC) vai agir por conta própria, sem jogador presente interagindo com ele agora. Decida uma ação plausível pra esse personagem, coerente com seu papel e personalidade.
Responda APENAS com um JSON no formato {"tipo": "mover"|"conversar"|"ambiente", "detalhe": "...", "participante_b_id": "id de outro personagem presente, ou vazio"}.
Use "conversar" só se houver outro personagem presente que faça sentido conversar com o ator agora (informe o id dele em participante_b_id). Use "ambiente" para uma ação simples do dia a dia (servir um cliente, patrulhar, arrumar algo). Use "mover" se o personagem teria razão pra ir a outro lugar (não decida o destino, só a intenção)."#;

#[derive(Debug, Deserialize)]
struct AcaoAutonomaProposta {
    #[serde(default)]
    tipo: String,
    #[serde(default)]
    detalhe: String,
    #[serde(default)]
    participante_b_id: String,
}

#[derive(Debug, Deserialize)]
struct ConversaProposta {
    #[serde(default)]
    falas: Vec<FalaProposta>,
}

#[derive(Debug, Deserialize)]
struct FalaProposta {
    npc_id: String,
    #[serde(default)]
    texto: String,
}

/// Um tick do Livre-Arbítrio: sorteia uma localização com NPCs elegíveis
/// (`autonomo: true`, vivo, não-combatente — mesmo filtro de
/// `orchestrator::rotear_agentes`), sorteia um deles como ator, e decide (via
/// LLM, perfil de custo "mundo") o que ele faz agora. Fire-and-forget: quem
/// chama (`main.rs::processar_turno`) não espera isso terminar.
pub async fn tick(pool: SqlitePool, llm: LlmClient, guardrail: std::sync::Arc<GuardrailSaida>, turno_global: u64) {
    let npcs = match db::list_npcs(&pool).await {
        Ok(n) => n,
        Err(err) => {
            tracing::warn!(%err, "livre-arbitrio: falha ao listar npcs");
            return;
        }
    };

    let mut por_local: HashMap<String, Vec<Npc>> = HashMap::new();
    for npc in npcs.into_iter().filter(|n| n.autonomo && n.status != NpcStatus::Morto && n.hp.is_none()) {
        por_local.entry(npc.location_id.clone()).or_default().push(npc);
    }

    let locais: Vec<&String> = por_local.keys().collect();
    if locais.is_empty() {
        return;
    }
    let location_id = locais[rand::random::<usize>() % locais.len()].clone();
    let candidatos = &por_local[&location_id];

    let ator = &candidatos[rand::random::<usize>() % candidatos.len()];
    tracing::info!(turno_global, %location_id, ator_id = %ator.id, candidatos = candidatos.len(), "livre-arbitrio: tick disparado");

    let lista = candidatos
        .iter()
        .map(|n| {
            let papel = if n.descricao.is_empty() { "sem papel definido" } else { &n.descricao };
            format!("{} (id: {}, papel: {})", n.nome, n.id, papel)
        })
        .collect::<Vec<_>>()
        .join("; ");
    let entrada = format!("Personagem agindo: {} (id: {})\nOutros presentes em {}: {}", ator.nome, ator.id, location_id, lista);

    let proposta = match llm.complete(SYSTEM_PROMPT_ACAO_AUTONOMA, &entrada).await {
        Ok(resposta) => extrair_json::<AcaoAutonomaProposta>(&resposta),
        Err(err) => {
            tracing::warn!(%err, "livre-arbitrio: falha ao decidir acao autonoma");
            None
        }
    };

    let Some(proposta) = proposta else { return };
    let timestamp = chrono::Utc::now().to_rfc3339();

    if proposta.tipo == "conversar" {
        let participante_b = candidatos.iter().find(|n| n.id == proposta.participante_b_id && n.id != ator.id);
        match participante_b {
            Some(b) => gerar_conversa(&pool, &llm, &guardrail, ator, b, &location_id, turno_global, &timestamp).await,
            // LLM propôs conversar mas não deu um participante_b_id válido —
            // mesmo princípio defensivo de mestre::avaliar_destinatarios: não
            // inventa participante, cai pra registrar como ambiente.
            None => registrar_ambiente(&pool, turno_global, ator, &location_id, "ambiente", &proposta.detalhe, &timestamp).await,
        }
    } else {
        registrar_ambiente(&pool, turno_global, ator, &location_id, &proposta.tipo, &proposta.detalhe, &timestamp).await;
    }
}

async fn registrar_ambiente(pool: &SqlitePool, turno_global: u64, ator: &Npc, location_id: &str, tipo_acao: &str, detalhe: &str, timestamp: &str) {
    let descricao = if detalhe.trim().is_empty() { format!("{} fez algo em {}", ator.nome, location_id) } else { detalhe.to_string() };
    if let Err(err) =
        db::registrar_acao_global(pool, turno_global as i64, &ator.id, "npc", tipo_acao, &descricao, location_id, timestamp).await
    {
        tracing::warn!(%err, "livre-arbitrio: falha ao registrar acao no log global");
    }
}

/// Gera uma troca curta entre dois NPCs (sem jogador), passa cada fala pelo
/// mesmo Guardrail de Saída usado no diálogo com o jogador, registra no Log
/// Global, e enfileira como evento ambiente pendente pra quem estiver na
/// mesma localização (ver `db::inserir_evento_ambiente_pendente`).
async fn gerar_conversa(
    pool: &SqlitePool,
    llm: &LlmClient,
    guardrail: &GuardrailSaida,
    a: &Npc,
    b: &Npc,
    location_id: &str,
    turno_global: u64,
    timestamp: &str,
) {
    let descricao_a = if a.descricao.is_empty() { "sem descricao".to_string() } else { a.descricao.clone() };
    let descricao_b = if b.descricao.is_empty() { "sem descricao".to_string() } else { b.descricao.clone() };
    let interesses_a = if a.interesses.is_empty() { String::new() } else { format!(" Motivações: {}.", a.interesses.join("; ")) };
    let interesses_b = if b.interesses.is_empty() { String::new() } else { format!(" Motivações: {}.", b.interesses.join("; ")) };
    let system = format!(
        "Você está narrando uma conversa breve e natural entre dois personagens de um RPG de fantasia medieval, sem o jogador presente.\n\
         {} (id: {}) — {}{}\n{} (id: {}) — {}{}\n\
         Responda APENAS com um JSON no formato {{\"falas\": [{{\"npc_id\": \"...\", \"texto\": \"...\"}}]}}, com 2 a 4 falas alternando entre os dois ids.\n\
         FORMATO OBRIGATÓRIO em cada texto: use *ação* para gesto/expressão (sem aspas) e -fala para diálogo direto (traço no início, sem aspas). Mantenha cada personagem na própria voz e papel, nunca invente fatos novos do mundo.",
        a.nome, a.id, descricao_a, interesses_a, b.nome, b.id, descricao_b, interesses_b,
    );
    let entrada = format!("Local: {location_id}. Gere a conversa agora.");

    let resposta = match llm.complete(&system, &entrada).await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(%err, "livre-arbitrio: falha ao gerar conversa ambiente");
            return;
        }
    };

    let Some(proposta) = extrair_json::<ConversaProposta>(&resposta) else {
        tracing::warn!(resposta = %resposta, "livre-arbitrio: resposta da conversa ambiente nao parseavel, descartando");
        return;
    };

    for fala in proposta.falas {
        if fala.npc_id != a.id && fala.npc_id != b.id {
            continue;
        }
        if fala.texto.trim().is_empty() {
            continue;
        }

        let texto_revisado = guardrail.revisar(&fala.texto, None).await;

        // Anti-repetição (ver Change-Economia-Viva-e-Consistencia): conversas
        // ambiente entre NPCs diferentes tendiam a abrir com a mesma saudação
        // genérica de sempre. Aqui não vale a pena pedir regeração de uma
        // fala isolada dentro de um JSON de várias — só descarta a fala
        // repetida, o resto da conversa segue.
        match db::frase_repetida(pool, location_id, &texto_revisado).await {
            Ok(Some(_)) => {
                tracing::info!(npc_id = %fala.npc_id, "livre-arbitrio: fala ambiente colidiu com frase recente, descartando");
                continue;
            }
            _ => {}
        }
        if let Err(err) = db::registrar_frase_recente(pool, location_id, &texto_revisado).await {
            tracing::warn!(%err, "livre-arbitrio: falha ao registrar frase recente");
        }

        if let Err(err) =
            db::registrar_acao_global(pool, turno_global as i64, &fala.npc_id, "npc", "conversa_ambiente", &texto_revisado, location_id, timestamp)
                .await
        {
            tracing::warn!(%err, "livre-arbitrio: falha ao registrar fala no log global");
        }

        let evento = Event::new(EventType::Dialogo, fala.npc_id.clone(), turno_global, serde_json::json!({ "texto": texto_revisado }));
        if let Err(err) = db::inserir_evento_ambiente_pendente(pool, location_id, &evento).await {
            tracing::warn!(%err, "livre-arbitrio: falha ao enfileirar evento ambiente");
        }
    }
}
