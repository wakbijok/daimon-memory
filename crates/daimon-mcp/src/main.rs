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
    /// Optional semantic tier — recall degrades to PG-only if absent (SDS: recall never hard-fails).
    pub(crate) embedder: Option<Arc<daimon_vec::Embedder>>,
    pub(crate) vector: Option<Arc<daimon_vec::VectorStore>>,
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

    // Optional semantic tier (graceful — server runs PG-only if Qdrant/embedder absent).
    let vector = match std::env::var("DAIMON_QDRANT_URL") {
        Ok(url) => match daimon_vec::VectorStore::connect(&url) {
            Ok(vs) => {
                if let Err(e) = vs.ensure().await {
                    tracing::warn!(%e, "qdrant ensure failed");
                }
                tracing::info!(%url, "semantic tier: qdrant connected");
                Some(Arc::new(vs))
            }
            Err(e) => {
                tracing::warn!(%e, "qdrant connect failed; recall is PG-only");
                None
            }
        },
        Err(_) => {
            tracing::info!("DAIMON_QDRANT_URL unset; recall is PG-only (keyword)");
            None
        }
    };
    let embedder = if vector.is_some() {
        match daimon_vec::Embedder::new() {
            Ok(e) => {
                tracing::info!("semantic tier: embedder (bge-small) loaded");
                Some(Arc::new(e))
            }
            Err(e) => {
                tracing::warn!(%e, "embedder init failed; recall is PG-only");
                None
            }
        }
    } else {
        None
    };

    let state = AppState {
        store,
        embedder,
        vector,
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
    let hits = hybrid_recall(&st, &scope, &req.query, &req.filters).await;
    (StatusCode::OK, Json(json!({"hits": hits})))
}

/// Hybrid recall: RRF fusion of keyword (Postgres FTS) + semantic (Qdrant dense).
/// Degrades to whichever tier is available; never hard-fails (SDS recall guarantee).
pub(crate) async fn hybrid_recall(
    st: &AppState,
    scope: &ContextScope,
    query: &str,
    filters: &RecallFilters,
) -> Vec<serde_json::Value> {
    use std::collections::HashMap;
    const K: f32 = 60.0;
    // uri -> (kind, title, abstract, fused_score, sources)
    let mut acc: HashMap<String, (String, String, String, f32, Vec<&'static str>)> = HashMap::new();

    // keyword (Postgres FTS)
    if let Ok(pg) = st.store.find(scope, query, filters).await {
        for (rank, h) in pg.iter().enumerate() {
            let e = acc.entry(h.uri.clone()).or_insert_with(|| {
                (
                    h.kind.as_str().to_string(),
                    h.title.clone(),
                    h.abstract_.clone(),
                    0.0,
                    vec![],
                )
            });
            e.3 += 1.0 / (K + rank as f32 + 1.0);
            e.4.push("keyword");
        }
    }

    // semantic (Qdrant dense) — only when both embedder + vector store are present.
    if !query.trim().is_empty() {
        if let (Some(emb), Some(vs)) = (&st.embedder, &st.vector) {
            if let Ok(mut q) = emb.embed(&[query.to_string()]) {
                if let Some(qv) = q.pop() {
                    if let Ok(vhits) = vs.search(scope.tenant_id, qv, filters.limit as u64).await {
                        for (rank, vh) in vhits.iter().enumerate() {
                            let p = &vh.payload;
                            let uri =
                                p.get("uri").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if uri.is_empty() {
                                continue;
                            }
                            let kind =
                                p.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let title =
                                p.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let abs = p
                                .get("abstract")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let e = acc.entry(uri).or_insert_with(|| (kind, title, abs, 0.0, vec![]));
                            e.3 += 1.0 / (K + rank as f32 + 1.0);
                            e.4.push("semantic");
                        }
                    }
                }
            }
        }
    }

    let mut out: Vec<serde_json::Value> = acc
        .into_iter()
        .map(|(uri, (kind, title, abs, score, srcs))| {
            json!({"uri": uri, "kind": kind, "title": title, "abstract": abs, "score": score, "sources": srcs})
        })
        .collect();
    out.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(filters.limit.max(1));
    out
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

