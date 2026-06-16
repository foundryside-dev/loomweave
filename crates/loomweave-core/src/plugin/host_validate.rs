//! Pure, self-free validation for plugin-reported entities, edges, unresolved
//! call sites, and findings — the B.3 per-field caps and the finding-shape
//! contract.
//!
//! Carved out of `host.rs` (clarion-2b8811da39) to separate the stateless
//! *validation* layer from the `PluginHost` *transport*/orchestration that
//! drives it. Every function here is a free function with no `self` and no I/O:
//! it inspects a decoded wire value and returns a verdict (an oversize offender,
//! a rejection reason, or a validated `HostFinding`). `PluginHost::analyze_file`
//! calls these as stage-0 (field-size) and finding-shape checks of its
//! four-stage pipeline; the caps and reasons live here so the rules are one
//! reviewable unit, and `host.rs` re-exports the public caps so existing paths
//! (`crate::plugin::host::MAX_ENTITY_FIELD_BYTES`, etc.) keep resolving.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::plugin::host::{RawEdge, RawEntity};
use crate::plugin::host_findings::HostFinding;
use crate::plugin::protocol::{AnalyzeFileFinding, UnresolvedCallSite};

/// Per-string length cap applied to [`RawEntity::id`], [`RawEntity::kind`],
/// [`RawEntity::qualified_name`], and [`RawSource::file_path`].
///
/// 4 KiB is well above any legitimate identifier or path in a real codebase
/// (the Linux `PATH_MAX` is 4096; Python fully-qualified names exceeding 1 KiB
/// are absent from elspeth's 425k LOC baseline). The cap is a trust-boundary
/// check, not a style constraint — pick a value that rejects `DoS` payloads
/// without false-positing on pathological-but-legitimate inputs.
///
/// [`RawSource::file_path`]: crate::plugin::host::RawSource
pub const MAX_ENTITY_FIELD_BYTES: usize = 4 * 1024;

/// Maximum UTF-8 byte length for one unresolved callee expression retained for
/// query-time inferred dispatch.
pub const MAX_UNRESOLVED_CALLEE_EXPR_BYTES: usize = 512;

/// Maximum plugin-reported findings accepted from one `analyze_file` response.
pub const MAX_PLUGIN_FINDINGS_PER_FILE: usize = 100;

/// Maximum UTF-8 byte length for one plugin-reported finding subcode.
pub const MAX_FINDING_SUBCODE_BYTES: usize = 128;

/// Maximum UTF-8 byte length for one plugin-reported severity label.
pub const MAX_FINDING_SEVERITY_BYTES: usize = 32;

/// Per-entity cap on the total serialised size of the untyped passthrough
/// maps [`RawEntity::extra`] and [`RawSource::extra`].
///
/// These flow into `properties_json` downstream (via
/// `loomweave-cli::analyze::map_entity_to_record`) as `serde_json::to_string`
/// output. Without a cap, a plugin could return 8 MiB frames consisting of
/// one tiny `qualified_name` plus a multi-MiB `extra` map that lives in the
/// database row and in every host-side clone until the run ends. 64 KiB is
/// well above any legitimate plugin-declared properties bag (WP3's wardline
/// payload is <2 KiB) while rejecting payload floods.
///
/// [`RawSource::extra`]: crate::plugin::host::RawSource
pub const MAX_ENTITY_EXTRA_BYTES: usize = 64 * 1024;

/// Per-string and serialised-map oversize check for [`RawEdge`].
/// Mirrors [`oversize_field`] in spirit: rejects any plugin-controlled string
/// or untyped passthrough map exceeding the B.3 per-field caps. Fields
/// checked in a stable order so the finding deterministically names the
/// first offender for the same input.
pub(crate) fn oversize_edge_field(raw: &RawEdge) -> Option<(&'static str, usize)> {
    for (name, len) in [
        ("kind", raw.kind.len()),
        ("from_id", raw.from_id.len()),
        ("to_id", raw.to_id.len()),
    ] {
        if len > MAX_ENTITY_FIELD_BYTES {
            return Some((name, len));
        }
    }
    if !raw.extra.is_empty() {
        let len = serde_json::to_vec(&raw.extra).map_or(0, |b| b.len());
        if len > MAX_ENTITY_EXTRA_BYTES {
            return Some(("extra", len));
        }
    }
    if let Some(props) = &raw.properties {
        let len = serde_json::to_vec(props).map_or(0, |b| b.len());
        if len > MAX_ENTITY_EXTRA_BYTES {
            return Some(("properties", len));
        }
    }
    None
}

pub(crate) fn oversize_field(raw: &RawEntity) -> Option<(&'static str, usize)> {
    for (name, len) in [
        ("id", raw.id.len()),
        ("kind", raw.kind.len()),
        ("qualified_name", raw.qualified_name.len()),
        ("source.file_path", raw.source.file_path.len()),
    ] {
        if len > MAX_ENTITY_FIELD_BYTES {
            return Some((name, len));
        }
    }

    // `extra` and `source.extra` flow to `properties_json` downstream. The
    // check is by serialised byte length rather than entry count — a single
    // entry with a multi-MiB Value is as toxic as many entries each small.
    // Serialisation is the next-downstream step anyway (via
    // loomweave-cli::analyze::map_entity_to_record), so the to_vec here is not
    // an additional allocation beyond what we were already going to pay.
    for (name, map) in [("extra", &raw.extra), ("source.extra", &raw.source.extra)] {
        if map.is_empty() {
            continue;
        }
        let len = serde_json::to_vec(map).map_or(0, |b| b.len());
        if len > MAX_ENTITY_EXTRA_BYTES {
            return Some((name, len));
        }
    }
    if !raw.tags.is_empty() {
        let len = serde_json::to_vec(&raw.tags).map_or(0, |b| b.len());
        if len > MAX_ENTITY_EXTRA_BYTES {
            return Some(("tags", len));
        }
    }

    None
}

pub(crate) fn invalid_unresolved_call_site_reason(
    site: &UnresolvedCallSite,
    accepted_ids: &BTreeSet<String>,
    file_len: Option<i64>,
) -> Option<String> {
    if !accepted_ids.contains(&site.caller_entity_id) {
        return Some("caller entity was not accepted for this file".to_owned());
    }
    if site.site_ordinal < 0 {
        return Some("site_ordinal is negative".to_owned());
    }
    if site.source_byte_start < 0 {
        return Some("source_byte_start is negative".to_owned());
    }
    if site.source_byte_end <= site.source_byte_start {
        return Some("source byte range is empty or reversed".to_owned());
    }
    if let Some(file_len) = file_len
        && site.source_byte_end > file_len
    {
        return Some("source byte range exceeds analyzed file length".to_owned());
    }
    if site.callee_expr.is_empty() {
        return Some("callee_expr is empty".to_owned());
    }
    if site.callee_expr.len() > MAX_UNRESOLVED_CALLEE_EXPR_BYTES {
        return Some(format!(
            "callee_expr exceeds {MAX_UNRESOLVED_CALLEE_EXPR_BYTES} bytes"
        ));
    }
    None
}

fn stringify_finding_metadata_value(value: serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::Null => Ok("null".to_owned()),
        serde_json::Value::Bool(v) => Ok(v.to_string()),
        serde_json::Value::Number(v) => Ok(v.to_string()),
        serde_json::Value::String(v) => Ok(v),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => serde_json::to_string(&value)
            .map_err(|e| format!("metadata value is not serializable: {e}")),
    }
}

pub(crate) fn validate_plugin_finding(
    raw: AnalyzeFileFinding,
    rule_id_prefix: &str,
    analyzed_path: &Path,
) -> Result<HostFinding, String> {
    if raw.subcode.is_empty() {
        return Err("subcode is empty".to_owned());
    }
    if raw.subcode.len() > MAX_FINDING_SUBCODE_BYTES {
        return Err(format!("subcode exceeds {MAX_FINDING_SUBCODE_BYTES} bytes"));
    }
    if !raw.subcode.starts_with(rule_id_prefix) {
        return Err(format!(
            "subcode {:?} is outside manifest rule_id_prefix {:?}",
            raw.subcode, rule_id_prefix
        ));
    }
    if raw.message.is_empty() {
        return Err("message is empty".to_owned());
    }
    if raw.message.len() > MAX_ENTITY_FIELD_BYTES {
        return Err(format!("message exceeds {MAX_ENTITY_FIELD_BYTES} bytes"));
    }
    if !raw.metadata.is_empty() {
        let len = serde_json::to_vec(&raw.metadata).map_or(0, |bytes| bytes.len());
        if len > MAX_ENTITY_EXTRA_BYTES {
            return Err(format!("metadata exceeds {MAX_ENTITY_EXTRA_BYTES} bytes"));
        }
    }

    let mut metadata = BTreeMap::new();
    if let Some(severity) = raw.severity {
        if severity.is_empty() {
            return Err("severity is empty".to_owned());
        }
        if severity.len() > MAX_FINDING_SEVERITY_BYTES {
            return Err(format!(
                "severity exceeds {MAX_FINDING_SEVERITY_BYTES} bytes"
            ));
        }
        if !matches!(severity.as_str(), "info" | "warning" | "error") {
            return Err(format!("unsupported severity {severity:?}"));
        }
        metadata.insert("severity".to_owned(), severity);
    }
    for (key, value) in raw.metadata {
        if key.is_empty() {
            return Err("metadata key is empty".to_owned());
        }
        if key.len() > MAX_ENTITY_FIELD_BYTES {
            return Err(format!(
                "metadata key exceeds {MAX_ENTITY_FIELD_BYTES} bytes"
            ));
        }
        let value = stringify_finding_metadata_value(value)?;
        if value.len() > MAX_ENTITY_FIELD_BYTES {
            return Err(format!(
                "metadata value for {key:?} exceeds {MAX_ENTITY_FIELD_BYTES} bytes"
            ));
        }
        metadata.insert(key, value);
    }
    metadata.insert(
        "anchor_file_path".to_owned(),
        analyzed_path.to_string_lossy().into_owned(),
    );

    Ok(HostFinding::plugin_reported(
        raw.subcode,
        raw.message,
        metadata,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::protocol::AnalyzeFileFinding;

    fn finding(subcode: &str, message: &str) -> AnalyzeFileFinding {
        AnalyzeFileFinding {
            subcode: subcode.to_owned(),
            message: message.to_owned(),
            severity: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn validate_plugin_finding_rejects_subcode_outside_rule_prefix() {
        let err = validate_plugin_finding(finding("OTHER-X", "m"), "PY-", Path::new("a.py"))
            .expect_err("subcode outside prefix must be rejected");
        assert!(err.contains("rule_id_prefix"), "{err}");
    }

    #[test]
    fn validate_plugin_finding_rejects_unsupported_severity() {
        let mut raw = finding("PY-CODE", "m");
        raw.severity = Some("fatal".to_owned());
        let err = validate_plugin_finding(raw, "PY-", Path::new("a.py"))
            .expect_err("severity outside {info,warning,error} must be rejected");
        assert!(err.contains("unsupported severity"), "{err}");
    }

    #[test]
    fn validate_plugin_finding_injects_anchor_file_path() {
        let ok = validate_plugin_finding(finding("PY-CODE", "m"), "PY-", Path::new("pkg/a.py"))
            .expect("a well-formed finding validates");
        assert_eq!(
            ok.metadata.get("anchor_file_path").map(String::as_str),
            Some("pkg/a.py"),
            "the analyzed path is recorded as anchor_file_path"
        );
    }

    #[test]
    fn invalid_unresolved_call_site_reason_rejects_empty_or_reversed_range() {
        let mut accepted = BTreeSet::new();
        accepted.insert("caller".to_owned());
        let site = UnresolvedCallSite {
            caller_entity_id: "caller".to_owned(),
            site_ordinal: 0,
            source_byte_start: 10,
            source_byte_end: 10, // empty range
            callee_expr: "f".to_owned(),
        };
        assert_eq!(
            invalid_unresolved_call_site_reason(&site, &accepted, Some(100)).as_deref(),
            Some("source byte range is empty or reversed"),
        );
    }

    #[test]
    fn invalid_unresolved_call_site_reason_rejects_unknown_caller() {
        let accepted = BTreeSet::new();
        let site = UnresolvedCallSite {
            caller_entity_id: "ghost".to_owned(),
            site_ordinal: 0,
            source_byte_start: 0,
            source_byte_end: 1,
            callee_expr: "f".to_owned(),
        };
        assert_eq!(
            invalid_unresolved_call_site_reason(&site, &accepted, None).as_deref(),
            Some("caller entity was not accepted for this file"),
        );
    }
}
