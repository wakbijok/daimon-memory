<div align="center">

# daimon-memory

**Persistent typed memory for AI agents. Deterministic, LLM-free recall across every tool.**

![status](https://img.shields.io/badge/status-experimental-orange) ![license](https://img.shields.io/badge/license-MIT-blue) ![rust](https://img.shields.io/badge/rust-edition%202024-orange)

</div>

---

## What it is

daimon-memory is a self-hostable memory backend for AI agents. It stores typed records (decisions, lessons, incidents, reminders, and more) in PostgreSQL as the canonical source, with Qdrant as a rebuildable semantic index. Recall is hybrid keyword + vector search fused by Reciprocal Rank Fusion -- no LLM in the recall path, so it is fast, cheap, and reproducible.

It is single-subject by design: one memory space per tenant, shared across every tool that connects to it. Establish a fact in Claude Code; recall it in Hermes. The same persona and operating discipline load into every agent at session start.

Full documentation: **[wakbijok.uk/man/daimon-memory](https://wakbijok.uk/man/daimon-memory/)**

---

## Quick start

**1. Clone and configure**

```bash
git clone https://github.com/wakbijok/daimon-memory && cd daimon-memory
cp .env.example .env
```

Open `.env` and set at minimum:

```
DAIMON_PG_PASSWORD=<something-strong>
DAIMON_API_KEY=<openssl rand -hex 32>
```

**2. Start the stack**

```bash
./install.sh
```

Pass `--yes` to accept all defaults non-interactively. The installer writes `.env`, brings up the Docker Compose stack, seeds the default protocols, and offers to run the persona wizard.

> First start downloads the embedding model (about 130 MB) once.

**3. Smoke test**

```bash
# Health
curl -s localhost:8080/readyz

# Store a memory
curl -s -XPOST localhost:8080/v1/memory \
  -H 'content-type: application/json' \
  -H 'Authorization: Bearer <your-api-key>' \
  -d '{
    "kind": "decision",
    "namespace": "resources/architecture/decisions",
    "title": "Adopt Postgres + Qdrant",
    "body": "Postgres is the canonical store; Qdrant is a rebuildable vector index.",
    "fields": { "context": "needed shared memory", "rationale": "deterministic recall, rebuildable index" }
  }'

# Recall
curl -s -XPOST localhost:8080/v1/recall \
  -H 'content-type: application/json' \
  -H 'Authorization: Bearer <your-api-key>' \
  -d '{"query":"how should we store memory"}'
```

**No API key set?** Omit the `Authorization` header. The API is open when `DAIMON_API_KEY` is unset -- fine on localhost, not on a shared network.

---

## Documentation

Everything beyond the smoke test lives at **[wakbijok.uk/man/daimon-memory](https://wakbijok.uk/man/daimon-memory/):**

- Installation detail (Docker Compose, Kubernetes, from source, AVX2 notes)
- Core concepts: memory kinds, namespace grammar, `daimon://` URIs, the persona + discipline system layer
- Usage: the REST API, MCP tool surface, the `daimon` CLI (ops, backup/restore, persona authoring)
- Integration guides for Claude Code, Codex, and Hermes
- Upgrade notes and observability

---

## Integrations

daimon-memory works with Claude Code, Codex, and Hermes today. Each integration ships its own installer under `integrations/<tool>/install.sh`.

A few things worth knowing before you wire them up:

- **Claude Code plugin** requires a local absolute path for the marketplace add step: the installer prints the exact command (`/plugin marketplace add <abs-path>/integrations/claude-code`).
- **MCP endpoint** (`/mcp`) speaks the synchronous JSON-RPC subset of MCP, not streamable-HTTP/SSE. Hosts that require SSE can use the REST hooks at `/v1` instead.
- Memories live in daimon independently of any tool, so uninstalling a tool never deletes your data.

---

## Contributing

Issues and PRs welcome. `cargo test --workspace` should pass; please keep the core crate free of I/O and model calls (that is what makes recall deterministic). For larger changes, open an issue to discuss the design first.

## License

[MIT](LICENSE).
