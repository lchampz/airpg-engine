use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod srd_tables;

/// Skills são determinísticas por padrão (ver Subagentes-e-Skills / Decisoes-Resolvidas).
/// `skill_dados` é o exemplo canônico: rolagem de dados / teste de atributo nunca é
/// decidido por geração de texto, sempre por esta função.
pub const SKILL_DADOS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResultadoDados {
    pub schema_version: u32,
    pub rolagem: u32,
    pub dificuldade: u32,
    pub sucesso: bool,
}

/// Rolagem genérica de um dado de N faces — base para `skill_dados` (teste
/// contra dificuldade) e para iniciativa/dano em combate (ver
/// Change-Sistema-de-Combate), que não têm o conceito de "sucesso/fracasso"
/// contra uma DC, só um valor.
pub fn skill_rolar_dado(faces: u32, seed: u64) -> u32 {
    // Gerador determinístico simples (LCG) para manter a skill previsível/testável.
    // Em produção o seed vem de `rand::random()` (aleatoriedade real), não fixo.
    ((seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407))
        % faces as u64
        + 1) as u32
}

pub fn skill_dados(dificuldade: u32, seed: u64) -> ResultadoDados {
    let rolagem = skill_rolar_dado(20, seed);
    ResultadoDados {
        schema_version: SKILL_DADOS_VERSION,
        rolagem,
        dificuldade,
        sucesso: rolagem >= dificuldade,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolagem_fica_entre_1_e_20() {
        for seed in 0..1000 {
            let r = skill_dados(10, seed);
            assert!(r.rolagem >= 1 && r.rolagem <= 20);
        }
    }

    #[test]
    fn rolar_dado_generico_respeita_faces() {
        for seed in 0..1000 {
            let r = skill_rolar_dado(6, seed);
            assert!(r >= 1 && r <= 6);
        }
    }
}
