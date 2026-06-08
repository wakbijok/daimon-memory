use thiserror::Error;

/// Errors surfaced by the daimon-memory core and backends.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// Control-layer write validation failed (missing/empty required field, etc.).
    #[error("validation error: {0}")]
    Validation(String),
    /// A record or path was not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Namespace string violated the grammar.
    #[error("invalid namespace: {0}")]
    InvalidNamespace(String),
    /// `daimon://` URI violated the grammar.
    #[error("invalid uri: {0}")]
    InvalidUri(String),
    /// Unknown / unregistered memory kind.
    #[error("unknown memory kind: {0}")]
    UnknownKind(String),
    /// A backend (Postgres/Qdrant/embedder) failure, opaque to the core.
    #[error("backend error: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;
