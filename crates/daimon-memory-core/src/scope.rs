use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The addressing tuple carried on every read/write (SDS v0.2 §4.1).
///
/// `tenant_id` is the hard isolation boundary (Postgres RLS); the optional fields
/// scope a record within the tenant and map onto namespace path segments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScope {
    pub tenant_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
}

impl ContextScope {
    /// A tenant-only scope (no project/service/run/agent narrowing).
    pub fn tenant(tenant_id: Uuid) -> Self {
        Self {
            tenant_id,
            project_id: None,
            service_id: None,
            run_id: None,
            agent_role: None,
        }
    }
}
