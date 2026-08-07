use crate::db;
use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::state::{Npc, NpcStatus};
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;

/// Ver Módulo 1 em Tarefas-Pendentes no vault: gera uma ficha de NPC nova sob
/// demanda (papel + personalidade), em vez de todo NPC precisar ser
/// hardcoded no seed. Usa o perfil de custo "personagens" (ver
/// Estrategia-Custo-Tokens) — pode ser um modelo mais caro/capaz, já que a
/// chamada é rara.
const SYSTEM_PROMPT_GERAR_PERSONAGEM: &str = r#"Você é um criador de personagens de um RPG de fantasia medieval. Dado um local e um contexto, invente um NPC novo, com personalidade distinta e papel claro na comunidade.
Responda APENAS com um JSON no formato {"nome": "...", "descricao": "papel + personalidade em 2-3 frases", "atitude_com_jogador": "neutro|hostil|desconfiado|aliado|amigavel", "autonomo": bool}.
A descrição deve deixar claro o que esse personagem FAZ (papel na comunidade) e o que ele NÃO tem autoridade para decidir (limites claros), pra outros personagens saberem quando não é da conta dele responder. Personalidade deve ser específica e memorável, nunca genérica."#;

const ATITUDES_VALIDAS: &[&str] = &["neutro", "hostil", "desconfiado", "aliado", "amigavel"];

#[derive(Debug, Deserialize)]
struct PersonagemProposto {
    #[serde(default)]
    nome: String,
    #[serde(default)]
    descricao: String,
    #[serde(default)]
    atitude_com_jogador: String,
    #[serde(default)]
    autonomo: bool,
}

/// Gera um NPC novo via LLM — nunca confia cegamente no resultado (mesmo
/// princípio de `mestre::avaliar_verificacao`/`avaliar_inicio_combate`):
/// rejeita (`None`) se nome/descrição vierem vazios, normaliza atitude para
/// um vocabulário conhecido.
pub async fn gerar_personagem(llm: &LlmClient, location_id: &str, contexto: &str) -> Option<Npc> {
    let entrada = format!("Local: {location_id}\nContexto/pedido: {contexto}");

    let proposta = match llm.complete(SYSTEM_PROMPT_GERAR_PERSONAGEM, &entrada).await {
        Ok(resposta) => extrair_json::<PersonagemProposto>(&resposta),
        Err(err) => {
            tracing::error!(%err, %location_id, "falha ao consultar o gerador de personagens");
            None
        }
    }?;

    if proposta.nome.trim().is_empty() || proposta.descricao.trim().is_empty() {
        tracing::warn!(%location_id, "gerador de personagens: nome ou descricao vazios, descartando");
        return None;
    }

    let atitude = if ATITUDES_VALIDAS.contains(&proposta.atitude_com_jogador.as_str()) {
        proposta.atitude_com_jogador
    } else {
        "neutro".to_string()
    };

    Some(Npc {
        id: gerar_id_unico(&proposta.nome, location_id),
        nome: proposta.nome,
        status: NpcStatus::Vivo,
        atitude_com_jogador: atitude,
        location_id: location_id.to_string(),
        autonomo: proposta.autonomo,
        hp: None,
        classe_armadura: None,
        dano_dado_faces: None,
        xp_recompensa: None,
        loot: vec![],
        descricao: proposta.descricao,
        deslocamento: None,
        imunidades: vec![],
        resistencias: vec![],
    })
}

/// Slug em snake_case do nome + um sufixo curto derivado do local, só pra
/// evitar colisão com os NPCs de seed e entre si — não precisa ser
/// criptograficamente único.
fn gerar_id_unico(nome: &str, location_id: &str) -> String {
    let slug: String = nome
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    let mut hash: u64 = 0;
    for b in format!("{nome}{location_id}").bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(b as u64);
    }

    format!("npc_gerado_{slug}_{:x}", hash & 0xffff)
}

/// Gera, persiste e registra no Log Global — ponto único chamado pelo
/// endpoint admin (ver `main.rs`).
pub async fn gerar_e_registrar_personagem(
    pool: &SqlitePool,
    llm: &LlmClient,
    turno_global: i64,
    location_id: &str,
    contexto: &str,
) -> Option<Npc> {
    let npc = gerar_personagem(llm, location_id, contexto).await?;

    if let Err(err) = db::upsert_npc(pool, &npc).await {
        tracing::error!(%err, npc_id = %npc.id, "falha ao persistir personagem gerado");
        return None;
    }

    let resumo = npc.descricao.chars().take(120).collect::<String>();
    if let Err(err) = db::registrar_acao_global(
        pool,
        turno_global,
        &npc.id,
        "sistema",
        "personagem_gerado",
        &format!("{} apareceu em {}: {}", npc.nome, location_id, resumo),
        location_id,
        &chrono::Utc::now().to_rfc3339(),
    )
    .await
    {
        tracing::error!(%err, npc_id = %npc.id, "falha ao registrar personagem gerado no log global");
    }

    Some(npc)
}
