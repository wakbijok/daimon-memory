//! `daimon` — ops CLI for daimon-memory.
//!
//! Subcommands:
//! - `migrate` — apply embedded SQL migrations (refinery) to Postgres.
//! - `reindex` — rebuild the entire Qdrant index from Postgres (the "Qdrant is
//!   rebuildable from the canonical store" guarantee, made real).
//! - `health`  — ping Postgres + Qdrant.
//! - `stats`   — record counts by kind + pending outbox + Qdrant point count.
//!
//! Config via libpq env (PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE) + DAIMON_QDRANT_URL.

use anyhow::Result;
use daimon_pg::PgConfig;
use daimon_vec::{Embedder, VectorStore};
use deadpool_postgres::{Config as DpConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use serde_json::json;
use tokio_postgres::NoTls;
use uuid::Uuid;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("../../migrations");
}

fn build_pool(cfg: &PgConfig) -> Result<Pool> {
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

fn qdrant_url() -> String {
    std::env::var("DAIMON_QDRANT_URL").unwrap_or_else(|_| "http://127.0.0.1:6334".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_string()))
        .init();

    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "migrate" => migrate().await,
        "reindex" => reindex().await,
        "health" => health().await,
        "stats" => stats().await,
        other => {
            eprintln!(
                "daimon {} — usage: daimon <migrate|reindex|health|stats>",
                if other.is_empty() { "(no command)" } else { other }
            );
            std::process::exit(2);
        }
    }
}

async fn migrate() -> Result<()> {
    let cfg = PgConfig::from_env();
    let conn_str = format!(
        "host={} port={} user={} password={} dbname={}",
        cfg.host, cfg.port, cfg.user, cfg.password, cfg.dbname
    );
    let (mut client, conn) = tokio_postgres::connect(&conn_str, NoTls).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let report = embedded::migrations::runner().run_async(&mut client).await?;
    let applied = report.applied_migrations();
    if applied.is_empty() {
        println!("migrate: up to date (no migrations applied)");
    } else {
        for m in applied {
            println!("migrate: applied V{} {}", m.version(), m.name());
        }
    }
    Ok(())
}

async fn reindex() -> Result<()> {
    let pool = build_pool(&PgConfig::from_env())?;
    let store = VectorStore::connect(&qdrant_url()).map_err(to_anyhow)?;
    store.ensure().await.map_err(to_anyhow)?;
    eprintln!("reindex: loading embedder…");
    let embedder = Embedder::new().map_err(to_anyhow)?;

    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT id, tenant_id, namespace, kind, title, abstract, uri_path
             FROM memory.records WHERE status='active'",
            &[],
        )
        .await?;
    let mut n = 0usize;
    for row in &rows {
        let id: Uuid = row.get("id");
        let tenant_id: Uuid = row.get("tenant_id");
        let title: String = row.get("title");
        let abstract_: String = row.get("abstract");
        let mut vecs = embedder
            .embed(&[format!("{title}. {abstract_}")])
            .map_err(to_anyhow)?;
        let vector = vecs.pop().unwrap_or_default();
        let payload = json!({
            "tenant_id": tenant_id.to_string(),
            "namespace": row.get::<_, String>("namespace"),
            "kind": row.get::<_, String>("kind"),
            "title": title,
            "abstract": abstract_,
            "uri": row.get::<_, String>("uri_path"),
        });
        store.upsert(id, vector, payload).await.map_err(to_anyhow)?;
        n += 1;
    }
    println!("reindex: rebuilt {n} record(s) into Qdrant");
    Ok(())
}

async fn health() -> Result<()> {
    let pool = build_pool(&PgConfig::from_env())?;
    let pg_ok = match pool.get().await {
        Ok(c) => c.simple_query("SELECT 1").await.is_ok(),
        Err(_) => false,
    };
    let qd_ok = match VectorStore::connect(&qdrant_url()) {
        Ok(vs) => vs.ensure().await.is_ok(),
        Err(_) => false,
    };
    println!(
        "{}",
        json!({"postgres": pg_ok, "qdrant": qd_ok, "healthy": pg_ok && qd_ok})
    );
    if pg_ok && qd_ok { Ok(()) } else { std::process::exit(1) }
}

async fn stats() -> Result<()> {
    let pool = build_pool(&PgConfig::from_env())?;
    let client = pool.get().await?;
    let by_kind = client
        .query(
            "SELECT kind, count(*)::bigint AS n FROM memory.records
             WHERE status='active' GROUP BY kind ORDER BY n DESC",
            &[],
        )
        .await?;
    let pending: i64 = client
        .query_one(
            "SELECT count(*)::bigint FROM memory.index_outbox WHERE processed_at IS NULL",
            &[],
        )
        .await?
        .get(0);
    let total: i64 = client
        .query_one(
            "SELECT count(*)::bigint FROM memory.records WHERE status='active'",
            &[],
        )
        .await?
        .get(0);
    let kinds: Vec<_> = by_kind
        .iter()
        .map(|r| json!({"kind": r.get::<_, String>("kind"), "count": r.get::<_, i64>("n")}))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "active_records": total,
            "by_kind": kinds,
            "outbox_pending": pending,
        }))?
    );
    Ok(())
}

fn to_anyhow(e: daimon_memory_core::MemoryError) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}
