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

impl Player {
    /// Estado inicial padrão de um personagem novo — usado tanto no seed de
    /// desenvolvimento quanto na criação automática de jogadores desconhecidos
    /// (ver Change-Sessoes-Multiusuario).
    pub fn seed(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            hp: Hp { atual: 10, maximo: 10 },
            atributos: Default::default(),
            inventario: vec![],
            location_id: "taverna_porto_velho".into(),
            nivel: 1,
            classe: "guerreiro".into(),
        }
    }
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

/// Cena: o fato objetivo de um `location_id`, criado e enriquecido só pelo
/// Mestre de Jogo — nunca por um NPC individual (ver Mestre-de-Jogo-e-Cena no
/// vault). Injetada como contexto read-only no prompt de todo NPC da cena.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Cena {
    pub location_id: String,
    pub nome: String,
    pub descricao: String,
    #[serde(default)]
    pub fatos_estabelecidos: Vec<String>,
}

/// Estado emocional de um NPC em relação a um jogador específico — combina
/// três camadas (ver Memoria-em-Camadas no vault):
///
/// - **PAD** (prazer/ativação/dominância): estado afetivo atual, mais
///   volátil. Ex: ativação alta + dominância baixa → pânico; ativação alta +
///   dominância alta → agressividade.
/// - **Relação** (confiança/respeito/afinidade + tags): mais estável, muda
///   devagar ao longo da campanha.
/// - **Interpretativo** (lente perceptiva + motivação imediata): como o NPC
///   está enquadrando o turno atual.
///
/// Tudo numérico é limitado a [-1.0, 1.0] na validação (ver
/// `consolidacao::aplicar_estado_emocional`) — os campos de texto livre
/// (`lente_perceptiva`, `motivacao_imediata`, `tags`) não são whitelisted,
/// servem só de contexto rico para o prompt, não para decisão de código.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct EstadoEmocional {
    pub prazer: f32,
    pub ativacao: f32,
    pub dominancia: f32,
    pub confianca: f32,
    pub respeito: f32,
    pub afinidade: f32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub lente_perceptiva: String,
    #[serde(default)]
    pub motivacao_imediata: String,
}

/// Memória de curto/médio prazo de um NPC em relação a um jogador específico
/// (par NPC×jogador — ver Change-Sessoes-Multiusuario, cada NPC tem uma
/// memória isolada por jogador). Ver Memoria-Narrativa/Memoria-em-Camadas.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoriaNpc {
    /// Janela verbatim: últimas trocas cruas (prompt, resposta).
    #[serde(default)]
    pub ctx: Vec<(String, String)>,
    /// Resumo rolante, regenerado (não empilhado) a cada consolidação.
    #[serde(default)]
    pub resumo: String,
    #[serde(default)]
    pub estado_emocional: EstadoEmocional,
    #[serde(default)]
    pub turnos_desde_consolidacao: u32,
}
