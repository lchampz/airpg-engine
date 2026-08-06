use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Envelope comum a todo evento do sistema (ver Catálogo-de-Eventos na documentação da engine).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub event_id: Uuid,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub source: String,
    pub turn: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

impl Event {
    pub fn new(event_type: EventType, source: impl Into<String>, turn: u64, payload: serde_json::Value) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            event_type,
            source: source.into(),
            turn,
            timestamp: Utc::now(),
            payload,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    AcaoJogador,
    AcaoValidada,
    AcaoRejeitada,
    Dialogo,
    ResultadoSkill,
    MudancaEstado,
    ErroGuardrail,
    FimDeTurno,
    ImpactoCampanha,
    /// Publicado pelo Mundo Vivo (Elixir) quando um NPC autônomo encontra o jogador.
    ColisaoJogadorAgente,
    /// Publicado pelo engine (Rust) de volta ao Mundo Vivo ao fim de uma interação reativa.
    InteracaoFinalizada,
    PedidoEsclarecimento,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcaoJogadorPayload {
    pub situation: String,
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcaoRejeitadaPayload {
    pub motivo: String,
    pub detalhe: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MudancaEstadoPayload {
    pub campo: String,
    pub operacao: String,
    pub valor: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ColisaoJogadorAgentePayload {
    pub agent_id: String,
    pub location_id: String,
}
