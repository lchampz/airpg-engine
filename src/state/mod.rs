use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Estado Rígido — única fonte de verdade do mundo. Ver nota "Estado-Rigido" na documentação da engine.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Player {
    pub id: String,
    pub hp: Hp,
    pub atributos: std::collections::HashMap<String, i32>,
    pub inventario: Vec<String>,
    pub location_id: String,
    pub nivel: u32,
    pub classe: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Hp {
    pub atual: i32,
    pub maximo: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Npc {
    pub id: String,
    pub nome: String,
    pub status: NpcStatus,
    pub atitude_com_jogador: String,
    pub location_id: String,
    pub autonomo: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NpcStatus {
    Vivo,
    Morto,
    Hostil,
    Aliado,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorldFlags {
    pub flags: std::collections::HashMap<String, serde_json::Value>,
    pub turno_atual: u64,
}
