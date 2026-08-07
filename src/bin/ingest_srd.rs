/// Ingestor offline de dados SRD 5.1 para RAG no AIRPG.
///
/// Fetch → Parse → Chunks → Embeddings → SQLite seed.
/// Execução: cargo run --bin ingest_srd -- --output seed.sql
use anyhow::{Context, Result};
use clap::Parser;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use sqlx::ConnectOptions;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Parser)]
#[command(name = "ingest_srd")]
#[command(about = "Ingest SRD 5.1 data, create embeddings, seed SQLite for RAG")]
struct Args {
    /// Output SQLite database path
    #[arg(short, long, default_value = "srd_chunks.sqlite")]
    db_path: PathBuf,

    /// Model to use for embeddings (default: bge-small-en-v1.5)
    #[arg(short, long, default_value = "BGESmallENV15")]
    model: String,

    /// Fetch fresh SRD data from GitHub (default: use embedded test data)
    #[arg(long)]
    fetch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SrdChunk {
    categoria: String,
    titulo: String,
    texto: String,
    embedding: Vec<f32>,
}

// Formato real do 5e-bits/5e-database (CC-BY-4.0), src/2014/en/*.json

#[derive(Debug, Deserialize)]
struct SrdCondition {
    name: String,
    desc: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SrdNamedRef {
    name: String,
}

#[derive(Debug, Deserialize)]
struct SrdSpell {
    name: String,
    desc: Vec<String>,
    #[serde(default)]
    higher_level: Vec<String>,
    level: u8,
    school: SrdNamedRef,
    #[serde(default)]
    classes: Vec<SrdNamedRef>,
}

#[derive(Debug, Deserialize)]
struct SrdRuleSection {
    name: String,
    desc: String,
}

const SRD_BASE_URL: &str = "https://raw.githubusercontent.com/5e-bits/5e-database/main/src/2014/en";

/// Tamanho-alvo de chunk (chars) pra seções de regra geral, que vêm como um
/// bloco de texto único — precisam ser fatiadas em pedaços menores pra
/// embeddings de qualidade (chunk grande demais dilui o vetor).
const RULE_CHUNK_TARGET_CHARS: usize = 700;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt::init();

    println!("🔄 Initializing embeddings model ({})", args.model);
    let model = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))
        .context("Failed to initialize embedding model")?;

    println!("📚 Loading SRD data...");
    let chunks = if args.fetch {
        load_srd_from_github().await?
    } else {
        load_srd_test_data()
    };

    println!("✨ Creating embeddings ({} chunks)...", chunks.len());
    let embedded_chunks = create_embeddings(model, chunks).context("Failed to embed chunks")?;

    println!("💾 Writing to SQLite: {}", args.db_path.display());
    seed_sqlite(&args.db_path, &embedded_chunks)
        .await
        .context("Failed to seed SQLite")?;

    println!(
        "✅ Done! {} chunks ingested with embeddings.",
        embedded_chunks.len()
    );
    Ok(())
}

/// Load minimal test SRD data for validation.
fn load_srd_test_data() -> Vec<(String, String, String)> {
    vec![
        // Spells
        ("magia".to_string(), "Magic Missile".to_string(),
         "A creature you can see within range. Consult the rules about magical damage and resistance.".to_string()),
        ("magia".to_string(), "Fireball".to_string(),
         "Each creature in a 20-foot-radius sphere centered on the point of impact is forced to make a Dexterity saving throw.".to_string()),
        ("magia".to_string(), "Cure Wounds".to_string(),
         "A creature you touch regains a number of hit points equal to 1d8 + your spellcasting ability modifier.".to_string()),

        // Conditions
        ("condicao".to_string(), "Charmed".to_string(),
         "A charmed creature can't attack the charmer or target the charmer with harmful abilities or magical effects.".to_string()),
        ("condicao".to_string(), "Frightened".to_string(),
         "A frightened creature has disadvantage on attack rolls and ability checks while the source of its fear is within line of sight.".to_string()),
        ("condicao".to_string(), "Poisoned".to_string(),
         "A poisoned creature has disadvantage on attack rolls and ability checks.".to_string()),

        // Actions
        ("acao".to_string(), "Dash".to_string(),
         "When you take the Dash action, you gain extra movement equal to your speed for the current turn.".to_string()),
        ("acao".to_string(), "Disengage".to_string(),
         "If you take the Disengage action, your movement doesn't provoke opportunity attacks for the rest of the turn.".to_string()),

        // Combat
        ("combate".to_string(), "Critical Hit".to_string(),
         "When you roll a 20 on the d20 for an attack roll, you hit regardless of your modifiers, and the attack is a critical hit.".to_string()),
        ("combate".to_string(), "Saving Throw".to_string(),
         "When an effect allows a target to make a saving throw to resist an effect, the target rolls a d20 and adds a modifier.".to_string()),
    ]
}

/// Fetch SRD 5.1 real data from 5e-bits/5e-database (GitHub, CC-BY-4.0).
/// Cobre: condições, magias, seções de regra geral (combate, ações, etc).
async fn load_srd_from_github() -> Result<Vec<(String, String, String)>> {
    let client = reqwest::Client::new();
    let mut chunks = Vec::new();

    println!("  → Baixando condições...");
    let conditions: Vec<SrdCondition> = fetch_json(&client, "5e-SRD-Conditions.json").await?;
    for c in conditions {
        chunks.push(("condicao".to_string(), c.name, c.desc.join(" ")));
    }

    println!("  → Baixando magias...");
    let spells: Vec<SrdSpell> = fetch_json(&client, "5e-SRD-Spells.json").await?;
    for s in spells {
        let classes = s
            .classes
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut texto = format!(
            "{} (nível {}, escola {}, classes: {}). {}",
            s.name,
            s.level,
            s.school.name,
            classes,
            s.desc.join(" ")
        );
        if !s.higher_level.is_empty() {
            texto.push_str(" Em níveis superiores: ");
            texto.push_str(&s.higher_level.join(" "));
        }
        chunks.push(("magia".to_string(), s.name, texto));
    }

    println!("  → Baixando seções de regra geral (combate, ações, etc)...");
    let rules: Vec<SrdRuleSection> = fetch_json(&client, "5e-SRD-Rule-Sections.json").await?;
    for r in rules {
        for (idx, texto) in chunk_paragraphs(&r.desc, RULE_CHUNK_TARGET_CHARS)
            .into_iter()
            .enumerate()
        {
            let titulo = if idx == 0 {
                r.name.clone()
            } else {
                format!("{} (parte {})", r.name, idx + 1)
            };
            chunks.push(("regra_geral".to_string(), titulo, texto));
        }
    }

    Ok(chunks)
}

async fn fetch_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    filename: &str,
) -> Result<T> {
    let url = format!("{SRD_BASE_URL}/{filename}");
    client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch {url}"))?
        .error_for_status()
        .with_context(|| format!("Non-success status fetching {url}"))?
        .json::<T>()
        .await
        .with_context(|| format!("Failed to parse JSON from {url}"))
}

/// Junta parágrafos (separados por linha em branco) em chunks de até
/// `target_chars`, sem quebrar um parágrafo no meio. Um parágrafo sozinho
/// maior que o alvo vira seu próprio chunk (melhor um chunk grande do que
/// cortar uma ideia ao meio).
fn chunk_paragraphs(texto: &str, target_chars: usize) -> Vec<String> {
    let paragrafos: Vec<&str> = texto
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    let mut chunks = Vec::new();
    let mut atual = String::new();

    for p in paragrafos {
        if !atual.is_empty() && atual.len() + p.len() > target_chars {
            chunks.push(atual.trim().to_string());
            atual = String::new();
        }
        if !atual.is_empty() {
            atual.push_str("\n\n");
        }
        atual.push_str(p);
    }
    if !atual.is_empty() {
        chunks.push(atual.trim().to_string());
    }

    chunks
}

/// Chunk format: (categoria, titulo, texto)
fn create_embeddings(
    mut model: TextEmbedding,
    chunks: Vec<(String, String, String)>,
) -> Result<Vec<SrdChunk>> {
    let textos: Vec<&str> = chunks.iter().map(|(_, _, t)| t.as_str()).collect();

    let embeddings = model
        .embed(textos, None)
        .context("Failed to create embeddings")?;

    let result = chunks
        .into_iter()
        .zip(embeddings)
        .map(|((categoria, titulo, texto), embedding)| SrdChunk {
            categoria,
            titulo,
            texto,
            embedding,
        })
        .collect();

    Ok(result)
}

/// Create SQLite database and seed with embedded chunks.
async fn seed_sqlite(path: &PathBuf, chunks: &[SrdChunk]) -> Result<()> {
    // Remove old database if it exists
    if path.exists() {
        fs::remove_file(path)?;
    }

    // Create and setup (create_if_missing, since this generates a fresh seed DB)
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .disable_statement_logging();
    let pool = SqlitePool::connect_with(opts)
        .await
        .context("Failed to create SQLite pool")?;

    // Create table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS regras_srd_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            categoria TEXT NOT NULL,
            titulo TEXT NOT NULL,
            texto TEXT NOT NULL,
            embedding BLOB NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&pool)
    .await
    .context("Failed to create table")?;

    // Insert chunks
    for chunk in chunks {
        let embedding_bytes = chunk
            .embedding
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect::<Vec<u8>>();

        sqlx::query(
            r#"
            INSERT INTO regras_srd_chunks (categoria, titulo, texto, embedding)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(&chunk.categoria)
        .bind(&chunk.titulo)
        .bind(&chunk.texto)
        .bind(embedding_bytes)
        .execute(&pool)
        .await
        .context("Failed to insert chunk")?;
    }

    pool.close().await;
    Ok(())
}
