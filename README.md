# daimon-memory

The **context plane** of the daimon ecosystem — a standalone, independently-deployable
cross-tool memory engine. A shared, queryable, semantic memory backend for Claude Code,
Codex, Hermes, and daimon's own AIOps agents.

> Scope: **daimon-memory only**, not the whole daimon. Built as a separate project first;
> daimon consumes it later.

## Design principles (SDS v0.2)

- **Recall is deterministic / zero-LLM** — embedding + keyword + filters; never gated on a model.
- **Capture is LLM-curated-per-protocol at the edge** — no big embedded VLM; every component's
  LLM task is small-scoped (the memory tier adds ~0 to aggregate inference).
- **Structure is control-enforced at the interface** — the typed schema is validated in code,
  not left to LLM discretion (protocol-trust drifts).
- **Stateless API tier + stateful data tier** — `daimon-mcp` scales via HPA; durability lives in
  the Postgres (canonical) + Qdrant (rebuildable index) StatefulSets.

Full specification: `daimon-docs/daimon-memory/sds/daimon-memory-sds.md`
(`git.wakbijok.uk/daimon/daimon-docs`).

## Workspace

| Crate / dir | Role | Status |
|-------------|------|--------|
| `crates/daimon-memory-core` | deterministic, LLM-free core: types, taxonomy, namespace/URI grammar, control-layer validation, `ContextMemory` trait | ✅ green, unit-tested |
| `crates/daimon-mcp` | cross-tool server: streamable-HTTP `/mcp` + REST `/v1`; stateless | 🟡 skeleton (health + control-layer validation; persistence next) |
| `migrations/V001__memory_schema.sql` | Postgres schema (records, namespaces, type_registry, outbox, RLS) | ✅ |
| `crates/daimon-pg` (next) | Postgres-backed `ContextMemory` (store + FTS recall) | ⏳ |
| `crates/daimon-cli` (next) | ops: migrate / reindex / backup / admin | ⏳ |

## Build & test

```
cargo test --workspace
```

## Deploy

GitOps manifests live in the homelab repo at `k8s-homelab-gitops/workloads/daimon-memory/`
(ArgoCD). The data tier (Postgres + Qdrant) deploys standalone; the `daimon-mcp` image is
built by CI (`.gitlab-ci.yml`, kaniko → `glcr.wakbijok.uk/daimon/daimon-memory/daimon-mcp`)
and the app `Deployment` is flipped from `replicas: 0` once the image exists.

## Status

Phase 1 (autonomous build, 2026-06-07): core + schema + control-layer + server skeleton are
green; Postgres store + deterministic recall wiring is the next slice.
