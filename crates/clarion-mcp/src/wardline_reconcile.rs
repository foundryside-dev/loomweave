//! Reconcile Wardline findings to Clarion entities by qualname (Flow B).
//!
//! `metadata.wardline.qualname` is the pre-composed dotted name, which for a
//! function/method entity is byte-identical to the `entity_id`'s segment-3
//! `canonical_qualified_name` (proven by `fixtures/entity_id.json`). Matching is
//! therefore a local string compare against Clarion's own catalog — no oracle.

use crate::filigree::WardlineFinding;

/// A Wardline finding's resolution against a Clarion entity. v1 produces only
/// `Exact` (byte-equal qualname) or `None`. `Heuristic` is reserved for a future
/// best-effort normalization pass and is never returned yet — kept in the enum
/// so the wire shape is stable when it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolutionConfidence {
    Exact,
    Heuristic,
    None,
}

#[derive(Debug, Clone)]
pub struct MatchedFinding {
    pub finding: WardlineFinding,
    pub resolution_confidence: ResolutionConfidence,
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileResult {
    pub matched: Vec<MatchedFinding>,
    pub omitted_no_qualname: usize,
}

/// The `entity_id`'s segment-3 `canonical_qualified_name`.
/// `python:function:demo.Foo.bar` -> `Some("demo.Foo.bar")`. `None` when the id
/// lacks three `{plugin}:{kind}:{qualname}` segments or the qualname is empty.
pub fn entity_qualname(entity_id: &str) -> Option<&str> {
    let mut parts = entity_id.splitn(3, ':');
    let _plugin = parts.next()?;
    let _kind = parts.next()?;
    let qualname = parts.next()?;
    (!qualname.is_empty()).then_some(qualname)
}

/// The dotted qualname a finding targets, from `metadata.wardline.qualname`.
/// `None` when the key is absent (malformed / non-Python) — counted as omitted.
fn finding_qualname(finding: &WardlineFinding) -> Option<&str> {
    finding.metadata.get("wardline")?.get("qualname")?.as_str()
}

fn resolution_confidence(entity_qn: &str, finding_qn: &str) -> ResolutionConfidence {
    if entity_qn == finding_qn {
        ResolutionConfidence::Exact
    } else {
        ResolutionConfidence::None
    }
}

/// Filter `findings` to those that resolve to `entity_id`, tagging each with its
/// confidence. Findings with no `wardline.qualname` are counted in
/// `omitted_no_qualname`, never dropped silently.
pub fn reconcile_for_entity(entity_id: &str, findings: Vec<WardlineFinding>) -> ReconcileResult {
    let Some(target) = entity_qualname(entity_id) else {
        return ReconcileResult::default();
    };
    let mut result = ReconcileResult::default();
    for finding in findings {
        match finding_qualname(&finding) {
            Some(qn) => {
                let confidence = resolution_confidence(target, qn);
                if confidence != ResolutionConfidence::None {
                    result.matched.push(MatchedFinding { finding, resolution_confidence: confidence });
                }
            }
            None => result.omitted_no_qualname += 1,
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filigree::WardlineFinding;

    fn finding(qualname: Option<&str>) -> WardlineFinding {
        let metadata = match qualname {
            Some(q) => serde_json::json!({ "wardline": { "qualname": q } }),
            None => serde_json::json!({ "wardline": { "kind": "DEFECT" } }),
        };
        WardlineFinding {
            rule_id: "WLN-X".to_owned(),
            message: "m".to_owned(),
            severity: Some("high".to_owned()),
            status: Some("open".to_owned()),
            line_start: Some(1),
            line_end: Some(1),
            fingerprint: Some("fp".to_owned()),
            file_id: Some("file-1".to_owned()),
            metadata,
        }
    }

    #[test]
    fn extracts_segment_three_qualname_incl_locals_and_nested() {
        assert_eq!(entity_qualname("python:function:demo.Foo.bar"), Some("demo.Foo.bar"));
        assert_eq!(
            entity_qualname("python:function:demo.outer.<locals>.inner"),
            Some("demo.outer.<locals>.inner")
        );
        assert_eq!(entity_qualname("python:function:hello"), Some("hello"));
        assert_eq!(entity_qualname("python:module:"), None); // empty qualname
        assert_eq!(entity_qualname("notanid"), None);
    }

    #[test]
    fn exact_match_binds_other_qualname_is_none() {
        let r = reconcile_for_entity(
            "python:function:demo.Foo.bar",
            vec![finding(Some("demo.Foo.bar")), finding(Some("demo.other"))],
        );
        assert_eq!(r.matched.len(), 1);
        assert_eq!(r.matched[0].resolution_confidence, ResolutionConfidence::Exact);
        assert_eq!(r.omitted_no_qualname, 0);
    }

    #[test]
    fn missing_qualname_is_counted_omitted_not_matched() {
        let r = reconcile_for_entity("python:function:demo.Foo.bar", vec![finding(None)]);
        assert!(r.matched.is_empty());
        assert_eq!(r.omitted_no_qualname, 1);
    }

    #[test]
    fn unparseable_entity_id_yields_empty_no_panic() {
        let r = reconcile_for_entity("notanid", vec![finding(Some("demo.Foo.bar"))]);
        assert!(r.matched.is_empty());
        assert_eq!(r.omitted_no_qualname, 0);
    }
}
