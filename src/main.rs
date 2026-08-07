mod consolidacao;
mod db;
mod events;
mod guardrail;
mod jsonutil;
mod llm;
mod mestre;
mod orchestrator;
mod reacoes;
mod skills;
mod state;
mod state_changes;

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use events::{AcaoJogadorPayload, ColisaoJogadorAgentePayload, Event, EventType};
use futures::StreamExt;
use guardrail::GuardrailSaida;
use llm::LlmClient;
use orchestrator::Orchestrator;
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;
use std::sync::{atomic::{AtomicU64, Ordering}, Arc};
use tower_http::cors::{Any, CorsLayer};

const NATS_SUBJECT: &str = "airpg.events";
/// Usado quando o chamador não envia `x-player-id` (ex: curl manual, clientes
/// antigos) — ver Change-Sessoes-Multiusuario.
const PLAYER_ID_PADRAO: &str = "player_01";

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
        .route("/player", get(obter_player))
        .route("/npcs", get(listar_npcs))
        .route("/npcs/:id", get(obter_npc))
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

/// Extrai o `player_id` da sessão a partir do header `x-player-id` (ver
/// Change-Sessoes-Multiusuario). O frontend gera e persiste esse id em
/// `localStorage` na primeira visita.
fn player_id_de(headers: &HeaderMap) -> String {
    headers
        .get("x-player-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or(PLAYER_ID_PADRAO)
        .to_string()
}

#[derive(Deserialize)]
struct PlayerIdQuery {
    player_id: Option<String>,
}

async fn obter_player(State(app): State<AppState>, headers: HeaderMap, Query(q): Query<PlayerIdQuery>) -> Json<state::Player> {
    let player_id = q.player_id.unwrap_or_else(|| player_id_de(&headers));
    let player = db::get_ou_criar_player(&app.pool, &player_id).await.unwrap_or_else(|_| state::Player::seed(player_id));
    Json(player)
}

async fn listar_npcs(State(app): State<AppState>) -> Json<Vec<state::Npc>> {
    Json(db::list_npcs(&app.pool).await.unwrap_or_default())
}

async fn obter_npc(State(app): State<AppState>, Path(id): Path<String>) -> Json<Option<state::Npc>> {
    Json(db::get_npc(&app.pool, &id).await.unwrap_or(None))
}

/// Endpoint de turno: valida a ação (Guardrail de Entrada), resolve a Cena do
/// Mestre de Jogo para a location do jogador, roteia até MAX_AGENTES_POR_TURNO
/// NPCs presentes ali (ver Orquestrador-Central / Decisoes-Resolvidas), gera a
/// reação de cada um em paralelo (com memória por par NPC×jogador — ver
/// Change-Sessoes-Multiusuario), filtra pelo Guardrail de Saída, publica no
/// NATS e devolve o lote de eventos do turno.
async fn processar_turno(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(acao): Json<AcaoJogadorPayload>,
) -> Json<TurnResult> {
    let turno = app.turno.fetch_add(1, Ordering::SeqCst);
    let player_id = player_id_de(&headers);

    let mut player = match db::get_ou_criar_player(&app.pool, &player_id).await {
        Ok(p) => p,
        Err(err) => {
            tracing::error!(%err, %player_id, "falha ao carregar/criar jogador, usando fallback em memoria");
            state::Player::seed(player_id.clone())
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
            let cena = mestre::resolver_cena(&app.pool, &app.llm, &player.location_id).await;

            // Sistema de dados: o Mestre de Jogo só decide SE um teste é
            // necessário (e o atributo/dificuldade); o resultado em si é
            // sempre a rolagem determinística de `skills::skill_dados`, nunca
            // texto gerado (ver Subagentes-e-Skills). O resultado numérico
            // vira restrição obrigatória para toda narração deste turno.
            let verificacao = mestre::avaliar_verificacao(&app.llm, &acao.response).await;
            let resultado_dados = if verificacao.precisa_teste {
                let seed = rand::random::<u64>();
                let resultado = skills::skill_dados(verificacao.dificuldade, seed);
                tracing::info!(
                    turno,
                    atributo = %verificacao.atributo,
                    dificuldade = verificacao.dificuldade,
                    rolagem = resultado.rolagem,
                    sucesso = resultado.sucesso,
                    "teste de dados rolado"
                );
                Some(resultado)
            } else {
                None
            };

            let npcs = db::list_npcs(&app.pool).await.unwrap_or_default();
            let roteados = app.orchestrator.rotear_agentes(&player, &npcs);

            tracing::info!(
                turno,
                cena = %cena.nome,
                agentes = ?roteados.iter().map(|n| &n.id).collect::<Vec<_>>(),
                "agentes roteados para o turno"
            );

            let respostas = futures::future::join_all(roteados.iter().map(|npc| {
                let pool = app.pool.clone();
                let llm = app.llm.clone();
                let guardrail = app.guardrail_saida.clone();
                let npc = (*npc).clone();
                let cena = cena.clone();
                let player_id = player_id.clone();
                let texto_jogador = acao.response.clone();
                let resultado_dados = resultado_dados.clone();
                async move {
                    let texto = reacoes::processar_interacao(&pool, &llm, &guardrail, &npc, &player_id, &cena, resultado_dados.as_ref(), &texto_jogador).await;
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

            if let Some(resultado) = &resultado_dados {
                eventos.push(Event::new(
                    EventType::ResultadoSkill,
                    "mestre_de_jogo",
                    turno,
                    serde_json::json!({
                        "atributo": verificacao.atributo,
                        "descricao": verificacao.descricao,
                        "rolagem": resultado.rolagem,
                        "dificuldade": resultado.dificuldade,
                        "sucesso": resultado.sucesso,
                    }),
                ));
            }

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
/// e jogador específicos — exatamente como o roteamento normal, o pool não
/// sabe (nem precisa saber) que veio de uma colisão autônoma (ver
/// Pool-de-Agentes / Mundo-Vivo). Ao final, publica `interacao_finalizada`
/// para o Elixir retomar a autonomia do NPC com aquele jogador.
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

            tracing::info!(agent_id = %payload.agent_id, player_id = %payload.player_id, "colisao recebida do mundo vivo, ativando pool de agentes");
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
    let cena = mestre::resolver_cena(&app.pool, &app.llm, &colisao.location_id).await;
    let abertura = "O NPC encontra o jogador por acaso.";
    let texto = reacoes::processar_interacao(&app.pool, &app.llm, &app.guardrail_saida, &npc, &colisao.player_id, &cena, None, abertura).await;

    let evento_dialogo = Event::new(EventType::Dialogo, npc.id.clone(), turno, serde_json::json!({ "texto": texto }));
    publicar_lote(&app, &[evento_dialogo]).await;

    let evento_finalizado = Event::new(
        EventType::InteracaoFinalizada,
        "orquestrador",
        turno,
        serde_json::json!({ "agent_id": npc.id, "player_id": colisao.player_id, "resultado": "interacao_reativa_concluida" }),
    );
    let body = serde_json::to_vec(&evento_finalizado).unwrap_or_default();
    if let Err(err) = client.publish(NATS_SUBJECT, body.into()).await {
        tracing::warn!(%err, "falha ao publicar interacao_finalizada");
    }
}
