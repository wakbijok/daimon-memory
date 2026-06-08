//! daimon-indexer - the singleton outbox drainer (SDS v0.2 §8.7).
//!
//! Drains `memory.index_outbox` (written transactionally by the store) → embeds the
//! record's L0 abstract → upserts/deletes the Qdrant point → marks the row processed.
//! MUST run as a singleton (double-index guard). `--once` processes one batch and exits
//! (CI / smoke test); otherwise it loops with a short idle sleep.
//!
//! Connects as the DB owner for the MVP (RLS bypassed → sees all tenants' outbox rows);
//! a non-superuser drainer would set `app.tenant_id` per row.

use daimon_pg::PgConfig;
use daimon_vec::{Embedder, VectorStore};
use deadpool_postgres::{Config as DpConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use serde_json::json;
use std::time::Duration;
use tokio_postgres::NoTls;
use uuid::Uuid;

fn build_pool(cfg: &PgConfig) -> anyhow::Result<Pool> {
    let mut dp = DpConfig::new();
    dp.host = Some(cfg.host.clone());
    dp.port = Some(cfg.port);
    dp.user = Some(cfg.user.clone());
    dp.password = Some(cfg.password.clone());
    dp.dbname = Some(cfg.dbname.clone());
    dp.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    });
    Ok(dp.create_pool(Some(Runtime::Tokio1), NoTls)?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .init();

    let once = std::env::args().any(|a| a == "--once");
    let pool = build_pool(&PgConfig::from_env())?;
    let qdrant_url =
        std::env::var("DAIMON_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6334".to_string());
    let store = VectorStore::connect(&qdrant_url).map_err(to_anyhow)?;
    store.ensure().await.map_err(to_anyhow)?;
    tracing::info!("indexer: loading embedder (bge-small, first run downloads the model)…");
    let embedder = Embedder::new().map_err(to_anyhow)?;
    tracing::info!(%qdrant_url, "indexer: ready");

    loop {
        let n = drain_batch(&pool, &store, &embedder).await?;
        if n > 0 {
            tracing::info!(processed = n, "indexer: drained batch");
        }
        if once {
            tracing::info!("indexer: --once complete");
            break;
        }
        if n == 0 {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    Ok(())
}

fn to_anyhow(e: daimon_memory_core::MemoryError) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}

async fn drain_batch(pool: &Pool, store: &VectorStore, embedder: &Embedder) -> anyhow::Result<usize> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT id, record_id, tenant_id, op FROM memory.index_outbox
             WHERE processed_at IS NULL ORDER BY id LIMIT 50",
            &[],
        )
        .await?;
    let mut n = 0usize;
    for row in &rows {
        let outbox_id: i64 = row.get("id");
        let record_id: Uuid = row.get("record_id");
        let tenant_id: Uuid = row.get("tenant_id");
        let op: String = row.get("op");

        match op.as_str() {
            "upsert" => {
                if let Some(rec) = client
                    .query_opt(
                        "SELECT namespace, kind, title, abstract, body, importance, uri_path
                         FROM memory.records WHERE id=$1 AND status='active'",
                        &[&record_id],
                    )
                    .await?
                {
                    let namespace: String = rec.get("namespace");
                    let kind: String = rec.get("kind");
                    let title: String = rec.get("title");
                    let abstract_: String = rec.get("abstract");
                    let body: String = rec.get("body");
                    let importance: i16 = rec.get("importance");
                    let uri: String = rec.get("uri_path");
                    // Embed title + body (capped; bge-small truncates ~512 tokens). Keep the
                    // cap in sync with the reindex CLI so live + reindexed vectors agree.
                    let body_capped: String = body.chars().take(2000).collect();
                    let text = format!("{title}. {body_capped}");
                    let mut vecs = embedder.embed(&[text]).map_err(to_anyhow)?;
                    let vector = vecs.pop().unwrap_or_default();
                    let payload = json!({
                        "tenant_id": tenant_id.to_string(),
                        "namespace": namespace,
                        "kind": kind,
                        "title": title,
                        "abstract": abstract_,
                        "importance": importance,
                        "uri": uri,
                    });
                    store.upsert(record_id, vector, payload).await.map_err(to_anyhow)?;
                }
                // if the record is gone/forgotten, nothing to index - still mark processed.
            }
            "delete" => {
                // best-effort: the point may not exist.
                let _ = store.delete(record_id).await;
            }
            other => tracing::warn!(op = other, "indexer: unknown outbox op, skipping"),
        }

        client
            .execute(
                "UPDATE memory.index_outbox SET processed_at=now() WHERE id=$1",
                &[&outbox_id],
            )
            .await?;
        n += 1;
    }
    Ok(n)
}
