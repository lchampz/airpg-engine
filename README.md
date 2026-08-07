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
- `POST /turn` — recebe `{ "situation": "...", "response": "..." }`: valida a ação (Guardrail de Entrada), roteia até 4 NPCs presentes na mesma `location_id` do jogador, gera a reação de cada um em paralelo via LLM, filtra pelo Guardrail de Saída, propõe e aplica mudanças de estado (ver abaixo), publica tudo no NATS e devolve o lote de eventos do turno (`{ "turno": n, "eventos": [...] }`)

## Status

IA real ponta a ponta, testado contra Ollama local (`llama3.2`) via LiteLLM, local e via Docker:

- **Guardrail de Entrada**: checagem mínima hoje (ação não-vazia), sem consultar o Estado Rígido ainda para validar viabilidade da ação
- **Pool de Agentes reativo com roteamento real**: consulta NPCs por `location_id` no SQLite, ativa até 4 por turno em paralelo — testado com 2 NPCs na mesma sala respondendo e um terceiro em outra sala corretamente ignorado
- **Guardrail de Saída**: segunda chamada de LLM pedindo veredito estruturado (`{"aprovado": bool, "motivo": ...}`), com fallback seguro se não parseável ou se a chamada falhar
- **Persistência de mudança de estado** (`src/state_changes.rs`): o LLM só **sugere** mudanças (dano/cura em `player.hp`, itens em `player.inventario`) via um terceiro tipo de chamada estruturada; o engine valida contra uma whitelist de campo+operação, aplica com bounds (HP nunca sai de `[0, máximo]`, remoção de item exige que ele exista), persiste no SQLite e só então emite `mudanca_estado`. Testado: dano real reduziu HP de 10→0, cura no turno seguinte confirmou que o 0 tinha persistido (não era só estado em memória do request), e diálogo comum não gera mudança nenhuma (sem alucinação de efeito mecânico)
- **NATS conectado nos dois sentidos**: engine publica todo evento do turno e assina o barramento do `airpg-world`; ao receber `colisao_jogador_agente` de um NPC autônomo, ativa o Pool de Agentes para aquele NPC especificamente e devolve `interacao_finalizada` para o Mundo Vivo retomar autonomia — ciclo completo testado local e via Docker

**Latência observada localmente**: 6–46s por turno com `llama3.2` em CPU via Ollama, variando com quantas chamadas de LLM um turno dispara (diálogo × N agentes + guardrail × N + proposta de mudança de estado). Acima da meta de 5s/turno definida em `Decisoes-Resolvidas` na maioria dos casos — aceitável para debugar comportamento da IA, não para uso real. Produção trocaria de provedor só via config do LiteLLM, sem mudar código do engine.

Ainda não implementado: o Guardrail de Entrada não consulta o Estado Rígido para validar viabilidade mecânica da ação (ex: usar item que não existe), sem lógica de combate/ordem de iniciativa, e mudanças de estado propostas não são visíveis aos NPCs no mesmo turno em que ocorrem (o contexto passado para a proposta de mudança inclui os diálogos gerados, mas os diálogos não sabem antecipadamente que uma mudança vai ser aplicada).

## Contexto arquitetural

Este repositório é um dos submódulos do monorepo [airpg](https://github.com/lchampz/airpg). A documentação completa da arquitetura (eventos, fluxo de turno, decisões técnicas) vive no vault Obsidian do projeto.
