use serde::{Deserialize, Serialize};

/// Cliente para o gateway LLM (LiteLLM). A engine nunca fala direto com um
/// provedor específico — sempre através deste endpoint OpenAI-compatível,
/// para poder trocar Ollama local por qualquer provedor sem tocar no código
/// (ver Stack-Escolhida / Criterios-de-Escolha).
#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: String,
}

impl LlmClient {
    pub fn from_env() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: std::env::var("LITELLM_URL").unwrap_or_else(|_| "http://localhost:4000".into()),
            api_key: std::env::var("LITELLM_API_KEY").unwrap_or_else(|_| "sk-airpg-local-dev".into()),
            model: std::env::var("LITELLM_MODEL").unwrap_or_else(|_| "airpg-local".into()),
        }
    }

    /// Clona este cliente trocando model/api_base/api_key — usado para os
    /// perfis de custo (ver Estrategia-Custo-Tokens no vault): "mundo"
    /// (barato, geopolítica/tick autônomo) e "personagens" (pode ser mais
    /// caro, geração sob demanda). `None` mantém o valor atual do cliente
    /// base (ou seja, o padrão do ambiente).
    pub fn with_config(&self, model: impl Into<String>, api_base: Option<String>, api_key: Option<String>) -> Self {
        Self {
            http: self.http.clone(),
            base_url: api_base.unwrap_or_else(|| self.base_url.clone()),
            api_key: api_key.unwrap_or_else(|| self.api_key.clone()),
            model: model.into(),
        }
    }

    pub async fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
        self.complete_with_history(system, &[], user).await
    }

    /// Como `complete`, mas incluindo o histórico recente de troca (memória de
    /// curto prazo do agente — ver Memoria-Narrativa/AgentLoop-por-Personagem
    /// no vault). `historico` é uma lista de pares (mensagem do jogador,
    /// resposta do agente), do mais antigo para o mais recente.
    pub async fn complete_with_history(
        &self,
        system: &str,
        historico: &[(String, String)],
        user: &str,
    ) -> anyhow::Result<String> {
        let mut messages = vec![ChatMessage { role: "system", content: system }];
        for (prompt, resposta) in historico {
            messages.push(ChatMessage { role: "user", content: prompt });
            messages.push(ChatMessage { role: "assistant", content: resposta });
        }
        messages.push(ChatMessage { role: "user", content: user });

        let req = ChatRequest { model: &self.model, messages, temperature: 0.7 };

        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json::<ChatResponse>()
            .await?;

        Ok(resp
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default())
    }
}
