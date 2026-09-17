FROM rust:slim-bookworm AS builder
WORKDIR /app
# g++ (libstdc++): dependência de build de onig_sys (via tokenizers, puxado
# pelo fastembed/RAG) — sem isso o link falha com "cannot find -lstdc++".
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev g++ && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/airpg-engine ./airpg-engine
# Runtime do ONNX (baixado em build-time pelo `ort`, ver Cargo.toml) — o
# fastembed/RAG carrega essa lib dinamicamente; sem copiar o cache de build
# pra cá, o container sobe mas falha ao inicializar embeddings em runtime.
COPY --from=builder /root/.cache/ort.pyke.io /root/.cache/ort.pyke.io
EXPOSE 8080
CMD ["./airpg-engine"]
