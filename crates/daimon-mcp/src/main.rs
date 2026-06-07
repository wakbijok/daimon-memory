//! `daimon-mcp` — the cross-tool surface for daimon-memory.
//!
//! Phase-1 status: serves health/readiness and runs the **deterministic
//! control-layer validation** ([`validate_write`]) over HTTP. Persistence
//! (Postgres) + deterministic recall (Postgres FTS, then Qdrant hybrid) wire in
//! next. The server is stateless (SDS v0.2 §8.7) — no local state, HPA-scalable.

use axum::{
    Json, Router,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use daimon_memory_core::{MemoryError, MemoryWrite, validate_write};
use serde_json::json;
use std::env;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .init();

    let bind = env::var("DAIMON_MCP_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let app = Router::new()
        .route("/health", get(health))
        .route("/readyz", get(readyz))
        .route("/v1/memory", post(store))
        .route("/v1/recall", post(recall))
        .route("/v1/read", get(read))
        .route("/mcp", post(mcp));

    let addr: SocketAddr = bind.parse()?;
    tracing::info!(%addr, "daimon-mcp listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "daimon-mcp",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn readyz() -> impl IntoResponse {
    // TODO(Phase-1): ping Postgres + Qdrant once the store is wired.
    Json(json!({"ready": true, "note": "data-tier checks pending (Phase-1)"}))
}

/// POST /v1/memory — validate (deterministic control layer) then (soon) persist.
async fn store(Json(w): Json<MemoryWrite>) -> impl IntoResponse {
    match validate_write(&w) {
        Ok(()) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "status": "validated",
                "detail": "control-layer validation passed; persistence wiring in progress (Phase-1)",
                "kind": w.kind.as_str(),
                "namespace": w.namespace,
            })),
        ),
        Err(MemoryError::Validation(m)) | Err(MemoryError::InvalidNamespace(m)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "validation", "detail": m})),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "bad_request", "detail": e.to_string()})),
        ),
    }
}

async fn recall() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"detail": "recall wiring in progress (Phase-1)"})),
    )
}

async fn read() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"detail": "read wiring in progress (Phase-1)"})),
    )
}

async fn mcp() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({"detail": "MCP endpoint wiring in progress (Phase-1)"})),
    )
}
