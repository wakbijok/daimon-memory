use crate::error::{MemoryError, Result};
use crate::namespace::Namespace;
use crate::record::MemoryWrite;
use serde_json::Value;

/// The control-layer write contract (SDS v0.2 §4.5) — **deterministic, no LLM**.
///
/// Enforces (all in code, never by model judgment):
/// 1. the namespace grammar (SDS A.2),
/// 2. the universal `title` + `body` are non-empty,
/// 3. every kind-specific required field is present and non-empty (SDS A.1),
/// 4. `importance` ∈ 0..=100 and `confidence` ∈ 0.0..=1.0.
///
/// Returns `Ok(())` or a [`MemoryError::Validation`] / `InvalidNamespace`.
pub fn validate_write(w: &MemoryWrite) -> Result<()> {
    // (1) namespace grammar — parsing IS the validation.
    Namespace::parse(&w.namespace)?;

    // (2) universal fields.
    if w.title.trim().is_empty() {
        return Err(MemoryError::Validation("title must not be empty".into()));
    }
    if w.body.trim().is_empty() {
        return Err(MemoryError::Validation("body must not be empty".into()));
    }

    // (3) kind-specific required fields.
    for field in w.kind.required_fields() {
        match w.fields.get(*field) {
            Some(v) if !is_empty_value(v) => {}
            _ => {
                return Err(MemoryError::Validation(format!(
                    "missing or empty required field '{field}' for kind '{}'",
                    w.kind.as_str()
                )));
            }
        }
    }

    // (4) bounded ranges.
    if w.importance > 100 {
        return Err(MemoryError::Validation(format!(
            "importance must be 0..=100, got {}",
            w.importance
        )));
    }
    if !(0.0..=1.0).contains(&w.confidence) {
        return Err(MemoryError::Validation(format!(
            "confidence must be 0.0..=1.0, got {}",
            w.confidence
        )));
    }

    Ok(())
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::MemoryKind;
    use serde_json::json;

    fn base_decision() -> MemoryWrite {
        MemoryWrite {
            kind: MemoryKind::Decision,
            namespace: "shared-canonical/coding/decisions".into(),
            title: "Use Postgres as canonical store".into(),
            body: "Adopt Postgres + Qdrant split.".into(),
            fields: json!({"context": "needed a store", "rationale": "rebuildable index"})
                .as_object()
                .unwrap()
                .clone(),
            source_refs: vec![],
            tags: vec![],
            importance: 50,
            confidence: 1.0,
        }
    }

    #[test]
    fn valid_decision_passes() {
        assert!(validate_write(&base_decision()).is_ok());
    }

    #[test]
    fn missing_required_field_fails() {
        let mut w = base_decision();
        w.fields.remove("rationale");
        let err = validate_write(&w).unwrap_err();
        assert!(matches!(err, MemoryError::Validation(_)));
    }

    #[test]
    fn empty_required_field_fails() {
        let mut w = base_decision();
        w.fields.insert("rationale".into(), json!("   "));
        assert!(validate_write(&w).is_err());
    }

    #[test]
    fn empty_title_or_body_fails() {
        let mut w = base_decision();
        w.title = "  ".into();
        assert!(validate_write(&w).is_err());
        let mut w2 = base_decision();
        w2.body = "".into();
        assert!(validate_write(&w2).is_err());
    }

    #[test]
    fn bad_namespace_fails() {
        let mut w = base_decision();
        w.namespace = "resources/coding".into(); // superseded vocabulary
        assert!(matches!(validate_write(&w), Err(MemoryError::InvalidNamespace(_))));
    }

    #[test]
    fn out_of_range_importance_and_confidence_fail() {
        let mut w = base_decision();
        w.importance = 200;
        assert!(validate_write(&w).is_err());
        let mut w2 = base_decision();
        w2.confidence = 1.5;
        assert!(validate_write(&w2).is_err());
    }
}
