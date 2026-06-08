use crate::error::{MemoryError, Result};
use crate::kind::MemoryKind;
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Namespace root. Reserved roots plus `<consumer>-private`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceRoot {
    /// `shared-canonical/` - the shared team brain (control-gated writes).
    SharedCanonical,
    /// `<consumer>-private/` - per-consumer scratch (e.g. `izu-private`).
    ConsumerPrivate(String),
    /// `session/` - ephemeral, TTL'd.
    Session,
}

impl NamespaceRoot {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "shared-canonical" => Ok(NamespaceRoot::SharedCanonical),
            "session" => Ok(NamespaceRoot::Session),
            other => {
                if let Some(consumer) = other.strip_suffix("-private") {
                    if is_consumer(consumer) {
                        return Ok(NamespaceRoot::ConsumerPrivate(consumer.to_string()));
                    }
                }
                Err(MemoryError::InvalidNamespace(format!("bad root: {other}")))
            }
        }
    }
}

impl fmt::Display for NamespaceRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NamespaceRoot::SharedCanonical => write!(f, "shared-canonical"),
            NamespaceRoot::ConsumerPrivate(c) => write!(f, "{c}-private"),
            NamespaceRoot::Session => write!(f, "session"),
        }
    }
}

/// A validated namespace: `root "/" segment ( "/" segment )*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    pub root: NamespaceRoot,
    pub path: Vec<String>,
}

impl Namespace {
    pub fn parse(s: &str) -> Result<Self> {
        let mut parts = s.split('/');
        let root = parts
            .next()
            .ok_or_else(|| MemoryError::InvalidNamespace("empty".into()))?;
        let root = NamespaceRoot::parse(root)?;
        let mut path = Vec::new();
        for seg in parts {
            if !is_valid_segment(seg) {
                return Err(MemoryError::InvalidNamespace(format!("bad segment: {seg}")));
            }
            path.push(seg.to_string());
        }
        if path.is_empty() {
            return Err(MemoryError::InvalidNamespace(format!(
                "namespace needs at least one segment after root: {s}"
            )));
        }
        Ok(Namespace { root, path })
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.root)?;
        for seg in &self.path {
            write!(f, "/{seg}")?;
        }
        Ok(())
    }
}

impl FromStr for Namespace {
    type Err = MemoryError;
    fn from_str(s: &str) -> Result<Self> {
        Namespace::parse(s)
    }
}

/// Recall granularity tier. Recall ranks on L0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    L0,
    L1,
    L2,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::L0 => "L0",
            Tier::L1 => "L1",
            Tier::L2 => "L2",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "L0" => Ok(Tier::L0),
            "L1" => Ok(Tier::L1),
            "L2" => Ok(Tier::L2),
            o => Err(MemoryError::InvalidUri(format!("bad tier: {o}"))),
        }
    }
}

/// A `daimon://{tenant}/{namespace}/{record_type}/{record_id}[#tier]` address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryUri {
    pub tenant_id: Uuid,
    pub namespace: Namespace,
    pub record_type: MemoryKind,
    pub record_id: Uuid,
    /// Default L1 when omitted.
    pub tier: Tier,
}

impl MemoryUri {
    pub fn parse(s: &str) -> Result<Self> {
        let rest = s
            .strip_prefix("daimon://")
            .ok_or_else(|| MemoryError::InvalidUri(format!("missing daimon:// scheme: {s}")))?;
        // Split off the optional #tier.
        let (path, tier) = match rest.split_once('#') {
            Some((p, t)) => (p, Tier::parse(t)?),
            None => (rest, Tier::L1),
        };
        let comps: Vec<&str> = path.split('/').collect();
        // tenant / <namespace: >=2 comps> / record_type / record_id  => min 5 comps.
        if comps.len() < 5 {
            return Err(MemoryError::InvalidUri(format!(
                "too few components: {s}"
            )));
        }
        let tenant_id = Uuid::parse_str(comps[0])
            .map_err(|_| MemoryError::InvalidUri(format!("bad tenant uuid: {}", comps[0])))?;
        let record_id = Uuid::parse_str(comps[comps.len() - 1])
            .map_err(|_| MemoryError::InvalidUri(format!("bad record uuid: {}", comps[comps.len() - 1])))?;
        let record_type = MemoryKind::parse(comps[comps.len() - 2])?;
        let namespace = Namespace::parse(&comps[1..comps.len() - 2].join("/"))?;
        Ok(MemoryUri {
            tenant_id,
            namespace,
            record_type,
            record_id,
            tier,
        })
    }

    /// Render the tenant-relative client form: `daimon://{namespace}/{kind}/{id}#{tier}`.
    /// Drops the tenant segment (the client re-attaches it from request scope on input).
    /// Mirrors `Display` exactly minus the tenant slot, so it round-trips with `parse_scoped`.
    pub fn display_relative(&self) -> String {
        format!(
            "daimon://{}/{}/{}#{}",
            self.namespace,
            self.record_type.as_str(),
            self.record_id,
            self.tier.as_str()
        )
    }

    /// Parse a client-supplied URI that may be **full** (tenant as first path segment) or
    /// **tenant-relative** (namespace root first). Detection: the first path segment after
    /// `daimon://` is tried as a `Uuid`; if it parses, the URI is full and we delegate to
    /// `parse` unchanged; otherwise we re-attach `default_tenant` and delegate to `parse`.
    pub fn parse_scoped(s: &str, default_tenant: Uuid) -> Result<Self> {
        let rest = s
            .strip_prefix("daimon://")
            .ok_or_else(|| MemoryError::InvalidUri(format!("missing daimon:// scheme: {s}")))?;
        let path = rest.split('#').next().unwrap_or(rest);
        let first = path.split('/').next().unwrap_or("");
        if Uuid::parse_str(first).is_ok() {
            MemoryUri::parse(s)
        } else {
            MemoryUri::parse(&format!("daimon://{default_tenant}/{rest}"))
        }
    }
}

impl fmt::Display for MemoryUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "daimon://{}/{}/{}/{}#{}",
            self.tenant_id,
            self.namespace,
            self.record_type.as_str(),
            self.record_id,
            self.tier.as_str()
        )
    }
}

/// Strip the `{tenant}/` segment from a full `daimon://{tenant}/...` string, yielding the
/// tenant-relative client form `daimon://{rest}`. Used for the raw `uri_path` strings carried
/// by `MemoryHit`/`MemoryRecord` (which have no `#tier`). Returns the input unchanged if it is
/// not a recognizable full URI (defensive; the DB always stores the full form).
pub fn strip_tenant_segment(uri: &str) -> String {
    match uri.strip_prefix("daimon://") {
        Some(rest) => match rest.split_once('/') {
            Some((_tenant, tail)) => format!("daimon://{tail}"),
            None => uri.to_string(),
        },
        None => uri.to_string(),
    }
}

/// Consumer label: `[a-z0-9]([a-z0-9-])*` (no `-private` suffix logic here).
fn is_consumer(s: &str) -> bool {
    is_valid_segment(s)
}

/// Path segment grammar: `[a-z0-9]([a-z0-9-])*`.
fn is_valid_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shared_canonical() {
        let ns = Namespace::parse("shared-canonical/coding/decisions").unwrap();
        assert_eq!(ns.root, NamespaceRoot::SharedCanonical);
        assert_eq!(ns.path, vec!["coding", "decisions"]);
        assert_eq!(ns.to_string(), "shared-canonical/coding/decisions");
    }

    #[test]
    fn parse_consumer_private() {
        let ns = Namespace::parse("izu-private/scratch").unwrap();
        assert_eq!(ns.root, NamespaceRoot::ConsumerPrivate("izu".into()));
    }

    #[test]
    fn reject_bad_root_and_segment() {
        assert!(Namespace::parse("resources/x").is_err()); // old vocabulary, superseded
        assert!(Namespace::parse("shared-canonical/Bad_Seg").is_err()); // uppercase + underscore
        assert!(Namespace::parse("shared-canonical/-leading").is_err()); // leading dash
        assert!(Namespace::parse("shared-canonical").is_err()); // no segment
    }

    #[test]
    fn uri_roundtrip() {
        let s = "daimon://00000000-0000-0000-0000-000000000001/shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa#L0";
        let uri = MemoryUri::parse(s).unwrap();
        assert_eq!(uri.record_type, MemoryKind::Decision);
        assert_eq!(uri.tier, Tier::L0);
        assert_eq!(uri.to_string(), s);
    }

    #[test]
    fn uri_defaults_to_l1() {
        let s = "daimon://00000000-0000-0000-0000-000000000001/session/run-7/runbook/00000000-0000-0000-0000-0000000000bb";
        let uri = MemoryUri::parse(s).unwrap();
        assert_eq!(uri.tier, Tier::L1);
    }

    #[test]
    fn uri_rejects_garbage() {
        assert!(MemoryUri::parse("https://x/y").is_err());
        assert!(MemoryUri::parse("daimon://too/few").is_err());
    }

    #[test]
    fn display_relative_drops_tenant() {
        let full = "daimon://00000000-0000-0000-0000-000000000001/shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa#L0";
        let uri = MemoryUri::parse(full).unwrap();
        assert_eq!(
            uri.display_relative(),
            "daimon://shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa#L0"
        );
    }

    #[test]
    fn parse_scoped_accepts_full() {
        let tenant = Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap();
        let full = "daimon://00000000-0000-0000-0000-000000000001/shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa#L0";
        let uri = MemoryUri::parse_scoped(full, tenant).unwrap();
        // Full URI keeps its own embedded tenant, NOT the default.
        assert_eq!(uri.tenant_id, Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap());
        assert_eq!(uri.to_string(), full);
    }

    #[test]
    fn parse_scoped_accepts_relative_and_uses_default_tenant() {
        let tenant = Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap();
        let rel = "daimon://shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa#L0";
        let uri = MemoryUri::parse_scoped(rel, tenant).unwrap();
        assert_eq!(uri.tenant_id, tenant);
        assert_eq!(uri.record_type, MemoryKind::Decision);
        assert_eq!(uri.tier, Tier::L0);
        assert_eq!(uri.display_relative(), rel);
    }

    #[test]
    fn parse_scoped_relative_defaults_to_l1() {
        let tenant = Uuid::parse_str("00000000-0000-0000-0000-000000000099").unwrap();
        let rel = "daimon://session/run-7/runbook/00000000-0000-0000-0000-0000000000bb";
        let uri = MemoryUri::parse_scoped(rel, tenant).unwrap();
        assert_eq!(uri.tenant_id, tenant);
        assert_eq!(uri.tier, Tier::L1);
    }

    #[test]
    fn strip_tenant_segment_drops_first_segment() {
        let full = "daimon://00000000-0000-0000-0000-000000000001/shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa";
        assert_eq!(
            strip_tenant_segment(full),
            "daimon://shared-canonical/coding/decision/00000000-0000-0000-0000-0000000000aa"
        );
    }
}
