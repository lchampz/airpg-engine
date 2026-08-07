use crate::events::Event;
use crate::state::{Cena, Combate, MemoriaNpc, Npc, Player};
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

    migrar_npc_memoria(&pool).await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS npc_memoria (
            npc_id TEXT NOT NULL,
            player_id TEXT NOT NULL,
            ctx TEXT NOT NULL,
            resumo TEXT NOT NULL DEFAULT '',
            estado_emocional TEXT NOT NULL DEFAULT '{}',
            turnos_desde_consolidacao INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (npc_id, player_id)
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // Cena: fato objetivo de um location_id, criado pelo Mestre de Jogo (ver
    // Mestre-de-Jogo-e-Cena no vault) — nunca por um NPC individual.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS cenas (
            location_id TEXT PRIMARY KEY,
            nome TEXT NOT NULL,
            descricao TEXT NOT NULL,
            fatos TEXT NOT NULL DEFAULT '[]'
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // Um combate ativo por jogador (ver Change-Sistema-de-Combate — 1v1 no MVP).
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS combates (
            player_id TEXT PRIMARY KEY,
            data TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // Histórico de eventos por jogador — persiste o chat entre reloads de
    // página (ver Change-Chat-Screen: hoje é só estado de componente React,
    // perdido ao recarregar). `evento` é o JSON serializado do Event inteiro.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS eventos_historico (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            player_id TEXT NOT NULL,
            turno INTEGER NOT NULL,
            evento TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    seed_se_vazio(&pool).await?;

    Ok(pool)
}

/// `npc_memoria` mudou de esquema (chave composta npc_id+player_id, campos
/// novos) durante o desenvolvimento pré-lançamento. Memória de curto prazo é
/// efêmera por natureza (ver Memoria-Narrativa) — recriar a tabela e perder
/// memória antiga é aceitável, não há usuário real ainda.
async fn migrar_npc_memoria(pool: &SqlitePool) -> anyhow::Result<()> {
    let existe: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='npc_memoria'",
    )
    .fetch_optional(pool)
    .await?;

    if existe.is_some() {
        let colunas: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as("PRAGMA table_info(npc_memoria)").fetch_all(pool).await?;
        let tem_player_id = colunas.iter().any(|(_, nome, ..)| nome == "player_id");
        if !tem_player_id {
            sqlx::query("DROP TABLE npc_memoria").execute(pool).await?;
        }
    }

    Ok(())
}

/// Seed mínimo para permitir testar o roteamento real (cap de agentes/turno,
/// ver Decisoes-Resolvidas) sem depender ainda de um fluxo de criação de
/// campanha. Só popula se as tabelas estiverem vazias — não sobrescreve.
async fn seed_se_vazio(pool: &SqlitePool) -> anyhow::Result<()> {
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM npcs").fetch_one(pool).await?;
    if count.0 > 0 {
        // Tabela já populada de uma sessão anterior — ainda assim garante que
        // o combatente de exemplo exista, já que ele foi adicionado ao seed
        // depois que muitos volumes de dev já tinham sido criados (ver
        // Change-Sistema-de-Combate). Sem isso, testar combate exigiria
        // apagar o volume manualmente.
        if get_npc(pool, "npc_lobo_floresta").await?.is_none() {
            upsert_npc(pool, &npc_lobo_seed()).await?;
        }
        return Ok(());
    }

    upsert_player(pool, &Player::seed("player_01")).await?;

    let npcs = vec![
        Npc {
            id: "npc_taverneiro_bram".into(),
            nome: "Bram".into(),
            status: crate::state::NpcStatus::Vivo,
            atitude_com_jogador: "neutro".into(),
            location_id: "taverna_porto_velho".into(),
            autonomo: true,
            hp: None,
            classe_armadura: None,
            dano_dado_faces: None,
            xp_recompensa: None,
            loot: vec![],
        },
        Npc {
            id: "npc_cliente_gerta".into(),
            nome: "Gerta".into(),
            status: crate::state::NpcStatus::Vivo,
            atitude_com_jogador: "neutro".into(),
            location_id: "taverna_porto_velho".into(),
            autonomo: false,
            hp: None,
            classe_armadura: None,
            dano_dado_faces: None,
            xp_recompensa: None,
            loot: vec![],
        },
        Npc {
            id: "npc_guarda_holt".into(),
            nome: "Holt".into(),
            status: crate::state::NpcStatus::Vivo,
            atitude_com_jogador: "desconfiado".into(),
            location_id: "floresta_negra".into(),
            autonomo: true,
            hp: None,
            classe_armadura: None,
            dano_dado_faces: None,
            xp_recompensa: None,
            loot: vec![],
        },
        npc_lobo_seed(),
    ];
    for npc in &npcs {
        upsert_npc(pool, npc).await?;
    }

    Ok(())
}

/// Combatente de exemplo, para testar o sistema de combate (ver
/// Change-Sistema-de-Combate) sem depender de criação de conteúdo.
fn npc_lobo_seed() -> Npc {
    Npc {
        id: "npc_lobo_floresta".into(),
        nome: "Lobo Selvagem".into(),
        status: crate::state::NpcStatus::Hostil,
        atitude_com_jogador: "hostil".into(),
        location_id: "floresta_negra".into(),
        autonomo: false,
        hp: Some(crate::state::Hp { atual: 12, maximo: 12 }),
        classe_armadura: Some(12),
        dano_dado_faces: Some(4),
        xp_recompensa: Some(50),
        loot: vec!["presa_de_lobo".into()],
    }
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

/// Diferente da versão inicial (single-player fixo), agora **cria** o jogador
/// com estado padrão na primeira vez que um `id` desconhecido aparece — ver
/// Change-Sessoes-Multiusuario. Nunca retorna `None`.
pub async fn get_ou_criar_player(pool: &SqlitePool, id: &str) -> anyhow::Result<Player> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data FROM players WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    match row {
        Some((data,)) => Ok(serde_json::from_str(&data)?),
        None => {
            let player = Player::seed(id);
            upsert_player(pool, &player).await?;
            Ok(player)
        }
    }
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

/// Máximo de trocas (prompt+resposta) mantidas na janela verbatim — o resto
/// vira resumo/estado emocional na consolidação (ver Memoria-em-Camadas).
pub const MAX_TROCAS_MEMORIA: usize = 3;
/// A cada quantos turnos a memória de um par (NPC, jogador) é consolidada.
pub const TURNOS_POR_CONSOLIDACAO: u32 = 5;

pub async fn get_memoria(pool: &SqlitePool, npc_id: &str, player_id: &str) -> anyhow::Result<MemoriaNpc> {
    let row: Option<(String, String, String, i64)> = sqlx::query_as(
        "SELECT ctx, resumo, estado_emocional, turnos_desde_consolidacao FROM npc_memoria WHERE npc_id = ? AND player_id = ?",
    )
    .bind(npc_id)
    .bind(player_id)
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some((ctx, resumo, estado, turnos)) => MemoriaNpc {
            ctx: serde_json::from_str(&ctx)?,
            resumo,
            estado_emocional: serde_json::from_str(&estado).unwrap_or_default(),
            turnos_desde_consolidacao: turnos as u32,
        },
        None => MemoriaNpc::default(),
    })
}

pub async fn salvar_memoria(pool: &SqlitePool, npc_id: &str, player_id: &str, memoria: &MemoriaNpc) -> anyhow::Result<()> {
    let ctx = serde_json::to_string(&memoria.ctx)?;
    let estado = serde_json::to_string(&memoria.estado_emocional)?;
    sqlx::query(
        "INSERT INTO npc_memoria (npc_id, player_id, ctx, resumo, estado_emocional, turnos_desde_consolidacao) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(npc_id, player_id) DO UPDATE SET ctx = excluded.ctx, resumo = excluded.resumo, estado_emocional = excluded.estado_emocional, turnos_desde_consolidacao = excluded.turnos_desde_consolidacao",
    )
    .bind(npc_id)
    .bind(player_id)
    .bind(ctx)
    .bind(&memoria.resumo)
    .bind(estado)
    .bind(memoria.turnos_desde_consolidacao as i64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Registra uma troca na janela verbatim e incrementa o contador de
/// consolidação. Não persiste sozinho — o chamador decide se consolida antes
/// de persistir (ver `reacoes::processar_interacao`).
pub fn registrar_troca(memoria: &mut MemoriaNpc, prompt: String, resposta: String) {
    memoria.ctx.push((prompt, resposta));
    if memoria.ctx.len() > MAX_TROCAS_MEMORIA {
        memoria.ctx.drain(0..memoria.ctx.len() - MAX_TROCAS_MEMORIA);
    }
    memoria.turnos_desde_consolidacao += 1;
}

pub async fn get_cena(pool: &SqlitePool, location_id: &str) -> anyhow::Result<Option<Cena>> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT nome, descricao, fatos FROM cenas WHERE location_id = ?")
            .bind(location_id)
            .fetch_optional(pool)
            .await?;

    Ok(match row {
        Some((nome, descricao, fatos)) => Some(Cena {
            location_id: location_id.to_string(),
            nome,
            descricao,
            fatos_estabelecidos: serde_json::from_str(&fatos)?,
        }),
        None => None,
    })
}

pub async fn upsert_cena(pool: &SqlitePool, cena: &Cena) -> anyhow::Result<()> {
    let fatos = serde_json::to_string(&cena.fatos_estabelecidos)?;
    sqlx::query(
        "INSERT INTO cenas (location_id, nome, descricao, fatos) VALUES (?, ?, ?, ?)
         ON CONFLICT(location_id) DO UPDATE SET nome = excluded.nome, descricao = excluded.descricao, fatos = excluded.fatos",
    )
    .bind(&cena.location_id)
    .bind(&cena.nome)
    .bind(&cena.descricao)
    .bind(fatos)
    .execute(pool)
    .await?;
    Ok(())
}

/// Máximo de fatos por Cena — a simulação de mundo em background (ver
/// Change-Simulacao-Mundo-Background) roda indefinidamente, então a lista
/// precisa de um teto para não crescer sem limite. Os mais antigos saem.
pub const MAX_FATOS_POR_CENA: usize = 20;

/// Adiciona um fato a uma Cena já existente — usado pela simulação de mundo
/// em background do Mundo Vivo (Elixir). Só adiciona a cenas que já existem
/// (não cria uma nova aqui — criação é sempre via `mestre::resolver_cena`).
pub async fn adicionar_fato_a_cena(pool: &SqlitePool, location_id: &str, fato: &str) -> anyhow::Result<bool> {
    let Some(mut cena) = get_cena(pool, location_id).await? else {
        return Ok(false);
    };

    cena.fatos_estabelecidos.push(fato.to_string());
    if cena.fatos_estabelecidos.len() > MAX_FATOS_POR_CENA {
        let excesso = cena.fatos_estabelecidos.len() - MAX_FATOS_POR_CENA;
        cena.fatos_estabelecidos.drain(0..excesso);
    }
    upsert_cena(pool, &cena).await?;
    Ok(true)
}

pub async fn get_combate(pool: &SqlitePool, player_id: &str) -> anyhow::Result<Option<Combate>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data FROM combates WHERE player_id = ?")
        .bind(player_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(data,)| serde_json::from_str(&data)).transpose()?)
}

pub async fn salvar_combate(pool: &SqlitePool, combate: &Combate) -> anyhow::Result<()> {
    let data = serde_json::to_string(combate)?;
    sqlx::query("INSERT INTO combates (player_id, data) VALUES (?, ?) ON CONFLICT(player_id) DO UPDATE SET data = excluded.data")
        .bind(&combate.player_id)
        .bind(data)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn encerrar_combate(pool: &SqlitePool, player_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM combates WHERE player_id = ?").bind(player_id).execute(pool).await?;
    Ok(())
}

/// Ver Change-Fluxo-de-Morte — apaga só o que pertence a este jogador
/// especificamente. `npcs`/`cenas` (dados de mundo, compartilhados) não são
/// tocados.
pub async fn reiniciar_dados_do_jogador(pool: &SqlitePool, player_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM players WHERE id = ?").bind(player_id).execute(pool).await?;
    sqlx::query("DELETE FROM npc_memoria WHERE player_id = ?").bind(player_id).execute(pool).await?;
    sqlx::query("DELETE FROM combates WHERE player_id = ?").bind(player_id).execute(pool).await?;
    Ok(())
}

/// Ver Change-Fluxo-de-Morte — destrutivo para TODOS os jogadores da
/// instância, não só quem pediu o reinício. Apaga tudo e roda o seed de novo.
pub async fn reiniciar_mundo(pool: &SqlitePool) -> anyhow::Result<()> {
    for tabela in ["players", "npcs", "npc_memoria", "cenas", "combates"] {
        sqlx::query(&format!("DELETE FROM {tabela}")).execute(pool).await?;
    }
    seed_se_vazio(pool).await?;
    Ok(())
}

/// Ver Change-Chat-Screen: persiste o lote de eventos de um turno para que o
/// frontend possa reconstruir o histórico do chat ao recarregar a página.
pub async fn registrar_eventos_historico(pool: &SqlitePool, player_id: &str, eventos: &[Event]) -> anyhow::Result<()> {
    for evento in eventos {
        let data = serde_json::to_string(evento)?;
        sqlx::query("INSERT INTO eventos_historico (player_id, turno, evento) VALUES (?, ?, ?)")
            .bind(player_id)
            .bind(evento.turn as i64)
            .bind(data)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Últimos `limite` eventos do jogador, do mais antigo para o mais novo (como
/// o chat espera renderizar).
pub async fn historico_do_jogador(pool: &SqlitePool, player_id: &str, limite: i64) -> anyhow::Result<Vec<Event>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT evento FROM eventos_historico WHERE player_id = ? ORDER BY id DESC LIMIT ?",
    )
    .bind(player_id)
    .bind(limite)
    .fetch_all(pool)
    .await?;

    let mut eventos: Vec<Event> = rows
        .into_iter()
        .map(|(data,)| serde_json::from_str(&data))
        .collect::<Result<_, _>>()?;
    eventos.reverse();
    Ok(eventos)
}

