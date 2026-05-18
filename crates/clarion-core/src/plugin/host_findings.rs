//! Host-level finding subcodes and constructors.
//!
//! Resource and framing findings live in `limits.rs` next to the types they
//! reference (`ContentLengthCeiling`, `EntityCountCap`, etc.). The subcodes
//! here cover protocol, ontology, and manifest-capability failures that belong
//! to the supervisor layer.

use std::collections::BTreeMap;

use crate::plugin::limits::{
    FINDING_DISABLED_PATH_ESCAPE, FINDING_ENTITY_CAP, FINDING_OOM_KILLED, FINDING_PATH_ESCAPE,
};
use crate::plugin::protocol::UnresolvedCallSite;

/// Emitted when a plugin emits an entity whose `kind` is not in the manifest's
/// `entity_kinds` list (ADR-022 ontology boundary).
pub const FINDING_UNDECLARED_KIND: &str = "CLA-INFRA-PLUGIN-UNDECLARED-KIND";

/// Emitted when a plugin emits an entity whose `id` string does not match the
/// expected `entity_id(plugin_id, kind, qualified_name)` (UQ-WP2-11).
pub const FINDING_ENTITY_ID_MISMATCH: &str = "CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH";

/// Emitted when the manifest contains a capability not supported in v0.1
/// (ADR-021 §Layer 1).
pub const FINDING_UNSUPPORTED_CAPABILITY: &str = "CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY";

/// Emitted when a plugin returns an entity whose JSON shape fails to
/// deserialise into `RawEntity` (missing required field, wrong type, etc.).
///
/// Structurally invalid entities are dropped rather than failing the run, so
/// the finding is the only signal the operator gets that the plugin emitted
/// malformed output. Without this, a plugin bug that silently produces garbage
/// for a subset of entities looks identical to "no entities found".
pub const FINDING_MALFORMED_ENTITY: &str = "CLA-INFRA-PLUGIN-MALFORMED-ENTITY";

/// Emitted when the host is asked to analyze a file whose path is not
/// representable as UTF-8. The wire protocol is JSON (UTF-8 only), so the host
/// cannot forward the path to the plugin; the file is skipped with this finding
/// and the run continues.
///
/// Linux filenames are arbitrary byte sequences. Using `to_string_lossy` at
/// the wire boundary would replace invalid bytes with U+FFFD, yielding a path
/// the plugin cannot open and an obscure "plugin returned no entities" symptom.
/// Failing loudly with this finding keeps the diagnostic at the host layer.
pub const FINDING_NON_UTF8_PATH: &str = "CLA-INFRA-HOST-NON-UTF8-PATH";

/// Emitted when a plugin returns an entity with a string field longer than
/// `MAX_ENTITY_FIELD_BYTES`. Entity is dropped; plugin is not killed.
///
/// Without this bound, a plugin could emit up to `EntityCountCap` entities each
/// carrying multi-MB `qualified_name`/`kind`/`id`/`file_path` strings. The
/// identity check duplicates `qualified_name` through `format!()`, so the memory
/// cost is at least 2x the incoming string per offending entity, making this a
/// RAM-amplification vector even under the 8 MiB Content-Length ceiling.
pub const FINDING_ENTITY_FIELD_OVERSIZE: &str = "CLA-INFRA-PLUGIN-ENTITY-FIELD-OVERSIZE";

/// Emitted when a plugin returns an edge whose JSON shape fails to deserialise
/// into `RawEdge` (missing required field, wrong type, etc.). Symmetric with
/// [`FINDING_MALFORMED_ENTITY`]; edge is dropped, run continues.
pub const FINDING_MALFORMED_EDGE: &str = "CLA-INFRA-PLUGIN-MALFORMED-EDGE";

/// Emitted when a plugin emits an edge whose `kind` is not in the manifest's
/// `edge_kinds` list (ADR-022 ontology boundary, edge variant). Drop + finding;
/// no kill.
pub const FINDING_UNDECLARED_EDGE_KIND: &str = "CLA-INFRA-PLUGIN-UNDECLARED-EDGE-KIND";

/// Emitted when a plugin returns an edge with a string field longer than
/// `MAX_ENTITY_FIELD_BYTES`. Edge is dropped; plugin is not killed. Same
/// rationale as [`FINDING_ENTITY_FIELD_OVERSIZE`] (RAM amplification).
pub const FINDING_EDGE_FIELD_OVERSIZE: &str = "CLA-INFRA-PLUGIN-EDGE-FIELD-OVERSIZE";

/// Emitted when `stats.unresolved_call_sites` contains a row that cannot be
/// tied back to the accepted entities and source bytes for this `analyze_file`
/// response. The row is dropped; aggregate counters are retained.
pub const FINDING_MALFORMED_UNRESOLVED_CALL_SITE: &str =
    "CLA-INFRA-PLUGIN-MALFORMED-UNRESOLVED-CALL-SITE";

/// Informational diagnostic accumulated during a host's lifetime.
///
/// Collected into `self.findings` on each enforcement action. Drained via
/// `PluginHost::take_findings`. Will eventually be persisted as ADR-004
/// Findings; for Sprint 1 they are collected only.
#[derive(Debug, Clone)]
pub struct HostFinding {
    /// Finding subcode, e.g. `"CLA-INFRA-PLUGIN-PATH-ESCAPE"`.
    pub subcode: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Structured metadata (keys: `"offending_path"`, `"entity_id"`, etc.).
    pub metadata: BTreeMap<String, String>,
}

impl HostFinding {
    pub(super) fn undeclared_kind(kind: &str, qualified_name: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("kind".to_owned(), kind.to_owned());
        metadata.insert("qualified_name".to_owned(), qualified_name.to_owned());
        Self {
            subcode: FINDING_UNDECLARED_KIND,
            message: format!("entity kind {kind:?} is not declared in the manifest ontology"),
            metadata,
        }
    }

    pub(super) fn entity_id_mismatch(got: &str, expected: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("got".to_owned(), got.to_owned());
        metadata.insert("expected".to_owned(), expected.to_owned());
        Self {
            subcode: FINDING_ENTITY_ID_MISMATCH,
            message: format!("entity id mismatch: got {got:?}, expected {expected:?}"),
            metadata,
        }
    }

    pub(super) fn path_escape(offending_path: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("offending_path".to_owned(), offending_path.to_owned());
        Self {
            subcode: FINDING_PATH_ESCAPE,
            message: format!("entity source path escapes project root: {offending_path:?}"),
            metadata,
        }
    }

    pub(super) fn disabled_path_escape() -> Self {
        Self {
            subcode: FINDING_DISABLED_PATH_ESCAPE,
            message: "path-escape circuit breaker tripped; plugin killed".to_owned(),
            metadata: BTreeMap::new(),
        }
    }

    pub(super) fn entity_cap_exceeded_finding(cap: usize, would_reach: usize) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("cap".to_owned(), cap.to_string());
        metadata.insert("would_reach".to_owned(), would_reach.to_string());
        Self {
            subcode: FINDING_ENTITY_CAP,
            message: format!("entity cap {cap} would be exceeded (would reach {would_reach})"),
            metadata,
        }
    }

    pub(super) fn unsupported_capability(msg: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("detail".to_owned(), msg.to_owned());
        Self {
            subcode: FINDING_UNSUPPORTED_CAPABILITY,
            message: format!("manifest has unsupported capability: {msg}"),
            metadata,
        }
    }

    pub(super) fn non_utf8_path(lossy_repr: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("path_lossy".to_owned(), lossy_repr.to_owned());
        Self {
            subcode: FINDING_NON_UTF8_PATH,
            message: format!(
                "file skipped: path is not valid UTF-8 and cannot be expressed \
                 on the JSON wire protocol: {lossy_repr:?}"
            ),
            metadata,
        }
    }

    pub(super) fn malformed_entity(serde_err: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("serde_error".to_owned(), serde_err.to_owned());
        Self {
            subcode: FINDING_MALFORMED_ENTITY,
            message: format!("plugin emitted an entity that failed to deserialise: {serde_err}"),
            metadata,
        }
    }

    pub(super) fn malformed_edge(serde_err: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("serde_error".to_owned(), serde_err.to_owned());
        Self {
            subcode: FINDING_MALFORMED_EDGE,
            message: format!("plugin emitted an edge that failed to deserialise: {serde_err}"),
            metadata,
        }
    }

    pub(super) fn undeclared_edge_kind(kind: &str, from_id: &str, to_id: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("kind".to_owned(), kind.to_owned());
        metadata.insert("from_id".to_owned(), from_id.to_owned());
        metadata.insert("to_id".to_owned(), to_id.to_owned());
        Self {
            subcode: FINDING_UNDECLARED_EDGE_KIND,
            message: format!("edge kind {kind:?} is not declared in the manifest ontology"),
            metadata,
        }
    }

    pub(super) fn edge_field_oversize(
        field: &'static str,
        actual_bytes: usize,
        limit_bytes: usize,
    ) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("field".to_owned(), field.to_owned());
        metadata.insert("actual_bytes".to_owned(), actual_bytes.to_string());
        metadata.insert("limit_bytes".to_owned(), limit_bytes.to_string());
        Self {
            subcode: FINDING_EDGE_FIELD_OVERSIZE,
            message: format!(
                "edge field {field:?} is {actual_bytes} bytes, over the {limit_bytes}-byte limit"
            ),
            metadata,
        }
    }

    pub(super) fn malformed_unresolved_call_site(site: &UnresolvedCallSite, reason: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("caller_entity_id".to_owned(), site.caller_entity_id.clone());
        metadata.insert("site_ordinal".to_owned(), site.site_ordinal.to_string());
        metadata.insert(
            "source_byte_start".to_owned(),
            site.source_byte_start.to_string(),
        );
        metadata.insert(
            "source_byte_end".to_owned(),
            site.source_byte_end.to_string(),
        );
        metadata.insert("reason".to_owned(), reason.to_owned());
        Self {
            subcode: FINDING_MALFORMED_UNRESOLVED_CALL_SITE,
            message: format!("plugin emitted malformed unresolved call site: {reason}"),
            metadata,
        }
    }

    pub(super) fn entity_field_oversize(
        field: &'static str,
        actual_bytes: usize,
        limit_bytes: usize,
    ) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("field".to_owned(), field.to_owned());
        metadata.insert("actual_bytes".to_owned(), actual_bytes.to_string());
        metadata.insert("limit_bytes".to_owned(), limit_bytes.to_string());
        Self {
            subcode: FINDING_ENTITY_FIELD_OVERSIZE,
            message: format!(
                "entity field {field:?} is {actual_bytes} bytes, over the {limit_bytes}-byte limit"
            ),
            metadata,
        }
    }

    /// Emitted by the CLI wrapper once the child has been reaped and its exit
    /// status indicates a signal consistent with an `RLIMIT_AS` kill (SIGKILL
    /// or SIGSEGV). Lives on [`HostFinding`] rather than being constructed in
    /// the CLI so the finding-subcode API is centralised.
    pub fn oom_killed(plugin_id: &str, signal: i32) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("plugin_id".to_owned(), plugin_id.to_owned());
        metadata.insert("signal".to_owned(), signal.to_string());
        Self {
            subcode: FINDING_OOM_KILLED,
            message: format!(
                "plugin {plugin_id} killed by signal {signal} \
                 (likely RLIMIT_AS enforcement per ADR-021 §2d)"
            ),
            metadata,
        }
    }
}
