//! Pure RRF fusion - the deterministic ranking policy, extracted from the I/O of
//! `hybrid_recall` so it can be unit-tested without Postgres or Qdrant.
//!
//! Inputs are already-fetched rank lists (keyword from PG FTS, semantic from Qdrant).
//! [`fuse`] does Reciprocal Rank Fusion, the advisory importance boost, the semantic-arm
//! namespace post-filter (the keyword arm is scoped in SQL), a deterministic tie-break,
//! and the final truncate. No model, no network: same inputs -> byte-identical output.

use daimon_memory_core::{MemoryHit, strip_tenant_segment};
use serde_json::{Value, json};

const K: f32 = 60.0;
/// Max additive importance boost: ~4 inter-rank RRF gaps at importance 100 (one gap with
/// K=60 is ~0.000264). Advisory - in the common (shallow-rank) case it never lets a
/// single-arm hit overtake a both-arms hit, but it is intentionally a thumb on the scale,
/// not a hard guarantee at very deep ranks.
const IMPORTANCE_BOOST_MAX: f32 = 0.001;

fn rrf(rank: usize) -> f32 {
    1.0 / (K + rank as f32 + 1.0)
}

/// A semantic-arm candidate, projected from a Qdrant payload into plain data so fusion
/// needs no Qdrant types. `namespace` drives the post-filter; `score` is the raw cosine.
#[derive(Debug, Clone, Default)]
pub struct SemHit {
    pub uri: String,
    pub kind: String,
    pub title: String,
    pub abstract_: String,
    pub namespace: String,
    pub importance: u8,
    pub score: f32,
}

struct Acc {
    kind: String,
    title: String,
    abstract_: String,
    rrf: f32,
    sources: Vec<&'static str>,
    importance: u8,
    raw_keyword: f32,
    raw_semantic: f32,
}

/// Fuse the two rank lists into the ranked recall response.
///
/// - `keyword`: PG FTS hits, in rank order (already namespace/kind/since scoped in SQL).
/// - `semantic`: Qdrant hits, in rank order (tenant-scoped; `namespace_prefix` is applied
///   here because Qdrant cannot prefix-match a keyword payload).
/// - `limit`: max results (clamped to at least 1; callers also clamp the upper bound).
pub fn fuse(
    keyword: &[MemoryHit],
    semantic: &[SemHit],
    namespace_prefix: Option<&str>,
    limit: usize,
) -> Vec<Value> {
    use std::collections::HashMap;
    let mut acc: HashMap<String, Acc> = HashMap::new();

    for (rank, h) in keyword.iter().enumerate() {
        let e = acc.entry(h.uri.clone()).or_insert_with(|| Acc {
            kind: h.kind.as_str().to_string(),
            title: h.title.clone(),
            abstract_: h.abstract_.clone(),
            rrf: 0.0,
            sources: vec![],
            importance: h.importance,
            raw_keyword: 0.0,
            raw_semantic: 0.0,
        });
        e.rrf += rrf(rank);
        e.sources.push("keyword");
        if e.importance == 0 {
            e.importance = h.importance;
        }
        e.raw_keyword = h.score; // raw ts_rank
    }

    // The semantic arm keeps its own independent rank index, so a hit dropped by the
    // namespace post-filter must not shift the RRF rank of the hits after it.
    let mut srank = 0usize;
    for h in semantic {
        if h.uri.is_empty() {
            continue;
        }
        if let Some(prefix) = namespace_prefix
            && !h.namespace.starts_with(prefix)
        {
            continue; // cross-namespace semantic leak; the keyword arm is scoped in SQL
        }
        let e = acc.entry(h.uri.clone()).or_insert_with(|| Acc {
            kind: h.kind.clone(),
            title: h.title.clone(),
            abstract_: h.abstract_.clone(),
            rrf: 0.0,
            sources: vec![],
            importance: h.importance,
            raw_keyword: 0.0,
            raw_semantic: 0.0,
        });
        e.rrf += rrf(srank);
        e.sources.push("semantic");
        if e.importance == 0 {
            e.importance = h.importance;
        }
        e.raw_semantic = h.score; // raw cosine
        srank += 1;
    }

    let mut out: Vec<Value> = acc
        .into_iter()
        .map(|(uri, a)| {
            let boosted = a.rrf + (a.importance as f32 / 100.0) * IMPORTANCE_BOOST_MAX;
            json!({
                "uri": strip_tenant_segment(&uri),
                "kind": a.kind,
                "title": a.title,
                "abstract": a.abstract_,
                "score": boosted,
                "importance": a.importance,
                "sources": a.sources,
                "scores": {"rrf": a.rrf, "raw_keyword": a.raw_keyword, "raw_semantic": a.raw_semantic}
            })
        })
        .collect();
    // Sort by boosted score; deterministic tie-break (importance desc, then uri asc) removes
    // the HashMap-iteration non-determinism on equal scores.
    out.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .partial_cmp(&a["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b["importance"].as_u64().cmp(&a["importance"].as_u64()))
            .then_with(|| a["uri"].as_str().cmp(&b["uri"].as_str()))
    });
    out.truncate(limit.max(1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon_memory_core::MemoryKind;

    fn kw(uri: &str, score: f32, importance: u8) -> MemoryHit {
        MemoryHit {
            uri: uri.to_string(),
            kind: MemoryKind::Decision,
            title: "t".into(),
            abstract_: "a".into(),
            score,
            importance,
        }
    }
    fn sem(uri: &str, namespace: &str, score: f32, importance: u8) -> SemHit {
        SemHit {
            uri: uri.to_string(),
            kind: "decision".into(),
            title: "t".into(),
            abstract_: "a".into(),
            namespace: namespace.into(),
            importance,
            score,
        }
    }
    fn uri_at(out: &[Value], i: usize) -> &str {
        out[i]["uri"].as_str().unwrap()
    }

    #[test]
    fn both_arms_hit_outranks_single_arm_importance_max() {
        // A record found by BOTH arms (rank 0 each) must beat a keyword-only record at
        // rank 0 even when the latter is importance 100 - this is the cap's headline case.
        let full = "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000a1";
        let single = "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000a2";
        let keyword = vec![kw(full, 0.9, 0), kw(single, 0.9, 100)];
        let semantic = vec![sem(full, "resources/x", 0.9, 0)];
        let out = fuse(&keyword, &semantic, None, 10);
        assert_eq!(uri_at(&out, 0), strip_tenant_segment(full));
        assert_eq!(out[0]["sources"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn semantic_hit_outside_prefix_is_dropped() {
        let in_scope = "daimon://agent/lessons/agent_lesson/00000000-0000-0000-0000-0000000000b1";
        let out_scope = "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000b2";
        let semantic = vec![
            sem(out_scope, "resources/x", 0.99, 0), // nearer, but out of scope
            sem(in_scope, "agent/lessons", 0.50, 0),
        ];
        let out = fuse(&[], &semantic, Some("agent/"), 10);
        assert_eq!(out.len(), 1);
        assert_eq!(uri_at(&out, 0), strip_tenant_segment(in_scope));
    }

    #[test]
    fn prefix_filter_does_not_shift_ranks_of_kept_hits() {
        // Drop the rank-0 hit by namespace; the kept hit must score as if it were rank 0,
        // not rank 1 (the semantic rank index only advances on kept hits).
        let kept = "daimon://agent/lessons/agent_lesson/00000000-0000-0000-0000-0000000000c1";
        let dropped = "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000c2";
        let with_drop = fuse(
            &[],
            &vec![
                sem(dropped, "resources/x", 0.9, 0),
                sem(kept, "agent/x", 0.9, 0),
            ],
            Some("agent/"),
            10,
        );
        let without_drop = fuse(&[], &vec![sem(kept, "agent/x", 0.9, 0)], Some("agent/"), 10);
        assert_eq!(
            with_drop[0]["scores"]["rrf"].as_f64(),
            without_drop[0]["scores"]["rrf"].as_f64(),
        );
    }

    #[test]
    fn equal_scores_tie_break_is_deterministic() {
        // Two single-arm keyword hits, identical rank/score, differing importance then uri.
        let lo = "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000d1";
        let hi = "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000d2";
        let keyword = vec![kw(lo, 0.5, 10), kw(hi, 0.5, 90)];
        let a = fuse(&keyword, &[], None, 10);
        let b = fuse(&keyword, &[], None, 10);
        assert_eq!(a, b, "fusion must be deterministic across runs");
        // higher importance wins the tie despite both being rank 0
        assert_eq!(uri_at(&a, 0), strip_tenant_segment(hi));
    }

    #[test]
    fn limit_truncates_and_zero_still_returns_one() {
        let keyword: Vec<MemoryHit> = (0..5)
            .map(|i| {
                kw(
                    &format!(
                        "daimon://resources/x/decision/00000000-0000-0000-0000-00000000000{i}"
                    ),
                    0.9 - i as f32 * 0.1,
                    0,
                )
            })
            .collect();
        assert_eq!(fuse(&keyword, &[], None, 3).len(), 3);
        assert_eq!(fuse(&keyword, &[], None, 0).len(), 1, "limit 0 clamps to 1");
    }

    #[test]
    fn output_uris_are_tenant_relative() {
        let full = "daimon://00000000-0000-0000-0000-000000000001/resources/x/decision/00000000-0000-0000-0000-0000000000e1";
        let out = fuse(&[kw(full, 0.5, 0)], &[], None, 10);
        assert_eq!(
            uri_at(&out, 0),
            "daimon://resources/x/decision/00000000-0000-0000-0000-0000000000e1"
        );
    }
}
