//! `daimon-mcp` - the cross-tool surface for daimon-memory.
//!
//! Phase-1 MVP: a **stateless** REST surface over the Postgres-backed engine -
//! validated `store`, deterministic full-text `recall`, and `read`. Tenant is taken
//! from the `X-Daimon-Tenant` header (default for the MVP); bearer-auth → tenant and
//! the streamable-HTTP `/mcp` JSON-RPC surface are the next slices. Qdrant vector
//! hybrid follows the keyword-recall MVP. No local state → HPA-scalable.

use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use daimon_memory_core::{
    ContextMemory, ContextScope, MemoryError, MemoryUri, MemoryWrite, RecallFilters,
    strip_tenant_segment,
};
use daimon_pg::{PgConfig, PgStore};
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

mod fusion;
mod mcp;

use fusion::{SemHit, fuse};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: Arc<PgStore>,
    /// Optional semantic tier - recall degrades to PG-only if absent (recall never hard-fails).
    pub(crate) embedder: Option<Arc<daimon_vec::Embedder>>,
    pub(crate) vector: Option<Arc<daimon_vec::VectorStore>>,
    pub(crate) default_tenant: Uuid,
    /// Valid bearer tokens. Collected from `DAIMON_API_KEY` plus any `DAIMON_API_KEY_*`
    /// env var (one per client - e.g. `DAIMON_API_KEY_CLAUDE`), so per-client tokens can be
    /// revoked independently. When non-empty, every route except /health + /readyz requires
    /// `Authorization: Bearer <one-of-these>`. `None` = open (dev/quickstart only).
    pub(crate) api_tokens: Option<Arc<Vec<String>>>,
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

    // Optional semantic tier (graceful - server runs PG-only if Qdrant/embedder absent).
    let vector = match std::env::var("DAIMON_QDRANT_URL") {
        Ok(url) => match daimon_vec::VectorStore::connect(&url) {
            Ok(vs) => {
                // Only claim "connected" when ensure actually succeeded; per-request
                // searches retry, so a startup failure is degraded, not fatal.
                match vs.ensure().await {
                    Ok(()) => tracing::info!(%url, "semantic tier: qdrant connected"),
                    Err(e) => {
                        tracing::warn!(%e, %url, "semantic tier: qdrant ensure failed at startup; semantic recall degraded until it recovers")
                    }
                }
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

    // Collect every configured bearer token: the bare DAIMON_API_KEY plus any
    // DAIMON_API_KEY_* (one per client, so each can be revoked on its own). Order/count
    // is not logged as a value; only how many are active.
    let mut tokens: Vec<String> = env::vars()
        .filter(|(k, _)| k == "DAIMON_API_KEY" || k.starts_with("DAIMON_API_KEY_"))
        .filter_map(|(_, v)| {
            let v = v.trim().to_string();
            (!v.is_empty()).then_some(v)
        })
        .collect();
    tokens.sort();
    tokens.dedup();
    let api_tokens = if tokens.is_empty() {
        tracing::warn!(
            "auth: no DAIMON_API_KEY[_*] set - the API is OPEN; anyone with network reach can read/write memory"
        );
        None
    } else {
        tracing::info!(
            tokens = tokens.len(),
            "auth: bearer token required on /v1 + /mcp"
        );
        Some(Arc::new(tokens))
    };

    let state = AppState {
        store,
        embedder,
        vector,
        default_tenant,
        api_tokens,
    };
    // /health + /readyz stay unauthenticated (probes); everything else is token-gated
    // when DAIMON_API_KEY is set.
    let protected = Router::new()
        .route("/v1/memory", post(store_h))
        .route("/v1/recall", post(recall_h))
        .route("/v1/read", get(read_h))
        .route("/mcp", post(mcp::handle))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth_mw));
    let app = Router::new()
        .route("/health", get(health))
        .route("/readyz", get(readyz))
        .merge(protected)
        .with_state(state);

    let addr: SocketAddr = bind.parse()?;
    tracing::info!(%addr, %default_tenant, version = env!("CARGO_PKG_VERSION"), "daimon-mcp listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // Graceful shutdown: on SIGTERM (k8s rollout/evict) or Ctrl-C, stop accepting new
    // connections and let in-flight requests finish instead of cutting them mid-store.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("daimon-mcp shut down cleanly");
    Ok(())
}

/// Resolves on SIGTERM or Ctrl-C (whichever first).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining in-flight requests");
}

fn tenant_from(headers: &HeaderMap, default: Uuid) -> Uuid {
    headers
        .get("x-daimon-tenant")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or(default)
}

/// Bearer-token gate. Fail-closed when any token is configured; pass-through when none.
async fn auth_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = st.api_tokens.as_deref() else {
        return next.run(req).await;
    };
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if token_matches(provided, expected) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized", "detail": "missing or invalid bearer token"})),
        )
            .into_response()
    }
}

/// True if `provided` equals any configured token. Folds over all tokens without
/// short-circuiting, so acceptance does not leak which key matched; the per-token compare
/// is constant-time. An empty `provided` (no/blank header) never matches a non-empty token.
fn token_matches(provided: &str, expected: &[String]) -> bool {
    let p = provided.as_bytes();
    expected
        .iter()
        .fold(false, |acc, t| acc | constant_time_eq(p, t.as_bytes()))
}

/// Length-leaking only; per-byte comparison runs in constant time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod auth_tests {
    use super::{recall_tier, token_matches};

    fn keys() -> Vec<String> {
        vec!["admin-aaa".into(), "claude-bbb".into(), "izu-ccc".into()]
    }

    #[test]
    fn any_configured_token_is_accepted() {
        let k = keys();
        assert!(token_matches("admin-aaa", &k));
        assert!(token_matches("claude-bbb", &k));
        assert!(token_matches("izu-ccc", &k));
    }

    #[test]
    fn wrong_empty_or_prefix_token_is_rejected() {
        let k = keys();
        assert!(!token_matches("nope", &k));
        assert!(!token_matches("", &k)); // no/blank Authorization header
        assert!(!token_matches("claude-bb", &k)); // length-mismatch prefix
        assert!(!token_matches("claude-bbbb", &k));
    }

    #[test]
    fn no_configured_tokens_matches_nothing() {
        assert!(!token_matches("anything", &[]));
    }

    #[test]
    fn recall_tier_needs_both_semantic_halves() {
        assert_eq!(recall_tier(true, true), "hybrid");
        assert_eq!(recall_tier(false, true), "keyword"); // no AVX2 / embedder init failed
        assert_eq!(recall_tier(true, false), "keyword"); // qdrant absent
        assert_eq!(recall_tier(false, false), "keyword");
    }
}

/// Which recall path this process can serve: "hybrid" (keyword + semantic) when both the
/// embedder and Qdrant were available at startup, else "keyword". "unhealthy" is reported
/// by /readyz when Postgres (the keyword tier itself) is unreachable.
fn recall_tier(has_embedder: bool, has_vector: bool) -> &'static str {
    if has_embedder && has_vector {
        "hybrid"
    } else {
        "keyword"
    }
}

async fn health(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "daimon-mcp",
        "version": env!("CARGO_PKG_VERSION"),
        "recall_tier": recall_tier(st.embedder.is_some(), st.vector.is_some()),
    }))
}

/// A backlog older than this is surfaced as a warning on /readyz (readiness stays OK -
/// it signals "semantic recall is going stale", not "stop sending traffic"). ~10 min.
const OUTBOX_STALE_SECS: f64 = 600.0;

async fn readyz(State(st): State<AppState>) -> impl IntoResponse {
    if !st.store.ping().await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready": false, "reason": "postgres unreachable", "recall_tier": "unhealthy"})),
        );
    }
    // Readiness is PG-reachability only. Outbox lag is advisory: a stalled/dead indexer
    // (the known failure mode the retired monitoring stack used to catch) shows up here as
    // a warning field without flapping the probe.
    let mut body = json!({
        "ready": true,
        "recall_tier": recall_tier(st.embedder.is_some(), st.vector.is_some()),
    });
    if let Some((pending, oldest)) = st.store.outbox_lag().await {
        body["outbox_pending"] = json!(pending);
        body["outbox_oldest_age_secs"] = json!(oldest);
        if pending > 0 && oldest.map(|a| a > OUTBOX_STALE_SECS).unwrap_or(false) {
            body["outbox_warning"] = json!(format!(
                "{pending} unprocessed for >{:.0}s - is daimon-indexer running?",
                OUTBOX_STALE_SECS
            ));
        }
    }
    (StatusCode::OK, Json(body))
}

/// POST /v1/memory - validate (deterministic control layer) + persist.
async fn store_h(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(w): Json<MemoryWrite>,
) -> impl IntoResponse {
    let scope = ContextScope::tenant(tenant_from(&headers, st.default_tenant));
    match st.store.store(&scope, w).await {
        Ok(uri) => (
            StatusCode::CREATED,
            Json(json!({"uri": uri.display_relative()})),
        ),
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

/// POST /v1/recall - deterministic recall (no LLM).
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
/// Degrades to whichever tier is available; never hard-fails. This function owns only the
/// I/O (DB + Qdrant + embedder); the deterministic ranking math lives in [`fusion::fuse`].
pub(crate) async fn hybrid_recall(
    st: &AppState,
    scope: &ContextScope,
    query: &str,
    filters: &RecallFilters,
) -> Vec<serde_json::Value> {
    // Clamp once at the fusion boundary so both arms share the same bound (the keyword arm
    // clamps again in SQL; the semantic arm previously passed the wire value straight to top_k).
    let limit = filters.limit.clamp(1, 200);

    // keyword (Postgres FTS)
    let keyword = match st.store.find(scope, query, filters).await {
        Ok(pg) => pg,
        Err(e) => {
            tracing::warn!(%e, "recall: keyword arm failed; continuing without it");
            vec![]
        }
    };

    // semantic (Qdrant dense) - only when both embedder + vector store are present.
    let mut semantic: Vec<SemHit> = vec![];
    if !query.trim().is_empty()
        && let (Some(emb), Some(vs)) = (&st.embedder, &st.vector)
    {
        match emb.embed(&[query.to_string()]) {
            Ok(mut q) => {
                if let Some(qv) = q.pop() {
                    // namespace_prefix is post-filtered in fuse() (Qdrant can't prefix-match
                    // a keyword payload), so over-fetch to avoid starving scoped recalls
                    // whose nearest tenant-wide neighbours live in other namespaces.
                    let fetch_k = if filters.namespace_prefix.is_some() {
                        (limit * 5).min(200)
                    } else {
                        limit
                    } as u64;
                    let kind = filters.kind.map(|k| k.as_str());
                    let since_epoch = filters.since.map(|t| t.timestamp());
                    match vs
                        .search(scope.tenant_id, qv, fetch_k, kind, since_epoch)
                        .await
                    {
                        Ok(vhits) => semantic = vhits.iter().map(sem_hit_from_payload).collect(),
                        Err(e) => {
                            tracing::warn!(%e, "recall: semantic arm failed; continuing keyword-only")
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(%e, "recall: query embedding failed; continuing keyword-only")
            }
        }
    }

    fuse(
        &keyword,
        &semantic,
        filters.namespace_prefix.as_deref(),
        limit,
    )
}

/// Project a Qdrant hit's payload into the plain [`SemHit`] fusion consumes.
fn sem_hit_from_payload(vh: &daimon_vec::VecHit) -> SemHit {
    let p = &vh.payload;
    let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    SemHit {
        uri: s("uri"),
        kind: s("kind"),
        title: s("title"),
        abstract_: s("abstract"),
        namespace: s("namespace"),
        importance: p.get("importance").and_then(|v| v.as_u64()).unwrap_or(0) as u8,
        score: vh.score,
    }
}

#[derive(Deserialize)]
struct ReadQuery {
    uri: String,
}

/// GET /v1/read?uri=daimon://... - lazy full-content fetch.
async fn read_h(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReadQuery>,
) -> impl IntoResponse {
    let scope = ContextScope::tenant(tenant_from(&headers, st.default_tenant));
    let uri = match MemoryUri::parse_scoped(&q.uri, scope.tenant_id) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_uri", "detail": e.to_string()})),
            );
        }
    };
    match st.store.read(&scope, &uri).await {
        Ok(mut rec) => {
            rec.uri = strip_tenant_segment(&rec.uri);
            (StatusCode::OK, Json(json!({"record": rec})))
        }
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
