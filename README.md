# airpg-engine

Núcleo do turno do AIRPG, em Rust (Axum + Tokio + SQLite/sqlx).

Responsabilidades: Orquestrador Central, Guardrails Narrativos (entrada/saída), Pool de Agentes reativo, Skills determinísticas, e a fonte única de verdade do Estado Rígido do jogo.

Este crate é agnóstico de frontend e de provedor de LLM — expõe apenas HTTP/SSE e fala com qualquer modelo através de um gateway LiteLLM (OpenAI-compatible). O contrato público com o frontend é o conjunto de eventos tipados em `src/events`.

## Rodando localmente (com LLM local via Ollama + LiteLLM)

Pré-requisitos rodando em paralelo:

```bash
# 1. Ollama servindo localmente (porta 11434)
ollama serve   # ou já roda como serviço

# 2. LiteLLM como gateway OpenAI-compatible (porta 4000), ver litellm.config.yaml na raiz do monorepo
pipx install 'litellm[proxy]==1.55.0' --python python3.12   # versões mais novas exigem toolchain Rust mais recente para o bridge nativo
litellm --config ../litellm.config.yaml --port 4000

# 3. NATS local (porta 4222) — barramento com o airpg-world
brew install nats-server
nats-server

# 4. o engine
cargo run
```

Variáveis de ambiente (todas com default sensato para dev local):
- `DATABASE_URL` (default `sqlite://airpg.db?mode=rwc`)
- `NATS_URL` (default `nats://localhost:4222`)
- `LITELLM_URL` (default `http://localhost:4000`)
- `LITELLM_API_KEY` (default `sk-airpg-local-dev`, deve casar com `general_settings.master_key` do `litellm.config.yaml`)
- `LITELLM_MODEL` (default `airpg-local`, o `model_name` configurado no LiteLLM)

Endpoints:
- `GET /health`
- `POST /turn` — recebe `{ "situation": "...", "response": "..." }`: roda o Guardrail de Entrada, gera o diálogo do agente reativo via LLM, filtra pelo Guardrail de Saída (segunda chamada de LLM), publica o evento no NATS e devolve o evento final

## Status

Walking skeleton com IA real ponta a ponta, testado contra Ollama local (`llama3.2`) via LiteLLM:
- Guardrail de Entrada valida a ação (checagem mínima hoje, sem consultar o Estado Rígido ainda)
- Diálogo do agente reativo é gerado por LLM real (hoje fixo em um único NPC — roteamento completo do Pool de Agentes com cap de 4/turno ainda não plugado neste endpoint)
- Guardrail de Saída roda uma segunda chamada de LLM pedindo um veredito estruturado em JSON (`{"aprovado": bool, "motivo": ...}`), com fallback seguro (aprova) se a resposta não for parseável ou a chamada falhar
- NATS conectado de fato: publica todo evento do turno e assina o barramento compartilhado com o `airpg-world` (recebe `colisao_jogador_agente` de NPCs autônomos, hoje só loga)

**Latência observada localmente**: 13–46s por turno com `llama3.2` em CPU via Ollama (duas chamadas de LLM sequenciais: diálogo + guardrail de saída). Muito acima da meta de 5s/turno definida em `Decisoes-Resolvidas` — aceitável para debugar comportamento da IA, não para uso real. Antes de otimizar, considerar: modelo menor/quantizado, rodar as duas chamadas em paralelo quando possível, ou aceitar que produção usará um provedor hospedado (a troca é só de config no LiteLLM, o código do engine não muda).

Ainda não implementado: roteamento completo do Pool de Agentes (múltiplos NPCs por turno), persistência real do Estado Rígido nos endpoints (o pool SQLite existe, tabelas ainda não são lidas/escritas), reação do engine ao receber `colisao_jogador_agente` (hoje só loga, não aciona o Pool de Agentes automaticamente).

## Contexto arquitetural

Este repositório é um dos submódulos do monorepo [airpg](https://github.com/lchampz/airpg). A documentação completa da arquitetura (eventos, fluxo de turno, decisões técnicas) vive no vault Obsidian do projeto.
