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

    Ok(pool)
}
