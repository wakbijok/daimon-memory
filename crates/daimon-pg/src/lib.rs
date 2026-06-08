//! Postgres-backed [`ContextMemory`] for daimon-memory.
//!
//! - **store**: runs the deterministic control-layer validation, computes a content
//!   hash for dedup, derives the canonical URI, and persists the record + namespace
//!   + an outbox row (PG→Qdrant) in one transaction.
//! - **find**: deterministic recall - Postgres full-text (`tsvector`) ranking + filters,
//!   **no LLM** (Qdrant vector hybrid is the next slice).
//!
//! Every operation is tenant-scoped two ways: it sets the RLS GUC `app.tenant_id`
//! (for non-superuser roles) AND filters `tenant_id = $` explicitly (correct even when
//! the connecting role bypasses RLS - the MVP runs as the DB owner).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use daimon_memory_core::{
    ContextMemory, ContextScope, MemoryError, MemoryHit, MemoryKind, MemoryRecord, MemoryUri,
    MemoryWrite, Namespace, RecallFilters, Result, WriteMode, validate_write,
};
use deadpool_postgres::{Config as DpConfig, ManagerConfig, Pool, RecyclingMethod, Runtime};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tokio_postgres::{NoTls, Row};
use uuid::Uuid;

/// Connection settings (typically from libpq-style env vars).
#[derive(Debug, Clone)]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub dbname: String,
}

impl PgConfig {
    pub fn from_env() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        PgConfig {
            host: env("PGHOST", "127.0.0.1"),
            port: env("PGPORT", "5432").parse().unwrap_or(5432),
            user: env("PGUSER", "daimon"),
            password: env("PGPASSWORD", ""),
            dbname: env("PGDATABASE", "daimon_memory"),
        }
    }
}

/// Postgres store implementing [`ContextMemory`].
pub struct PgStore {
    pool: Pool,
}

impl PgStore {
    pub fn connect(cfg: &PgConfig) -> Result<Self> {
        let mut dp = DpConfig::new();
        dp.host = Some(cfg.host.clone());
        dp.port = Some(cfg.port);
        dp.user = Some(cfg.user.clone());
        dp.password = Some(cfg.password.clone());
        dp.dbname = Some(cfg.dbname.clone());
        dp.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        });
        let pool = dp
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .map_err(|e| MemoryError::Backend(format!("pool: {e}")))?;
        Ok(PgStore { pool })
    }

    /// `true` if the store can reach Postgres (readiness probe).
    pub async fn ping(&self) -> bool {
        match self.pool.get().await {
            Ok(c) => c.simple_query("SELECT 1").await.is_ok(),
            Err(_) => false,
        }
    }
}

fn backend<E: std::fmt::Display>(e: E) -> MemoryError {
    MemoryError::Backend(e.to_string())
}

/// Deterministic content hash for dedup (MVP JCS-lite: kind + title + body + sorted fields).
fn content_sha(w: &MemoryWrite) -> String {
    let sorted: BTreeMap<String, &serde_json::Value> = w.fields.iter().map(|(k, v)| (k.clone(), v)).collect();
    let canon = serde_json::json!({
        "kind": w.kind.as_str(),
        "title": w.title,
        "body": w.body,
        "fields": sorted,
    });
    let s = serde_json::to_string(&canon).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

fn abstract_of(body: &str) -> String {
    const N: usize = 280;
    if body.chars().count() <= N {
        body.to_string()
    } else {
        body.chars().take(N).collect::<String>() + "…"
    }
}

fn uri_of(tenant: Uuid, namespace: &str, kind: MemoryKind, id: Uuid) -> String {
    format!("daimon://{tenant}/{namespace}/{}/{id}", kind.as_str())
}

fn map_record(row: &Row) -> Result<MemoryRecord> {
    let kind = MemoryKind::parse(row.get::<_, String>("kind").as_str())?;
    let fields: serde_json::Value = row.get("fields");
    let source_refs: serde_json::Value = row.get("source_refs");
    Ok(MemoryRecord {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        namespace: row.get("namespace"),
        kind,
        title: row.get("title"),
        body: row.get("body"),
        fields: fields.as_object().cloned().unwrap_or_default(),
        source_refs: serde_json::from_value(source_refs).unwrap_or_default(),
        tags: row.get::<_, Vec<String>>("tags"),
        importance: row.get::<_, i16>("importance") as u8,
        confidence: row.get::<_, f32>("confidence"),
        created_at: row.get::<_, DateTime<Utc>>("created_at"),
        updated_at: row.get::<_, DateTime<Utc>>("updated_at"),
        uri: row.get("uri_path"),
    })
}

#[async_trait]
impl ContextMemory for PgStore {
    async fn store(&self, scope: &ContextScope, write: MemoryWrite) -> Result<MemoryUri> {
        validate_write(&write)?;
        let tenant = scope.tenant_id;
        let tenant_s = tenant.to_string();
        let sha = content_sha(&write);
        let abstract_ = abstract_of(&write.body);
        let fields = serde_json::Value::Object(write.fields.clone());
        let source_refs = serde_json::Value::Array(
            write.source_refs.iter().cloned().map(serde_json::Value::String).collect(),
        );

        let mut client = self.pool.get().await.map_err(backend)?;
        let tx = client.transaction().await.map_err(backend)?;
        tx.execute("SELECT set_config('app.tenant_id', $1, true)", &[&tenant_s])
            .await
            .map_err(backend)?;

        // Dedup: return the existing active record's URI if the content hash matches.
        if let Some(r) = tx
            .query_opt(
                "SELECT uri_path FROM memory.records WHERE tenant_id=$1 AND content_sha=$2 AND status='active'",
                &[&tenant, &sha],
            )
            .await
            .map_err(backend)?
        {
            let uri: String = r.get(0);
            tx.commit().await.map_err(backend)?;
            return MemoryUri::parse(&uri);
        }

        // Update-mode kinds keep current state: supersede the prior active record(s) for the
        // same subject (namespace + kind + title) and enqueue their removal from the vector
        // index, so an edited persona/protocol/runbook does not leave a stale duplicate.
        if write.kind.write_mode() == WriteMode::Update {
            let superseded = tx
                .query(
                    "UPDATE memory.records SET status='superseded', updated_at=now()
                     WHERE tenant_id=$1 AND namespace=$2 AND kind=$3 AND title=$4 AND status='active'
                     RETURNING id",
                    &[&tenant, &write.namespace, &write.kind.as_str(), &write.title],
                )
                .await
                .map_err(backend)?;
            for row in &superseded {
                let sid: Uuid = row.get("id");
                tx.execute(
                    "INSERT INTO memory.index_outbox (record_id, tenant_id, op) VALUES ($1,$2,'delete')",
                    &[&sid, &tenant],
                )
                .await
                .map_err(backend)?;
            }
        }

        let id = Uuid::new_v4();
        let uri = uri_of(tenant, &write.namespace, write.kind, id);
        let importance = write.importance as i16;
        tx.execute(
            "INSERT INTO memory.records
               (id, tenant_id, namespace, kind, title, body, abstract, fields, source_refs,
                tags, importance, confidence, content_sha, uri_path)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
            &[
                &id,
                &tenant,
                &write.namespace,
                &write.kind.as_str(),
                &write.title,
                &write.body,
                &abstract_,
                &fields,
                &source_refs,
                &write.tags,
                &importance,
                &write.confidence,
                &sha,
                &uri,
            ],
        )
        .await
        .map_err(backend)?;

        tx.execute(
            "INSERT INTO memory.namespaces (tenant_id, path) VALUES ($1,$2) ON CONFLICT DO NOTHING",
            &[&tenant, &write.namespace],
        )
        .await
        .map_err(backend)?;

        tx.execute(
            "INSERT INTO memory.index_outbox (record_id, tenant_id, op) VALUES ($1,$2,'upsert')",
            &[&id, &tenant],
        )
        .await
        .map_err(backend)?;

        tx.commit().await.map_err(backend)?;
        MemoryUri::parse(&uri)
    }

    async fn find(
        &self,
        scope: &ContextScope,
        query: &str,
        filters: &RecallFilters,
    ) -> Result<Vec<MemoryHit>> {
        let tenant = scope.tenant_id;
        let tenant_s = tenant.to_string();
        let limit = (filters.limit.clamp(1, 200)) as i64;
        // Fixed parameter positions ($1..$6), all always bound (NULL for absent optionals).
        let q: String = query.trim().to_string();
        let kind_opt: Option<String> = filters.kind.map(|k| k.as_str().to_string());
        let ns_like: Option<String> = filters.namespace_prefix.as_ref().map(|p| format!("{p}%"));
        let since: Option<DateTime<Utc>> = filters.since;

        let mut client = self.pool.get().await.map_err(backend)?;
        let tx = client.transaction().await.map_err(backend)?;
        tx.execute("SELECT set_config('app.tenant_id', $1, true)", &[&tenant_s])
            .await
            .map_err(backend)?;

        const FTS: &str = "to_tsvector('english', coalesce(title,'')||' '||coalesce(abstract,'')||' '||coalesce(body,''))";
        let sql = format!(
            "SELECT uri_path, kind, title, abstract, importance,
                    CASE WHEN $2 = '' THEN 0::real
                         ELSE ts_rank({FTS}, plainto_tsquery('english', $2)) END AS score
             FROM memory.records
             WHERE tenant_id = $1
               AND status = 'active'
               AND ($2 = '' OR {FTS} @@ plainto_tsquery('english', $2))
               AND ($3::text IS NULL OR kind = $3)
               AND ($4::text IS NULL OR namespace LIKE $4)
               AND ($5::timestamptz IS NULL OR created_at >= $5)
             ORDER BY score DESC, importance DESC, created_at DESC
             LIMIT $6"
        );

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            vec![&tenant, &q, &kind_opt, &ns_like, &since, &limit];
        let rows = tx.query(&sql, &params).await.map_err(backend)?;
        tx.commit().await.map_err(backend)?;

        let mut hits = Vec::with_capacity(rows.len());
        for row in &rows {
            let kind = MemoryKind::parse(row.get::<_, String>("kind").as_str())?;
            hits.push(MemoryHit {
                uri: row.get("uri_path"),
                kind,
                title: row.get("title"),
                abstract_: row.get("abstract"),
                score: row.get::<_, f32>("score"),
                importance: row.get::<_, i16>("importance") as u8,
            });
        }
        Ok(hits)
    }

    async fn read(&self, scope: &ContextScope, uri: &MemoryUri) -> Result<MemoryRecord> {
        let tenant = scope.tenant_id;
        let tenant_s = tenant.to_string();
        let mut client = self.pool.get().await.map_err(backend)?;
        let tx = client.transaction().await.map_err(backend)?;
        tx.execute("SELECT set_config('app.tenant_id', $1, true)", &[&tenant_s])
            .await
            .map_err(backend)?;
        let row = tx
            .query_opt(
                "SELECT * FROM memory.records WHERE tenant_id=$1 AND id=$2 AND status<>'forgotten'",
                &[&tenant, &uri.record_id],
            )
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        match row {
            Some(r) => map_record(&r),
            None => Err(MemoryError::NotFound(uri.display_relative())),
        }
    }

    async fn list(&self, scope: &ContextScope, prefix: &str) -> Result<Vec<MemoryUri>> {
        Namespace::parse(prefix).ok(); // best-effort grammar check; prefix may be a partial path
        let tenant = scope.tenant_id;
        let tenant_s = tenant.to_string();
        let like = format!("{prefix}%");
        let mut client = self.pool.get().await.map_err(backend)?;
        let tx = client.transaction().await.map_err(backend)?;
        tx.execute("SELECT set_config('app.tenant_id', $1, true)", &[&tenant_s])
            .await
            .map_err(backend)?;
        let rows = tx
            .query(
                "SELECT uri_path FROM memory.records WHERE tenant_id=$1 AND status='active' AND namespace LIKE $2 ORDER BY created_at DESC LIMIT 500",
                &[&tenant, &like],
            )
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        rows.iter()
            .map(|r| MemoryUri::parse(&r.get::<_, String>("uri_path")))
            .collect()
    }

    async fn forget(&self, scope: &ContextScope, uri: &MemoryUri) -> Result<()> {
        let tenant = scope.tenant_id;
        let tenant_s = tenant.to_string();
        let mut client = self.pool.get().await.map_err(backend)?;
        let tx = client.transaction().await.map_err(backend)?;
        tx.execute("SELECT set_config('app.tenant_id', $1, true)", &[&tenant_s])
            .await
            .map_err(backend)?;
        let n = tx
            .execute(
                "UPDATE memory.records SET status='forgotten', updated_at=now() WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &uri.record_id],
            )
            .await
            .map_err(backend)?;
        tx.execute(
            "INSERT INTO memory.index_outbox (record_id, tenant_id, op) VALUES ($1,$2,'delete')",
            &[&uri.record_id, &tenant],
        )
        .await
        .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        if n == 0 {
            return Err(MemoryError::NotFound(uri.display_relative()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_is_deterministic_and_order_independent() {
        // Same key→value mapping ({a:1, b:2}), inserted in different ORDER → same hash.
        let mk = |order: [&str; 2]| {
            let mut fields = serde_json::Map::new();
            for k in order {
                let v = if k == "a" { 1 } else { 2 };
                fields.insert(k.to_string(), serde_json::json!(v));
            }
            MemoryWrite {
                kind: MemoryKind::Decision,
                namespace: "resources/x".into(),
                title: "t".into(),
                body: "b".into(),
                fields,
                source_refs: vec![],
                tags: vec![],
                importance: 0,
                confidence: 1.0,
            }
        };
        assert_eq!(content_sha(&mk(["a", "b"])), content_sha(&mk(["b", "a"])));
    }

    #[test]
    fn abstract_truncates() {
        let long = "x".repeat(400);
        let a = abstract_of(&long);
        assert!(a.chars().count() <= 281); // 280 + ellipsis
        assert_eq!(abstract_of("short"), "short");
    }

    #[test]
    fn uri_is_parseable() {
        let id = Uuid::new_v4();
        let t = Uuid::new_v4();
        let s = uri_of(t, "resources/coding/decisions", MemoryKind::Decision, id);
        let uri = MemoryUri::parse(&s).unwrap();
        assert_eq!(uri.record_id, id);
        assert_eq!(uri.tenant_id, t);
        assert_eq!(uri.record_type, MemoryKind::Decision);
    }
}
