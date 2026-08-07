//! RAG (Retrieval-Augmented Generation) sobre o SRD 5.1 (ver
//! Change-RAG-SRD-e-Desktop no vault). Objetivo: fundamentar a narração do
//! Mestre de Jogo/NPCs em texto de regra real, em vez de deixar o LLM
//! inventar CD/dano/efeito de condição pela metade.
//!
//! Deliberadamente **sem `sqlite-vec`** (extensão nativa de SQLite): com
//! ~700 chunks (368 vetores de 384 dimensões ≈ 1MB), busca por similaridade
//! por força bruta em memória é instantânea e evita depender de uma
//! extensão nativa por plataforma — o que complicaria exatamente o
//! empacotamento desktop "leve" que é o objetivo final desta frente. Se o
//! volume de chunks crescer ordens de magnitude (ex: bestiário completo,
//! múltiplos sourcebooks), reavaliar para `sqlite-vec` nesse momento.

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use sqlx::sqlite::SqlitePool;
use std::sync::Mutex;

#[derive(Debug, Clone, serde::Serialize)]
pub struct RegraChunk {
    pub categoria: String,
    pub titulo: String,
    pub texto: String,
}

/// Índice carregado uma vez na inicialização do engine e mantido em memória
/// pelo resto do processo — os chunks do SRD não mudam em runtime.
pub struct RagIndex {
    chunks: Vec<(RegraChunk, Vec<f32>)>,
    embedder: Mutex<TextEmbedding>,
}

/// Carrega o índice a partir de `regras_srd_chunks`. Retorna `None` (não
/// erro) se a tabela estiver vazia — RAG é aditivo/opcional: sem dados
/// ingeridos (ver `bin/ingest_srd.rs`), o engine continua funcionando
/// exatamente como antes, só sem o contexto de regra extra no prompt.
pub async fn carregar(pool: &SqlitePool) -> anyhow::Result<Option<RagIndex>> {
    let rows: Vec<(String, String, String, Vec<u8>)> =
        sqlx::query_as("SELECT categoria, titulo, texto, embedding FROM regras_srd_chunks")
            .fetch_all(pool)
            .await?;

    if rows.is_empty() {
        tracing::info!(
            "rag: tabela regras_srd_chunks vazia, RAG desativado (rode ingest_srd para popular)"
        );
        return Ok(None);
    }

    let chunks: Vec<(RegraChunk, Vec<f32>)> = rows
        .into_iter()
        .map(|(categoria, titulo, texto, embedding_bytes)| {
            let embedding = decodificar_embedding(&embedding_bytes);
            (
                RegraChunk {
                    categoria,
                    titulo,
                    texto,
                },
                embedding,
            )
        })
        .collect();

    let embedder = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))?;

    tracing::info!(chunks = chunks.len(), "rag: índice carregado");
    Ok(Some(RagIndex {
        chunks,
        embedder: Mutex::new(embedder),
    }))
}

fn decodificar_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn similaridade_cosseno(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norma_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norma_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norma_a == 0.0 || norma_b == 0.0 {
        return 0.0;
    }
    dot / (norma_a * norma_b)
}

impl RagIndex {
    /// Busca os `k` chunks mais relevantes pra uma query em texto livre
    /// (a ação do jogador, ou o texto que o Mestre de Jogo está avaliando).
    /// Roda a inferência de embedding em `spawn_blocking` — é trabalho de
    /// CPU (ONNX Runtime), não deve bloquear o executor async do Axum.
    pub async fn buscar_relevantes(
        self: &std::sync::Arc<Self>,
        query: &str,
        k: usize,
    ) -> Vec<RegraChunk> {
        let index = self.clone();
        let query = query.to_string();
        match tokio::task::spawn_blocking(move || index.buscar_relevantes_sync(&query, k)).await {
            Ok(resultado) => resultado,
            Err(err) => {
                tracing::error!(%err, "rag: task de busca falhou (panic?)");
                vec![]
            }
        }
    }

    fn buscar_relevantes_sync(&self, query: &str, k: usize) -> Vec<RegraChunk> {
        let query_embedding = {
            let mut embedder = match self.embedder.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    tracing::error!(%err, "rag: mutex do embedder envenenado");
                    return vec![];
                }
            };
            match embedder.embed(vec![query], None) {
                Ok(mut embeddings) if !embeddings.is_empty() => embeddings.remove(0),
                Ok(_) => return vec![],
                Err(err) => {
                    tracing::error!(%err, "rag: falha ao gerar embedding da query");
                    return vec![];
                }
            }
        };

        let mut pontuados: Vec<(f32, &RegraChunk)> = self
            .chunks
            .iter()
            .map(|(chunk, emb)| (similaridade_cosseno(&query_embedding, emb), chunk))
            .collect();

        pontuados.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        pontuados
            .into_iter()
            .take(k)
            .map(|(_, chunk)| chunk.clone())
            .collect()
    }
}

/// Formata os chunks recuperados pra injeção direta em prompt de LLM.
/// Retorna string vazia se não houver nada relevante (chamador decide se
/// omite a seção inteira do prompt nesse caso).
pub fn formatar_para_prompt(chunks: &[RegraChunk]) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    chunks
        .iter()
        .map(|c| format!("[{}] {}: {}", c.categoria, c.titulo, c.texto))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similaridade_de_vetor_com_ele_mesmo_e_maxima() {
        let v = vec![0.5, 0.3, -0.2, 0.8];
        let sim = similaridade_cosseno(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "esperado ~1.0, obtido {sim}");
    }

    #[test]
    fn similaridade_de_vetores_ortogonais_e_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((similaridade_cosseno(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn similaridade_com_vetor_zero_nao_gera_nan_nem_divisao_por_zero() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(similaridade_cosseno(&a, &b), 0.0);
    }

    #[test]
    fn decodifica_embedding_ida_e_volta() {
        let original = vec![1.5_f32, -2.25, 0.0, 100.0];
        let bytes: Vec<u8> = original.iter().flat_map(|f| f.to_le_bytes()).collect();
        let decodificado = decodificar_embedding(&bytes);
        assert_eq!(original, decodificado);
    }

    #[test]
    fn formatar_para_prompt_vazio_retorna_string_vazia() {
        assert_eq!(formatar_para_prompt(&[]), "");
    }

    #[test]
    fn formatar_para_prompt_inclui_categoria_titulo_e_texto() {
        let chunks = vec![RegraChunk {
            categoria: "condicao".to_string(),
            titulo: "Charmed".to_string(),
            texto: "não pode atacar quem o encantou".to_string(),
        }];
        let formatado = formatar_para_prompt(&chunks);
        assert!(formatado.contains("condicao"));
        assert!(formatado.contains("Charmed"));
        assert!(formatado.contains("não pode atacar"));
    }
}
