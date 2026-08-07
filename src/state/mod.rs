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
    /// Ver Change-Sistema-de-XP-Progressao. Nível é sempre derivado de xp via
    /// `nivel_por_xp` — nunca setado diretamente pelo LLM.
    #[serde(default)]
    pub xp: u32,
}

/// Limiares de XP por nível (índice = nível - 1). Constante no código, não no
/// banco — ver Change-Sistema-de-XP-Progressao.
pub const LIMIARES_XP: &[u32] = &[0, 100, 250, 450, 700, 1000, 1400, 1900, 2500, 3200];
/// Classe de armadura padrão do jogador sem equipamento (ver Change-Sistema-de-Combate).
pub const PLAYER_CA_PADRAO: u32 = 10;

pub fn nivel_por_xp(xp: u32) -> u32 {
    LIMIARES_XP.iter().filter(|&&limiar| xp >= limiar).count() as u32
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
            xp: 0,
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
    /// `None` = NPC não combatente (ex: Bram, o taverneiro). Só criaturas
    /// hostis/monstros preenchem os campos de combate abaixo — ver
    /// Change-Sistema-de-Combate.
    #[serde(default)]
    pub hp: Option<Hp>,
    #[serde(default)]
    pub classe_armadura: Option<u32>,
    /// Faces do dado de dano do ataque básico (ex: 6 = 1d6).
    #[serde(default)]
    pub dano_dado_faces: Option<u32>,
    #[serde(default)]
    pub xp_recompensa: Option<u32>,
    #[serde(default)]
    pub loot: Vec<String>,
    /// Campos de ficha de bestiário — descritivos, não mecânicos (ver
    /// Change-Bestiario). Diferem de `hp`/`classe_armadura`/`dano_dado_faces`,
    /// que alimentam o motor de combate.
    #[serde(default)]
    pub descricao: String,
    #[serde(default)]
    pub deslocamento: Option<String>,
    #[serde(default)]
    pub imunidades: Vec<String>,
    #[serde(default)]
    pub resistencias: Vec<String>,
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

/// Um combate ativo — jogador contra um único NPC hostil por vez no MVP (ver
/// Change-Sistema-de-Combate; múltiplos combatentes simultâneos ficaram fora
/// do escopo inicial de propósito). Persistido por `player_id`: um jogador só
/// pode estar em um combate por vez.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Combate {
    pub player_id: String,
    pub npc_id: String,
    pub rodada: u32,
    /// true = vez do jogador agir; false = o NPC já agiu nesta rodada e o
    /// resultado foi resolvido no mesmo turno (não há espera de input do NPC,
    /// ele reage no mesmo /turn — ver Design em Change-Sistema-de-Combate).
    pub ativo: bool,
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
