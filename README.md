<div align="center">

# daimon-memory

**A shared, cross-tool memory engine for AI assistants — where recall is deterministic and capture is curated.**

One memory backend for Claude Code, Codex, Hermes, and your own agents.
Postgres for truth, Qdrant for meaning, MCP + REST for everyone.

[Why](#the-problem) · [Features](#features) · [Architecture](#architecture) · [Quick start](#quick-start) · [Connect a tool](#connect-your-tools) · [Roadmap](#status--roadmap)

![status](https://img.shields.io/badge/status-experimental-orange) ![license](https://img.shields.io/badge/license-MIT-blue) ![rust](https://img.shields.io/badge/rust-edition%202024-orange)

</div>

---

## The problem

AI assistants are growing memories — but each one keeps its **own**, and they don't talk to each other.

- **Siloed.** Your coding agent remembers one thing, your chat agent another. Switch tools and the context is gone. Your "second brain" is shattered across a dozen apps.
- **Manual & drifting.** Hand-curated Markdown (`CLAUDE.md`, `AGENTS.md`, notes) doesn't scale, goes stale, and gets copy-pasted between tools.
- **"Smart" memory is heavy.** Many memory systems bundle a large language/vision model to *extract* memories from raw conversation. That's powerful — but it pushes the hardware requirement so high that the memory layer alone can demand a frontier GPU. If one component needs a big model, the whole stack does.

**daimon-memory** is a lightweight, self-hostable memory backend that any tool can read and write — designed so the expensive part (a large model) is *never in the loop*.

## Introduction

daimon-memory is a single service backed by **PostgreSQL** (the canonical store) and **Qdrant** (a rebuildable semantic index), exposed over the **Model Context Protocol (MCP)** and a plain **REST** API. Point your tools at it and they share one memory.

Two principles keep it light and trustworthy:

> **1 · Recall is deterministic and zero-LLM.** Retrieval is embeddings + keyword + filters — no model decides what comes back. It's fast, cheap, and reproducible.
>
> **2 · Capture is curated, structured by code.** Writes are *typed* and validated at the interface; malformed ones are rejected. The consuming tool does any small-scope extraction and hands daimon-memory the result — daimon-memory itself runs **no extraction model**.

The result: semantic, cross-tool memory that runs comfortably on a single node, with recall you can trust to be deterministic.

## Features

- 🔗 **Cross-tool** — one tenant-scoped store behind every assistant.
- 🧠 **Hybrid recall** — PostgreSQL full-text (`tsvector`) **+** Qdrant dense vectors, fused with **Reciprocal Rank Fusion**. Find a record by *meaning* even when no keywords match — **with no LLM in the recall path**.
- 🧱 **Typed, validated capture** — nine canonical record kinds (decision, runbook, incident summary, service topology, known failure mode, remediation pattern, project convention, agent lesson, resource summary) with per-kind required fields, plus per-namespace extensibility.
- ✋ **Curated, not raw** — no raw-transcript dumping; memories come from explicit `remember` calls or by mirroring a tool's own curated writes.
- 🔖 **Addressable** — every record gets a `daimon://{tenant}/{namespace}/{kind}/{id}` URI, a browsable namespace tree, and L0/L1/L2 tiers (abstract → overview → full).
- 🔒 **Tenant isolation** — PostgreSQL Row-Level Security (fail-closed) + explicit predicates.
- ♻️ **Rebuildable index** — Qdrant is disposable; rebuild it from PostgreSQL with one command.
- 📤 **No dual-write** — a transactional **outbox** is drained to Qdrant asynchronously by a singleton indexer.
- 📈 **Scales** — stateless API tier (autoscaling) over a stateful data tier; "design for 3, run 1".
- 🔌 **MCP-native + REST** — `remember` / `recall` / `read` as MCP tools *and* `/v1` REST endpoints.

## Architecture

daimon-memory is a deterministic Rust service — there is **no model in the request path**. The "framework" is its component model and contracts:

```
   your tools  (Claude Code · Codex · Hermes · your agents)
        │   MCP (/mcp)        REST (/v1)
        ▼
   ┌──────────────┐    recall = RRF( Postgres FTS , Qdrant dense )   ← zero-LLM
   │  daimon-mcp  │    store  = validate (control layer) → Postgres + outbox
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
| `daimon-memory-core` | Deterministic, **LLM-free** core: the `MemoryKind` taxonomy, the `daimon://` URI / namespace grammar, the control-layer write validation, and the `ContextMemory` trait. Pure logic, fully unit-tested. |
| `daimon-pg` | PostgreSQL-backed store: validated writes (content-hash dedup, outbox in one transaction) + deterministic full-text recall; tenant-scoped. |
| `daimon-vec` | In-process [fastembed](https://github.com/Anush008/fastembed-rs) **bge-small-en-v1.5** (384-d) embedder + a Qdrant vector store. |
| `daimon-indexer` | Singleton drainer: PostgreSQL outbox → embed → Qdrant upsert/delete. |
| `daimon-mcp` | Stateless server: REST `/v1` + MCP JSON-RPC `/mcp`; **hybrid recall**; degrades gracefully to keyword-only if the vector tier is down. |
| `daimon-cli` (`daimon`) | Operations: `migrate` / `reindex` / `health` / `stats`. |

**Data model** (`migrations/V001__memory_schema.sql`): `records` (canonical), `namespaces` (the tree), `type_registry` (taxonomy + extensibility), `index_outbox` (Postgres → Qdrant).

## Tech stack

**Rust** (edition 2024) · tokio · axum · PostgreSQL (tokio-postgres + deadpool, refinery migrations) · Qdrant (qdrant-client) · fastembed / ONNX Runtime for embeddings · Reciprocal Rank Fusion for hybrid ranking.

## Quick start

There are **two installers**: one for the **server** (this section) and one per **client tool**
([below](#connect-your-tools)). Both are interactive wizards — they explain and prompt for each
setting.

Run the guided server installer — it asks how you want to run daimon-memory, prompts for each
value, writes `.env`, and starts the stack:

```bash
git clone <repo-url> && cd daimon-memory
./install.sh
```

Prefer to do it by hand? Use Docker Compose directly:

```bash
cp .env.example .env      # edit as needed
docker compose up --build
```

Then:

```bash
curl -s localhost:8080/readyz

# store a typed memory
curl -s -XPOST localhost:8080/v1/memory -H 'content-type: application/json' -d '{
  "kind": "decision",
  "namespace": "shared-canonical/architecture/decisions",
  "title": "Adopt Postgres + Qdrant",
  "body": "Use Postgres as the canonical store and Qdrant as a rebuildable vector index.",
  "fields": { "context": "needed a shared memory store", "rationale": "deterministic recall, rebuildable index" }
}'

# recall (hybrid keyword + semantic)
curl -s -XPOST localhost:8080/v1/recall -H 'content-type: application/json' -d '{"query":"how should we store memory"}'
```

> First start downloads the embedding model (~130 MB) once.

### From source

```bash
cargo test --workspace          # build + unit tests
# point the binaries at a running Postgres + Qdrant via env (see Configuration), then:
cargo run --bin daimon -- migrate
cargo run --bin daimon-mcp        # serves :8080  (/v1 + /mcp)
cargo run --bin daimon-indexer    # outbox → Qdrant (separate process)
```

## Connect your tools

Point any MCP-capable assistant at `http://localhost:8080/mcp` (or wherever you host daimon-mcp). It exposes the `remember`, `recall`, and `read` tools.

### Hermes — native memory provider ✅

A first-class Hermes memory provider: **automatic recall** on every turn plus **curated capture** (mirrors Hermes's own memory writes and adds a `daimon_remember` tool). No fork required.

```bash
cd integrations/hermes
./install.sh                 # guided: prompts for endpoint, tenant, namespace, activation
# or non-interactive:
./install.sh --endpoint http://localhost:8080 --activate --yes
```
Uninstall: `./install.sh --uninstall`.

### Claude Code — via MCP

Add daimon-memory as an MCP server (e.g. in `.mcp.json`):

```json
{ "mcpServers": {
    "daimon-memory": {
      "type": "http",
      "url": "http://localhost:8080/mcp",
      "headers": { "X-Daimon-Tenant": "<your-tenant-uuid>" }
} } }
```

### Codex — via MCP

Add it to your Codex MCP configuration:

```toml
[mcp_servers.daimon-memory]
url = "http://localhost:8080/mcp"
transport = "http"
```

> Dedicated installers for Claude Code and Codex (with passive-capture hooks, like the Hermes one) are on the [roadmap](#status--roadmap). Memories live in daimon-memory independently of any tool — uninstalling a tool never deletes your memory.

## Configuration

The server and indexer are configured via environment variables:

| Variable | Purpose | Default |
|----------|---------|---------|
| `DAIMON_MCP_BIND` | Listen address | `0.0.0.0:8080` |
| `PGHOST` `PGPORT` `PGUSER` `PGPASSWORD` `PGDATABASE` | PostgreSQL connection | — |
| `DAIMON_QDRANT_URL` | Qdrant **gRPC** endpoint | `http://127.0.0.1:6334` |
| `DAIMON_DEFAULT_TENANT` | Tenant used when no `X-Daimon-Tenant` header is sent | a fixed dev UUID |
| `RUST_LOG` | Log level | `info` |

**API** — REST: `POST /v1/memory`, `POST /v1/recall`, `GET /v1/read?uri=`, `GET /readyz`, `GET /health`. MCP: `POST /mcp` (`initialize` / `tools/list` / `tools/call`).

## Deployment

- **Docker / Compose** — the included `Dockerfile` builds both server binaries; `docker-compose.yml` runs the full stack.
- **Kubernetes** — daimon-mcp is a stateless `Deployment` (with an HPA); PostgreSQL and Qdrant are `StatefulSet`s. Build the image from the `Dockerfile` (e.g. with kaniko in CI) and apply your manifests.

## Status & roadmap

> ⚠️ **Experimental.** The engine is feature-complete and tested, but APIs may change before a tagged release. Not yet recommended for production.

**Working today:** typed control-layer writes · deterministic keyword recall · semantic recall (bge-small + Qdrant) · hybrid RRF fusion · the outbox→Qdrant indexer · REST + MCP surfaces · ops CLI · the Hermes integration.

**Planned:** bearer-token auth → tenant mapping · a least-privilege DB role so RLS is the active enforcer · streamable-HTTP `/mcp` (SSE) · a **shared identity/persona layer** (one consistent assistant persona across tools) · dedicated Claude Code & Codex installers.

## Contributing

Issues and PRs welcome. `cargo test --workspace` should pass; please keep the core crate free of I/O and model calls (that's what makes recall deterministic). For larger changes, open an issue to discuss the design first.

## License

[MIT](LICENSE).
