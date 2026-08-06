mod db;
mod events;
mod guardrail;
mod llm;
mod orchestrator;
mod skills;
mod state;

use axum::{extract::State, routing::{get, post}, Json, Router};
use events::{AcaoJogadorPayload, Event, EventType};
use futures::StreamExt;
use guardrail::GuardrailSaida;
use llm::LlmClient;
use orchestrator::Orchestrator;
use sqlx::sqlite::SqlitePool;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

const NATS_SUBJECT: &str = "airpg.events";

#[derive(Clone)]
struct AppState {
    #[allow(dead_code)]
    pool: SqlitePool,
    orchestrator: Arc<Orchestrator>,
    guardrail_saida: Arc<GuardrailSaida>,
    nats: Option<async_nats::Client>,
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

    if let Some(client) = &nats {
        spawn_subscriber(client.clone());
    }

    let state = AppState {
        pool,
        orchestrator: Arc::new(Orchestrator),
        guardrail_saida: Arc::new(GuardrailSaida::new(llm.clone())),
        nats,
    };

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

/// Assina o barramento compartilhado com o airpg-world (Elixir) — hoje só loga
/// o que chega (ex: colisao_jogador_agente), a próxima iteração deve acionar
/// o Pool de Agentes reativo a partir daqui (ver Mundo-Vivo / Stack-Escolhida).
fn spawn_subscriber(client: async_nats::Client) {
    tokio::spawn(async move {
        match client.subscribe(NATS_SUBJECT).await {
            Ok(mut sub) => {
                while let Some(msg) = sub.next().await {
                    let payload = String::from_utf8_lossy(&msg.payload);
                    tracing::info!(%payload, "evento recebido do barramento (airpg.events)");
                }
            }
            Err(err) => tracing::error!(%err, "falha ao assinar {NATS_SUBJECT}"),
        }
    });
}

async fn health() -> &'static str {
    "ok"
}

/// Endpoint de turno: valida a ação (Guardrail de Entrada), gera a reação do
/// agente reativo via LLM, filtra pelo Guardrail de Saída, publica no NATS,
/// e devolve o evento final. Roteamento completo do Pool de Agentes (múltiplos
/// NPCs, cap de 4/turno) ainda não plugado aqui — este endpoint simula um
/// único agente reativo fixo para permitir debugar o comportamento da IA.
async fn processar_turno(
    State(app): State<AppState>,
    Json(acao): Json<AcaoJogadorPayload>,
) -> Json<Event> {
    let player = state::Player {
        id: "player_01".into(),
        hp: state::Hp { atual: 10, maximo: 10 },
        atributos: Default::default(),
        inventario: vec![],
        location_id: "taverna_porto_velho".into(),
        nivel: 1,
        classe: "guerreiro".into(),
    };

    let evento = match app.orchestrator.validar_acao(&player, &acao) {
        Err(rejeicao) => Event::new(
            EventType::AcaoRejeitada,
            "guardrail_entrada",
            0,
            serde_json::to_value(&rejeicao).unwrap(),
        ),
        Ok(()) => {
            let bruto = gerar_dialogo_agente(&acao).await;
            let aprovado = app.guardrail_saida.revisar(&bruto).await;

            Event::new(
                EventType::Dialogo,
                "npc_taverneiro",
                0,
                serde_json::json!({ "texto": aprovado }),
            )
        }
    };

    if let Some(client) = &app.nats {
        let body = serde_json::to_vec(&evento).unwrap_or_default();
        if let Err(err) = client.publish(NATS_SUBJECT, body.into()).await {
            tracing::warn!(%err, "falha ao publicar evento no NATS");
        }
    }

    Json(evento)
}

async fn gerar_dialogo_agente(acao: &AcaoJogadorPayload) -> String {
    let llm = LlmClient::from_env();
    let system = "Você é Bram, um taverneiro amigável e desconfiado de forasteiros, num RPG de fantasia medieval. Responda em 1-2 frases curtas, em português, em personagem, nunca saindo do papel.";
    match llm.complete(system, &acao.response).await {
        Ok(texto) => texto,
        Err(err) => {
            tracing::error!(%err, "falha ao chamar o LLM para dialogo do agente");
            "Bram franze a testa, sem saber o que responder.".to_string()
        }
    }
}
