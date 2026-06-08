//! `daimon-memory-core` - the deterministic, LLM-free core of **daimon-memory**.
//!
//! This crate holds everything that must be *enforced by code, not LLM discretion*:
//! the typed taxonomy ([`MemoryKind`]), the namespace and
//! URI grammar ([`Namespace`], [`MemoryUri`]), the control-layer write validation
//! ([`validate_write`]), and the [`ContextMemory`] trait every backend implements.
//!
//! It performs **no I/O and calls no model** - pure logic, fully unit-testable.
//! Postgres / Qdrant backends live in sibling crates and depend on this one.

pub mod error;
pub mod scope;
pub mod kind;
pub mod namespace;
pub mod record;
pub mod validate;
pub mod memory;

pub use error::{MemoryError, Result};
pub use kind::{MemoryKind, WriteMode};
pub use memory::ContextMemory;
pub use namespace::{MemoryUri, Namespace, NamespaceRoot, Tier, strip_tenant_segment};
pub use record::{MemoryHit, MemoryRecord, MemoryWrite, RecallFilters};
pub use scope::ContextScope;
pub use validate::validate_write;
