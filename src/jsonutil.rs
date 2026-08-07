use serde::de::DeserializeOwned;

/// Modelos locais não garantem JSON puro (às vezes envolvem em texto/markdown).
/// Extrai o primeiro objeto `{...}` da resposta antes de tentar desserializar.
/// Usado por qualquer chamada de LLM que espera saída estruturada (Guardrail
/// de Saída, propostas de mudança de estado) — ver Decisoes-Resolvidas.
pub fn extrair_json<T: DeserializeOwned>(resposta: &str) -> Option<T> {
    let inicio = resposta.find('{')?;
    let fim = resposta.rfind('}')?;
    serde_json::from_str(&resposta[inicio..=fim]).ok()
}
