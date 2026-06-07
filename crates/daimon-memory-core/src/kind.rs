use crate::error::{MemoryError, Result};
use serde::{Deserialize, Serialize};

/// Whether a record type appends a new immutable entry or updates current state.
/// Control-layer dispatch (SDS A.1) - never caller-selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Immutable log: each write is a new record; "reversal" appends a new entry.
    Append,
    /// Current-state: a new write supersedes the prior record for the same subject.
    Update,
}

/// The canonical typed-record taxonomy (SDS A.1; anchor proposal §9 `MemoryKind`).
///
/// The set is closed in code so the control layer can validate required fields and
/// pick the write-mode deterministically. Custom consumer types are handled by the
/// extensibility registry (server-side), not this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Decision,
    Runbook,
    IncidentSummary,
    ServiceTopology,
    KnownFailureMode,
    RemediationPattern,
    ProjectConvention,
    AgentLesson,
    ResourceSummary,
}

impl MemoryKind {
    /// All canonical kinds (registry iteration / validation self-tests).
    pub const ALL: [MemoryKind; 9] = [
        MemoryKind::Decision,
        MemoryKind::Runbook,
        MemoryKind::IncidentSummary,
        MemoryKind::ServiceTopology,
        MemoryKind::KnownFailureMode,
        MemoryKind::RemediationPattern,
        MemoryKind::ProjectConvention,
        MemoryKind::AgentLesson,
        MemoryKind::ResourceSummary,
    ];

    /// snake_case wire form (also the URI `record_type` segment).
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Decision => "decision",
            MemoryKind::Runbook => "runbook",
            MemoryKind::IncidentSummary => "incident_summary",
            MemoryKind::ServiceTopology => "service_topology",
            MemoryKind::KnownFailureMode => "known_failure_mode",
            MemoryKind::RemediationPattern => "remediation_pattern",
            MemoryKind::ProjectConvention => "project_convention",
            MemoryKind::AgentLesson => "agent_lesson",
            MemoryKind::ResourceSummary => "resource_summary",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        MemoryKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| MemoryError::UnknownKind(s.to_string()))
    }

    /// Write-mode per kind (SDS A.1). Append = immutable log; Update = current-state.
    pub fn write_mode(&self) -> WriteMode {
        match self {
            MemoryKind::Decision
            | MemoryKind::IncidentSummary
            | MemoryKind::KnownFailureMode
            | MemoryKind::RemediationPattern
            | MemoryKind::AgentLesson => WriteMode::Append,
            MemoryKind::Runbook
            | MemoryKind::ServiceTopology
            | MemoryKind::ProjectConvention
            | MemoryKind::ResourceSummary => WriteMode::Update,
        }
    }

    /// Required structured fields the control layer enforces at write-time, in
    /// addition to the universal `title` + `body` (validated in [`crate::validate`]).
    pub fn required_fields(&self) -> &'static [&'static str] {
        match self {
            MemoryKind::Decision => &["context", "rationale"],
            MemoryKind::Runbook => &["steps"],
            MemoryKind::IncidentSummary => &["impact", "resolution"],
            MemoryKind::ServiceTopology => &["service", "dependencies"],
            MemoryKind::KnownFailureMode => &["symptom", "cause"],
            MemoryKind::RemediationPattern => &["problem", "fix"],
            MemoryKind::ProjectConvention => &["rule"],
            MemoryKind::AgentLesson => &["lesson"],
            MemoryKind::ResourceSummary => &["source"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip_all() {
        for k in MemoryKind::ALL {
            assert_eq!(MemoryKind::parse(k.as_str()).unwrap(), k);
        }
    }

    #[test]
    fn parse_unknown_errors() {
        assert!(matches!(
            MemoryKind::parse("not_a_kind"),
            Err(MemoryError::UnknownKind(_))
        ));
    }

    #[test]
    fn write_modes_are_assigned() {
        assert_eq!(MemoryKind::Decision.write_mode(), WriteMode::Append);
        assert_eq!(MemoryKind::ServiceTopology.write_mode(), WriteMode::Update);
    }

    #[test]
    fn every_kind_has_required_fields() {
        for k in MemoryKind::ALL {
            assert!(!k.required_fields().is_empty(), "{} has no required fields", k.as_str());
        }
    }

    #[test]
    fn serde_is_snake_case() {
        let j = serde_json::to_string(&MemoryKind::IncidentSummary).unwrap();
        assert_eq!(j, "\"incident_summary\"");
    }
}
