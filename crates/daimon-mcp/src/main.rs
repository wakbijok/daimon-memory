//! `daimon-mcp` — the cross-tool surface for daimon-memory.
//!
//! Phase-1 MVP: a **stateless** REST surface over the Postgres-backed engine —
//! validated `store`, deterministic full-text `recall`, and `read`. Tenant is taken
//! from the `X-Daimon-Tenant` header (default for the MVP); bearer-auth → tenant and
//! the streamable-HTTP `/mcp` JSON-RPC surface are the next slices. Qdrant vector
//! hybrid follows the keyword-recall MVP. No local state → HPA-scalable (SDS v0.2 §8.7).

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use daimon_memory_core::{
    ContextMemory, ContextScope, MemoryError, MemoryUri, MemoryWrite, RecallFilters,
};
use daimon_pg::{PgConfig, PgStore};
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

mod mcp;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<PgStore>,
    pub(crate) default_tenant: Uuid,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .init();

    let bind = env::var("DAIMON_MCP_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let store = Arc::new(PgStore::connect(&PgConfig::from_env())?);
    let default_tenant = env::var("DAIMON_DEFAULT_TENANT")
        .ok()
        .and_then(|s| Uuid::parse_str(&s).ok())
        .unwrap_or_else(|| Uuid::parse_str("00000000-0000-0000-0000-0000000000d1").unwrap());

    let state = AppState {
        store,
        default_tenant,
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/readyz", get(readyz))
        .route("/v1/memory", post(store_h))
        .route("/v1/recall", post(recall_h))
        .route("/v1/read", get(read_h))
        .route("/mcp", post(mcp::handle))
        .with_state(state);

    let addr: SocketAddr = bind.parse()?;
    tracing::info!(%addr, %default_tenant, "daimon-mcp listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn tenant_from(headers: &HeaderMap, default: Uuid) -> Uuid {
    headers
        .get("x-daimon-tenant")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(default)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "daimon-mcp",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn readyz(State(st): State<AppState>) -> impl IntoResponse {
    if st.store.ping().await {
        (StatusCode::OK, Json(json!({"ready": true})))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false, "reason": "postgres unreachable"})),
        )
    }
}

/// POST /v1/memory — validate (deterministic control layer) + persist.
async fn store_h(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(w): Json<MemoryWrite>,
) -> impl IntoResponse {
    let scope = ContextScope::tenant(tenant_from(&headers, st.default_tenant));
    match st.store.store(&scope, w).await {
        Ok(uri) => (StatusCode::CREATED, Json(json!({"uri": uri.to_string()}))),
        Err(MemoryError::Validation(m)) | Err(MemoryError::InvalidNamespace(m)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "validation", "detail": m})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "backend", "detail": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct RecallReq {
    #[serde(default)]
    query: String,
    #[serde(default)]
    filters: RecallFilters,
}

/// POST /v1/recall — deterministic recall (no LLM).
async fn recall_h(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RecallReq>,
) -> impl IntoResponse {
    let scope = ContextScope::tenant(tenant_from(&headers, st.default_tenant));
    match st.store.find(&scope, &req.query, &req.filters).await {
        Ok(hits) => (StatusCode::OK, Json(json!({"hits": hits}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "backend", "detail": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct ReadQuery {
    uri: String,
}

/// GET /v1/read?uri=daimon://... — lazy full-content fetch.
async fn read_h(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReadQuery>,
) -> impl IntoResponse {
    let scope = ContextScope::tenant(tenant_from(&headers, st.default_tenant));
    let uri = match MemoryUri::parse(&q.uri) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_uri", "detail": e.to_string()})),
            );
        }
    };
    match st.store.read(&scope, &uri).await {
        Ok(rec) => (StatusCode::OK, Json(json!({"record": rec}))),
        Err(MemoryError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "not_found", "uri": q.uri})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "backend", "detail": e.to_string()})),
        ),
    }
}

