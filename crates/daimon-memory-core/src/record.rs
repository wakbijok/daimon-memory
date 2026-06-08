use crate::kind::MemoryKind;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

fn default_confidence() -> f32 {
    1.0
}

/// A write request from a consumer (SDS §4.1 `MemoryWrite`).
///
/// The AI proposes `kind`, `title`, `body`, structured `fields`, and a target
/// `namespace`; the control layer ([`crate::validate_write`]) disposes everything
/// deterministic (schema, placement authorization, write-mode, dedup, redaction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryWrite {
    pub kind: MemoryKind,
    /// Target namespace string, validated against the grammar (SDS A.2).
    pub namespace: String,
    pub title: String,
    pub body: String,
    /// Kind-specific structured fields (the per-type required-field contract).
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// References to other planes (e.g. `vault://...`, audit id) - NOT secret values.
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Advisory rerank-boost band, 0..=100 (SDS A.5).
    #[serde(default)]
    pub importance: u8,
    /// Guard-sensitive confidence 0.0..=1.0 (proposal §10.5).
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// A stored record returned by `read` (SDS §3.2 canonical row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub namespace: String,
    pub kind: MemoryKind,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub fields: Map<String, Value>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub importance: u8,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The canonical `daimon://` address as a string.
    pub uri: String,
}

/// A ranked recall result (SDS §4.4). Carries the L0 abstract + URI; full content
/// is a lazy `read(uri)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub uri: String,
    pub kind: MemoryKind,
    pub title: String,
    /// L0 abstract (or raw-turn-first-N-chars when distillation is off).
    #[serde(rename = "abstract")]
    pub abstract_: String,
    pub score: f32,
    /// Record importance (0-100); advisory rerank-boost band, exposed for fusion weighting.
    #[serde(default)]
    pub importance: u8,
}

/// Deterministic recall filters (SDS §4.4) - applied as DB predicates, no LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallFilters {
    #[serde(default)]
    pub kind: Option<MemoryKind>,
    /// Namespace path prefix (e.g. `shared-canonical/coding`).
    #[serde(default)]
    pub namespace_prefix: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

impl Default for RecallFilters {
    fn default() -> Self {
        Self {
            kind: None,
            namespace_prefix: None,
            project_id: None,
            since: None,
            limit: default_limit(),
        }
    }
}
