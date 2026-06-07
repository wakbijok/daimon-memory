# Multi-stage build for the daimon-mcp + daimon-indexer binaries.
# Built in-cluster by a kaniko Job (no CI runner / local Docker required).
#
# Builder uses the FULL rust image (build-essential, pkg-config, perl) + cmake/protoc
# for the -sys crates (aws-lc, prost/tonic). Runtime carries the ONNX runtime deps
# (libgomp1, libstdc++6) that fastembed/ort need at startup.
# bookworm builder so the binary's glibc matches the bookworm-slim runtime below
# (the default rust:1.94 is trixie-based -> glibc 2.38, which bookworm-slim lacks).
FROM rust:1.94-slim-bookworm AS builder
# Cap parallelism so the homelab build node doesn't OOM on the ort/tonic codegen.
ENV CARGO_BUILD_JOBS=2 \
    CARGO_PROFILE_RELEASE_DEBUG=false
WORKDIR /app
# slim base -> install all build deps explicitly (cc, openssl, cmake, protoc).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential cmake pkg-config protobuf-compiler libssl-dev \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --bin daimon-mcp --bin daimon-indexer --bin daimon

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libgomp1 libstdc++6 \
 && rm -rf /var/lib/apt/lists/*
# Migrations shipped alongside the binaries (applied by `daimon migrate`).
COPY --from=builder /app/migrations /app/migrations
# Both binaries: daimon-mcp (API server) + daimon-indexer (outbox->Qdrant singleton).
# The indexer Deployment overrides the entrypoint with `command: ["daimon-indexer"]`.
# NOTE: the embedder downloads bge-small (~130MB) from HF on first run — the pod needs
# egress, or bake the model cache into the image in a follow-up.
COPY --from=builder /app/target/release/daimon-mcp /usr/local/bin/daimon-mcp
COPY --from=builder /app/target/release/daimon-indexer /usr/local/bin/daimon-indexer
COPY --from=builder /app/target/release/daimon /usr/local/bin/daimon
ENV DAIMON_MCP_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/daimon-mcp"]
