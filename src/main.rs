mod db;
mod events;
mod guardrail;
mod jsonutil;
mod llm;
mod orchestrator;
mod reacoes;
mod skills;
mod state;
mod state_changes;

use axum::{extract::State, routing::{get, post}, Json, Router};
use events::{AcaoJogadorPayload, ColisaoJogadorAgentePayload, Event, EventType};
use futures::StreamExt;
use guardrail::GuardrailSaida;
use llm::LlmClient;
use orchestrator::Orchestrator;
use sqlx::sqlite::SqlitePool;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};
use tower_http::cors::{Any, CorsLayer};

const NATS_SUBJECT: &str = "airpg.events";
const PLAYER_ID: &str = "player_01";

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    orchestrator: Arc<Orchestrator>,
    llm: LlmClient,
    guardrail_saida: Arc<GuardrailSaida>,
    nats: Option<async_nats::Client>,
    turno: Arc<AtomicU64>,
}

#[derive(serde::Serialize)]
struct TurnResult {
    turno: u64,
    eventos: Vec<Event>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://airpg.db?mode=rwc".into());
    let pool = db::init_pool(&database_url).await?;

    let llm = LlmClient::from_env();

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".into());
    let nats = match async_nats::connect(&nats_url).await {
        Ok(client) => {
            tracing::info!(%nats_url, "conectado ao NATS");
            Some(client)
        }
        Err(err) => {
            tracing::warn!(%err, "NATS indisponivel, engine seguira sem barramento cross-process");
            None
        }
    };

    let state = AppState {
        pool,
        orchestrator: Arc::new(Orchestrator),
        llm: llm.clone(),
        guardrail_saida: Arc::new(GuardrailSaida::new(llm)),
        nats: nats.clone(),
        turno: Arc::new(AtomicU64::new(0)),
    };

    if let Some(client) = &nats {
        spawn_subscriber(client.clone(), state.clone());
    }

    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/turn", post(processar_turno))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("airpg-engine ouvindo em 0.0.0.0:8080");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

/// Endpoint de turno: valida a ação (Guardrail de Entrada), roteia até
/// MAX_AGENTES_POR_TURNO NPCs presentes na mesma location do jogador (ver
/// Orquestrador-Central / Decisoes-Resolvidas), gera a reação de cada um em
/// paralelo, filtra pelo Guardrail de Saída, publica no NATS e devolve o
/// lote de eventos do turno.
async fn processar_turno(
    State(app): State<AppState>,
    Json(acao): Json<AcaoJogadorPayload>,
) -> Json<TurnResult> {
    let turno = app.turno.fetch_add(1, Ordering::SeqCst);

    let mut player = match db::get_player(&app.pool, PLAYER_ID).await {
        Ok(Some(p)) => p,
        _ => {
            tracing::error!("player nao encontrado no estado rigido, usando fallback");
            state::Player {
                id: PLAYER_ID.into(),
                hp: state::Hp { atual: 10, maximo: 10 },
                atributos: Default::default(),
                inventario: vec![],
                location_id: "taverna_porto_velho".into(),
                nivel: 1,
                classe: "guerreiro".into(),
            }
        }
    };

    let eventos = match app.orchestrator.validar_acao(&player, &acao) {
        Err(rejeicao) => vec![Event::new(
            EventType::AcaoRejeitada,
            "guardrail_entrada",
            turno,
            serde_json::to_value(&rejeicao).unwrap(),
        )],
        Ok(()) => {
            let npcs = db::list_npcs(&app.pool).await.unwrap_or_default();
            let roteados = app.orchestrator.rotear_agentes(&player, &npcs);

            tracing::info!(
                turno,
                agentes = ?roteados.iter().map(|n| &n.id).collect::<Vec<_>>(),
                "agentes roteados para o turno"
            );

            let respostas = futures::future::join_all(roteados.iter().map(|npc| {
                let llm = app.llm.clone();
                let guardrail = app.guardrail_saida.clone();
                let npc = (*npc).clone();
                let texto_jogador = acao.response.clone();
                async move {
                    let texto = reacoes::dialogar(&llm, &guardrail, &npc, &texto_jogador).await;
                    (npc.id.clone(), texto)
                }
            }))
            .await;

            let mut eventos: Vec<Event> = respostas
                .iter()
                .map(|(npc_id, texto)| {
                    Event::new(EventType::Dialogo, npc_id.clone(), turno, serde_json::json!({ "texto": texto }))
                })
                .collect();

            // Persistência de mudança de estado: o LLM só sugere (ver
            // Estado-Rigido / state_changes.rs); o engine valida, aplica e
            // persiste. A grande maioria dos turnos não muda nada — só
            // eventos onde a proposta passa a whitelist entram no lote.
            let contexto = format!(
                "Ação do jogador: {}\n{}",
                acao.response,
                respostas.iter().map(|(id, t)| format!("{id} disse: {t}")).collect::<Vec<_>>().join("\n")
            );
            let propostas = state_changes::propor_mudancas(&app.llm, &contexto).await;
            for proposta in &propostas {
                match state_changes::aplicar(&mut player, turno, proposta) {
                    Ok(evento) => eventos.push(evento),
                    Err(motivo) => tracing::warn!(%motivo, "proposta de mudanca de estado rejeitada"),
                }
            }
            if !propostas.is_empty() {
                if let Err(err) = db::upsert_player(&app.pool, &player).await {
                    tracing::error!(%err, "falha ao persistir estado do jogador");
                }
            }

            eventos.push(app.orchestrator.evento_fim_de_turno(turno, &roteados));
            eventos
        }
    };

    publicar_lote(&app, &eventos).await;

    Json(TurnResult { turno, eventos })
}

async fn publicar_lote(app: &AppState, eventos: &[Event]) {
    let Some(client) = &app.nats else { return };
    for evento in eventos {
        let body = serde_json::to_vec(evento).unwrap_or_default();
        if let Err(err) = client.publish(NATS_SUBJECT, body.into()).await {
            tracing::warn!(%err, "falha ao publicar evento no NATS");
        }
    }
}

/// Assina o barramento compartilhado com o airpg-world (Elixir). Ao receber
/// `colisao_jogador_agente`, ativa o Pool de Agentes reativo para aquele NPC
/// especificamente — exatamente como o roteamento normal, o pool não sabe
/// (nem precisa saber) que veio de uma colisão autônoma (ver Pool-de-Agentes
/// / Mundo-Vivo). Ao final, publica `interacao_finalizada` para o Elixir
/// retomar a autonomia do NPC.
fn spawn_subscriber(client: async_nats::Client, app: AppState) {
    tokio::spawn(async move {
        let mut sub = match client.subscribe(NATS_SUBJECT).await {
            Ok(sub) => sub,
            Err(err) => {
                tracing::error!(%err, "falha ao assinar {NATS_SUBJECT}");
                return;
            }
        };

        while let Some(msg) = sub.next().await {
            let evento = match serde_json::from_slice::<Event>(&msg.payload) {
                Ok(evento) => evento,
                Err(err) => {
                    let corpo = String::from_utf8_lossy(&msg.payload);
                    tracing::warn!(%err, corpo = %corpo, "evento do barramento nao pode ser deserializado, ignorando");
                    continue;
                }
            };
            if evento.event_type != EventType::ColisaoJogadorAgente {
                continue;
            }

            let Ok(payload) = serde_json::from_value::<ColisaoJogadorAgentePayload>(evento.payload.clone()) else {
                tracing::warn!("colisao_jogador_agente com payload invalido");
                continue;
            };

            tracing::info!(agent_id = %payload.agent_id, "colisao recebida do mundo vivo, ativando pool de agentes");
            tokio::spawn(reagir_a_colisao(client.clone(), app.clone(), payload));
        }
    });
}

async fn reagir_a_colisao(client: async_nats::Client, app: AppState, colisao: ColisaoJogadorAgentePayload) {
    let npc = match db::get_npc(&app.pool, &colisao.agent_id).await {
        Ok(Some(npc)) => npc,
        _ => {
            tracing::warn!(agent_id = %colisao.agent_id, "npc da colisao nao encontrado no estado rigido");
            return;
        }
    };

    let turno = app.turno.fetch_add(1, Ordering::SeqCst);
    let abertura = "O NPC encontra o jogador por acaso.";
    let texto = reacoes::dialogar(&app.llm, &app.guardrail_saida, &npc, abertura).await;

    let evento_dialogo = Event::new(EventType::Dialogo, npc.id.clone(), turno, serde_json::json!({ "texto": texto }));
    publicar_lote(&app, &[evento_dialogo]).await;

    let evento_finalizado = Event::new(
        EventType::InteracaoFinalizada,
        "orquestrador",
        turno,
        serde_json::json!({ "agent_id": npc.id, "resultado": "interacao_reativa_concluida" }),
    );
    let body = serde_json::to_vec(&evento_finalizado).unwrap_or_default();
    if let Err(err) = client.publish(NATS_SUBJECT, body.into()).await {
        tracing::warn!(%err, "falha ao publicar interacao_finalizada");
    }
}
