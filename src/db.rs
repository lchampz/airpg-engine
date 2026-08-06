use crate::state::{Npc, Player};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

/// Estado Rígido persiste em SQLite no MVP (ver Stack-Escolhida / Estado-Rigido).
/// Postgres é o caminho de evolução caso multiplayer entre em escopo.
pub async fn init_pool(database_url: &str) -> anyhow::Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS players (
            id TEXT PRIMARY KEY,
            data TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS npcs (
            id TEXT PRIMARY KEY,
            data TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    seed_se_vazio(&pool).await?;

    Ok(pool)
}

/// Seed mínimo para permitir testar o roteamento real (cap de agentes/turno,
/// ver Decisoes-Resolvidas) sem depender ainda de um fluxo de criação de
/// campanha. Só popula se as tabelas estiverem vazias — não sobrescreve.
async fn seed_se_vazio(pool: &SqlitePool) -> anyhow::Result<()> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM npcs").fetch_one(pool).await?;
    if count.0 > 0 {
        return Ok(());
    }

    let player = Player {
        id: "player_01".into(),
        hp: crate::state::Hp { atual: 10, maximo: 10 },
        atributos: Default::default(),
        inventario: vec![],
        location_id: "taverna_porto_velho".into(),
        nivel: 1,
        classe: "guerreiro".into(),
    };
    upsert_player(pool, &player).await?;

    let npcs = vec![
        Npc {
            id: "npc_taverneiro_bram".into(),
            nome: "Bram".into(),
            status: crate::state::NpcStatus::Vivo,
            atitude_com_jogador: "neutro".into(),
            location_id: "taverna_porto_velho".into(),
            autonomo: true,
        },
        Npc {
            id: "npc_cliente_gerta".into(),
            nome: "Gerta".into(),
            status: crate::state::NpcStatus::Vivo,
            atitude_com_jogador: "neutro".into(),
            location_id: "taverna_porto_velho".into(),
            autonomo: false,
        },
        Npc {
            id: "npc_guarda_holt".into(),
            nome: "Holt".into(),
            status: crate::state::NpcStatus::Vivo,
            atitude_com_jogador: "desconfiado".into(),
            location_id: "floresta_negra".into(),
            autonomo: true,
        },
    ];
    for npc in &npcs {
        upsert_npc(pool, npc).await?;
    }

    Ok(())
}

pub async fn upsert_player(pool: &SqlitePool, player: &Player) -> anyhow::Result<()> {
    let data = serde_json::to_string(player)?;
    sqlx::query("INSERT INTO players (id, data) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data")
        .bind(&player.id)
        .bind(data)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_player(pool: &SqlitePool, id: &str) -> anyhow::Result<Option<Player>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data FROM players WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(data,)| serde_json::from_str(&data)).transpose()?)
}

pub async fn upsert_npc(pool: &SqlitePool, npc: &Npc) -> anyhow::Result<()> {
    let data = serde_json::to_string(npc)?;
    sqlx::query("INSERT INTO npcs (id, data) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data")
        .bind(&npc.id)
        .bind(data)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_npc(pool: &SqlitePool, id: &str) -> anyhow::Result<Option<Npc>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data FROM npcs WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(data,)| serde_json::from_str(&data)).transpose()?)
}

pub async fn list_npcs(pool: &SqlitePool) -> anyhow::Result<Vec<Npc>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT data FROM npcs").fetch_all(pool).await?;
    rows.into_iter().map(|(data,)| serde_json::from_str(&data).map_err(Into::into)).collect()
}
