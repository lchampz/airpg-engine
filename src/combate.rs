use crate::db;
use crate::events::Event;
use crate::events::EventType;
use crate::skills::skill_rolar_dado;
use crate::state::{nivel_por_xp, Combate, Npc, Player, PLAYER_CA_PADRAO};
use sqlx::sqlite::SqlitePool;

/// Sistema de combate — jogador contra um único NPC hostil por vez (ver
/// Change-Sistema-de-Combate; múltiplos combatentes simultâneos ficou fora do
/// escopo inicial de propósito). Toda rolagem é `skills::skill_rolar_dado`,
/// nunca decidida por texto gerado — o Mestre de Jogo só decide *quando* um
/// combate começa e *que tipo* de ação o jogador está tentando, nunca o
/// resultado numérico em si (mesmo princípio do sistema de testes de dados).
const DANO_JOGADOR_FACES_PADRAO: u32 = 6;
const DIFICULDADE_FUGA_PADRAO: u32 = 12;

pub enum TipoAcaoCombate {
    Atacar,
    Fugir,
    Outro,
}

pub struct ResultadoRodada {
    pub eventos: Vec<Event>,
    pub combate_encerrado: bool,
}

/// Só NPCs com `hp`/`classe_armadura` definidos podem entrar em combate (ver
/// Change-Sistema-de-Combate — `Option` é proposital, nem todo NPC é
/// combatente).
pub fn npcs_combatentes<'a>(npcs: &'a [Npc]) -> Vec<&'a Npc> {
    npcs.iter().filter(|n| n.hp.is_some() && n.status != crate::state::NpcStatus::Morto).collect()
}

pub async fn iniciar(pool: &SqlitePool, player_id: &str, npc_id: &str, seed: u64) -> anyhow::Result<Combate> {
    // Iniciativa é só flavour no MVP (rodadas resolvem os dois lados no mesmo
    // turno) — rolada mesmo assim para manter o conceito presente e permitir
    // evoluir para ordem real depois sem quebrar o schema.
    let _iniciativa_jogador = skill_rolar_dado(20, seed);
    let _iniciativa_npc = skill_rolar_dado(20, seed.wrapping_add(1));

    let combate = Combate { player_id: player_id.to_string(), npc_id: npc_id.to_string(), rodada: 1, ativo: true };
    db::salvar_combate(pool, &combate).await?;
    Ok(combate)
}

/// Resolve uma rodada completa: ação do jogador (se for ataque) seguida da
/// reação do NPC (se ele sobreviver e o jogador não tiver fugido). Aplica
/// dano/morte/XP/loot diretamente no `Player`/`Npc` em memória — o chamador é
/// responsável por persistir depois (mesmo padrão de `state_changes.rs`).
pub fn resolver_rodada(
    combate: &mut Combate,
    player: &mut Player,
    npc: &mut Npc,
    acao: TipoAcaoCombate,
    turno: u64,
    seed: u64,
) -> ResultadoRodada {
    let mut eventos = Vec::new();
    let mut seed = seed;
    let mut proximo_seed = || {
        seed = seed.wrapping_mul(2862933555777941757).wrapping_add(3037000493);
        seed
    };

    if matches!(acao, TipoAcaoCombate::Fugir) {
        let dificuldade = DIFICULDADE_FUGA_PADRAO;
        let rolagem = skill_rolar_dado(20, proximo_seed());
        let fugiu = rolagem >= dificuldade;
        eventos.push(Event::new(
            EventType::AtaqueResolvido,
            player.id.clone(),
            turno,
            serde_json::json!({ "tipo": "fuga", "rolagem": rolagem, "dificuldade": dificuldade, "sucesso": fugiu }),
        ));
        if fugiu {
            combate.ativo = false;
            eventos.push(Event::new(
                EventType::CombateEncerrado,
                "orquestrador",
                turno,
                serde_json::json!({ "npc_id": npc.id, "motivo": "jogador_fugiu" }),
            ));
            return ResultadoRodada { eventos, combate_encerrado: true };
        }
        // Fuga falhou: o NPC ainda ataca de volta abaixo.
    }

    if matches!(acao, TipoAcaoCombate::Atacar) {
        let ca_npc = npc.classe_armadura.unwrap_or(10);
        let rolagem = skill_rolar_dado(20, proximo_seed());
        let acertou = rolagem >= ca_npc;

        let mut dano_aplicado = 0;
        if acertou {
            dano_aplicado = skill_rolar_dado(DANO_JOGADOR_FACES_PADRAO, proximo_seed()) as i32;
            if let Some(hp) = &mut npc.hp {
                hp.atual = (hp.atual - dano_aplicado).max(0);
            }
        }

        eventos.push(Event::new(
            EventType::AtaqueResolvido,
            player.id.clone(),
            turno,
            serde_json::json!({ "tipo": "ataque", "alvo": npc.id, "rolagem": rolagem, "ca_alvo": ca_npc, "acerto": acertou, "dano_aplicado": dano_aplicado }),
        ));

        if let Some(hp) = &npc.hp {
            if hp.atual == 0 {
                npc.status = crate::state::NpcStatus::Morto;
                aplicar_recompensa(player, npc, turno, &mut eventos);
                combate.ativo = false;
                eventos.push(Event::new(
                    EventType::CombateEncerrado,
                    "orquestrador",
                    turno,
                    serde_json::json!({ "npc_id": npc.id, "motivo": "inimigo_derrotado" }),
                ));
                return ResultadoRodada { eventos, combate_encerrado: true };
            }
        }
    }

    // Reação do NPC — só chega aqui se ele continua vivo e o jogador não fugiu.
    let ca_jogador = PLAYER_CA_PADRAO;
    let rolagem_npc = skill_rolar_dado(20, proximo_seed());
    let npc_acertou = rolagem_npc >= ca_jogador;
    let mut dano_npc = 0;
    if npc_acertou {
        dano_npc = skill_rolar_dado(npc.dano_dado_faces.unwrap_or(4), proximo_seed()) as i32;
        player.hp.atual = (player.hp.atual - dano_npc).max(0);
    }

    eventos.push(Event::new(
        EventType::AtaqueResolvido,
        npc.id.clone(),
        turno,
        serde_json::json!({ "tipo": "ataque", "alvo": player.id, "rolagem": rolagem_npc, "ca_alvo": ca_jogador, "acerto": npc_acertou, "dano_aplicado": dano_npc }),
    ));

    if player.hp.atual == 0 {
        combate.ativo = false;
        eventos.push(Event::new(EventType::JogadorMorreu, "orquestrador", turno, serde_json::json!({ "causa": format!("combate contra {}", npc.nome) })));
        eventos.push(Event::new(
            EventType::CombateEncerrado,
            "orquestrador",
            turno,
            serde_json::json!({ "npc_id": npc.id, "motivo": "jogador_morreu" }),
        ));
        return ResultadoRodada { eventos, combate_encerrado: true };
    }

    combate.rodada += 1;
    ResultadoRodada { eventos, combate_encerrado: false }
}

/// XP e loot são dados fixos do próprio NPC, não uma proposta do LLM — sem
/// ambiguidade a resolver, então sem chamada de LLM nem risco de alucinação
/// (ver Change-Sistema-de-XP-Progressao / Subagentes-e-Skills).
fn aplicar_recompensa(player: &mut Player, npc: &Npc, turno: u64, eventos: &mut Vec<Event>) {
    if let Some(xp) = npc.xp_recompensa {
        let nivel_anterior = player.nivel;
        player.xp += xp;
        player.nivel = nivel_por_xp(player.xp);

        eventos.push(Event::new(
            EventType::MudancaEstado,
            "orquestrador",
            turno,
            serde_json::json!({ "campo": "player.xp", "operacao": "somar", "valor": xp, "atual": player.xp }),
        ));

        if player.nivel > nivel_anterior {
            eventos.push(Event::new(
                EventType::SubiuDeNivel,
                "orquestrador",
                turno,
                serde_json::json!({ "nivel_anterior": nivel_anterior, "nivel_novo": player.nivel }),
            ));
        }
    }

    for item in &npc.loot {
        if !player.inventario.contains(item) {
            player.inventario.push(item.clone());
            eventos.push(Event::new(
                EventType::MudancaEstado,
                "orquestrador",
                turno,
                serde_json::json!({ "campo": "player.inventario", "operacao": "adicionar", "valor": item }),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Hp, NpcStatus};

    fn jogador() -> Player {
        Player {
            id: "p1".into(),
            hp: Hp { atual: 10, maximo: 10 },
            atributos: Default::default(),
            inventario: vec![],
            location_id: "arena".into(),
            nivel: 1,
            classe: "guerreiro".into(),
            xp: 0,
        }
    }

    fn lobo() -> Npc {
        Npc {
            id: "npc_lobo".into(),
            nome: "Lobo".into(),
            status: NpcStatus::Hostil,
            atitude_com_jogador: "hostil".into(),
            location_id: "arena".into(),
            autonomo: false,
            hp: Some(Hp { atual: 12, maximo: 12 }),
            classe_armadura: Some(12),
            dano_dado_faces: Some(4),
            xp_recompensa: Some(50),
            loot: vec!["presa".into()],
            descricao: String::new(),
            deslocamento: None,
            imunidades: vec![],
            resistencias: vec![],
        }
    }

    fn combate() -> Combate {
        Combate { player_id: "p1".into(), npc_id: "npc_lobo".into(), rodada: 1, ativo: true }
    }

    #[test]
    fn npc_nunca_fica_com_hp_negativo_em_muitas_rodadas() {
        for seed in 0..500 {
            let mut c = combate();
            let mut p = jogador();
            let mut n = lobo();
            let r = resolver_rodada(&mut c, &mut p, &mut n, TipoAcaoCombate::Atacar, 0, seed);
            assert!(n.hp.as_ref().unwrap().atual >= 0);
            assert!(p.hp.atual >= 0);
            let _ = r;
        }
    }

    #[test]
    fn npc_morre_gera_xp_e_loot_e_encerra_combate() {
        // Ataca repetidamente até matar (com HP baixo pra convergir rápido em poucas rodadas de teste).
        let mut c = combate();
        let mut p = jogador();
        let mut n = lobo();
        n.hp = Some(Hp { atual: 1, maximo: 12 });

        let mut seed = 7u64;
        let mut encerrado = false;
        for _ in 0..30 {
            let r = resolver_rodada(&mut c, &mut p, &mut n, TipoAcaoCombate::Atacar, 0, seed);
            seed = seed.wrapping_add(1);
            if r.combate_encerrado {
                encerrado = true;
                if n.status == NpcStatus::Morto {
                    assert_eq!(p.xp, 50);
                    assert!(p.inventario.contains(&"presa".to_string()));
                }
                break;
            }
            if p.hp.atual == 0 {
                break;
            }
        }
        assert!(encerrado, "combate deveria ter encerrado em 30 rodadas (jogador ou npc morre)");
    }

    #[test]
    fn fuga_bem_sucedida_encerra_combate_sem_dano_do_npc() {
        // Busca um seed onde a fuga (rolagem >= 12) tem sucesso.
        for seed in 0..200 {
            let mut c = combate();
            let mut p = jogador();
            let mut n = lobo();
            let r = resolver_rodada(&mut c, &mut p, &mut n, TipoAcaoCombate::Fugir, 0, seed);
            if r.combate_encerrado && p.hp.atual == 10 {
                // fugiu sem sofrer ataque de volta
                return;
            }
        }
        panic!("nenhum seed testado produziu fuga bem sucedida — heurística pode estar errada");
    }
}
