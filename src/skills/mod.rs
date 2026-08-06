use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

pub fn skill_dados(dificuldade: u32, seed: u64) -> ResultadoDados {
    // Gerador determinístico simples (LCG) para manter a skill previsível/testável;
    // trocar por uma fonte de aleatoriedade real antes de produção.
    let rolagem = ((seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)) % 20 + 1) as u32;
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
}
