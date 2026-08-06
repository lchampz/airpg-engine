mod db;
mod events;
mod orchestrator;
mod skills;
mod state;

use axum::{extract::State, routing::{get, post}, Json, Router};
use events::{AcaoJogadorPayload, Event, EventType};
use orchestrator::Orchestrator;
use sqlx::sqlite::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    #[allow(dead_code)]
    pool: SqlitePool,
    orchestrator: Arc<Orchestrator>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://airpg.db?mode=rwc".into());
    let pool = db::init_pool(&database_url).await?;

    let state = AppState {
        pool,
        orchestrator: Arc::new(Orchestrator),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/turn", post(processar_turno))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("airpg-engine ouvindo em 0.0.0.0:8080");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health() -> &'static str {
    "ok"
}

/// Endpoint mínimo de turno: recebe a ação do jogador, roda o Guardrail de Entrada,
/// e devolve o evento resultante. Roteamento para o Pool de Agentes e integração com
/// o Mundo Vivo (NATS) ficam para a próxima iteração — este é o esqueleto inicial.
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
        Ok(()) => Event::new(
            EventType::AcaoValidada,
            "guardrail_entrada",
            0,
            serde_json::to_value(&acao).unwrap(),
        ),
        Err(rejeicao) => Event::new(
            EventType::AcaoRejeitada,
            "guardrail_entrada",
            0,
            serde_json::to_value(&rejeicao).unwrap(),
        ),
    };

    Json(evento)
}
