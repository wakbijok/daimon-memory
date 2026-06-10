<div align="center">

# daimon-memory

**Shared memory and shared discipline for AI assistants. Deterministic recall, curated capture, one persona and one operating discipline across every tool.**

One backend for Claude Code, Codex, Hermes, and your own agents.
Postgres for truth, Qdrant for meaning, MCP + REST for everyone, and a typed instruction layer that travels with you.

[Why](#the-problem) · [Features](#features) · [System layer](#the-system-layer-persona--discipline) · [Retrieval](#retrieval-how-recall-works) · [Architecture](#architecture) · [Quick start](#quick-start) · [Connect a tool](#connect-your-tools) · [CLI](#the-daimon-cli) · [Roadmap](#status--roadmap)

![status](https://img.shields.io/badge/status-experimental-orange) ![license](https://img.shields.io/badge/license-MIT-blue) ![rust](https://img.shields.io/badge/rust-edition%202024-orange)

</div>

---

## The problem

AI assistants are growing memories, but each one keeps its **own**, and they don't talk to each other.

- **Siloed.** Your coding agent remembers one thing, your chat agent another. Switch tools and the context is gone. Your "second brain" is shattered across a dozen apps.
- **Manual and drifting.** Hand-curated Markdown (`CLAUDE.md`, `AGENTS.md`, notes) doesn't scale, goes stale, and gets copy-pasted between tools.
- **"Smart" memory is heavy.** Many memory systems bundle a large language/vision model to *extract* memories from raw conversation. That is powerful, but it pushes the hardware requirement so high that the memory layer alone can demand a frontier GPU.
- **Memory without discipline just piles up.** A store is not enough. The value is the operating discipline on top: recall before answering, capture a decision the moment it is made, log a correction and never repeat it. Most tools leave that to chance, so the store starves.

**daimon-memory** is a lightweight, self-hostable memory backend **and** a portable operating discipline that any tool can adopt, designed so the expensive part (a large model) is *never in the loop*.

## What you get

Two layers, one service:

1. **The substrate.** PostgreSQL (the canonical store) + Qdrant (a rebuildable semantic index), exposed over **MCP** and **REST**. Typed, validated, addressable memory that every tool reads and writes.
2. **The system layer.** A shared **persona**, a **behavioral discipline**, and a **save discipline**, authored once and loaded into every tool at session start, plus a deterministic **save-nudge engine** that keeps the agent actually saving. No extraction model anywhere.

Three principles keep it light and trustworthy:

> **1 · Recall is deterministic and zero-LLM.** Retrieval is embeddings + keyword + filters, fused by rank. No model decides what comes back, so it is fast, cheap, and reproducible.
>
> **2 · Capture is curated, structured by code.** Writes are *typed* and validated at the interface; malformed ones are rejected. The tool hands daimon the distilled result. daimon runs no extraction model.
>
> **3 · Discipline is config, not code.** Persona and protocols are typed records you author with a CLI (or import from files), loaded into every tool by its hooks. The agent curates content; deterministic hooks govern only *timing*.

## Features

- 🔗 **Cross-tool memory:** one tenant-scoped store behind every assistant; establish a fact once, recall it everywhere.
- 🧠 **Hybrid recall:** PostgreSQL full-text (GIN-indexed `tsvector`) **+** Qdrant dense vectors (HNSW), fused by **Reciprocal Rank Fusion**, with importance weighting and raw component scores surfaced. Find a record by *meaning* even when no keywords match, **with no LLM in the recall path**.
- 🧱 **Typed, validated capture:** **twelve** canonical record kinds with per-kind required fields, plus per-namespace extensibility.
- 🛠️ **Guided tool surface:** beyond `remember`/`recall`/`read`, the high-frequency saves get dedicated tools (`log_decision`, `log_lesson`, `log_incident`, `add_reminder`) that prefill the kind and name the required fields, plus `browse` (dedup) and a gated `forget`.
- 🎭 **Shared persona + discipline:** one identity and one operating discipline, authored by CLI, loaded into Claude Code, Codex, and Hermes at session start. Replaces per-tool `CLAUDE.md`/`AGENTS.md`/`SOUL.md` drift.
- ⏱️ **Deterministic save-nudges:** no auto-capture model. Instead, hooks scan each turn for a save-worthy signal that was not captured (or a quiet stretch) and nudge the agent to call the exact tool. Timing is enforced; content stays the agent's.
- ✋ **Curated, not raw:** no raw-transcript dumping. Memories come from explicit calls, mirrored tool-native memory, or the guided tools.
- 🔖 **Addressable:** every record gets a `daimon://{tenant}/{namespace}/{kind}/{id}` URI, a browsable namespace tree, and L0/L1/L2 tiers (abstract, overview, full).
- 🔒 **Tenant isolation:** PostgreSQL Row-Level Security (fail-closed) plus explicit predicates.
- ♻️ **Rebuildable index:** Qdrant is disposable; rebuild it from PostgreSQL with `daimon reindex`. Update-mode kinds supersede cleanly (no stale duplicates).
- 📤 **No dual-write:** a transactional **outbox** is drained to Qdrant asynchronously by a singleton indexer.
- 📈 **Scales:** a stateless API tier (autoscaling) over a stateful data tier; "design for 3, run 1".
- 🔌 **MCP-native and REST:** all tools over MCP `/mcp` *and* `/v1` REST endpoints.

## The system layer: persona + discipline

This is what turns a store into a disciplined operating system for an agent. Three typed records — the persona under `agent/persona`, the two disciplines under `agent/protocol` — authored by the `daimon` CLI, loaded into every tool at session start:

| Record | kind | what it carries |
|---|---|---|
| **Persona** | `persona` | the shared identity: who the agent is, its voice, its hard boundaries, and your profile |
| **Behavioral Discipline** | `protocol` | how the agent works: recall before reasoning, verify before claiming done, surface trade-offs, fail loudly and learn once |
| **Memory Save Discipline** | `protocol` | when and what to persist: which signal maps to which kind/tool, recall-before-write dedup, curated-not-raw, right namespace |

Each tool's SessionStart path recalls the system namespace, reads the full bodies, and injects them once per session as operating instructions. **The same record is authored once and three different agents wake up as the same operator with the same disciplines.**

The **save-nudge engine** makes the save discipline bite. daimon has no auto-capture, so without help an agent under-saves. Deterministic hooks (no model) close the loop:

- **Signal nudge:** the previous turn matched a save-signal class (decision, incident, lesson, follow-up, convention) and no save tool ran that turn, so it nudges, naming the exact tool ("save it with `log_decision`").
- **Cadence nudge:** after N quiet turns with no save, a capture-pass nudge. `N` is configurable (`DAIMON_NUDGE_CADENCE`, default 5; `DAIMON_NUDGE=off` disables).
- **Session-end pass:** sweeps the session for anything uncaptured.

It works across Claude Code (`UserPromptSubmit` + `SessionEnd`), Codex (`UserPromptSubmit`, lagged, parsing its rollout), and Hermes (in-process `sync_turn`).

## Retrieval: how recall works

Recall is indexed hybrid search, never a line scan:

- **Keyword:** a GIN index on `tsvector(title + abstract + body)`, ranked by `ts_rank`.
- **Semantic:** bge-small-en-v1.5 (384-d) embeddings of `title + body`, searched by Qdrant HNSW (cosine).
- **Fusion:** the two rank lists combined by Reciprocal Rank Fusion, with a small **importance** boost so persona/protocols and high-value records surface, and a deterministic tie-break.
- **Filters:** btree-indexed `kind`, `namespace_prefix`, `since`; GIN-indexed `tags`.
- **Interpretable:** each hit carries `scores: {rrf, raw_keyword, raw_semantic}` so the fused rank is explainable.

The vector index is fully rebuildable from Postgres: `daimon reindex`.

## Architecture

daimon-memory is a deterministic Rust service; there is **no model in the request path**.

```
   your tools  (Claude Code · Codex · Hermes · your agents)
        │   each tool's hooks: session-start persona/recall · per-turn recall + save-nudge · capture mirror
        │   MCP (/mcp)        REST (/v1)
        ▼
   ┌──────────────┐    recall = RRF( Postgres FTS , Qdrant dense ) + importance   (zero-LLM)
   │  daimon-mcp  │    store  = validate (control layer) -> Postgres + outbox
   └──────┬───────┘
          │  ContextMemory trait
   ┌──────▼───────┐      ┌───────────────┐       ┌────────────────┐
   │  daimon-pg   │      │  daimon-vec   │◄──────│ daimon-indexer │
   │  PostgreSQL  │      │ Qdrant + bge  │ embed  │ outbox drainer │
   │ (canonical)  │      │ (rebuildable) │       │  (singleton)   │
   └──────────────┘      └───────────────┘       └────────────────┘
```

A Rust workspace (edition 2024):

| Crate | Role |
|-------|------|
| `daimon-memory-core` | Deterministic, **LLM-free** core: the `MemoryKind` taxonomy (12 kinds, with required fields + write-mode), the `daimon://` URI / namespace grammar, control-layer write validation, the `ContextMemory` trait. Pure logic, unit-tested. |
| `daimon-pg` | PostgreSQL store: validated writes (content-hash dedup, Update-mode supersede, outbox in one transaction) plus deterministic full-text recall; tenant-scoped, RLS. |
| `daimon-vec` | In-process [fastembed](https://github.com/Anush008/fastembed-rs) **bge-small-en-v1.5** (384-d) embedder plus a Qdrant vector store. |
| `daimon-indexer` | Singleton drainer: PostgreSQL outbox, embed `title + body`, Qdrant upsert/delete. |
| `daimon-mcp` | Stateless server: REST `/v1` + MCP JSON-RPC `/mcp`; **hybrid recall** with importance weighting; the 9-tool surface; degrades to keyword-only if the vector tier is down. |
| `daimon-cli` (`daimon`) | Ops + management: `migrate` / `reindex` / `health` / `stats` / `export` / `import`, and the system-layer authoring `persona` / `protocol seed` / `protocol import`. |

**Data model** (`migrations/`): `records` (canonical, with `importance` + status), `namespaces` (the tree), `type_registry` (taxonomy + extensibility), `index_outbox` (Postgres to Qdrant).

## Tech stack

**Rust** (edition 2024) · tokio · axum · reqwest (CLI) · PostgreSQL (tokio-postgres + deadpool, refinery migrations) · Qdrant (qdrant-client) · fastembed / ONNX Runtime for embeddings · Reciprocal Rank Fusion for hybrid ranking.

## Quick start

Two installers: one for the **server** (this section) and one per **client tool** ([below](#connect-your-tools)).

Run the guided server installer. It prompts for each value, writes `.env`, starts the stack, **seeds the default protocols**, and offers to run the **persona wizard**:

```bash
git clone <repo-url> && cd daimon-memory
./install.sh
```

By hand with Docker Compose:

```bash
cp .env.example .env
docker compose up --build
# then set up the system layer (the binary ships in the image):
docker compose exec daimon-mcp daimon protocol seed     # default disciplines
docker compose exec -it daimon-mcp daimon persona        # your identity wizard
```

Smoke test:

```bash
curl -s localhost:8080/readyz

curl -s -XPOST localhost:8080/v1/memory -H 'content-type: application/json' -d '{
  "kind": "decision",
  "namespace": "resources/architecture/decisions",
  "title": "Adopt Postgres + Qdrant",
  "body": "Use Postgres as the canonical store and Qdrant as a rebuildable vector index.",
  "fields": { "context": "needed a shared memory store", "rationale": "deterministic recall, rebuildable index" }
}'

curl -s -XPOST localhost:8080/v1/recall -H 'content-type: application/json' -d '{"query":"how should we store memory"}'
```

> First start downloads the embedding model (about 130 MB) once.

### From source

```bash
cargo test --workspace
cargo run --bin daimon -- migrate
cargo run --bin daimon-mcp        # serves :8080  (/v1 + /mcp)
cargo run --bin daimon-indexer    # outbox to Qdrant (separate process)
```

## Connect your tools

Each client installer wires three things: **session-start** (persona + disciplines + recent context), **per-turn recall + save-nudge**, and **capture** (mirroring the tool's own memory and/or the guided tools).

### Hermes (native memory provider) ✅

A first-class Hermes memory provider: automatic recall each turn, the persona/discipline block in the system prompt, an in-process save-nudge, and curated capture (mirrors Hermes's own memory writes; `daimon_remember`/`daimon_recall`/`daimon_read` tools).

```bash
cd integrations/hermes
./install.sh --endpoint http://localhost:8080 --activate --yes
```
Uninstall: `./install.sh --uninstall`.

### Claude Code (plugin) ✅

A marketplace plugin: hot-memory recall + the persona/discipline loader on `SessionStart`, the save-nudge on `UserPromptSubmit`, a `SessionEnd` sweep, the full MCP tool surface, a `/daimon` command, and a mirror of Claude's own auto-memory into `agent/lessons`.

```bash
cd integrations/claude-code
./install.sh --endpoint http://localhost:8080
# then in Claude Code:
#   /plugin marketplace add <abs-path>/integrations/claude-code
#   /plugin install daimon-memory@daimon-memory
```

### Codex (plugin) ✅

A fully-automated plugin (Codex has a `codex plugin` CLI): persona/discipline loader + recall + save-nudge, the MCP tools, and **native-memory mirroring**, the installer enables Codex's own memory and a hook mirrors it (read from Codex's SQLite store) into `agent/lessons`.

```bash
cd integrations/codex
./install.sh --endpoint http://localhost:8080 --yes
```
Restart Codex. Verify with `codex plugin list | grep daimon`.

> Memories live in daimon independently of any tool, so uninstalling a tool never deletes your memory.

## The daimon CLI

The `daimon` binary (shipped in the server image, or `cargo build --bin daimon`) is the ops + system-layer management surface:

```bash
daimon migrate            # apply schema migrations
daimon reindex            # drop + rebuild the Qdrant index from Postgres (prunes stale points)
daimon health             # ping Postgres + Qdrant
daimon stats              # record counts by kind + outbox + Qdrant points
daimon export > memory.jsonl           # backup: every record as JSONL (all statuses)
daimon import memory.jsonl             # restore (idempotent); then run: daimon reindex
daimon persona            # interactive wizard: author the shared persona
daimon protocol seed      # write the bundled default disciplines
daimon protocol import <file-or-dir>   # import protocols from markdown (the ov-style file path)
```

Persona and protocol content is born at run time and lives in the server, never baked into the binary, so a public install ships the question-asker, not anyone's values.

## Configuration

Server + indexer (environment):

| Variable | Purpose | Default |
|----------|---------|---------|
| `DAIMON_MCP_BIND` | Listen address | `0.0.0.0:8080` |
| `DAIMON_API_KEY` | Bearer token. When set, `/v1/*` + `/mcp` require `Authorization: Bearer <token>` (`/health` + `/readyz` stay open for probes). **Unset = the API is open** - fine on localhost, not on a shared network. | (unset = no auth) |
| `PGHOST` `PGPORT` `PGUSER` `PGPASSWORD` `PGDATABASE` | PostgreSQL connection | `127.0.0.1` / `5432` / `daimon` / (empty) / `daimon_memory` |
| `DAIMON_QDRANT_URL` | Qdrant **gRPC** endpoint | indexer/CLI: `http://127.0.0.1:6334`; server: unset = semantic tier disabled (keyword-only recall) |
| `DAIMON_DEFAULT_TENANT` | Tenant used when no `X-Daimon-Tenant` header is sent | a fixed dev UUID |
| `RUST_LOG` | Log level | `info` |

Clients send the same token: the Claude Code / Codex installers ask for it (`--api-key`), Hermes reads `DAIMON_API_KEY` from its env, and the `daimon` CLI's persona/protocol commands pick it up from the environment.

Client save-nudge (per tool):

| Variable | Purpose | Default |
|----------|---------|---------|
| `DAIMON_NUDGE_CADENCE` | quiet turns before a cadence nudge; `0` disables cadence | `5` |
| `DAIMON_NUDGE` | `off` disables all nudges | `on` |

**API.** REST: `POST /v1/memory`, `POST /v1/recall`, `GET /v1/read?uri=`, `GET /readyz`, `GET /health`. MCP `POST /mcp` exposes: `remember`, `recall`, `read`, `log_decision`, `log_lesson`, `log_incident`, `add_reminder`, `browse`, `forget`.

## Deployment

- **Docker / Compose.** The included `Dockerfile` builds the binaries; `docker-compose.yml` runs the full stack.
- **Kubernetes.** daimon-mcp is a stateless `Deployment` (with an HPA); PostgreSQL and Qdrant are `StatefulSet`s. The embedder needs **AVX2**; schedule daimon-mcp + daimon-indexer onto an AVX2 node. Build the image (for example with kaniko) and apply your manifests, or sync via GitOps.

## Backup and restore

**PostgreSQL is the canonical store and is NOT rebuildable** - persona, decisions, lessons live only there. Qdrant is disposable (`daimon reindex` rebuilds it); the outbox makes it eventually consistent. Back up Postgres like you would any database you care about:

```bash
# logical dump (compose; -T = no TTY, keeps the stream byte-clean):
docker compose exec -T postgres pg_dump -U daimon daimon_memory | gzip > daimon-$(date +%F).sql.gz
# restore:
gunzip -c daimon-2026-06-10.sql.gz | docker compose exec -T postgres psql -U daimon daimon_memory

# storage-engine-agnostic (works across Postgres versions / into a fresh stack):
docker compose exec -T daimon-mcp daimon export > memory.jsonl
docker compose exec -T daimon-mcp daimon import -  < memory.jsonl   # idempotent
docker compose exec -T daimon-mcp daimon reindex                    # rebuild vectors after import
```

On Kubernetes, run `pg_dump` from a CronJob to durable storage (NFS/object store). `daimon reindex` only rebuilds the **vector index** - it cannot recover lost records.

## Observability

There is no metrics stack; the signals live on the endpoints:

- `GET /readyz` returns `{ready, outbox_pending, outbox_oldest_age_secs}` and, when the backlog is older than ~10 min, an `outbox_warning`. A growing `outbox_pending` means the **indexer is stalled or dead** and semantic recall is going stale - the one number worth alerting on. Readiness itself only flips on Postgres being unreachable.
- `GET /health` reports the running `version` (handy to confirm a rollout landed).
- `daimon stats` prints record counts by kind + pending outbox depth.
- Both binaries shut down gracefully on SIGTERM (k8s rollout/evict): the server drains in-flight requests, the indexer stops cleanly between batches (every batch is already crash-safe).

## Upgrading an existing deployment

- **Run `daimon reindex` once after upgrading.** The semantic index payload now carries `created_at` (used by the `since` filter); points indexed by an older version lack it and are excluded whenever a `since` filter applies, until reindexed.
- **Pre-G1 namespaces:** if you have records under the retired `shared-canonical/*` / `*-private/*` roots, move them to the current roots (`user/`, `agent/`, `resources/`, `session/`) - e.g. `UPDATE memory.records SET namespace = 'agent/persona', uri_path = replace(uri_path, '/shared-canonical/system/', '/agent/persona/') WHERE namespace = 'shared-canonical/system';` per old path, or `daimon export` | rewrite | `daimon import` into a fresh database - then `daimon reindex`. Personas/protocols left under the old roots are not found by the session-start loader (it reads `agent/`).

## Status and roadmap

> ⚠️ **Experimental.** Feature-complete; the deterministic core - domain model, URI/namespace grammar, write validation, and the **RRF recall fusion** - is unit-tested, and the save-nudge engine has node tests with a cross-client parity check. Integration paths against live Postgres/Qdrant are still exercised manually. APIs may change before a tagged release. Not yet recommended for production.

**Working today:** typed control-layer writes (12 kinds, Update-mode supersede) · hybrid recall (GIN keyword + HNSW semantic, RRF, importance-weighted, raw scores, kind/since filters on both arms) · the outbox-to-Qdrant indexer (per-row retry + dead-letter) · REST + MCP (9-tool surface) · shared-secret bearer auth (`DAIMON_API_KEY`) · the shared persona + behavioral/save discipline layer · the deterministic save-nudge engine across all three tools · per-tool capture (Claude auto-memory mirror, Codex native-memory mirror, Hermes curated capture) · the `daimon` CLI (ops + backup `export`/`import` + persona + protocol seed/import) · Hermes, Claude Code, and Codex integrations.

**Planned:** per-tenant auth tokens · a least-privilege DB role so RLS is the active enforcer · streamable-HTTP `/mcp` SSE · sharper recall ranking (larger embedder, optional reranker) · memory consolidation/decay · integration tests against real Postgres/Qdrant.

## Contributing

Issues and PRs welcome. `cargo test --workspace` should pass; please keep the core crate free of I/O and model calls (that is what makes recall deterministic). For larger changes, open an issue to discuss the design first.

## License

[MIT](LICENSE).
