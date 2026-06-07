# daimon-memory

**The context plane for AI tools** — a shared, cross-tool memory engine where *recall is
deterministic (zero-LLM)* and *capture is curated*. One backend for Claude Code, Codex, Hermes,
and daimon's own AIOps agents.

> Scope: **daimon-memory only**, not the whole daimon. It's built as a standalone component
> first; daimon (the agent) consumes it later.

---

## The problem

AI assistants are getting good memories — but each one keeps its *own*, and they don't talk to
each other:

- **Siloed & fragmented.** Claude Code remembers one thing, Hermes another, Codex a third. Switch
  tools and you lose the thread. Your "second brain" is shattered across four apps.
- **Manual & drifting.** Hand-curated Markdown (`CLAUDE.md`, `SOUL.md`, notes) doesn't scale, goes
  stale, and has to be copied between tools.
- **Heavy "smart" memory is too heavy.** Full memory systems (e.g. OpenViking) are genuinely
  powerful, but they bundle a large embedded VLM to *extract* memories from raw conversation.
  That pushes the inference requirement so high it can't run comfortably on a homelab GPU — and if
  one component of an agent needs a frontier model, the whole agent does.

What's needed is a **lightweight, shared memory backend** that any tool can read and write, where
the expensive part (a big model) is *not* in the loop.

## Introduction

daimon-memory is that backend — a single Rust service backed by **Postgres** (the canonical store)
and **Qdrant** (a rebuildable semantic index), exposed over **MCP** and **REST**, and consumed by
every tool you use.

Its design follows one framing: **stores knowledge; daimon decides; tools act.** The memory plane
holds durable, typed knowledge; the agent reasons over it; the tools carry out work. Two principles
keep it light and trustworthy:

1. **Per-component LLM minimization.** Recall is **deterministic and zero-LLM** (embeddings +
   keyword + filters). Capture is **curated at the edge** — the consuming tool does its own
   small-scope extraction and hands us the result; daimon-memory runs *no* extraction model. The
   memory tier adds ≈0 to aggregate inference cost.
2. **Deterministic facts via control.** Structure is enforced by **code at the interface**, not by
   LLM discretion. Writes are typed and validated; malformed ones are rejected — so recall returns
   facts with a known shape, not whatever a model felt like emitting.

## Features & capabilities

- **Cross-tool, shared memory** — one tenant-scoped store behind every tool.
- **Deterministic hybrid recall** — Postgres full-text (`tsvector`) + Qdrant dense vectors fused
  with **Reciprocal Rank Fusion**. Semantic recall by *meaning* (a paraphrased query finds the
  record even when no words match) with **no LLM in the recall path**.
- **Typed, control-enforced capture** — 9 canonical record kinds (decision, runbook,
  incident_summary, service_topology, known_failure_mode, remediation_pattern, project_convention,
  agent_lesson, resource_summary) with per-kind required fields, plus per-namespace extensibility.
- **Curated, not raw** — no raw-turn dumping; capture comes from explicit `remember` calls or by
  mirroring a tool's own curated memory writes.
- **Addressable** — every record has a `daimon://{tenant}/{namespace}/{kind}/{id}` URI; a
  browsable namespace tree; L0/L1/L2 tiers (abstract → overview → full).
- **Tenant isolation** — Postgres Row-Level Security (fail-closed) + explicit predicates.
- **Rebuildable index** — Qdrant is disposable; `daimon reindex` reconstructs it from Postgres.
- **No dual-write** — a transactional **outbox** in Postgres is drained asynchronously to Qdrant by
  a singleton indexer.
- **Scales** — stateless API tier (HPA) over a stateful data tier (Postgres + Qdrant StatefulSets);
  "design for 3, run 1".
- **MCP-native + REST** — `remember` / `recall` / `read` as MCP tools and as `/v1` REST endpoints.

## Architecture & framework

daimon-memory is a deterministic Rust service, **not** an agentic-framework app — there is no model
in the request path. The "framework" is the component model + contracts:

```
 consuming tools (Claude Code · Codex · Hermes · AIOps)
        │  MCP (/mcp)        REST (/v1)
        ▼
   ┌──────────────┐   recall = RRF( Postgres FTS , Qdrant dense )   [zero-LLM]
   │  daimon-mcp  │   store  = validate (control layer) → Postgres + outbox
   └──────┬───────┘
          │ ContextMemory trait
   ┌──────▼───────┐        ┌───────────────┐       ┌──────────────┐
   │  daimon-pg   │        │  daimon-vec   │◄──────│ daimon-indexer│
   │  Postgres    │        │ Qdrant + bge  │  embed │  outbox drain │
   │ (canonical)  │        │ (rebuildable) │       │  (singleton)  │
   └──────────────┘        └───────────────┘       └──────────────┘
```

Crates (Rust workspace, edition 2024):

| Crate | Role |
|-------|------|
| `daimon-memory-core` | Deterministic, **LLM-free** core: `MemoryKind` taxonomy, `Namespace`/`MemoryUri` grammar, the control-layer `validate_write`, the `ContextMemory` trait. Pure logic, fully unit-tested. |
| `daimon-pg` | Postgres-backed `ContextMemory`: validated store (content-hash dedup, outbox in one txn) + deterministic full-text recall; tenant-scoped (RLS GUC + explicit predicate). |
| `daimon-vec` | In-process **fastembed bge-small-en-v1.5** (384-d) embedder + Qdrant `VectorStore` (tenant-filtered search). |
| `daimon-indexer` | Singleton outbox drainer: `memory.index_outbox` → embed → Qdrant upsert/delete. |
| `daimon-mcp` | Stateless server: REST `/v1` + MCP JSON-RPC `/mcp`; **hybrid RRF recall**; graceful degrade to keyword-only. |
| `daimon-cli` (`daimon`) | Ops: `migrate` / `reindex` / `health` / `stats`. |

Data model: `memory.records` (canonical), `memory.namespaces` (the tree), `memory.type_registry`
(taxonomy + extensibility), `memory.index_outbox` (PG→Qdrant). Full schema in
`migrations/V001__memory_schema.sql`.

## Tech stack

- **Rust** (edition 2024) · **tokio** · **axum 0.8** (HTTP) · **async-trait**
- **Postgres** via **tokio-postgres** + **deadpool-postgres**; migrations via **refinery**
- **Qdrant** via **qdrant-client 1.18**; embeddings via **fastembed 5** (bge-small, ONNX/`ort`)
- Recall: Postgres `tsvector` (keyword) + Qdrant cosine (dense) + **RRF** fusion
- **sha2** (content dedup) · **serde** / **serde_json**
- Deploy: **k3s** + **ArgoCD** (GitOps) + **SealedSecrets** + **kaniko** build → **glcr** registry

## Quick start (local)

```bash
cargo test --workspace            # build + unit tests

# run against a Postgres + Qdrant (e.g. port-forwarded from the cluster):
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER=daimon PGDATABASE=daimon_memory PGPASSWORD=...
export DAIMON_QDRANT_URL=http://127.0.0.1:6334
./target/debug/daimon migrate     # apply schema
./target/debug/daimon-mcp         # serves :8080  (/v1 + /mcp)
./target/debug/daimon-indexer     # outbox → Qdrant (separate process)

# smoke test
curl -s localhost:8080/readyz
curl -s -XPOST localhost:8080/v1/recall -H 'content-type: application/json' -d '{"query":"..."}'
```

## Install into your tools

> All paths point a tool at a running daimon-mcp via `DAIMON_ENDPOINT` (e.g. the in-cluster
> LoadBalancer, or `http://127.0.0.1:18080` for a local instance). Detailed guides live in
> **daimon-docs** (`daimon-memory/integrations/`).

### Hermes ✅ (built + tested)

A native Hermes **memory provider** — automatic recall (`prefetch`) + curated capture
(`on_memory_write` mirror + a `daimon_remember` tool). No Hermes fork.

```bash
cd integrations/hermes
./install.sh --endpoint http://10.100.30.27               # install + configure (.env), not activated
./install.sh --endpoint http://10.100.30.27 --activate    # also switch Hermes to daimon
hermes memory status                                       # provider: daimon
```
Uninstall: `./install.sh --uninstall` then `hermes memory setup openviking` (or `hermes memory off`).

### Claude Code 🔜 (MCP — works manually today; installer planned)

daimon-mcp speaks MCP, so register it as an MCP server. Add to your Claude Code MCP config:

```json
{ "mcpServers": {
    "daimon-memory": {
      "type": "http",
      "url": "http://10.100.30.27/mcp",
      "headers": { "X-Daimon-Tenant": "<tenant-uuid>" }
} } }
```
This exposes `remember` / `recall` / `read`. A dedicated installer (MCP + hooks-based passive
capture) is on the roadmap.

### Codex 🔜 (MCP — works manually today; installer planned)

Codex consumes MCP servers; add daimon-mcp to its MCP config (e.g. `~/.codex/config.toml`):

```toml
[mcp_servers.daimon-memory]
url = "http://10.100.30.27/mcp"
transport = "http"
```
Recall-only is fine — Codex can query daimon-memory even without a capture hook. Installer planned.

### Uninstall (general)

Remove the tool's MCP/provider config entry and restart it. The Hermes installer has an explicit
`--uninstall`. No data is touched — memories live in daimon-memory, independent of any tool.

## Configuration

Server (`daimon-mcp` / `daimon-indexer`), via env:

| var | purpose | default |
|-----|---------|---------|
| `DAIMON_MCP_BIND` | listen address | `0.0.0.0:8080` |
| `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE` | Postgres (libpq) | — |
| `DAIMON_QDRANT_URL` | Qdrant **gRPC** endpoint | `http://127.0.0.1:6334` |
| `DAIMON_DEFAULT_TENANT` | tenant when no `X-Daimon-Tenant` header | dev tenant |
| `RUST_LOG` | log level | `info` |

API surface: REST `POST /v1/memory`, `POST /v1/recall`, `GET /v1/read?uri=`, `GET /readyz`,
`GET /health`; MCP `POST /mcp` (`initialize` / `tools/list` / `tools/call` → remember/recall/read).

## Deployment

GitOps manifests live in the homelab repo at `k8s-homelab-gitops/workloads/daimon-memory/` (ArgoCD).
The data tier (Postgres + Qdrant) deploys standalone; the `daimon-mcp` image is built in-cluster by
a kaniko Job → `glcr.wakbijok.uk/daimon/daimon-memory/daimon-mcp`. See
`daimon-docs/daimon-memory/` for the SDS, deployment notes, and integration guides.

## Status & roadmap

**Phase 1 — feature-complete** (proven live against the deployed data tier): typed control-layer
store, deterministic keyword recall, semantic recall (bge-small + Qdrant), hybrid RRF fusion, the
outbox→Qdrant indexer, REST + MCP surfaces, ops CLI, and the Hermes integration.

Next: in-cluster image + rollout (in progress) · bearer-auth → tenant · non-superuser DB role so
RLS is the active enforcer · streamable-HTTP `/mcp` SSE · **persona/soul layer** (one shared
identity across tools — see `daimon-docs/daimon-memory/proposals/persona-soul-layer.md`) · Claude
Code + Codex installers.

## Docs & links

- Spec: `daimon-docs/daimon-memory/sds/` · Proposals: `daimon-docs/daimon-memory/proposals/`
- Integrations: `daimon-docs/daimon-memory/integrations/`
- Repo: `git.wakbijok.uk/daimon/daimon-memory` · Docs: `git.wakbijok.uk/daimon/daimon-docs`

## License

MIT.
