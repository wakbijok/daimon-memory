-- daimon-memory — initial schema (standalone deployment, dedicated database).
-- In the dAImon monorepo this corresponds to the SDS "V017" delta; standalone on a
-- fresh DB it is V001. Postgres 17 (gen_random_uuid is in core; no pgcrypto needed).
--
-- Principles enforced here (SDS v0.2):
--  - Postgres is the CANONICAL source of truth; Qdrant is a rebuildable index.
--  - Tenant isolation via RLS (fail-closed: unset GUC -> zero rows).
--  - Recall ranks on the L0 `abstract`; full content is `body` (L2). (L1 overview +
--    a separate chunks table are deferred — see SDS §3.3; MVP folds tiers onto records.)

CREATE SCHEMA IF NOT EXISTS memory;

-- ---------------------------------------------------------------------------
-- type_registry — the canonical taxonomy + the extensibility surface (SDS §3.8).
-- Canonical kinds are global (tenant_id NULL); consumers may register custom
-- per-tenant types without a migration.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory.type_registry (
    record_type   TEXT        NOT NULL,
    -- zero-uuid = global canonical type (PK can't use COALESCE; a NOT NULL default does the job).
    tenant_id     UUID        NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000'::uuid,
    json_schema   JSONB       NOT NULL DEFAULT '{}'::jsonb,
    write_mode    TEXT        NOT NULL CHECK (write_mode IN ('append','update')),
    is_canonical  BOOLEAN     NOT NULL DEFAULT FALSE,
    version       INT         NOT NULL DEFAULT 1,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (record_type, tenant_id)
);

INSERT INTO memory.type_registry (record_type, write_mode, is_canonical) VALUES
    ('decision','append',TRUE),
    ('runbook','update',TRUE),
    ('incident_summary','append',TRUE),
    ('service_topology','update',TRUE),
    ('known_failure_mode','append',TRUE),
    ('remediation_pattern','append',TRUE),
    ('project_convention','update',TRUE),
    ('agent_lesson','append',TRUE),
    ('resource_summary','update',TRUE)
ON CONFLICT DO NOTHING;

-- ---------------------------------------------------------------------------
-- records — canonical content (SDS §3.2). One row per memory.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory.records (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       UUID        NOT NULL,
    namespace       TEXT        NOT NULL,
    owner_user_id   UUID,
    agent_id        TEXT,
    kind            TEXT        NOT NULL,
    title           TEXT        NOT NULL,
    body            TEXT        NOT NULL,          -- L2 full content
    abstract        TEXT        NOT NULL DEFAULT '', -- L0 (embedded/ranked); raw-first-N when distil off
    fields          JSONB       NOT NULL DEFAULT '{}'::jsonb,
    source_refs     JSONB       NOT NULL DEFAULT '[]'::jsonb,
    tags            TEXT[]      NOT NULL DEFAULT '{}',
    importance      SMALLINT    NOT NULL DEFAULT 0  CHECK (importance BETWEEN 0 AND 100),
    confidence      REAL        NOT NULL DEFAULT 1.0 CHECK (confidence BETWEEN 0.0 AND 1.0),
    content_sha     TEXT        NOT NULL,           -- server-computed (RFC 8785 JCS), dedup key
    schema_version  INT         NOT NULL DEFAULT 1,
    status          TEXT        NOT NULL DEFAULT 'active'
                        CHECK (status IN ('active','superseded','reversed','forgotten')),
    supersedes_id   UUID        REFERENCES memory.records(id) ON DELETE SET NULL,
    reverses_id     UUID        REFERENCES memory.records(id) ON DELETE SET NULL,
    uri_path        TEXT        NOT NULL,           -- daimon://<tenant>/<namespace>/<kind>/<id>
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS records_tenant_ns_idx     ON memory.records (tenant_id, namespace);
CREATE INDEX IF NOT EXISTS records_tenant_kind_idx   ON memory.records (tenant_id, kind);
CREATE INDEX IF NOT EXISTS records_created_idx        ON memory.records (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS records_tags_gin          ON memory.records USING GIN (tags);
CREATE INDEX IF NOT EXISTS records_fields_gin        ON memory.records USING GIN (fields);
-- Deterministic full-text recall fallback (BM25-ish) on title+abstract+body.
CREATE INDEX IF NOT EXISTS records_fts_gin           ON memory.records
    USING GIN (to_tsvector('english', coalesce(title,'') || ' ' || coalesce(abstract,'') || ' ' || coalesce(body,'')));
-- Dedup: one active row per (tenant, content hash).
CREATE UNIQUE INDEX IF NOT EXISTS records_dedup_active
    ON memory.records (tenant_id, content_sha) WHERE status = 'active';

-- ---------------------------------------------------------------------------
-- namespaces — the browseable URI tree (SDS §3.5).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory.namespaces (
    tenant_id     UUID NOT NULL,
    path          TEXT NOT NULL,        -- e.g. 'shared-canonical/coding/decisions'
    owner_user_id UUID,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, path)
);

-- ---------------------------------------------------------------------------
-- index_outbox — transactional outbox PG -> Qdrant (SDS §7.3). Drained by the
-- singleton indexer. Never a synchronous dual-write.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS memory.index_outbox (
    id           BIGSERIAL   PRIMARY KEY,
    record_id    UUID        NOT NULL REFERENCES memory.records(id) ON DELETE CASCADE,
    tenant_id    UUID        NOT NULL,
    op           TEXT        NOT NULL CHECK (op IN ('upsert','delete')),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ,
    attempts     INT         NOT NULL DEFAULT 0,
    locked_by    TEXT
);
CREATE INDEX IF NOT EXISTS outbox_unprocessed_idx
    ON memory.index_outbox (created_at) WHERE processed_at IS NULL;

-- ---------------------------------------------------------------------------
-- Row-Level Security — tenant isolation, FAIL-CLOSED.
-- The app sets `SET app.tenant_id = '<uuid>'` per session; unset -> NULL -> no rows.
-- ---------------------------------------------------------------------------
ALTER TABLE memory.records    ENABLE ROW LEVEL SECURITY;
ALTER TABLE memory.namespaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE memory.index_outbox ENABLE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS records_tenant_isolation ON memory.records;
CREATE POLICY records_tenant_isolation ON memory.records
    USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);

DROP POLICY IF EXISTS namespaces_tenant_isolation ON memory.namespaces;
CREATE POLICY namespaces_tenant_isolation ON memory.namespaces
    USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);

DROP POLICY IF EXISTS outbox_tenant_isolation ON memory.index_outbox;
CREATE POLICY outbox_tenant_isolation ON memory.index_outbox
    USING (tenant_id = current_setting('app.tenant_id', true)::uuid)
    WITH CHECK (tenant_id = current_setting('app.tenant_id', true)::uuid);
-- NOTE: type_registry is intentionally NOT RLS-restricted for canonical (global) rows;
-- per-tenant custom types are filtered in-query. Tighten in a later migration if needed.
