use crate::events::{AcaoJogadorPayload, AcaoRejeitadaPayload, Event, EventType};
use crate::state::{Npc, Player};

/// Cap de agentes ativados por turno — ver Decisoes-Resolvidas na documentação da engine.
pub const MAX_AGENTES_POR_TURNO: usize = 4;

/// Timeout por AgentLoop ativado, em segundos — ver Decisoes-Resolvidas.
pub const AGENT_TIMEOUT_SECS: u64 = 8;

pub struct Orchestrator;

impl Orchestrator {
    /// Guardrail de Entrada simplificado: valida se a ação é mecanicamente possível.
    /// Implementação real deve consultar o Estado Rígido (SQLite) — aqui, checagem mínima
    /// de exemplo para manter o esqueleto compilável e testável.
    pub fn validar_acao(&self, player: &Player, acao: &AcaoJogadorPayload) -> Result<(), AcaoRejeitadaPayload> {
        if acao.response.trim().is_empty() {
            return Err(AcaoRejeitadaPayload {
                motivo: "acao_vazia".into(),
                detalhe: "a ação do jogador não pode ser vazia".into(),
            });
        }
        let _ = player;
        Ok(())
    }

    /// Roteamento: todo NPC na mesma location do jogador é candidato; cap de
    /// MAX_AGENTES_POR_TURNO por turno (ver Decisoes-Resolvidas).
    pub fn rotear_agentes<'a>(&self, player: &Player, npcs: &'a [Npc]) -> Vec<&'a Npc> {
        npcs.iter()
            .filter(|npc| npc.location_id == player.location_id)
            .take(MAX_AGENTES_POR_TURNO)
            .collect()
    }

    pub fn evento_fim_de_turno(&self, turno: u64, agentes: &[&Npc]) -> Event {
        Event::new(
            EventType::FimDeTurno,
            "orquestrador",
            turno,
            serde_json::json!({
                "turno": turno,
                "agentes_participantes": agentes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Hp, NpcStatus};
    use std::collections::HashMap;

    fn jogador() -> Player {
        Player {
            id: "player_01".into(),
            hp: Hp { atual: 10, maximo: 10 },
            atributos: HashMap::new(),
            inventario: vec![],
            location_id: "taverna".into(),
            nivel: 1,
            classe: "guerreiro".into(),
        }
    }

    #[test]
    fn rejeita_acao_vazia() {
        let orch = Orchestrator;
        let p = jogador();
        let acao = AcaoJogadorPayload { situation: "".into(), response: "".into() };
        assert!(orch.validar_acao(&p, &acao).is_err());
    }

    #[test]
    fn roteamento_respeita_cap_e_localizacao() {
        let orch = Orchestrator;
        let p = jogador();
        let npcs: Vec<Npc> = (0..10)
            .map(|i| Npc {
                id: format!("npc_{i}"),
                nome: format!("NPC {i}"),
                status: NpcStatus::Vivo,
                atitude_com_jogador: "neutro".into(),
                location_id: if i % 2 == 0 { "taverna".into() } else { "floresta".into() },
                autonomo: false,
            })
            .collect();
        let roteados = orch.rotear_agentes(&p, &npcs);
        assert!(roteados.len() <= MAX_AGENTES_POR_TURNO);
        assert!(roteados.iter().all(|n| n.location_id == "taverna"));
    }
}
