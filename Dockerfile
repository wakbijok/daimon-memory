# Multi-stage build for the daimon-mcp + daimon-indexer + daimon binaries.
# Built in-cluster by a kaniko Job (no CI runner / local Docker required) - see
# deploy/build-job.yaml. Run with kaniko --cache so the cargo-chef dep layer is reused.
#
# IMPORTANT: build AND run on trixie (Debian 13, glibc 2.38). ort's prebuilt ONNX
# Runtime references glibc 2.38 symbols (__isoc23_strtol*), so a bookworm (2.36)
# builder cannot link it, and a bookworm-slim runtime cannot run a trixie-built
# binary ("GLIBC_2.38 not found"). Both stages trixie keeps it consistent.
#
# The full rust image (buildpack-deps) brings build-essential, pkg-config, libssl-dev,
# perl; add cmake + protoc for the -sys crates (aws-lc, prost/tonic). The runtime
# carries the ONNX deps (libgomp1, libstdc++6) the statically-linked ort pulls in.
#
# cargo-chef splits dependency compilation into its OWN cached layer: `cargo chef cook`
# compiles only the third-party deps (from recipe.json). With kaniko --cache, an unchanged
# recipe.json (deps unchanged) is a layer-cache HIT, so the ~20-min ort/fastembed/tokenizers
# rebuild is skipped and only the changed workspace crates recompile (~2-3 min builds).
FROM rust:1.94 AS chef
# 8 jobs to use the 12-core build node (celebrimbor); was 2 for the 4-core VMs.
ENV CARGO_BUILD_JOBS=8 \
    CARGO_PROFILE_RELEASE_DEBUG=false
WORKDIR /app
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake protobuf-compiler \
 && rm -rf /var/lib/apt/lists/* \
 && cargo install cargo-chef --locked

# Planner: distill the dependency graph into recipe.json (cheap; no compilation).
FROM chef AS planner
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# Builder: cook deps FIRST (cached layer, only re-runs when recipe.json changes), then
# compile the workspace source on top (only changed crates recompile - deps already built).
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock* ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --bin daimon-mcp --bin daimon-indexer --bin daimon

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libgomp1 libstdc++6 \
 && rm -rf /var/lib/apt/lists/*
# Migrations shipped alongside the binaries (applied by `daimon migrate`).
COPY --from=builder /app/migrations /app/migrations
# Both binaries: daimon-mcp (API server) + daimon-indexer (outbox->Qdrant singleton).
# The indexer Deployment overrides the entrypoint with `command: ["daimon-indexer"]`.
# NOTE: the embedder downloads bge-small (~130MB) from HF on first run - the pod needs
# egress, or bake the model cache into the image in a follow-up.
COPY --from=builder /app/target/release/daimon-mcp /usr/local/bin/daimon-mcp
COPY --from=builder /app/target/release/daimon-indexer /usr/local/bin/daimon-indexer
COPY --from=builder /app/target/release/daimon /usr/local/bin/daimon
ENV DAIMON_MCP_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/daimon-mcp"]
