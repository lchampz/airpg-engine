use crate::combate::TipoAcaoCombate;
use crate::db;
use crate::jsonutil::extrair_json;
use crate::llm::LlmClient;
use crate::state::{Cena, Npc};
use serde::Deserialize;
use sqlx::sqlite::SqlitePool;

/// O Mestre de Jogo é o único papel que cria e enriquece fatos de mundo (ver
/// Mestre-de-Jogo-e-Cena no vault). Agentes/NPCs nunca inventam isso — só leem
/// a Cena já resolvida, como contexto read-only.
const SYSTEM_PROMPT_CRIACAO: &str = r#"Você é o Mestre de Jogo de um RPG de fantasia medieval. Dado um identificador técnico de local, invente um nome evocativo e uma descrição curta (1-2 frases) para esse lugar.
Responda APENAS com um JSON no formato {"nome": "...", "descricao": "..."}."#;

#[derive(Debug, Deserialize)]
struct CenaGerada {
    nome: String,
    descricao: String,
}

/// Resolve a Cena de um `location_id`: carrega do Estado Rígido se já existe,
/// senão pede ao Mestre de Jogo para criar uma vez. Chamadas seguintes para o
/// mesmo `location_id` não pagam custo de LLM nenhum.
pub async fn resolver_cena(pool: &SqlitePool, llm: &LlmClient, location_id: &str) -> Cena {
    if let Ok(Some(cena)) = db::get_cena(pool, location_id).await {
        return cena;
    }

    let entrada = format!("Identificador do local: {location_id}");
    let gerada = match llm.complete(SYSTEM_PROMPT_CRIACAO, &entrada).await {
        Ok(resposta) => extrair_json::<CenaGerada>(&resposta),
        Err(err) => {
            tracing::error!(%err, %location_id, "falha ao consultar o Mestre de Jogo para criar a cena");
            None
        }
    };

    let cena = match gerada {
        Some(g) => Cena { location_id: location_id.to_string(), nome: g.nome, descricao: g.descricao, fatos_estabelecidos: vec![] },
        None => Cena {
            location_id: location_id.to_string(),
            nome: location_id.replace('_', " "),
            descricao: "Um lugar ainda pouco descrito.".to_string(),
            fatos_estabelecidos: vec![],
        },
    };

    if let Err(err) = db::upsert_cena(pool, &cena).await {
        tracing::error!(%err, %location_id, "falha ao persistir cena nova");
    }

    cena
}

const ATRIBUTOS_VALIDOS: &[&str] =
    &["forca", "destreza", "constituicao", "inteligencia", "sabedoria", "carisma"];
const DIFICULDADE_MIN: u32 = 5;
const DIFICULDADE_MAX: u32 = 25;

const SYSTEM_PROMPT_VERIFICACAO: &str = r#"Você é o Mestre de Jogo de um RPG de fantasia medieval, seguindo regras de mesa clássicas (estilo D&D). Decida se a ação do jogador exige um teste de dados (skill check) e, se sim, qual atributo e dificuldade (DC).
Responda APENAS com um JSON no formato {"precisa_teste": bool, "atributo": "forca|destreza|constituicao|inteligencia|sabedoria|carisma", "dificuldade": 5 a 25, "descricao": "o que está sendo tentado, em poucas palavras"}.
Exija teste (precisa_teste: true) para ações com risco real de falha ou consequência: persuadir, enganar, escalar, arrombar, lutar, resistir a veneno/medo, notar algo escondido, equilibrar-se, etc.
NÃO exija teste (precisa_teste: false) para ações triviais, sociais neutras ou puramente narrativas: cumprimentar, andar, perguntar algo direto, observar o óbvio, comprar algo com dinheiro suficiente."#;

#[derive(Debug, Deserialize)]
struct VerificacaoProposta {
    precisa_teste: bool,
    #[serde(default)]
    atributo: String,
    #[serde(default)]
    dificuldade: u32,
    #[serde(default)]
    descricao: String,
}

/// Resultado validado de uma decisão do Mestre de Jogo sobre se/como testar a
/// ação do jogador — nunca aplicado sem passar pelos limites abaixo. Ver
/// Subagentes-e-Skills: a rolagem em si (`skills::skill_dados`) é sempre
/// determinística, só a decisão de *quando* testar vem do LLM.
pub struct VerificacaoTeste {
    pub precisa_teste: bool,
    pub atributo: String,
    pub dificuldade: u32,
    pub descricao: String,
}

/// O Mestre de Jogo decide se a ação do turno exige um teste — nunca o
/// resultado do teste em si, que é sempre a rolagem determinística de
/// `skills::skill_dados`. Isso é o que impede a IA de simplesmente narrar um
/// sucesso ou fracasso por conveniência dramática.
pub async fn avaliar_verificacao(llm: &LlmClient, acao: &str) -> VerificacaoTeste {
    let proposta = match llm.complete(SYSTEM_PROMPT_VERIFICACAO, acao).await {
        Ok(resposta) => extrair_json::<VerificacaoProposta>(&resposta),
        Err(err) => {
            tracing::error!(%err, "falha ao consultar o Mestre de Jogo para decidir teste de dados");
            None
        }
    };

    match proposta {
        Some(p) if p.precisa_teste => VerificacaoTeste {
            precisa_teste: true,
            atributo: if ATRIBUTOS_VALIDOS.contains(&p.atributo.as_str()) { p.atributo } else { "geral".to_string() },
            dificuldade: p.dificuldade.clamp(DIFICULDADE_MIN, DIFICULDADE_MAX),
            descricao: p.descricao,
        },
        _ => VerificacaoTeste { precisa_teste: false, atributo: String::new(), dificuldade: 0, descricao: String::new() },
    }
}

const SYSTEM_PROMPT_INICIO_COMBATE: &str = r#"Você é o Mestre de Jogo de um RPG de fantasia medieval. Existem criaturas hostis presentes na cena. Decida se a ação do jogador inicia combate contra uma delas.
Responda APENAS com um JSON no formato {"inicia_combate": bool, "alvo_id": "id da criatura ou vazio"}.
Só inicie combate se o jogador claramente ataca, ameaça fisicamente, ou é atacado primeiro. Conversa, negociação ou observação NÃO inicia combate."#;

#[derive(Debug, Deserialize)]
struct InicioCombateProposto {
    inicia_combate: bool,
    #[serde(default)]
    alvo_id: String,
}

/// Decide se a ação do turno inicia combate contra um dos NPCs combatentes
/// presentes — nunca decide o resultado do combate em si (isso é sempre
/// `combate::resolver_rodada`, determinístico). Só considera `alvo_id` que de
/// fato está na lista de combatentes recebida — não confia cegamente no que o
/// LLM devolve.
pub async fn avaliar_inicio_combate(llm: &LlmClient, acao: &str, combatentes: &[&Npc]) -> Option<String> {
    if combatentes.is_empty() {
        return None;
    }

    let lista = combatentes.iter().map(|n| format!("{} (id: {})", n.nome, n.id)).collect::<Vec<_>>().join(", ");
    let entrada = format!("Criaturas hostis presentes: {lista}\nAção do jogador: {acao}");

    let proposta = match llm.complete(SYSTEM_PROMPT_INICIO_COMBATE, &entrada).await {
        Ok(resposta) => extrair_json::<InicioCombateProposto>(&resposta),
        Err(err) => {
            tracing::error!(%err, "falha ao consultar o Mestre de Jogo para decidir inicio de combate");
            None
        }
    };

    proposta
        .filter(|p| p.inicia_combate)
        .and_then(|p| combatentes.iter().find(|n| n.id == p.alvo_id).map(|n| n.id.clone()))
}

const SYSTEM_PROMPT_ACAO_COMBATE: &str = r#"Você é o Mestre de Jogo de um RPG de fantasia medieval. O jogador está em combate. Classifique a ação dele.
Responda APENAS com um JSON no formato {"tipo": "atacar" | "fugir" | "outro"}."#;

#[derive(Debug, Deserialize)]
struct AcaoCombateProposta {
    tipo: String,
}

/// Classifica a ação do jogador dentro de um combate já em andamento — o
/// resultado mecânico (acerto/dano) continua sempre em `combate::resolver_rodada`.
pub async fn avaliar_acao_combate(llm: &LlmClient, acao: &str) -> TipoAcaoCombate {
    match llm.complete(SYSTEM_PROMPT_ACAO_COMBATE, acao).await {
        Ok(resposta) => match extrair_json::<AcaoCombateProposta>(&resposta) {
            Some(p) if p.tipo == "atacar" => TipoAcaoCombate::Atacar,
            Some(p) if p.tipo == "fugir" => TipoAcaoCombate::Fugir,
            _ => TipoAcaoCombate::Outro,
        },
        Err(err) => {
            tracing::error!(%err, "falha ao classificar acao de combate, tratando como ataque");
            TipoAcaoCombate::Atacar
        }
    }
}
