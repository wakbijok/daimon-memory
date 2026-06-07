# Multi-stage build for the daimon-mcp server.
# Built by GitLab CI (kaniko) — local container tooling is not required.
FROM rust:1.94-slim AS builder
WORKDIR /app
# Build deps cache layer
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --bin daimon-mcp

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
# Migrations shipped alongside the binary (applied by `daimon` CLI / init job later).
COPY --from=builder /app/migrations /app/migrations
COPY --from=builder /app/target/release/daimon-mcp /usr/local/bin/daimon-mcp
ENV DAIMON_MCP_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/daimon-mcp"]
