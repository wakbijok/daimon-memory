//! `daimon` - ops CLI for daimon-memory.
//!
//! Subcommands:
//! - `migrate` - apply embedded SQL migrations (refinery) to Postgres.
//! - `reindex` - drop + rebuild the entire Qdrant index from Postgres (the "Qdrant is
//!   rebuildable from the canonical store" guarantee, made real - including PRUNING
//!   points whose records were forgotten/superseded).
//! - `health`  - ping Postgres + Qdrant.
//! - `stats`   - record counts by kind + pending outbox + Qdrant point count.
//! - `export`  - dump every record (all statuses) as JSONL to stdout (backup).
//! - `import <file|->` - restore an export (idempotent: conflicting rows are skipped).
//! - `persona` / `protocol seed|import` - author the shared system layer.
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
        "export" => export().await,
        "import" => {
            let path = std::env::args()
                .nth(2)
                .ok_or_else(|| anyhow::anyhow!("usage: daimon import <file|-> (- = stdin)"))?;
            import(&path).await
        }
        "persona" => persona().await,
        "protocol" => protocol().await,
        other => {
            eprintln!(
                "daimon {} - usage: daimon <migrate|reindex|health|stats|export|import <file|->|persona|protocol>",
                if other.is_empty() {
                    "(no command)"
                } else {
                    other
                }
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
    let report = embedded::migrations::runner()
        .run_async(&mut client)
        .await?;
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
    // Drop + recreate so the rebuild also PRUNES stale points (forgotten/superseded
    // records whose delete never reached Qdrant). Upsert-only rebuilds leak retracted
    // memories back into semantic recall. Semantic recall degrades to keyword-only for
    // the seconds the collection is being repopulated.
    eprintln!("reindex: recreating collection (prunes stale points)…");
    store.recreate().await.map_err(to_anyhow)?;
    eprintln!("reindex: loading embedder…");
    let embedder = Embedder::new().map_err(to_anyhow)?;

    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT id, tenant_id, namespace, kind, title, abstract, body, importance, uri_path,
                    extract(epoch FROM created_at)::bigint AS created_epoch
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
        let body: String = row.get("body");
        let importance: i16 = row.get("importance");
        // Embed title + body (capped; keep in sync with the indexer).
        let body_capped: String = body.chars().take(2000).collect();
        let mut vecs = embedder
            .embed(&[format!("{title}. {body_capped}")])
            .map_err(to_anyhow)?;
        let vector = vecs.pop().unwrap_or_default();
        let payload = json!({
            "tenant_id": tenant_id.to_string(),
            "namespace": row.get::<_, String>("namespace"),
            "kind": row.get::<_, String>("kind"),
            "title": title,
            "abstract": abstract_,
            "importance": importance,
            "uri": row.get::<_, String>("uri_path"),
            "created_at": row.get::<_, i64>("created_epoch"),
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
    // Mirrors /readyz: keyword recall needs Postgres only; hybrid additionally needs
    // Qdrant plus a CPU the embedder can run on (AVX2 on x86_64).
    let recall_tier = if !pg_ok {
        "unhealthy"
    } else if qd_ok && daimon_vec::embedder_supported() {
        "hybrid"
    } else {
        "keyword"
    };
    println!(
        "{}",
        json!({"postgres": pg_ok, "qdrant": qd_ok, "recall_tier": recall_tier, "healthy": pg_ok && qd_ok})
    );
    if pg_ok && qd_ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
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

/// Dump every record (ALL statuses, full fidelity) as one JSON object per line to stdout.
/// Postgres is the canonical, non-rebuildable store - this is the storage-engine-agnostic
/// backup counterpart to `reindex` (which only rebuilds the disposable Qdrant side).
/// Timestamps are exported as ISO-8601 text so import round-trips without a date dependency.
async fn export() -> Result<()> {
    use std::io::Write;
    let pool = build_pool(&PgConfig::from_env())?;
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT id, tenant_id, namespace, owner_user_id, agent_id, kind, title, body,
                    abstract, fields, source_refs,
                    tags, importance, confidence, content_sha, schema_version, status,
                    supersedes_id, reverses_id, uri_path,
                    to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS created_at,
                    to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS updated_at
             FROM memory.records ORDER BY created_at",
            &[],
        )
        .await?;
    let mut out = std::io::stdout().lock();
    for row in &rows {
        let rec = json!({
            "id": row.get::<_, Uuid>("id").to_string(),
            "tenant_id": row.get::<_, Uuid>("tenant_id").to_string(),
            "namespace": row.get::<_, String>("namespace"),
            "owner_user_id": row.get::<_, Option<Uuid>>("owner_user_id").map(|u| u.to_string()),
            "agent_id": row.get::<_, Option<String>>("agent_id"),
            "kind": row.get::<_, String>("kind"),
            "title": row.get::<_, String>("title"),
            "body": row.get::<_, String>("body"),
            "abstract": row.get::<_, String>("abstract"),
            "fields": row.get::<_, serde_json::Value>("fields"),
            "source_refs": row.get::<_, serde_json::Value>("source_refs"),
            "tags": row.get::<_, Vec<String>>("tags"),
            "importance": row.get::<_, i16>("importance"),
            "confidence": row.get::<_, f32>("confidence"),
            "content_sha": row.get::<_, String>("content_sha"),
            "schema_version": row.get::<_, i32>("schema_version"),
            "status": row.get::<_, String>("status"),
            "supersedes_id": row.get::<_, Option<Uuid>>("supersedes_id").map(|u| u.to_string()),
            "reverses_id": row.get::<_, Option<Uuid>>("reverses_id").map(|u| u.to_string()),
            "uri_path": row.get::<_, String>("uri_path"),
            "created_at": row.get::<_, String>("created_at"),
            "updated_at": row.get::<_, String>("updated_at"),
        });
        writeln!(out, "{}", serde_json::to_string(&rec)?)?;
    }
    eprintln!("export: {} record(s)", rows.len());
    Ok(())
}

/// Restore a JSONL export. Idempotent: existing ids are skipped (ON CONFLICT DO NOTHING),
/// so re-running a partial restore is safe. Rebuild the vector index afterwards with
/// `daimon reindex` (import does not write the outbox).
async fn import(path: &str) -> Result<()> {
    use std::io::Read;
    let mut raw = String::new();
    if path == "-" {
        std::io::stdin().read_to_string(&mut raw)?;
    } else {
        raw = std::fs::read_to_string(path)?;
    }
    let pool = build_pool(&PgConfig::from_env())?;
    let client = pool.get().await?;

    let s = |v: &serde_json::Value, k: &str| -> String {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let mut inserted = 0usize;
    let mut skipped = 0usize;
    for (idx, line) in raw.lines().enumerate() {
        let line_no = idx + 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("line {line_no}: invalid JSON: {e}"))?;
        let id =
            Uuid::parse_str(&s(&v, "id")).map_err(|_| anyhow::anyhow!("line {line_no}: bad id"))?;
        let tenant = Uuid::parse_str(&s(&v, "tenant_id"))
            .map_err(|_| anyhow::anyhow!("line {line_no}: bad tenant_id"))?;
        let supersedes: Option<Uuid> = v
            .get("supersedes_id")
            .and_then(|x| x.as_str())
            .and_then(|x| Uuid::parse_str(x).ok());
        let reverses: Option<Uuid> = v
            .get("reverses_id")
            .and_then(|x| x.as_str())
            .and_then(|x| Uuid::parse_str(x).ok());
        let fields = v.get("fields").cloned().unwrap_or_else(|| json!({}));
        let source_refs = v.get("source_refs").cloned().unwrap_or_else(|| json!([]));
        let tags: Vec<String> = v
            .get("tags")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let importance = v.get("importance").and_then(|x| x.as_i64()).unwrap_or(0) as i16;
        let confidence = v.get("confidence").and_then(|x| x.as_f64()).unwrap_or(1.0) as f32;
        let schema_version = v
            .get("schema_version")
            .and_then(|x| x.as_i64())
            .unwrap_or(1) as i32;
        let (namespace, kind, title, body, abstract_) = (
            s(&v, "namespace"),
            s(&v, "kind"),
            s(&v, "title"),
            s(&v, "body"),
            s(&v, "abstract"),
        );
        let (content_sha, status, uri_path) =
            (s(&v, "content_sha"), s(&v, "status"), s(&v, "uri_path"));
        let (created_at, updated_at) = (s(&v, "created_at"), s(&v, "updated_at"));
        let owner_user_id: Option<Uuid> = v
            .get("owner_user_id")
            .and_then(|x| x.as_str())
            .and_then(|x| Uuid::parse_str(x).ok());
        let agent_id: Option<String> = v.get("agent_id").and_then(|x| x.as_str()).map(String::from);

        // Bare ON CONFLICT (no target) absorbs ANY unique violation - both the (id) PK and
        // the records_dedup_active partial index on (tenant_id, content_sha). Restoring into
        // a non-empty DB where the same content was re-stored under a new id must skip, not
        // abort the whole import.
        let n = client
            .execute(
                "INSERT INTO memory.records
                   (id, tenant_id, namespace, owner_user_id, agent_id, kind, title, body,
                    abstract, fields, source_refs,
                    tags, importance, confidence, content_sha, schema_version, status,
                    supersedes_id, reverses_id, uri_path, created_at, updated_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
                         $21::timestamptz,$22::timestamptz)
                 ON CONFLICT DO NOTHING",
                &[
                    &id,
                    &tenant,
                    &namespace,
                    &owner_user_id,
                    &agent_id,
                    &kind,
                    &title,
                    &body,
                    &abstract_,
                    &fields,
                    &source_refs,
                    &tags,
                    &importance,
                    &confidence,
                    &content_sha,
                    &schema_version,
                    &status,
                    &supersedes,
                    &reverses,
                    &uri_path,
                    &created_at,
                    &updated_at,
                ],
            )
            .await?;
        if n == 1 {
            inserted += 1;
            client
                .execute(
                    "INSERT INTO memory.namespaces (tenant_id, path) VALUES ($1,$2) ON CONFLICT DO NOTHING",
                    &[&tenant, &namespace],
                )
                .await?;
        } else {
            skipped += 1;
        }
    }
    println!("import: {inserted} inserted, {skipped} already present");
    println!("import: now rebuild the vector index: daimon reindex");
    Ok(())
}

fn prompt_line(prompt: &str, default: &str) -> String {
    use std::io::Write;
    print!("  {prompt}");
    if !default.is_empty() {
        print!(" [{default}]");
    }
    print!(": ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    let s = s.trim();
    if s.is_empty() {
        default.to_string()
    } else {
        s.to_string()
    }
}

/// Interactive persona wizard (the ov-style management surface). Prompts for the AI's
/// identity + the user's profile and writes ONE `persona` record to
/// agent/persona via the REST API, the single sanctioned writer of the
/// persona kind. Config: DAIMON_ENDPOINT (default http://127.0.0.1:8080), DAIMON_TENANT.
async fn persona() -> Result<()> {
    let endpoint = std::env::var("DAIMON_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
        .trim_end_matches('/')
        .to_string();
    let tenant = std::env::var("DAIMON_TENANT")
        .unwrap_or_else(|_| "00000000-0000-0000-0000-0000000000d1".to_string());
    let default_user = std::env::var("USER").unwrap_or_default();

    println!("daimon persona setup  ->  {endpoint}");
    println!("Defines the shared identity every connected tool adopts at session start.\n");
    let ai_name = prompt_line("AI name (how it refers to itself)", "Assistant");
    let role = prompt_line("AI role (what it helps you do)", "collaborative partner");
    let voice = prompt_line(
        "Voice / tone",
        "direct, concise, technical; challenges weak ideas",
    );
    let avoid = prompt_line(
        "What it must NOT do",
        "no hype, no hedging, no fabricated context",
    );
    let user_name = prompt_line("Your name (how the AI addresses you)", &default_user);
    let user_job = prompt_line("Your work / role", "");
    let boundaries = prompt_line(
        "Hard boundaries",
        "never read private dirs; never modify credentials without approval; persist memory only via daimon-memory",
    );

    let identity = format!("I am {ai_name}, {user_name}'s {role}. Not the base model.");
    let voice_full = format!("{voice}. Avoid: {avoid}.");
    let body = format!(
        "# Operator Persona\n\nI am {ai_name}, {user_name}'s {role}. Not the base model.\n\n\
         ## Voice\n{voice}\n\n## What I do not do\n{avoid}\n\n\
         ## User\n- Name: {user_name}\n- Work: {user_job}\n\n## Boundaries\n{boundaries}"
    );
    let record = json!({
        "kind": "persona",
        "namespace": "agent/persona",
        "title": "Operator Persona",
        "body": body,
        "fields": { "identity": identity, "voice": voice_full, "boundaries": boundaries },
        "tags": ["persona", "system"],
        "importance": 95
    });

    let mut req = reqwest::Client::new()
        .post(format!("{endpoint}/v1/memory"))
        .header("x-daimon-tenant", &tenant)
        .json(&record);
    if let Some(token) = api_key() {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let txt = resp.text().await.unwrap_or_default();
    if status.is_success() {
        println!("\npersona saved -> {txt}");
        Ok(())
    } else {
        anyhow::bail!("persona write failed ({status}): {txt}");
    }
}

/// Bearer token for the REST endpoint (same env the server + client hooks use).
fn api_key() -> Option<String> {
    std::env::var("DAIMON_API_KEY")
        .ok()
        .filter(|t| !t.trim().is_empty())
}

fn endpoint_tenant() -> (String, String) {
    let endpoint = std::env::var("DAIMON_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string())
        .trim_end_matches('/')
        .to_string();
    let tenant = std::env::var("DAIMON_TENANT")
        .unwrap_or_else(|_| "00000000-0000-0000-0000-0000000000d1".to_string());
    (endpoint, tenant)
}

async fn post_memory(endpoint: &str, tenant: &str, record: &serde_json::Value) -> Result<String> {
    let mut req = reqwest::Client::new()
        .post(format!("{endpoint}/v1/memory"))
        .header("x-daimon-tenant", tenant)
        .json(record);
    if let Some(token) = api_key() {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let txt = resp.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(txt)
    } else {
        anyhow::bail!("write failed ({status}): {txt}")
    }
}

// ---- protocol subcommand: seed bundled defaults, or import from markdown file(s) ----
// The base protocol templates ship in the binary; `import` reads the same frontmatter+body
// format from any file or directory (the ov-style config-from-files capability).

const BEHAVIORAL_MD: &str = include_str!("../templates/behavioral-discipline.md");
const SAVE_MD: &str = include_str!("../templates/memory-save-discipline.md");

async fn protocol() -> Result<()> {
    match std::env::args().nth(2).unwrap_or_default().as_str() {
        "seed" => protocol_seed().await,
        "import" => {
            let path = std::env::args()
                .nth(3)
                .ok_or_else(|| anyhow::anyhow!("usage: daimon protocol import <file-or-dir>"))?;
            protocol_import(&path).await
        }
        other => {
            eprintln!(
                "daimon protocol {} - usage: daimon protocol <seed|import <file-or-dir>>",
                if other.is_empty() {
                    "(no subcommand)"
                } else {
                    other
                }
            );
            std::process::exit(2);
        }
    }
}

async fn protocol_seed() -> Result<()> {
    let (endpoint, tenant) = endpoint_tenant();
    for md in [BEHAVIORAL_MD, SAVE_MD] {
        let rec = parse_protocol_md(md)?;
        let txt = post_memory(&endpoint, &tenant, &rec.record()).await?;
        println!("seeded protocol '{}' -> {}", rec.title, txt);
    }
    Ok(())
}

async fn protocol_import(path: &str) -> Result<()> {
    let (endpoint, tenant) = endpoint_tenant();
    let p = std::path::Path::new(path);
    let files: Vec<std::path::PathBuf> = if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|q| q.extension().map(|e| e == "md").unwrap_or(false))
            .collect();
        v.sort();
        v
    } else {
        vec![p.to_path_buf()]
    };
    if files.is_empty() {
        anyhow::bail!("no .md protocol files found at {path}");
    }
    for f in files {
        let md = std::fs::read_to_string(&f)?;
        let rec = parse_protocol_md(&md)?;
        let txt = post_memory(&endpoint, &tenant, &rec.record()).await?;
        println!("imported '{}' from {} -> {}", rec.title, f.display(), txt);
    }
    Ok(())
}

struct ProtoRec {
    title: String,
    scope: String,
    rules: String,
    namespace: String,
    body: String,
}
impl ProtoRec {
    fn record(&self) -> serde_json::Value {
        json!({
            "kind": "protocol",
            "namespace": self.namespace,
            "title": self.title,
            "body": self.body,
            "fields": {"scope": self.scope, "rules": self.rules},
            "tags": ["system", "protocol"],
            "importance": 95
        })
    }
}

// Parse a protocol markdown file: optional frontmatter (title/namespace/scope/rules) fenced by
// `---`, then the body. Only `title` is required; scope/rules default to a derived summary.
fn parse_protocol_md(md: &str) -> Result<ProtoRec> {
    let mut title = String::new();
    let mut scope = String::new();
    let mut rules = String::new();
    let mut namespace = "agent/protocol".to_string();
    let trimmed = md.trim_start();
    let body;
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let v = v.trim().to_string();
                    match k.trim() {
                        "title" => title = v,
                        "scope" => scope = v,
                        "rules" => rules = v,
                        "namespace" => namespace = v,
                        _ => {}
                    }
                }
            }
            body = rest[end + 4..].trim_start().to_string();
        } else {
            body = trimmed.to_string();
        }
    } else {
        body = trimmed.to_string();
    }
    if title.is_empty() {
        anyhow::bail!("protocol file missing 'title' in frontmatter");
    }
    if scope.is_empty() {
        scope = "see body".to_string();
    }
    if rules.is_empty() {
        rules = body
            .lines()
            .next()
            .unwrap_or("see body")
            .chars()
            .take(200)
            .collect();
        if rules.trim().is_empty() {
            rules = "see body".to_string();
        }
    }
    Ok(ProtoRec {
        title,
        scope,
        rules,
        namespace,
        body,
    })
}

fn to_anyhow(e: daimon_memory_core::MemoryError) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}
