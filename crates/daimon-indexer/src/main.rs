//! daimon-indexer - the singleton outbox drainer.
//!
//! Drains `memory.index_outbox` (written transactionally by the store) → embeds the
//! record's L0 abstract → upserts/deletes the Qdrant point → marks the row processed.
//! MUST run as a singleton (double-index guard). `--once` processes one batch and exits
//! (CI / smoke test); otherwise it loops with a short idle sleep.
//!
//! Failure discipline: a row that fails is retried with its `attempts` counter, and
//! dead-lettered (processed_at set, attempts at the cap) after [`MAX_ATTEMPTS`] so one
//! poison record can never head-of-line-block the queue. Delete failures retry like
//! upserts - a swallowed delete would leak a retracted memory back into semantic recall.
//! The loop itself survives pool/DB-level errors with a backoff instead of exiting.
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

/// Per-row retry cap before dead-lettering. Dead-lettered rows are queryable:
/// `SELECT * FROM memory.index_outbox WHERE attempts >= 10 AND processed_at IS NOT NULL`.
/// Recover with `daimon reindex` after fixing the cause.
const MAX_ATTEMPTS: i32 = 10;

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

    // Graceful shutdown: flips on SIGTERM/Ctrl-C. Each batch is already crash-safe (Qdrant
    // upsert before the processed_at mark, idempotent by record id), so we just stop cleanly
    // between batches rather than getting SIGKILLed mid-sleep.
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    spawn_signal_watcher(shutdown.clone());

    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!("indexer: shutdown signal received; exiting between batches");
            break;
        }
        match drain_batch(&pool, &store, &embedder).await {
            Ok((n, failed)) => {
                if n > 0 || failed > 0 {
                    tracing::info!(processed = n, failed, "indexer: drained batch");
                }
                if once {
                    tracing::info!("indexer: --once complete");
                    break;
                }
                let nap = if failed > 0 {
                    // Failing rows stay unprocessed and re-select immediately; back off so a
                    // Qdrant/embedder outage doesn't hot-loop the attempts straight to the cap.
                    Some(Duration::from_secs(15))
                } else if n == 0 {
                    Some(Duration::from_secs(5))
                } else {
                    None
                };
                if let Some(d) = nap {
                    sleep_or_shutdown(d, &shutdown).await;
                }
            }
            Err(e) => {
                if once {
                    return Err(e);
                }
                tracing::error!(%e, "indexer: batch failed (db/pool level); retrying");
                sleep_or_shutdown(Duration::from_secs(5), &shutdown).await;
            }
        }
    }
    Ok(())
}

fn spawn_signal_watcher(flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    tokio::spawn(async move {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        #[cfg(unix)]
        let term = async {
            if let Ok(mut s) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                s.recv().await;
            }
        };
        #[cfg(not(unix))]
        let term = std::future::pending::<()>();
        tokio::select! { _ = ctrl_c => {}, _ = term => {} }
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Sleep, but wake immediately if shutdown is signalled so the pod exits inside its
/// termination grace period instead of sitting out the full nap.
async fn sleep_or_shutdown(d: Duration, flag: &std::sync::atomic::AtomicBool) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {}
        _ = async {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        } => {}
    }
}

fn to_anyhow(e: daimon_memory_core::MemoryError) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}

/// Returns (processed, failed) for the batch. Row-level failures are contained here;
/// only pool/DB errors propagate.
async fn drain_batch(
    pool: &Pool,
    store: &VectorStore,
    embedder: &Embedder,
) -> anyhow::Result<(usize, usize)> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT id, record_id, tenant_id, op, attempts FROM memory.index_outbox
             WHERE processed_at IS NULL ORDER BY id LIMIT 50",
            &[],
        )
        .await?;
    // Outage guard: if Qdrant itself is down, every row would fail and burn its attempts
    // straight to the dead-letter cap within minutes. Don't charge rows for an
    // infrastructure outage - skip the batch (the main loop backs off) and retry later.
    if !rows.is_empty() && !store.healthy().await {
        tracing::warn!(
            pending = rows.len(),
            "indexer: qdrant unreachable; deferring batch without charging attempts"
        );
        return Ok((0, rows.len()));
    }
    let mut n = 0usize;
    let mut failed = 0usize;
    for row in &rows {
        let outbox_id: i64 = row.get("id");
        let record_id: Uuid = row.get("record_id");
        let op: String = row.get("op");
        let attempts: i32 = row.get("attempts");

        match process_row(&client, store, embedder, record_id, &op).await {
            Ok(()) => {
                client
                    .execute(
                        "UPDATE memory.index_outbox SET processed_at=now() WHERE id=$1",
                        &[&outbox_id],
                    )
                    .await?;
                n += 1;
            }
            Err(e) => {
                failed += 1;
                let tries = attempts + 1;
                if tries >= MAX_ATTEMPTS {
                    // Dead-letter: mark processed so the queue moves on; attempts stays at
                    // the cap as the marker. The vector index is now missing this record -
                    // `daimon reindex` reconciles once the cause is fixed.
                    client
                        .execute(
                            "UPDATE memory.index_outbox SET processed_at=now(), attempts=$2 WHERE id=$1",
                            &[&outbox_id, &tries],
                        )
                        .await?;
                    tracing::error!(%record_id, op, tries, %e,
                        "indexer: DEAD-LETTERED outbox row; fix the cause then run `daimon reindex`");
                } else {
                    client
                        .execute(
                            "UPDATE memory.index_outbox SET attempts=$2 WHERE id=$1",
                            &[&outbox_id, &tries],
                        )
                        .await?;
                    tracing::warn!(%record_id, op, tries, %e, "indexer: row failed; will retry");
                }
            }
        }
    }
    Ok((n, failed))
}

async fn process_row(
    client: &deadpool_postgres::Client,
    store: &VectorStore,
    embedder: &Embedder,
    record_id: Uuid,
    op: &str,
) -> anyhow::Result<()> {
    match op {
        "upsert" => {
            if let Some(rec) = client
                .query_opt(
                    "SELECT tenant_id, namespace, kind, title, abstract, body, importance, uri_path,
                            extract(epoch FROM created_at)::bigint AS created_epoch
                     FROM memory.records WHERE id=$1 AND status='active'",
                    &[&record_id],
                )
                .await?
            {
                let tenant_id: Uuid = rec.get("tenant_id");
                let namespace: String = rec.get("namespace");
                let kind: String = rec.get("kind");
                let title: String = rec.get("title");
                let abstract_: String = rec.get("abstract");
                let body: String = rec.get("body");
                let importance: i16 = rec.get("importance");
                let uri: String = rec.get("uri_path");
                let created_epoch: i64 = rec.get("created_epoch");
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
                    "created_at": created_epoch,
                });
                store.upsert(record_id, vector, payload).await.map_err(to_anyhow)?;
            }
            // if the record is gone/forgotten, nothing to index - still mark processed.
        }
        "delete" => {
            // NOT best-effort: a swallowed delete failure permanently leaks a retracted
            // memory back into semantic recall. Qdrant treats deleting a missing point as
            // success, so propagating real failures into the retry path is safe.
            store.delete(record_id).await.map_err(to_anyhow)?;
        }
        other => tracing::warn!(op = other, "indexer: unknown outbox op, skipping"),
    }
    Ok(())
}
