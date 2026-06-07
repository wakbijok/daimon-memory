use crate::error::Result;
use crate::namespace::MemoryUri;
use crate::record::{MemoryHit, MemoryRecord, MemoryWrite, RecallFilters};
use crate::scope::ContextScope;
use async_trait::async_trait;

/// The backend-agnostic memory contract (anchor proposal §9; SDS §4.1).
///
/// Implementations (Postgres + Qdrant in sibling crates) MUST keep these invariants:
/// - `find` is **deterministic and LLM-free** (embedding + keyword + filters);
/// - `store` runs the control-layer validation before any write;
/// - every operation is tenant-scoped via [`ContextScope`] (RLS fail-closed).
#[async_trait]
pub trait ContextMemory: Send + Sync {
    /// Validate + persist a record; returns its canonical URI.
    async fn store(&self, scope: &ContextScope, write: MemoryWrite) -> Result<MemoryUri>;

    /// Deterministic recall: ranked hits (L0 abstract + URI), no model call.
    async fn find(
        &self,
        scope: &ContextScope,
        query: &str,
        filters: &RecallFilters,
    ) -> Result<Vec<MemoryHit>>;

    /// Lazy full-content fetch by URI (L2 expansion).
    async fn read(&self, scope: &ContextScope, uri: &MemoryUri) -> Result<MemoryRecord>;

    /// List record URIs under a namespace path prefix.
    async fn list(&self, scope: &ContextScope, prefix: &str) -> Result<Vec<MemoryUri>>;

    /// Delete a record (deliberate, audited at the call site).
    async fn forget(&self, scope: &ContextScope, uri: &MemoryUri) -> Result<()>;
}
