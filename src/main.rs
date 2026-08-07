mod combate;
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
    /// Ids de todos os NPCs roteados pro turno (ver
    /// Change-Chat-Multiplos-Agentes-Proximidade) — não só os que geraram
    /// diálogo, pra popular "quem está na sala" no frontend.
    #[serde(default)]
    presentes: Vec<String>,
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
        .route("/player/reiniciar", post(reiniciar_jogador))
        .route("/npcs", get(listar_npcs))
        .route("/npcs/:id", get(obter_npc))
        .route("/combate", get(obter_combate))
        .route("/historico", get(obter_historico))
        .route("/cenas/:location_id/fatos", post(adicionar_fato_a_cena))
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

async fn obter_combate(State(app): State<AppState>, headers: HeaderMap) -> Json<Option<state::Combate>> {
    let player_id = player_id_de(&headers);
    Json(db::get_combate(&app.pool, &player_id).await.unwrap_or(None))
}

/// Ver Change-Chat-Screen: histórico persistido de eventos do jogador, pra
/// reconstruir o chat ao recarregar a página. Últimos 100 eventos.
async fn obter_historico(State(app): State<AppState>, headers: HeaderMap) -> Json<Vec<Event>> {
    let player_id = player_id_de(&headers);
    Json(db::historico_do_jogador(&app.pool, &player_id, 100).await.unwrap_or_default())
}

#[derive(Deserialize)]
struct NovoFatoPayload {
    fato: String,
}

/// Recebe fatos da simulação de mundo em background (ver
/// Change-Simulacao-Mundo-Background, `airpg-world`/`RegiaoMacro`). Só
/// adiciona a uma Cena que já existe — nunca cria uma nova aqui.
async fn adicionar_fato_a_cena(
    State(app): State<AppState>,
    Path(location_id): Path<String>,
    Json(payload): Json<NovoFatoPayload>,
) -> axum::http::StatusCode {
    match db::adicionar_fato_a_cena(&app.pool, &location_id, &payload.fato).await {
        Ok(true) => axum::http::StatusCode::OK,
        Ok(false) => axum::http::StatusCode::NOT_FOUND,
        Err(err) => {
            tracing::error!(%err, %location_id, "falha ao adicionar fato a cena");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct ReiniciarPayload {
    escopo: String,
}

/// Ver Change-Fluxo-de-Morte: dois caminhos de reinício. "jogador" apaga só o
/// registro daquele player_id (o mundo/NPCs continuam intactos). "mundo"
/// apaga tudo — é destrutivo para QUALQUER jogador na mesma instância, não só
/// quem morreu (decisão de produto em aberto se isso deveria existir em
/// produção multiusuário, ver o change).
async fn reiniciar_jogador(State(app): State<AppState>, headers: HeaderMap, Json(payload): Json<ReiniciarPayload>) -> Json<state::Player> {
    let player_id = player_id_de(&headers);

    if payload.escopo == "mundo" {
        if let Err(err) = db::reiniciar_mundo(&app.pool).await {
            tracing::error!(%err, "falha ao reiniciar o mundo");
        }
    } else if let Err(err) = db::reiniciar_dados_do_jogador(&app.pool, &player_id).await {
        tracing::error!(%err, %player_id, "falha ao reiniciar jogador");
    }

    let novo = state::Player::seed(&player_id);
    let _ = db::upsert_player(&app.pool, &novo).await;
    Json(novo)
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

    let combate_ativo = db::get_combate(&app.pool, &player_id).await.ok().flatten();

    let mut presentes: Vec<String> = Vec::new();

    let eventos = match app.orchestrator.validar_acao(&player, &acao) {
        Err(rejeicao) => vec![Event::new(
            EventType::AcaoRejeitada,
            "guardrail_entrada",
            turno,
            serde_json::to_value(&rejeicao).unwrap(),
        )],
        Ok(()) if combate_ativo.is_some() => {
            processar_turno_de_combate(&app, turno, &player_id, &mut player, combate_ativo.unwrap(), &acao).await
        }
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
            presentes = roteados.iter().map(|n| n.id.clone()).collect();

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
            let hp_antes_das_propostas = player.hp.atual;
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

            // Morte por dano ambiental (não-combate) — ver Change-Fluxo-de-Morte.
            // Morte em combate é detectada dentro de `combate::resolver_rodada`.
            if hp_antes_das_propostas > 0 && player.hp.atual == 0 {
                eventos.push(Event::new(EventType::JogadorMorreu, "orquestrador", turno, serde_json::json!({ "causa": "dano ambiental" })));
            }

            // Início de combate: o Mestre de Jogo decide SE a ação inicia
            // hostilidade contra um NPC combatente presente — nunca o
            // resultado do combate em si (ver Change-Sistema-de-Combate).
            let combatentes = combate::npcs_combatentes(&npcs)
                .into_iter()
                .filter(|n| n.location_id == player.location_id)
                .collect::<Vec<_>>();
            if !combatentes.is_empty() {
                if let Some(alvo_id) = mestre::avaliar_inicio_combate(&app.llm, &acao.response, &combatentes).await {
                    let seed = rand::random::<u64>();
                    match combate::iniciar(&app.pool, &player_id, &alvo_id, seed).await {
                        Ok(_) => {
                            tracing::info!(turno, npc_id = %alvo_id, "combate iniciado");
                            eventos.push(Event::new(
                                EventType::CombateIniciado,
                                "orquestrador",
                                turno,
                                serde_json::json!({ "npc_id": alvo_id }),
                            ));
                        }
                        Err(err) => tracing::error!(%err, "falha ao iniciar combate"),
                    }
                }
            }

            eventos.push(app.orchestrator.evento_fim_de_turno(turno, &roteados));
            eventos
        }
    };

    if let Err(err) = db::registrar_eventos_historico(&app.pool, &player_id, &eventos).await {
        tracing::warn!(%err, %player_id, "falha ao registrar historico de eventos");
    }

    publicar_lote(&app, &eventos).await;

    Json(TurnResult { turno, eventos, presentes })
}

/// Turno dentro de um combate ativo: classifica a ação (atacar/fugir/outro)
/// via Mestre de Jogo, resolve a rodada de forma determinística
/// (`combate::resolver_rodada`), persiste jogador/NPC/combate. Substitui
/// inteiramente o fluxo normal de diálogo/roteamento enquanto o combate durar
/// (ver Design em Change-Sistema-de-Combate — simplificação deliberada, sem
/// narração de NPCs de história durante o combate no MVP).
async fn processar_turno_de_combate(
    app: &AppState,
    turno: u64,
    player_id: &str,
    player: &mut state::Player,
    mut combate: state::Combate,
    acao: &AcaoJogadorPayload,
) -> Vec<Event> {
    let mut npc = match db::get_npc(&app.pool, &combate.npc_id).await {
        Ok(Some(n)) => n,
        _ => {
            tracing::error!(npc_id = %combate.npc_id, "npc do combate nao encontrado, encerrando combate");
            let _ = db::encerrar_combate(&app.pool, player_id).await;
            return vec![Event::new(
                EventType::CombateEncerrado,
                "orquestrador",
                turno,
                serde_json::json!({ "motivo": "erro_interno" }),
            )];
        }
    };

    let tipo_acao = mestre::avaliar_acao_combate(&app.llm, &acao.response).await;
    let seed = rand::random::<u64>();
    let resultado = combate::resolver_rodada(&mut combate, player, &mut npc, tipo_acao, turno, seed);

    if let Err(err) = db::upsert_npc(&app.pool, &npc).await {
        tracing::error!(%err, "falha ao persistir npc apos rodada de combate");
    }
    if let Err(err) = db::upsert_player(&app.pool, player).await {
        tracing::error!(%err, "falha ao persistir jogador apos rodada de combate");
    }

    if resultado.combate_encerrado {
        let _ = db::encerrar_combate(&app.pool, player_id).await;
    } else if let Err(err) = db::salvar_combate(&app.pool, &combate).await {
        tracing::error!(%err, "falha ao persistir estado do combate");
    }

    resultado.eventos
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
