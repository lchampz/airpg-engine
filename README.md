# airpg-engine

Núcleo do turno do AIRPG, em Rust (Axum + Tokio + SQLite/sqlx).

Responsabilidades: Orquestrador Central, Guardrails Narrativos (entrada/saída), Pool de Agentes reativo, Skills determinísticas, e a fonte única de verdade do Estado Rígido do jogo.

Este crate é agnóstico de frontend — expõe apenas HTTP/SSE. O contrato público é o conjunto de eventos tipados em `src/events`.

## Rodando localmente

```bash
cargo run
```

Endpoints:
- `GET /health`
- `POST /turn` — recebe `{ "situation": "...", "response": "..." }`, roda o Guardrail de Entrada e devolve o evento resultante

## Status

Esqueleto inicial (walking skeleton): valida ação, roteia agentes por localização com cap de 4/turno, tem uma skill determinística de exemplo (`skill_dados`). Ainda não implementados: chamadas reais a LLM, integração NATS com o `airpg-world` (Elixir), Guardrail de Saída, e persistência via `sqlx` (o pool já é criado, tabelas ainda não são lidas/escritas pelos endpoints).

## Contexto arquitetural

Este repositório é um dos submódulos do monorepo [airpg](https://github.com/lchampz/airpg). A documentação completa da arquitetura (eventos, fluxo de turno, decisões técnicas) vive no vault Obsidian do projeto.
