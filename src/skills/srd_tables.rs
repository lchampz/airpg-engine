//! Tabelas e fórmulas do SRD 5.1 traduzidas em código determinístico — mesmo
//! princípio de `skills/mod.rs`: regra numérica nunca é decidida por LLM, só
//! por função testável (ver Change-RAG-SRD-e-Desktop / Subagentes-e-Skills).

/// Bônus de proficiência por nível de personagem (ver SRD 5.1, tabela de
/// progressão — o mesmo bônus vale pra ataque, teste de perícia treinada e
/// CD de conjuração).
///
/// Sem call site em produção ainda (ver `skill_cd_conjuracao` abaixo pro
/// porquê) — `#[allow(dead_code)]` é intencional, não um TODO solto.
#[allow(dead_code)]
pub fn bonus_proficiencia(nivel: u8) -> i32 {
    match nivel {
        1..=4 => 2,
        5..=8 => 3,
        9..=12 => 4,
        13..=16 => 5,
        _ => 6,
    }
}

/// Modificador de atributo a partir do valor bruto (ex: Inteligência 16 → +3).
/// Fórmula do SRD: `(valor - 10) / 2`, arredondado pra baixo (inclusive
/// atributos abaixo de 10, que geram modificador negativo).
#[allow(dead_code)]
pub fn modificador_atributo(valor_atributo: i32) -> i32 {
    (valor_atributo - 10).div_euclid(2)
}

/// CD de conjuração = 8 + bônus de proficiência + modificador do atributo de
/// conjuração (ver SRD 5.1). Quem decide *que* magia foi conjurada e *qual*
/// atributo ela usa continua sendo o Mestre de Jogo/ficha do personagem —
/// esta função só resolve o número, nunca decide sozinha se uma CD se aplica.
///
/// Sem call site em produção ainda: o engine não tem um sistema de
/// conjuração de magia por NPC/jogador (nenhum "lançar feitiço" como ação
/// distinta existe hoje, só diálogo/combate físico genérico) — ver
/// Change-RAG-SRD-e-Desktop, Fase 3. Fica pronta e testada contra um valor
/// de referência real do SRD pra quando esse sistema existir.
#[allow(dead_code)]
pub fn skill_cd_conjuracao(nivel: u8, valor_atributo: i32) -> u32 {
    let cd = 8 + bonus_proficiencia(nivel) + modificador_atributo(valor_atributo);
    cd.max(0) as u32
}

/// Efeito de resistência/imunidade a um tipo de dano sobre o valor final —
/// ver SRD 5.1: imunidade anula completamente, resistência reduz à metade
/// (arredondado pra baixo), sem efeito caso contrário.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiplicadorDano {
    Imune,
    Resistente,
    Normal,
}

impl MultiplicadorDano {
    pub fn aplicar(self, dano: i32) -> i32 {
        match self {
            MultiplicadorDano::Imune => 0,
            MultiplicadorDano::Resistente => dano / 2,
            MultiplicadorDano::Normal => dano,
        }
    }
}

/// Resolve o multiplicador de dano de `tipo_dano` contra as
/// `imunidades`/`resistencias` de um alvo (campos já existentes em `Npc`,
/// ver `state/mod.rs` — populados no bestiário mas, antes desta função,
/// nunca checados mecanicamente em combate). Comparação case-insensitive:
/// dado vindo de LLM/seed pode variar capitalização ("Frio" vs "frio").
pub fn skill_resolver_resistencia(
    tipo_dano: &str,
    imunidades: &[String],
    resistencias: &[String],
) -> MultiplicadorDano {
    let alvo = tipo_dano.to_lowercase();
    if imunidades.iter().any(|i| i.to_lowercase() == alvo) {
        MultiplicadorDano::Imune
    } else if resistencias.iter().any(|r| r.to_lowercase() == alvo) {
        MultiplicadorDano::Resistente
    } else {
        MultiplicadorDano::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bonus_proficiencia_segue_a_tabela_do_srd() {
        assert_eq!(bonus_proficiencia(1), 2);
        assert_eq!(bonus_proficiencia(4), 2);
        assert_eq!(bonus_proficiencia(5), 3);
        assert_eq!(bonus_proficiencia(8), 3);
        assert_eq!(bonus_proficiencia(9), 4);
        assert_eq!(bonus_proficiencia(13), 5);
        assert_eq!(bonus_proficiencia(17), 6);
        assert_eq!(bonus_proficiencia(20), 6);
    }

    #[test]
    fn modificador_atributo_arredonda_para_baixo_incluindo_negativos() {
        assert_eq!(modificador_atributo(16), 3);
        assert_eq!(modificador_atributo(10), 0);
        assert_eq!(modificador_atributo(11), 0);
        assert_eq!(modificador_atributo(9), -1);
        assert_eq!(modificador_atributo(8), -1);
        assert_eq!(modificador_atributo(1), -5);
    }

    #[test]
    fn cd_conjuracao_nivel_2_inteligencia_16_e_13() {
        // Valor de referência citado no Change-RAG-SRD-e-Desktop: 8 (base) +
        // 2 (proficiência nível 1-4) + 3 (mod. de INT 16) = 13.
        assert_eq!(skill_cd_conjuracao(2, 16), 13);
    }

    #[test]
    fn cd_conjuracao_cresce_com_nivel_e_atributo() {
        assert!(skill_cd_conjuracao(9, 16) > skill_cd_conjuracao(2, 16));
        assert!(skill_cd_conjuracao(2, 18) > skill_cd_conjuracao(2, 16));
    }

    #[test]
    fn cd_conjuracao_nunca_fica_negativa_com_atributo_muito_baixo() {
        assert_eq!(skill_cd_conjuracao(1, 1), 8 + 2 - 5); // = 5, ainda positivo aqui
        let cd = skill_cd_conjuracao(1, 0);
        assert!(cd as i32 >= 0);
    }

    #[test]
    fn imunidade_zera_o_dano() {
        let imunidades = vec!["fogo".to_string()];
        let resistencias = vec![];
        let mult = skill_resolver_resistencia("fogo", &imunidades, &resistencias);
        assert_eq!(mult, MultiplicadorDano::Imune);
        assert_eq!(mult.aplicar(10), 0);
    }

    #[test]
    fn resistencia_reduz_a_metade_arredondando_para_baixo() {
        let imunidades = vec![];
        let resistencias = vec!["frio".to_string()];
        let mult = skill_resolver_resistencia("frio", &imunidades, &resistencias);
        assert_eq!(mult, MultiplicadorDano::Resistente);
        assert_eq!(mult.aplicar(7), 3);
        assert_eq!(mult.aplicar(10), 5);
    }

    #[test]
    fn sem_resistencia_ou_imunidade_dano_e_normal() {
        let imunidades = vec!["fogo".to_string()];
        let resistencias = vec!["frio".to_string()];
        let mult = skill_resolver_resistencia("cortante", &imunidades, &resistencias);
        assert_eq!(mult, MultiplicadorDano::Normal);
        assert_eq!(mult.aplicar(8), 8);
    }

    #[test]
    fn comparacao_de_tipo_de_dano_ignora_capitalizacao() {
        let imunidades = vec![];
        let resistencias = vec!["Frio".to_string()];
        assert_eq!(
            skill_resolver_resistencia("frio", &imunidades, &resistencias),
            MultiplicadorDano::Resistente
        );
    }
}
