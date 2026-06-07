# Multi-stage build for the daimon-mcp server.
# Built by GitLab CI (kaniko) — local container tooling is not required.
FROM rust:1.94-slim AS builder
WORKDIR /app
# Build deps cache layer
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --bin daimon-mcp --bin daimon-indexer

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
# Migrations shipped alongside the binaries (applied by `daimon` CLI / init job later).
COPY --from=builder /app/migrations /app/migrations
# Both binaries: daimon-mcp (API server) + daimon-indexer (outbox→Qdrant singleton).
# The indexer Deployment overrides the entrypoint with `command: ["daimon-indexer"]`.
# NOTE: the embedder downloads bge-small (~130MB) from HF on first run — the pod needs
# egress, or bake the model cache into the image in a follow-up.
COPY --from=builder /app/target/release/daimon-mcp /usr/local/bin/daimon-mcp
COPY --from=builder /app/target/release/daimon-indexer /usr/local/bin/daimon-indexer
ENV DAIMON_MCP_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/daimon-mcp"]
