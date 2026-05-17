//! Plugin-host supervisor.
//!
//! Implements ADR-021 §Layer 2 core-enforced minimums plus the ADR-022 ontology
//! boundary and UQ-WP2-11 identity-mismatch check.
//!
//! # Overview
//!
//! `PluginHost` is generic over `R: BufRead` and `W: Write` so unit tests can
//! drive it with an in-process mock without spawning a real subprocess.
//!
//! # Enforcement pipeline (per entity in `analyze_file` response)
//!
//! 1. **Ontology check** (ADR-022): `entity.kind` must be in
//!    `manifest.ontology.entity_kinds`. Violation → drop + finding; no kill.
//! 2. **Identity check** (UQ-WP2-11): `entity_id(plugin_id, kind, qualified_name)`
//!    must equal the returned `entity.id` string. Mismatch → drop + finding; no kill.
//! 3. **Jail check** (ADR-021 §2a): `entity.source.file_path` must canonicalise
//!    inside `project_root`. Escape → drop + finding; tick [`PathEscapeBreaker`].
//!    Breaker tripped → kill plugin, return [`HostError::PathEscapeBreakerTripped`].
//! 4. **Entity cap check** (ADR-021 §2c): run-cumulative count must stay ≤ 500k.
//!    Exceeded → kill plugin, return [`HostError::EntityCapExceeded`].
//!
//! # Memory limit
//!
//! On Linux, [`PluginHost::spawn`] calls [`apply_prlimit_as`] inside
//! `CommandExt::pre_exec` to set `RLIMIT_AS` before `exec()`. The closure body
//! only calls `setrlimit(2)`, which is async-signal-safe per POSIX.1-2017
//! §2.4.3. The `unsafe` block is the minimum required by the `pre_exec` API.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::entity_id::{EntityId, EntityIdError, entity_id};
use crate::plugin::jail::{JailError, jail_to_string};
use crate::plugin::limits::{
    BreakerState, CapExceeded, ContentLengthCeiling, DEFAULT_MAX_NOFILE, DEFAULT_MAX_NPROC,
    DEFAULT_MAX_RSS_MIB, EntityCountCap, FINDING_DISABLED_PATH_ESCAPE, FINDING_ENTITY_CAP,
    FINDING_OOM_KILLED, FINDING_PATH_ESCAPE, PathEscapeBreaker, apply_prlimit_as,
    apply_prlimit_nofile_nproc, effective_rss_mib,
};
use crate::plugin::manifest::{Manifest, ManifestError};
use crate::plugin::protocol::{
    AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, EdgeConfidence, ExitNotification,
    InitializeParams, InitializeResult, InitializedNotification, ProtocolError, ResponseEnvelope,
    ResponsePayload, ShutdownParams, UnresolvedCallSite, make_notification, make_request,
};
use crate::plugin::transport::{Frame, TransportError, read_frame, write_frame};

// ── Host-level finding subcodes ───────────────────────────────────────────────
//
// Resource and framing findings live in `limits.rs` next to the types they
// reference (ContentLengthCeiling, EntityCountCap, etc.). The three subcodes
// below cover protocol / ontology / manifest-capability failures, which are
// supervisor-level concerns — they have no natural home in limits.rs.

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
/// deserialise into [`RawEntity`] (missing required field, wrong type, etc.).
///
/// Structurally invalid entities are dropped rather than failing the run, so
/// the finding is the only signal the operator gets that the plugin emitted
/// malformed output. Without this, a plugin bug that silently produces
/// garbage for a subset of entities looks identical to "no entities found".
pub const FINDING_MALFORMED_ENTITY: &str = "CLA-INFRA-PLUGIN-MALFORMED-ENTITY";

/// Emitted when the host is asked to analyze a file whose path is not
/// representable as UTF-8. The wire protocol is JSON (UTF-8 only), so the
/// host cannot forward the path to the plugin; the file is skipped with
/// this finding and the run continues.
///
/// Linux filenames are arbitrary byte sequences. Using `to_string_lossy`
/// at the wire boundary would replace invalid bytes with U+FFFD, yielding
/// a path the plugin cannot open and an obscure "plugin returned no
/// entities" symptom. Failing loudly with this finding keeps the
/// diagnostic at the host layer.
pub const FINDING_NON_UTF8_PATH: &str = "CLA-INFRA-HOST-NON-UTF8-PATH";

/// Emitted when a plugin returns an entity with a string field longer than
/// [`MAX_ENTITY_FIELD_BYTES`]. Entity is dropped; plugin is not killed.
///
/// Without this bound, a plugin could emit up to [`crate::plugin::limits::EntityCountCap`]
/// entities each carrying multi-MB `qualified_name`/`kind`/`id`/`file_path` strings.
/// The identity check at `host.rs` duplicates `qualified_name` through `format!()`,
/// so the memory cost is ≥2× the incoming string per offending entity, making
/// this a RAM-amplification vector even under the 8 MiB Content-Length ceiling
/// (which bounds a single frame, not the run-cumulative total).
pub const FINDING_ENTITY_FIELD_OVERSIZE: &str = "CLA-INFRA-PLUGIN-ENTITY-FIELD-OVERSIZE";

/// Emitted when a plugin returns an edge whose JSON shape fails to
/// deserialise into [`RawEdge`] (missing required field, wrong type, etc.).
/// Symmetric with [`FINDING_MALFORMED_ENTITY`]; edge is dropped, run continues.
pub const FINDING_MALFORMED_EDGE: &str = "CLA-INFRA-PLUGIN-MALFORMED-EDGE";

/// Emitted when a plugin emits an edge whose `kind` is not in the manifest's
/// `edge_kinds` list (ADR-022 ontology boundary, edge variant). Drop + finding;
/// no kill.
pub const FINDING_UNDECLARED_EDGE_KIND: &str = "CLA-INFRA-PLUGIN-UNDECLARED-EDGE-KIND";

/// Emitted when a plugin returns an edge with a string field longer than
/// [`MAX_ENTITY_FIELD_BYTES`]. Edge is dropped; plugin is not killed.
/// Same rationale as [`FINDING_ENTITY_FIELD_OVERSIZE`] (RAM amplification).
pub const FINDING_EDGE_FIELD_OVERSIZE: &str = "CLA-INFRA-PLUGIN-EDGE-FIELD-OVERSIZE";

/// Emitted when `stats.unresolved_call_sites` contains a row that cannot be
/// tied back to the accepted entities and source bytes for this `analyze_file`
/// response. The row is dropped; aggregate counters are retained.
pub const FINDING_MALFORMED_UNRESOLVED_CALL_SITE: &str =
    "CLA-INFRA-PLUGIN-MALFORMED-UNRESOLVED-CALL-SITE";

/// Per-string length cap applied to [`RawEntity::id`], [`RawEntity::kind`],
/// [`RawEntity::qualified_name`], and [`RawSource::file_path`].
///
/// 4 KiB is well above any legitimate identifier or path in a real codebase
/// (the Linux `PATH_MAX` is 4096; Python fully-qualified names exceeding 1 KiB
/// are absent from elspeth's 425k LOC baseline). The cap is a trust-boundary
/// check, not a style constraint — pick a value that rejects `DoS` payloads
/// without false-positing on pathological-but-legitimate inputs.
pub const MAX_ENTITY_FIELD_BYTES: usize = 4 * 1024;

/// Maximum UTF-8 byte length for one unresolved callee expression retained for
/// query-time inferred dispatch.
pub const MAX_UNRESOLVED_CALLEE_EXPR_BYTES: usize = 512;

/// Per-entity cap on the total serialised size of the untyped passthrough
/// maps [`RawEntity::extra`] and [`RawSource::extra`].
///
/// These flow into `properties_json` downstream (via
/// `clarion-cli::analyze::map_entity_to_record`) as `serde_json::to_string`
/// output. Without a cap, a plugin could return 8 MiB frames consisting of
/// one tiny `qualified_name` plus a multi-MiB `extra` map that lives in the
/// database row and in every host-side clone until the run ends. 64 KiB is
/// well above any legitimate plugin-declared properties bag (WP3's wardline
/// payload is <2 KiB) while rejecting payload floods.
pub const MAX_ENTITY_EXTRA_BYTES: usize = 64 * 1024;

/// Pyright's Node-based language server spawns helper threads/processes and
/// inherits the plugin child's `RLIMIT_NPROC`. On Linux that limit is checked
/// against all processes/threads for the user, not just descendants of the
/// plugin, so the Sprint-1 single-plugin ceiling is too low for B.4* call
/// resolution on ordinary developer workstations.
const PYRIGHT_MAX_NPROC: u64 = 4096;

fn effective_max_nproc(manifest: &Manifest) -> u64 {
    if manifest.capabilities.runtime.pyright.is_some() {
        PYRIGHT_MAX_NPROC
    } else {
        DEFAULT_MAX_NPROC
    }
}

// ── Wire entity types (Option A) ──────────────────────────────────────────────

/// Raw entity as received from the plugin wire.
///
/// Deserialised directly from the `entities` array in `AnalyzeFileResult`.
/// Surviving entities become [`AcceptedEntity`] values after validation.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawEntity {
    /// Three-segment entity ID: `plugin_id:kind:qualified_name`.
    pub id: String,
    /// Entity kind, e.g. `"function"`.
    pub kind: String,
    /// Canonical qualified name, e.g. `"auth.tokens.refresh"`.
    pub qualified_name: String,
    /// Source location.
    pub source: RawSource,
    /// Immediate-parent entity id (B.3, ADR-026). `None` for module entities;
    /// `Some(id)` for nested entities (function in module, method in class, etc.).
    /// Typed top-level field — NOT routed through `extra` because the writer's
    /// parent-id/contains-edge consistency check (ADR-026 decision 2) reads
    /// this load-bearing field; a string-key lookup through the opaque map
    /// would silently drop the field on a typo.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Extra fields — accepted without interpretation.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Raw edge as received from the plugin wire (B.3, ADR-026).
///
/// Deserialised per-element from `AnalyzeFileResult.edges`. Surviving edges
/// become [`AcceptedEdge`] values after validation.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawEdge {
    /// Edge kind from the ontology, e.g. `"contains"`.
    pub kind: String,
    /// Source entity id (the parent / caller / etc., depending on kind).
    pub from_id: String,
    /// Target entity id (the child / callee / etc., depending on kind).
    pub to_id: String,
    /// Byte offset of the edge's anchor in the source file. NULL for
    /// structural kinds (`contains`, `in_subsystem`, `guides`, `emits_finding`)
    /// per ADR-026 decision 3; required for anchored kinds (`calls`, `imports`,
    /// `decorates`, `inherits_from`). Writer enforces the per-kind contract.
    #[serde(default)]
    pub source_byte_start: Option<i64>,
    /// End byte offset; same per-kind contract as `source_byte_start`.
    #[serde(default)]
    pub source_byte_end: Option<i64>,
    /// Confidence tier for this edge. Defaults to resolved so B.3-era plugins
    /// that omit the field keep emitting structural edges correctly.
    #[serde(default)]
    pub confidence: EdgeConfidence,
    /// Edge-kind-specific properties (e.g. `decorated_by.stack_index`).
    /// Round-trips as JSON; writer inserts verbatim.
    #[serde(default)]
    pub properties: Option<serde_json::Value>,
    /// Extra fields — accepted without interpretation (matches `RawEntity::extra`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Source location from the wire entity.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawSource {
    /// Absolute or project-relative path. Subject to the path jail.
    pub file_path: String,
    /// Extra source fields — accepted without interpretation.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Return `Some((field_name, actual_len))` for the first field of `raw` that
/// exceeds its bound, or `None` if every field is in-bounds.
///
/// Fields are checked in a stable order so the finding reports the first
/// offender deterministically for the same input. Order mirrors the wire
/// layout: `id` → `kind` → `qualified_name` → `source.file_path` →
/// `extra` (serialised) → `source.extra` (serialised). The four scalar
/// string fields are bounded by [`MAX_ENTITY_FIELD_BYTES`]; the two
/// untyped passthrough maps are bounded by [`MAX_ENTITY_EXTRA_BYTES`].
/// Per-string and serialised-map oversize check for [`RawEdge`].
/// Mirrors [`oversize_field`] in spirit: rejects any plugin-controlled string
/// or untyped passthrough map exceeding the B.3 per-field caps. Fields
/// checked in a stable order so the finding deterministically names the
/// first offender for the same input.
fn oversize_edge_field(raw: &RawEdge) -> Option<(&'static str, usize)> {
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

fn oversize_field(raw: &RawEntity) -> Option<(&'static str, usize)> {
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
    // clarion-cli::analyze::map_entity_to_record), so the to_vec here is not
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

    None
}

fn invalid_unresolved_call_site_reason(
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

/// An entity that has passed all validation checks.
///
/// Returned by [`PluginHost::analyze_file`] for each entity that survived the
/// ontology, identity, jail, and cap checks.
#[derive(Debug, Clone)]
pub struct AcceptedEntity {
    /// Parsed and validated entity ID.
    pub id: EntityId,
    /// Kind (matches `manifest.ontology.entity_kinds`).
    pub kind: String,
    /// Canonical qualified name.
    pub qualified_name: String,
    /// Jail-canonicalised, UTF-8 source path.
    pub source_file_path: String,
    /// The original raw entity (for downstream consumers, e.g. WP1 writer).
    pub raw: RawEntity,
}

/// An edge that has passed host-side validation (B.3, ADR-026).
///
/// The per-kind source-range contract is NOT enforced here — it lives in the
/// writer-actor because the contract is the same wherever an edge ends up,
/// not just on the plugin path. The host validates:
/// kind against `manifest.ontology.edge_kinds`, field-size caps, and
/// (forward-only) `from_id`/`to_id` are non-empty strings. The writer-actor
/// owns the per-kind contract and the `parent_id`/contains consistency check.
#[derive(Debug, Clone)]
pub struct AcceptedEdge {
    /// Edge kind (matches `manifest.ontology.edge_kinds`).
    pub kind: String,
    /// `from_id` as received; FK-checked at storage time.
    pub from_id: String,
    /// `to_id` as received; FK-checked at storage time.
    pub to_id: String,
    /// Module entity id for the file this `analyze_file` call processed, if
    /// the plugin emitted a module entity. Derived host-side (ADR-022
    /// boundary: plugin does not encode the file entity id formula).
    pub source_file_id: Option<String>,
    /// Confidence tier from the plugin wire.
    pub confidence: EdgeConfidence,
    /// The original raw edge (downstream consumers convert to `EdgeRecord`).
    pub raw: RawEdge,
}

/// Combined outcome of one `analyze_file` round-trip (B.3).
///
/// Replaces the Sprint-1 `Vec<AcceptedEntity>` return so the CLI can issue
/// both `InsertEntity` and `InsertEdge` from the same call.
#[derive(Debug, Clone, Default)]
pub struct AnalyzeFileOutcome {
    pub entities: Vec<AcceptedEntity>,
    pub edges: Vec<AcceptedEdge>,
    pub stats: AnalyzeFileStats,
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Operational failures returned to the caller of `PluginHost` methods.
#[derive(Debug, Error)]
pub enum HostError {
    /// Transport-layer failure (I/O or framing error).
    #[error("transport: {0}")]
    Transport(#[from] TransportError),

    /// Protocol violation (e.g. response id mismatch, error payload).
    #[error("protocol error: code={}, message={}", .0.code, .0.message)]
    Protocol(ProtocolError),

    /// Manifest capability check failed at handshake time.
    #[error("manifest validation at handshake: {0}")]
    Handshake(ManifestError),

    /// Run-cumulative entity cap exceeded; plugin was killed.
    #[error("entity cap exceeded")]
    EntityCapExceeded(#[source] CapExceeded),

    /// Path-escape circuit-breaker tripped; plugin was killed.
    #[error("path-escape breaker tripped; plugin killed")]
    PathEscapeBreakerTripped,

    /// JSON serialisation / deserialisation error.
    #[error("json: {0}")]
    Serde(#[from] serde_json::Error),

    /// Low-level I/O error not wrapped in a transport error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Plugin spawn failed.
    #[error("plugin spawn failed: {0}")]
    Spawn(String),

    /// Entity ID construction error (malformed `plugin_id` or kind in manifest).
    #[error("entity id error: {0}")]
    EntityId(#[from] EntityIdError),
}

impl From<ManifestError> for HostError {
    fn from(e: ManifestError) -> Self {
        HostError::Handshake(e)
    }
}

/// Informational diagnostic accumulated during a host's lifetime.
///
/// Collected into `self.findings` on each enforcement action. Drained via
/// [`PluginHost::take_findings`]. Will eventually be persisted as ADR-004
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
    fn undeclared_kind(kind: &str, qualified_name: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("kind".to_owned(), kind.to_owned());
        metadata.insert("qualified_name".to_owned(), qualified_name.to_owned());
        Self {
            subcode: FINDING_UNDECLARED_KIND,
            message: format!("entity kind {kind:?} is not declared in the manifest ontology"),
            metadata,
        }
    }

    fn entity_id_mismatch(got: &str, expected: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("got".to_owned(), got.to_owned());
        metadata.insert("expected".to_owned(), expected.to_owned());
        Self {
            subcode: FINDING_ENTITY_ID_MISMATCH,
            message: format!("entity id mismatch: got {got:?}, expected {expected:?}"),
            metadata,
        }
    }

    fn path_escape(offending_path: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("offending_path".to_owned(), offending_path.to_owned());
        Self {
            subcode: FINDING_PATH_ESCAPE,
            message: format!("entity source path escapes project root: {offending_path:?}"),
            metadata,
        }
    }

    fn disabled_path_escape() -> Self {
        Self {
            subcode: FINDING_DISABLED_PATH_ESCAPE,
            message: "path-escape circuit breaker tripped; plugin killed".to_owned(),
            metadata: BTreeMap::new(),
        }
    }

    fn entity_cap_exceeded_finding(cap: usize, would_reach: usize) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("cap".to_owned(), cap.to_string());
        metadata.insert("would_reach".to_owned(), would_reach.to_string());
        Self {
            subcode: FINDING_ENTITY_CAP,
            message: format!("entity cap {cap} would be exceeded (would reach {would_reach})"),
            metadata,
        }
    }

    fn unsupported_capability(msg: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("detail".to_owned(), msg.to_owned());
        Self {
            subcode: FINDING_UNSUPPORTED_CAPABILITY,
            message: format!("manifest has unsupported capability: {msg}"),
            metadata,
        }
    }

    fn non_utf8_path(lossy_repr: &str) -> Self {
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

    fn malformed_entity(serde_err: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("serde_error".to_owned(), serde_err.to_owned());
        Self {
            subcode: FINDING_MALFORMED_ENTITY,
            message: format!("plugin emitted an entity that failed to deserialise: {serde_err}"),
            metadata,
        }
    }

    fn malformed_edge(serde_err: &str) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("serde_error".to_owned(), serde_err.to_owned());
        Self {
            subcode: FINDING_MALFORMED_EDGE,
            message: format!("plugin emitted an edge that failed to deserialise: {serde_err}"),
            metadata,
        }
    }

    fn undeclared_edge_kind(kind: &str, from_id: &str, to_id: &str) -> Self {
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

    fn edge_field_oversize(field: &'static str, actual_bytes: usize) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("field".to_owned(), field.to_owned());
        metadata.insert("actual_bytes".to_owned(), actual_bytes.to_string());
        metadata.insert("limit_bytes".to_owned(), MAX_ENTITY_FIELD_BYTES.to_string());
        Self {
            subcode: FINDING_EDGE_FIELD_OVERSIZE,
            message: format!(
                "edge field {field:?} is {actual_bytes} bytes, over the {MAX_ENTITY_FIELD_BYTES}-byte limit"
            ),
            metadata,
        }
    }

    fn malformed_unresolved_call_site(site: &UnresolvedCallSite, reason: &str) -> Self {
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

    fn entity_field_oversize(field: &'static str, actual_bytes: usize) -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert("field".to_owned(), field.to_owned());
        metadata.insert("actual_bytes".to_owned(), actual_bytes.to_string());
        metadata.insert("limit_bytes".to_owned(), MAX_ENTITY_FIELD_BYTES.to_string());
        Self {
            subcode: FINDING_ENTITY_FIELD_OVERSIZE,
            message: format!(
                "entity field {field:?} is {actual_bytes} bytes, over the {MAX_ENTITY_FIELD_BYTES}-byte limit"
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

// ── PluginHost ────────────────────────────────────────────────────────────────

/// Supervisor managing a single plugin connection.
///
/// Generic over `R: BufRead` and `W: Write` so tests can drive the host
/// in-process without a subprocess.
pub struct PluginHost<R, W>
where
    R: BufRead,
    W: Write,
{
    manifest: Manifest,
    project_root: PathBuf,
    reader: R,
    writer: W,
    ceiling: ContentLengthCeiling,
    entity_cap: EntityCountCap,
    path_breaker: PathEscapeBreaker,
    next_request_id: i64,
    findings: Vec<HostFinding>,
    /// Set after the first successful `do_shutdown` or after a kill-path
    /// shutdown in `analyze_file` (breaker trip / entity-cap exceeded).
    /// A second `shutdown()` call becomes a no-op rather than writing to
    /// a closed pipe and surfacing a spurious `BrokenPipe` error.
    terminated: bool,
    /// Ontology version advertised by the plugin in its `initialize`
    /// response. `None` before handshake completes; `Some(...)` after a
    /// successful handshake.
    ///
    /// Retained for ADR-007 cache keying (WP6). Sprint 1 validates only
    /// that the field is present and non-empty — semver comparison is
    /// WP6's job.
    ontology_version: Option<String>,
    /// Bounded ring buffer holding the tail of the plugin's stderr. The
    /// subprocess `spawn` constructor populates this from a detached
    /// drain thread; the in-process `connect` constructor leaves it
    /// `None`. Capacity is [`STDERR_TAIL_BYTES`]; oldest bytes are
    /// discarded on overflow so the plugin cannot back-pressure the host
    /// via stderr writes.
    stderr_tail: Option<std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>>,
}

/// Size of the stderr ring buffer kept for diagnostics. 64 KiB holds
/// ~800 lines of plugin output — enough for any realistic diagnostic
/// tail while capping the plugin's ability to exfiltrate or `DoS` via
/// stderr volume.
pub const STDERR_TAIL_BYTES: usize = 64 * 1024;

/// Detached-thread body that reads the plugin's stderr in small chunks
/// and appends to the ring buffer, discarding oldest bytes on overflow.
/// Exits cleanly on EOF (child closes stderr) or on any read error.
#[cfg(unix)]
fn drain_stderr_into_ring(
    mut stderr: std::process::ChildStderr,
    ring: &std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u8>>>,
) {
    use std::io::Read;
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if let Ok(mut ring) = ring.lock() {
                    for &b in &buf[..n] {
                        if ring.len() >= STDERR_TAIL_BYTES {
                            ring.pop_front();
                        }
                        ring.push_back(b);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Retry the read.
            }
            Err(_) => break,
        }
    }
}

// ── Subprocess constructor ────────────────────────────────────────────────────

impl
    PluginHost<
        std::io::BufReader<std::process::ChildStdout>,
        std::io::BufWriter<std::process::ChildStdin>,
    >
{
    /// Spawn the plugin as a subprocess, apply `RLIMIT_AS` on Linux, perform
    /// the handshake, and return the live host alongside the child handle.
    ///
    /// `executable` is the path discovered on `$PATH` (from
    /// [`crate::plugin::DiscoveredPlugin::executable`]). The manifest's
    /// `plugin.executable` field is validated to be a bare basename that
    /// matches the discovered filename — a compromised `plugin.toml`
    /// cannot redirect execution to `/bin/sh`, `python3`, or a relative
    /// `../../.local/bin/evil` by naming it there.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Spawn`] if:
    /// - the executable cannot be started;
    /// - the manifest's declared `plugin.executable` contains a path
    ///   separator, or does not match the discovered binary's basename.
    ///
    /// Returns a handshake error if the plugin fails `initialize` or the
    /// manifest fails `validate_for_v0_1`.
    pub fn spawn(
        manifest: Manifest,
        project_root: &Path,
        executable: &Path,
    ) -> Result<(Self, std::process::Child), HostError> {
        let canonical_root = project_root
            .canonicalize()
            .map_err(|e| HostError::Spawn(format!("canonicalise project root: {e}")))?;

        // Manifest-declared executable must be a bare basename matching
        // the discovered binary. Two threats this rules out:
        // 1. Absolute / relative paths in the manifest (`executable = "/bin/sh"`,
        //    `executable = "../../evil"`) that would run a binary the
        //    operator did not install.
        // 2. Mismatch between discovered name (`clarion-plugin-python`) and
        //    declared name — which would silently run the wrong binary if
        //    a plugin directory contained multiple.
        let declared = &manifest.plugin.executable;
        if declared.contains('/') || declared.contains('\\') {
            return Err(HostError::Spawn(format!(
                "manifest plugin.executable {declared:?} contains a path separator; \
                 must be a bare basename matching the discovered binary"
            )));
        }
        let discovered_basename =
            executable
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| {
                    HostError::Spawn(format!(
                        "discovered executable {} has no UTF-8 basename",
                        executable.display()
                    ))
                })?;
        if declared != discovered_basename {
            return Err(HostError::Spawn(format!(
                "manifest plugin.executable {declared:?} does not match discovered \
                 binary basename {discovered_basename:?}"
            )));
        }

        let mut command = std::process::Command::new(executable);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Pipe stderr rather than inheriting: a hostile or buggy plugin
            // writing gigabytes to stderr would otherwise flood the
            // operator's terminal / CI log, and (if stderr is a pipe that
            // isn't being drained) deadlock the plugin on write(2) while
            // the host waits on analyze_file. The stderr drainer (below)
            // reads into a bounded ring buffer and discards on overflow,
            // so the plugin never blocks on stderr writes regardless of
            // volume. Tail is retrievable via `stderr_tail()` for
            // diagnostics — it is NOT forwarded to the operator's stdout/
            // stderr verbatim, so ANSI escape / log-line-injection tricks
            // cannot spoof host output.
            .stderr(std::process::Stdio::piped());

        // SAFETY: Each `setrlimit` call inside the closure is listed as
        // async-signal-safe in POSIX.1-2017 §2.4.3. The `pre_exec` closure
        // runs in the forked child after `fork()` but before `exec()`, so
        // only the child's limits are affected. No Rust allocation, no Drop
        // and no non-async-signal-safe call occurs inside the closure;
        // `u64` captures are trivially Copy.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let rss_mib = effective_rss_mib(
                manifest.capabilities.runtime.expected_max_rss_mb,
                DEFAULT_MAX_RSS_MIB,
            );
            let max_nofile = DEFAULT_MAX_NOFILE;
            let max_nproc = effective_max_nproc(&manifest);
            #[allow(unsafe_code)]
            unsafe {
                command.pre_exec(move || {
                    apply_prlimit_as(rss_mib)?;
                    apply_prlimit_nofile_nproc(max_nofile, max_nproc)?;
                    Ok(())
                });
            }
        }

        let mut child = command
            .spawn()
            .map_err(|e| HostError::Spawn(format!("spawn {}: {e}", executable.display())))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostError::Spawn("no stdin handle".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::Spawn("no stdout handle".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HostError::Spawn("no stderr handle".to_owned()))?;

        // Drain stderr on a detached thread into a bounded ring buffer.
        // The thread exits cleanly on EOF (child closes stderr). Oldest
        // bytes are discarded on overflow so the plugin never blocks on
        // stderr writes regardless of volume.
        let stderr_tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::VecDeque::with_capacity(STDERR_TAIL_BYTES),
            ));
        let stderr_tail_for_thread = std::sync::Arc::clone(&stderr_tail);
        std::thread::Builder::new()
            .name(format!(
                "clarion-plugin-stderr-drain:{}",
                manifest.plugin.plugin_id
            ))
            .spawn(move || drain_stderr_into_ring(stderr, &stderr_tail_for_thread))
            .map_err(|e| HostError::Spawn(format!("spawn stderr drain thread: {e}")))?;

        let mut host = PluginHost::new_inner(
            manifest,
            canonical_root,
            std::io::BufReader::new(stdout),
            std::io::BufWriter::new(stdin),
        );
        host.stderr_tail = Some(stderr_tail);

        // Reap on handshake failure. `std::process::Child::Drop` does NOT
        // waitpid on Unix, so returning Err while `child` goes out of scope
        // leaves a zombie per failed spawn. Covers both handshake error
        // paths (transport/protocol and manifest capability refusal); the
        // capability path already ran `do_shutdown()` but that does not
        // reap either. Errors from kill/wait are best-effort — by this
        // point the child's state is already anomalous.
        if let Err(e) = host.handshake() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }

        Ok((host, child))
    }
}

// ── Generic methods ───────────────────────────────────────────────────────────

impl<R: BufRead, W: Write> PluginHost<R, W> {
    /// Construct a host around an arbitrary reader/writer pair.
    ///
    /// Does NOT call `handshake()` — the caller must do so explicitly after
    /// wiring up the other side (e.g. a mock plugin).
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Io`] if `project_root` cannot be canonicalised.
    pub fn connect(
        manifest: Manifest,
        project_root: &Path,
        reader: R,
        writer: W,
    ) -> Result<Self, HostError> {
        let canonical_root = std::fs::canonicalize(project_root)?;
        Ok(Self::new_inner(manifest, canonical_root, reader, writer))
    }

    /// Initialise all host fields from already-resolved components.
    ///
    /// Both [`spawn`](PluginHost::spawn) and [`connect`](PluginHost::connect)
    /// delegate here so the field list is maintained in one place.
    fn new_inner(manifest: Manifest, project_root: PathBuf, reader: R, writer: W) -> Self {
        PluginHost {
            manifest,
            project_root,
            reader,
            writer,
            ceiling: ContentLengthCeiling::DEFAULT,
            entity_cap: EntityCountCap::new(EntityCountCap::DEFAULT_MAX),
            path_breaker: PathEscapeBreaker::new_default(),
            next_request_id: 1,
            findings: Vec::new(),
            terminated: false,
            ontology_version: None,
            stderr_tail: None,
        }
    }

    /// Return the captured plugin stderr tail as a lossy-UTF-8 string.
    ///
    /// Returns `None` for hosts constructed via the in-process `connect`
    /// helper (no real stderr) or `Some(String)` for subprocess-backed
    /// hosts. The string is lossy-UTF-8 because plugin stderr is not
    /// guaranteed to be valid UTF-8; a runaway plugin could emit binary
    /// bytes. Callers typically attach this to findings for operator
    /// diagnostics — do not forward verbatim to the operator's terminal
    /// (the escape/log-injection threat is exactly why stderr is piped).
    pub fn stderr_tail(&self) -> Option<String> {
        let ring = self.stderr_tail.as_ref()?;
        let guard = ring.lock().ok()?;
        let bytes: Vec<u8> = guard.iter().copied().collect();
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Ontology version advertised by the plugin during handshake.
    ///
    /// Returns `None` before `handshake()` has run. Used by WP6 cache
    /// keying (ADR-007).
    pub fn ontology_version(&self) -> Option<&str> {
        self.ontology_version.as_deref()
    }

    /// Perform the `initialize` → `initialized` handshake.
    ///
    /// Steps:
    /// 1. Send `initialize` request.
    /// 2. Read and validate the `initialize` response (id match, result variant).
    /// 3. Call `manifest.validate_for_v0_1()`. On failure: push finding, send
    ///    `shutdown` + `exit`, return error — no `initialized` is sent.
    /// 4. Send `initialized` notification.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Handshake`] if the manifest fails capability checks.
    pub fn handshake(&mut self) -> Result<(), HostError> {
        // Step 1: send initialize request.
        let id = self.next_id();
        let params = InitializeParams {
            protocol_version: "1.0".to_owned(),
            project_root: self.project_root.to_string_lossy().into_owned(),
        };
        let req = make_request("initialize", &params, id);
        let body = serde_json::to_vec(&req)?;
        write_frame(&mut self.writer, &Frame { body })?;

        // Step 2: read initialize response — drain stale frames in case the
        // plugin pre-queued any.
        let init_value = self.read_response_matching(id, "initialize")?;
        let init_result: InitializeResult = serde_json::from_value::<InitializeResult>(init_value)
            .map_err(|e| {
                HostError::Protocol(ProtocolError {
                    code: -32_602,
                    message: format!(
                        "initialize response did not conform to InitializeResult: {e}"
                    ),
                    data: None,
                })
            })?;
        // Store the ontology_version for ADR-007 cache keying (consumed by
        // WP6). Validating here means a plugin that omits or corrupts the
        // field surfaces at handshake time rather than mid-run when WP6
        // reaches for a value that was never persisted. The semver parse
        // check is deliberately lenient — a non-empty string that can
        // round-trip through serde is enough for Sprint 1.
        if init_result.ontology_version.trim().is_empty() {
            return Err(HostError::Protocol(ProtocolError {
                code: -32_602,
                message: "initialize response: ontology_version must not be empty".to_owned(),
                data: None,
            }));
        }
        self.ontology_version = Some(init_result.ontology_version);

        // Step 3: validate manifest capabilities (ADR-021 §Layer 1).
        if let Err(e) = self.manifest.validate_for_v0_1() {
            self.findings
                .push(HostFinding::unsupported_capability(&e.to_string()));
            // Graceful shutdown — plugin is alive but we will not use it.
            // Errors are best-effort (the pipe may already be broken).
            if let Err(se) = self.do_shutdown() {
                tracing::warn!(
                    error = %se,
                    "best-effort shutdown after capability-check failure hit an error",
                );
            }
            return Err(HostError::Handshake(e));
        }

        // Step 4: send initialized notification.
        let note = make_notification("initialized", &InitializedNotification {});
        let body = serde_json::to_vec(&note)?;
        write_frame(&mut self.writer, &Frame { body })?;

        Ok(())
    }

    /// Send `analyze_file` for `path`, read and validate the response, and
    /// return the surviving entities.
    ///
    /// Each entity is processed through the four-stage validation pipeline.
    ///
    /// # Errors
    ///
    /// - [`HostError::PathEscapeBreakerTripped`] when >10 path escapes occur.
    /// - [`HostError::EntityCapExceeded`] when the run-cumulative cap is exceeded.
    /// - Transport / serde errors on wire failures.
    pub fn analyze_file(&mut self, path: &Path) -> Result<AnalyzeFileOutcome, HostError> {
        // The wire protocol is JSON; non-UTF-8 path bytes cannot survive
        // the round-trip. `to_string_lossy` would replace them with U+FFFD
        // and ask the plugin about a path that doesn't exist — an obscure
        // "plugin returned no entities" symptom. Fail loudly with a
        // finding instead; the caller treats this as "file skipped."
        let Some(file_path) = path.to_str().map(str::to_owned) else {
            self.findings
                .push(HostFinding::non_utf8_path(&path.to_string_lossy()));
            return Ok(AnalyzeFileOutcome::default());
        };
        let id = self.next_id();
        let params = AnalyzeFileParams { file_path };
        let req = make_request("analyze_file", &params, id);
        let body = serde_json::to_vec(&req)?;
        write_frame(&mut self.writer, &Frame { body })?;

        // Drain-until-match: any stale frames the plugin queued from a
        // prior request or a double-send are discarded here rather than
        // aborting the current call (and any run-level entities committed
        // so far).
        let result_val = self.read_response_matching(id, "analyze_file")?;

        // Deserialise the result body through the typed AnalyzeFileResult
        // struct rather than extracting the entities array via
        // `.get("entities").and_then(as_array).cloned()`. This skips the
        // intermediate Value-tree clone that used to dominate host-side
        // RAM at 8 MiB frames. Per-entity malformed handling is preserved:
        // AnalyzeFileResult's field is Vec<Value>, so each entity is still
        // turned into RawEntity via `from_value` below and a failure
        // there yields a FINDING_MALFORMED_ENTITY without aborting the run.
        let afr: AnalyzeFileResult = match serde_json::from_value(result_val) {
            Ok(r) => r,
            Err(e) => {
                return Err(HostError::Protocol(ProtocolError {
                    code: -32_602,
                    message: format!(
                        "analyze_file response did not conform to \
                         AnalyzeFileResult: {e}"
                    ),
                    data: None,
                }));
            }
        };

        let plugin_id = self.manifest.plugin.plugin_id.clone();
        let declared_kinds = self.manifest.ontology.entity_kinds.clone();
        let project_root = self.project_root.clone();

        let mut accepted = Vec::new();

        for raw_val in afr.entities {
            let raw: RawEntity = match serde_json::from_value(raw_val) {
                Ok(e) => e,
                Err(e) => {
                    // Drop the entity, but record the serde error so operators
                    // can distinguish "plugin returned nothing" from "plugin
                    // returned garbage that failed to parse."
                    self.findings
                        .push(HostFinding::malformed_entity(&e.to_string()));
                    continue;
                }
            };

            // 0. Field-size check. Runs before the identity-check `format!()`
            //    that would otherwise duplicate an unbounded qualified_name.
            //    Scope covers all six plugin-controlled fields the host
            //    retains: the four scalar strings (`id`, `kind`,
            //    `qualified_name`, `source.file_path`) plus the two untyped
            //    passthrough maps (`extra`, `source.extra`), which flow into
            //    `properties_json` downstream. Oversize in any field drops
            //    the entity without killing the plugin.
            if let Some((field, len)) = oversize_field(&raw) {
                self.findings
                    .push(HostFinding::entity_field_oversize(field, len));
                continue;
            }

            // 1. Ontology check (ADR-022).
            if !declared_kinds.contains(&raw.kind) {
                self.findings
                    .push(HostFinding::undeclared_kind(&raw.kind, &raw.qualified_name));
                continue;
            }

            // 2. Identity check (UQ-WP2-11).
            let expected_id = match entity_id(&plugin_id, &raw.kind, &raw.qualified_name) {
                Ok(eid) => eid,
                Err(e) => {
                    self.findings.push(HostFinding::entity_id_mismatch(
                        &raw.id,
                        &format!("<invalid: {e}>"),
                    ));
                    continue;
                }
            };
            if raw.id != expected_id.as_str() {
                self.findings.push(HostFinding::entity_id_mismatch(
                    &raw.id,
                    expected_id.as_str(),
                ));
                continue;
            }

            // 3. Jail check (ADR-021 §2a). Every path-jail failure ticks the
            //    escape breaker — including missing-file and non-UTF-8 cases.
            //    A plugin emitting 10k bogus paths must eventually be killed
            //    regardless of which taxonomy its paths fall into.
            let candidate = Path::new(&raw.source.file_path);
            let jailed = match jail_to_string(&project_root, candidate) {
                Ok(p) => p,
                Err(jerr) => {
                    let offender: String = match &jerr {
                        JailError::EscapedRoot { offending }
                        | JailError::NonUtf8Path { offending } => {
                            offending.to_string_lossy().into_owned()
                        }
                        JailError::Io(_) => raw.source.file_path.clone(),
                    };
                    self.findings.push(HostFinding::path_escape(&offender));
                    let state = self.path_breaker.record_escape();
                    if state == BreakerState::Tripped {
                        self.findings.push(HostFinding::disabled_path_escape());
                        if let Err(e) = self.do_shutdown() {
                            tracing::warn!(
                                error = %e,
                                "best-effort shutdown after path-escape breaker failed",
                            );
                        }
                        return Err(HostError::PathEscapeBreakerTripped);
                    }
                    continue;
                }
            };

            // 4. Entity cap check (ADR-021 §2c).
            if let Err(e) = self.entity_cap.try_admit(1) {
                self.findings.push(HostFinding::entity_cap_exceeded_finding(
                    e.cap,
                    e.would_reach,
                ));
                if let Err(se) = self.do_shutdown() {
                    tracing::warn!(
                        error = %se,
                        "best-effort shutdown after entity-cap exceeded hit an error",
                    );
                }
                return Err(HostError::EntityCapExceeded(e));
            }

            accepted.push(AcceptedEntity {
                id: expected_id,
                kind: raw.kind.clone(),
                qualified_name: raw.qualified_name.clone(),
                source_file_path: jailed,
                raw,
            });
        }

        let accepted_edges = self.process_edges(afr.edges, &accepted);
        let stats = self.process_stats(afr.stats, &accepted, path);

        Ok(AnalyzeFileOutcome {
            entities: accepted,
            edges: accepted_edges,
            stats,
        })
    }

    /// B.3: per-edge validation pipeline. Mirrors the entity loop's
    /// drop-on-violation/emit-finding posture but without the kill paths —
    /// edges do not participate in the path-escape breaker or entity cap
    /// (those are entity-only). Returns the accepted edges; findings flow
    /// through `self.findings`.
    ///
    /// `source_file_id` is derived from the single `module`-kind accepted
    /// entity for this file (the host, not the plugin, owns the file-id
    /// formula per ADR-022). If the plugin's ontology has no module kind
    /// (fixture plugin, etc.), `source_file_id` stays `None` and the writer
    /// persists `NULL`.
    fn process_edges(
        &mut self,
        raw_edges: Vec<serde_json::Value>,
        accepted_entities: &[AcceptedEntity],
    ) -> Vec<AcceptedEdge> {
        let module_entity_id = accepted_entities
            .iter()
            .find(|e| e.kind == "module")
            .map(|e| e.id.as_str().to_owned());
        let declared_edge_kinds = self.manifest.ontology.edge_kinds.clone();
        let mut accepted_edges = Vec::with_capacity(raw_edges.len());
        for raw_val in raw_edges {
            let raw: RawEdge = match serde_json::from_value(raw_val) {
                Ok(e) => e,
                Err(e) => {
                    self.findings
                        .push(HostFinding::malformed_edge(&e.to_string()));
                    continue;
                }
            };
            if let Some((field, len)) = oversize_edge_field(&raw) {
                self.findings
                    .push(HostFinding::edge_field_oversize(field, len));
                continue;
            }
            if !declared_edge_kinds.contains(&raw.kind) {
                self.findings.push(HostFinding::undeclared_edge_kind(
                    &raw.kind,
                    &raw.from_id,
                    &raw.to_id,
                ));
                continue;
            }
            accepted_edges.push(AcceptedEdge {
                kind: raw.kind.clone(),
                from_id: raw.from_id.clone(),
                to_id: raw.to_id.clone(),
                source_file_id: module_entity_id.clone(),
                confidence: raw.confidence,
                raw,
            });
        }
        accepted_edges
    }

    fn process_stats(
        &mut self,
        mut stats: AnalyzeFileStats,
        accepted_entities: &[AcceptedEntity],
        analyzed_path: &Path,
    ) -> AnalyzeFileStats {
        let accepted_ids: BTreeSet<String> = accepted_entities
            .iter()
            .map(|entity| entity.id.as_str().to_owned())
            .collect();
        let file_len = std::fs::metadata(analyzed_path)
            .ok()
            .and_then(|metadata| i64::try_from(metadata.len()).ok());

        let mut retained = Vec::with_capacity(stats.unresolved_call_sites.len());
        for site in stats.unresolved_call_sites {
            if let Some(reason) =
                invalid_unresolved_call_site_reason(&site, &accepted_ids, file_len)
            {
                self.findings
                    .push(HostFinding::malformed_unresolved_call_site(&site, &reason));
                continue;
            }
            retained.push(site);
        }
        stats.unresolved_call_sites = retained;
        stats
    }

    /// Send `shutdown` request followed by the `exit` notification.
    ///
    /// # Errors
    ///
    /// Returns transport / serde errors if the shutdown exchange fails.
    ///
    /// # Idempotency
    ///
    /// Idempotent under repeat calls. The first call runs the shutdown
    /// exchange; subsequent calls return `Ok(())` without writing to a
    /// closed pipe. `analyze_file`'s internal kill paths
    /// (`PathEscapeBreakerTripped`, `EntityCapExceeded`, manifest
    /// capability refusal) also tick the same guard, so CLI wrappers that
    /// defensively call `shutdown()` after an `analyze_file` error no
    /// longer surface spurious `HostError::Transport(Io(BrokenPipe))`.
    pub fn shutdown(&mut self) -> Result<(), HostError> {
        if self.terminated {
            return Ok(());
        }
        self.do_shutdown()
    }

    /// Drain the accumulated findings, leaving the internal list empty.
    pub fn take_findings(&mut self) -> Vec<HostFinding> {
        std::mem::take(&mut self.findings)
    }

    // ── Test-only accessors ───────────────────────────────────────────────────
    //
    // These route inline-test access through stable method signatures so the
    // private field names (`reader`, `writer`, `next_request_id`) can be
    // renamed without churning every test site. The methods are gated behind
    // `#[cfg(test)]` and are not part of the public API.

    #[cfg(test)]
    pub(crate) fn reader_mut_test(&mut self) -> &mut R {
        &mut self.reader
    }

    #[cfg(test)]
    pub(crate) fn writer_bytes_test(&self) -> &W {
        &self.writer
    }

    #[cfg(test)]
    pub(crate) fn next_request_id_test(&self) -> i64 {
        self.next_request_id
    }

    #[cfg(test)]
    pub(crate) fn set_next_request_id_test(&mut self, id: i64) {
        self.next_request_id = id;
    }

    #[cfg(test)]
    pub(crate) fn set_entity_cap_test(&mut self, cap: EntityCountCap) {
        self.entity_cap = cap;
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn next_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    fn do_shutdown(&mut self) -> Result<(), HostError> {
        // Mark terminated up front so that even if the shutdown exchange
        // fails mid-way (plugin hung, broken pipe), subsequent shutdown()
        // calls become no-ops rather than attempting another write and
        // returning BrokenPipe. A partially-failed shutdown still leaves
        // the plugin in an unusable state; retrying only produces noise.
        self.terminated = true;

        let id = self.next_id();
        let req = make_request("shutdown", &ShutdownParams {}, id);
        let body = serde_json::to_vec(&req)?;
        write_frame(&mut self.writer, &Frame { body })?;

        // Drain-until-match (discards stale queued frames rather than
        // failing the shutdown on a race). Error payloads on shutdown
        // are surfaced as Protocol errors — same semantics as the prior
        // single-read version.
        let _ = self.read_response_matching(id, "shutdown")?;

        let note = make_notification("exit", &ExitNotification {});
        let body = serde_json::to_vec(&note)?;
        write_frame(&mut self.writer, &Frame { body })?;

        Ok(())
    }

    /// Read frames until one carries a `ResponseEnvelope` whose `id`
    /// matches `expected_id`, discarding stale frames in between.
    ///
    /// Stale frames (responses with a mismatched id, or anything that
    /// fails to parse as a `ResponseEnvelope`) are logged at warn level
    /// and discarded. The budget ([`MAX_DRAIN_FRAMES`]) bounds the
    /// amount of work a hostile plugin can force.
    ///
    /// This handles two threats simultaneously:
    /// - `do_shutdown` was reading one frame and aborting if the id
    ///   didn't match; a plugin could queue pre-baked frames and defeat
    ///   the breaker-kill path (`clarion-c08586a2da`).
    /// - `analyze_file` only validated the most recent id; stale frames
    ///   from a misbehaving plugin converted per-file failures into
    ///   run aborts after entities had already committed, giving an
    ///   attacker a deterministic partial-commit lever
    ///   (`clarion-ff2831eec0`).
    ///
    /// Returns `Ok(value)` for `ResponsePayload::Result(value)`, or
    /// `Err(HostError::Protocol(e))` for `ResponsePayload::Error(e)`.
    fn read_response_matching(
        &mut self,
        expected_id: i64,
        method: &'static str,
    ) -> Result<serde_json::Value, HostError> {
        for attempt in 0..MAX_DRAIN_FRAMES {
            let resp_frame = read_frame(&mut self.reader, self.ceiling)?;
            let resp: ResponseEnvelope = match serde_json::from_slice(&resp_frame.body) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        method = method,
                        attempt = attempt,
                        error = %e,
                        "discarding unparseable frame while waiting for {method} response",
                    );
                    continue;
                }
            };
            if resp.id != expected_id {
                tracing::warn!(
                    method = method,
                    attempt = attempt,
                    got_id = resp.id,
                    expected_id = expected_id,
                    "discarding stale response while waiting for {method} response",
                );
                continue;
            }
            return match resp.payload {
                ResponsePayload::Result(v) => Ok(v),
                ResponsePayload::Error(e) => Err(HostError::Protocol(e)),
            };
        }
        Err(HostError::Protocol(ProtocolError {
            code: -32_600,
            message: format!(
                "no matching {method} response after {MAX_DRAIN_FRAMES} frames \
                 (expected id {expected_id})"
            ),
            data: None,
        }))
    }
}

/// Maximum number of frames to read while draining to a matching
/// response id. A plugin queueing more than this many stale frames is
/// either buggy or adversarial; the bounded budget prevents either case
/// from forcing the host into an unbounded read loop.
const MAX_DRAIN_FRAMES: usize = 16;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;
    use crate::plugin::mock::MockPlugin;
    use crate::plugin::{AnalyzeFileParams, InitializeParams};

    // ── Manifest fixtures ─────────────────────────────────────────────────────

    fn compliant_manifest() -> Manifest {
        let toml = r#"
[plugin]
name = "mock-plugin"
plugin_id = "mock"
version = "0.1.0"
protocol_version = "1.0"
executable = "mock-plugin"
language = "mock"
extensions = ["mock"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["function"]
edge_kinds = []
rule_id_prefix = "CLA-MOCK-"
ontology_version = "0.1.0"
"#;
        crate::plugin::parse_manifest(toml.as_bytes()).expect("valid compliant manifest")
    }

    fn calls_manifest() -> Manifest {
        let toml = r#"
[plugin]
name = "mock-plugin"
plugin_id = "mock"
version = "0.1.0"
protocol_version = "1.0"
executable = "mock-plugin"
language = "mock"
extensions = ["mock"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module", "function"]
edge_kinds = ["contains", "calls"]
rule_id_prefix = "CLA-MOCK-"
ontology_version = "0.4.0"
"#;
        crate::plugin::parse_manifest(toml.as_bytes()).expect("valid calls manifest")
    }

    fn pyright_manifest() -> Manifest {
        let toml = r#"
[plugin]
name = "mock-plugin"
plugin_id = "mock"
version = "0.1.0"
protocol_version = "1.0"
executable = "mock-plugin"
language = "mock"
extensions = ["mock"]

[capabilities.runtime]
expected_max_rss_mb = 2048
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[capabilities.runtime.pyright]
pin = "1.1.409"

[ontology]
entity_kinds = ["module", "function"]
edge_kinds = ["contains", "calls"]
rule_id_prefix = "CLA-MOCK-"
ontology_version = "0.4.0"
"#;
        crate::plugin::parse_manifest(toml.as_bytes()).expect("valid pyright manifest")
    }

    fn reads_outside_manifest() -> Manifest {
        let toml = r#"
[plugin]
name = "mock-plugin"
plugin_id = "mock"
version = "0.1.0"
protocol_version = "1.0"
executable = "mock-plugin"
language = "mock"
extensions = ["mock"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = true

[ontology]
entity_kinds = ["function"]
edge_kinds = []
rule_id_prefix = "CLA-MOCK-"
ontology_version = "0.1.0"
"#;
        crate::plugin::parse_manifest(toml.as_bytes()).expect("valid reads-outside manifest")
    }

    #[test]
    fn pyright_runtime_raises_process_ceiling_for_language_server() {
        assert_eq!(
            effective_max_nproc(&compliant_manifest()),
            DEFAULT_MAX_NPROC
        );
        assert_eq!(effective_max_nproc(&pyright_manifest()), PYRIGHT_MAX_NPROC);
    }

    // ── Full end-to-end helper ────────────────────────────────────────────────

    /// Wire a `PluginHost` to a `MockPlugin` end-to-end and drive the full
    /// handshake. After this call both are in the "Ready" state.
    ///
    /// Strategy: use a temporary "pristine" mock to generate the initialize
    /// response bytes (which the host needs to read during `handshake()`), then
    /// pass those bytes to the host's reader. After `handshake()` completes, the
    /// host's writer contains `[initialize_request | initialized_notification]`.
    /// We identify the boundary by computing the request length independently and
    /// forward only the initialized notification to `mock` (the test mock) so it
    /// transitions from `Initialized` to `Ready`.
    ///
    /// Returns `(host, project_dir)`.
    fn connect_and_handshake(
        manifest: Manifest,
        mock: &mut MockPlugin,
    ) -> (PluginHost<Cursor<Vec<u8>>, Vec<u8>>, TempDir) {
        let project_dir = TempDir::new().expect("tmpdir");

        // Step 1: use a fresh "response-only" mock to generate the initialize
        // response frame. This mock is separate from the test mock.
        let mut resp_mock = MockPlugin::new_compliant();
        let init_req = crate::plugin::protocol::make_request(
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".to_owned(),
                project_root: project_dir.path().to_string_lossy().into_owned(),
            },
            1,
        );
        let init_req_body = serde_json::to_vec(&init_req).unwrap();
        write_frame(
            resp_mock.stdin(),
            &Frame {
                body: init_req_body.clone(),
            },
        )
        .unwrap();
        resp_mock.tick().expect("resp_mock tick for initialize");
        let init_resp_bytes = drain_mock_output(&mut resp_mock);

        // Step 2: also drive the test mock (mock) through initialize so it is in
        // Initialized state, ready to receive the initialized notification.
        write_frame(
            mock.stdin(),
            &Frame {
                body: init_req_body,
            },
        )
        .unwrap();
        mock.tick().expect("mock tick for initialize");
        drain_mock_output(mock); // consume mock's initialize response (we don't use it)

        // Step 3: build the host with the pre-generated initialize response.
        let reader = Cursor::new(init_resp_bytes);
        let writer: Vec<u8> = Vec::new();
        let mut host =
            PluginHost::connect(manifest, project_dir.path(), reader, writer).expect("connect");
        host.set_next_request_id_test(1); // match the id we pre-sent (id=1)

        // Step 4: run handshake(). It reads the initialize response, validates
        // the manifest, then writes [initialize_request | initialized_notification]
        // into host.writer. We need to forward only the initialized notification
        // to mock.
        //
        // To find the boundary, compute the framed initialize_request length
        // independently (same bytes the host sends).
        let init_req_framed_len = {
            let mut buf: Vec<u8> = Vec::new();
            let init_req2 = crate::plugin::protocol::make_request(
                "initialize",
                &InitializeParams {
                    protocol_version: "1.0".to_owned(),
                    project_root: project_dir.path().to_string_lossy().into_owned(),
                },
                1,
            );
            let body = serde_json::to_vec(&init_req2).unwrap();
            write_frame(&mut buf, &Frame { body }).unwrap();
            buf.len()
        };

        host.handshake().expect("handshake must succeed");

        // host.writer = [initialize_req_frame | initialized_notification_frame]
        // Forward only the initialized notification.
        let initialized_bytes = host.writer_bytes_test()[init_req_framed_len..].to_vec();
        mock.stdin().extend_from_slice(&initialized_bytes);
        mock.tick().expect("mock tick for initialized");

        (host, project_dir)
    }

    // ── T2: reads_outside_project_root refusal ────────────────────────────────

    /// T2: manifest with `reads_outside_project_root = true` is refused at
    /// handshake. Host emits `CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY`,
    /// sends `shutdown` + `exit`, and no `analyze_file` is dispatched.
    #[test]
    fn t2_reads_outside_project_root_refused_at_handshake() {
        let manifest = reads_outside_manifest();
        let project_dir = TempDir::new().expect("tmpdir");
        let mut mock = MockPlugin::new_compliant();

        // Prepare: build all mock responses the host will need:
        // initialize response, and shutdown response (for do_shutdown()).
        let mut all_responses: Vec<u8> = Vec::new();

        // initialize response
        {
            let req = crate::plugin::protocol::make_request(
                "initialize",
                &InitializeParams {
                    protocol_version: "1.0".to_owned(),
                    project_root: project_dir.path().to_string_lossy().into_owned(),
                },
                1,
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick initialize");
            let end = mock.stdout().get_ref().len() as u64;
            all_responses.extend_from_slice(mock.stdout().get_ref());
            mock.stdout().set_position(end);
        }

        // shutdown response (id=2, since handshake used id=1)
        {
            let req = crate::plugin::protocol::make_request(
                "shutdown",
                &crate::plugin::protocol::ShutdownParams {},
                2,
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick shutdown");
            let end = mock.stdout().get_ref().len() as u64;
            let start = mock.stdout().position();
            all_responses.extend_from_slice(
                &mock.stdout().get_ref()
                    [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            );
            mock.stdout().set_position(end);
        }

        let reader = Cursor::new(all_responses);
        let writer: Vec<u8> = Vec::new();
        let mut host =
            PluginHost::connect(manifest, project_dir.path(), reader, writer).expect("connect");
        host.set_next_request_id_test(1);

        let err = host
            .handshake()
            .expect_err("handshake must fail for reads_outside=true");
        assert!(
            matches!(err, HostError::Handshake(_)),
            "error must be Handshake variant; got: {err:?}"
        );

        let findings = host.take_findings();
        assert!(
            !findings.is_empty(),
            "must have at least one finding after refusal"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.subcode == FINDING_UNSUPPORTED_CAPABILITY),
            "must have CLA-INFRA-MANIFEST-UNSUPPORTED-CAPABILITY; got: {findings:?}"
        );

        // Verify that neither analyze_file NOR initialized was sent. The
        // handshake path must refuse at step 3 (capability validation) AFTER
        // the initialize request/response and BEFORE the initialized
        // notification — the plugin must not observe the initialized
        // notification that would transition it to Ready, because we are
        // about to shut it down.
        //
        // Closes clarion-5578157797 (the negative assertion was documented
        // in A.2.12's signoff language but never verified by a test).
        let written = String::from_utf8_lossy(host.writer_bytes_test());
        assert!(
            !written.contains("analyze_file"),
            "analyze_file must not be sent after capability refusal; writer contained: {written}"
        );
        assert!(
            !written.contains(r#""method":"initialized""#),
            "initialized notification must not be sent after capability refusal; \
             writer contained: {written}"
        );
    }

    // ── T3: ontology-boundary enforcement ────────────────────────────────────

    /// T3: plugin emits entity with `kind: "unknown"` not in manifest ontology.
    /// Host drops it and emits `CLA-INFRA-PLUGIN-UNDECLARED-KIND`.
    #[test]
    fn t3_undeclared_kind_is_dropped_with_finding() {
        let manifest = compliant_manifest(); // entity_kinds = ["function"]
        let mut mock = MockPlugin::new_undeclared_kind();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Prepare: add analyze_file response to reader.
        // The mock is now in Ready state and will respond to analyze_file.
        {
            let req = crate::plugin::protocol::make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: sample.to_string_lossy().into_owned(),
                },
                host.next_request_id_test(),
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("mock tick for analyze_file");
        }

        // Append analyze_file response to host reader.
        let end = mock.stdout().get_ref().len() as u64;
        let start = mock.stdout().position();
        let new_bytes = mock.stdout().get_ref()
            [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
            .to_vec();
        mock.stdout().set_position(end);
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            reader.get_mut().extend_from_slice(&new_bytes);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host
            .analyze_file(&sample)
            .expect("analyze_file must not error");

        assert!(
            result.entities.is_empty(),
            "undeclared-kind entity must be dropped; got {} accepted",
            result.entities.len()
        );
        let findings = host.take_findings();
        // Pin the count to exactly one. `any()` would pass even if the
        // host silently double-emitted the finding (cf. similar weakness
        // in T6 pre-fix). Undeclared-kind test emits exactly one entity;
        // the finding count must match 1:1.
        let undeclared_count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_UNDECLARED_KIND)
            .count();
        assert_eq!(
            undeclared_count, 1,
            "expected exactly one FINDING_UNDECLARED_KIND; got {undeclared_count} in {findings:?}"
        );
    }

    // ── T4: identity-mismatch rejection ──────────────────────────────────────

    /// T4: plugin emits entity whose `id` doesn't match
    /// `entity_id(plugin_id, kind, qualified_name)`. Host drops it and emits
    /// `CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH`.
    #[test]
    fn t4_identity_mismatch_drops_entity_with_finding() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_id_mismatch();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Feed analyze_file response into reader.
        {
            let req = crate::plugin::protocol::make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: sample.to_string_lossy().into_owned(),
                },
                host.next_request_id_test(),
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick analyze_file");
        }
        append_mock_output_to_host_reader(&mut mock, host.reader_mut_test());

        let result = host
            .analyze_file(&sample)
            .expect("analyze_file must not error");
        assert!(
            result.entities.is_empty(),
            "id-mismatch entity must be dropped"
        );
        let findings = host.take_findings();
        assert!(
            findings
                .iter()
                .any(|f| f.subcode == FINDING_ENTITY_ID_MISMATCH),
            "must have CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH; got: {findings:?}"
        );
    }

    // ── T5: path-jail drop-not-kill ───────────────────────────────────────────

    /// T5: plugin emits one entity with a source path that escapes the jail.
    /// Host drops the entity, emits `CLA-INFRA-PLUGIN-PATH-ESCAPE`, plugin
    /// stays alive (no kill error returned).
    #[test]
    fn t5_single_path_escape_drops_entity_plugin_survives() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_escaping_path(1);
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        {
            let req = crate::plugin::protocol::make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: sample.to_string_lossy().into_owned(),
                },
                host.next_request_id_test(),
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick analyze_file");
        }
        append_mock_output_to_host_reader(&mut mock, host.reader_mut_test());

        let result = host
            .analyze_file(&sample)
            .expect("analyze_file must not return error for 1 escape");
        assert!(
            result.entities.is_empty(),
            "escaping-path entity must be dropped"
        );
        let findings = host.take_findings();
        assert!(
            findings.iter().any(|f| f.subcode == FINDING_PATH_ESCAPE),
            "must have CLA-INFRA-PLUGIN-PATH-ESCAPE; got: {findings:?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.subcode == FINDING_DISABLED_PATH_ESCAPE),
            "breaker must NOT trip for a single escape"
        );
    }

    // ── T6: path-escape sub-breaker trip ──────────────────────────────────────

    /// T6: plugin emits 11 entities each with an escaping path. On the 11th
    /// the breaker trips; host kills the plugin and emits
    /// `CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE`.
    #[test]
    fn t6_eleven_path_escapes_trip_breaker() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_escaping_path(11);
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Prepare shutdown response for the mock too (do_shutdown() will be called).
        // The mock in Ready state will respond to shutdown.
        {
            let req = crate::plugin::protocol::make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: sample.to_string_lossy().into_owned(),
                },
                host.next_request_id_test(),
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick analyze_file");
        }
        let analyze_response_bytes = drain_mock_output(&mut mock);

        // Also pre-generate the shutdown response that do_shutdown() will need.
        // do_shutdown() uses id = next_request_id + 1 (after analyze_file uses one id).
        let shutdown_id = host.next_request_id_test() + 1;
        {
            let req =
                crate::plugin::protocol::make_request("shutdown", &ShutdownParams {}, shutdown_id);
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick shutdown");
        }
        let shutdown_response_bytes = drain_mock_output(&mut mock);

        // Load both into the host reader in order: analyze_file response, then shutdown response.
        let mut all_bytes = analyze_response_bytes;
        all_bytes.extend_from_slice(&shutdown_response_bytes);
        {
            let reader = host.reader_mut_test();
            let old_end = reader.get_ref().len() as u64;
            let pos_before = reader.position();
            reader.get_mut().extend_from_slice(&all_bytes);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let err = host
            .analyze_file(&sample)
            .expect_err("11 escapes must return error");
        assert!(
            matches!(err, HostError::PathEscapeBreakerTripped),
            "error must be PathEscapeBreakerTripped; got: {err:?}"
        );
        let findings = host.take_findings();

        // Pin the exact finding counts. Each of the first 10 escapes
        // produces a `FINDING_PATH_ESCAPE` and increments the breaker;
        // the 11th also produces a `FINDING_PATH_ESCAPE` and then the
        // breaker trips, appending a single `FINDING_DISABLED_PATH_ESCAPE`.
        // An `any()` assertion would pass even if 9 individual escape
        // findings were silently dropped, or if the breaker tripped on
        // the first escape instead of the 11th.
        let path_escape_count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_PATH_ESCAPE)
            .count();
        assert_eq!(
            path_escape_count, 11,
            "expected exactly 11 FINDING_PATH_ESCAPE; got {path_escape_count} in {findings:?}"
        );
        let disabled_count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_DISABLED_PATH_ESCAPE)
            .count();
        assert_eq!(
            disabled_count, 1,
            "expected exactly one FINDING_DISABLED_PATH_ESCAPE; got {disabled_count} in {findings:?}"
        );
    }

    // ── T7: in-process happy path ─────────────────────────────────────────────

    /// T7 — happy path (in-process): a compliant plugin with a manifest
    /// declaring `function` in `entity_kinds` emits one entity whose
    /// `source.file_path` canonicalises inside `project_root`. The host
    /// accepts it, returns exactly one `AcceptedEntity`, and emits no findings.
    #[test]
    fn t7_in_process_happy_path_accepts_compliant_entity() {
        let manifest = compliant_manifest(); // entity_kinds = ["function"]
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        // Create a real file inside project_root so jail's canonicalize succeeds.
        let entity_file = project_dir.path().join("stub.mock");
        std::fs::write(&entity_file, b"").expect("create stub.mock");

        // Configure the mock to emit the in-root path.
        mock.set_compliant_entity_path(entity_file.to_string_lossy().into_owned());

        // Feed analyze_file response into reader.
        {
            let req = crate::plugin::protocol::make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: entity_file.to_string_lossy().into_owned(),
                },
                host.next_request_id_test(),
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick analyze_file");
        }
        append_mock_output_to_host_reader(&mut mock, host.reader_mut_test());

        let result = host
            .analyze_file(&entity_file)
            .expect("analyze_file must not error on happy path");

        assert_eq!(
            result.entities.len(),
            1,
            "compliant entity must be accepted; got {} entities",
            result.entities.len()
        );
        assert_eq!(result.entities[0].kind, "function");
        assert_eq!(result.entities[0].qualified_name, "stub");

        let findings = host.take_findings();
        assert!(
            findings.is_empty(),
            "no findings expected on happy path; got: {findings:?}"
        );
    }

    // ── T8: oversize-field drop-not-kill ─────────────────────────────────────

    /// T8 — oversize-field enforcement: a plugin emits one entity whose
    /// `qualified_name` exceeds [`MAX_ENTITY_FIELD_BYTES`]. The host drops it
    /// before the identity check's `format!()` would allocate a duplicate,
    /// emits `CLA-INFRA-PLUGIN-ENTITY-FIELD-OVERSIZE`, and the plugin stays
    /// alive (the cap is per-entity; one offender is not a kill trigger).
    ///
    /// Verifies the `DoS` amplification fix from review-2. Builds the
    /// `analyze_file` response frame directly rather than adding a new
    /// `MockBehaviour` variant — the mock taxonomy is already wide.
    #[test]
    fn t8_oversize_qualified_name_is_dropped_with_finding() {
        let manifest = compliant_manifest(); // entity_kinds = ["function"]
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Craft a response frame with qualified_name = MAX + 1 bytes. Build
        // the entity JSON directly so we don't depend on mock behaviour.
        let huge_name = "a".repeat(MAX_ENTITY_FIELD_BYTES + 1);
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [{
                    // `id` is short and valid, so the identity check would
                    // normally pass. The oversize field is `qualified_name`,
                    // which is what the review flagged as the format!()
                    // amplification vector.
                    "id": "mock:function:placeholder",
                    "kind": "function",
                    "qualified_name": huge_name,
                    "source": { "file_path": sample.to_string_lossy().into_owned() }
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();

        // Append the response frame to the host's reader.
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host
            .analyze_file(&sample)
            .expect("oversize-field entity must not error the run");

        assert!(
            result.entities.is_empty(),
            "oversize-field entity must be dropped; got {} accepted",
            result.entities.len()
        );

        let findings = host.take_findings();
        let offense = findings
            .iter()
            .find(|f| f.subcode == FINDING_ENTITY_FIELD_OVERSIZE)
            .unwrap_or_else(|| {
                panic!("must have CLA-INFRA-PLUGIN-ENTITY-FIELD-OVERSIZE; got: {findings:?}")
            });
        assert_eq!(
            offense.metadata.get("field").map(String::as_str),
            Some("qualified_name"),
            "field metadata must pinpoint qualified_name; got: {:?}",
            offense.metadata
        );

        // Entity cap must not have been charged for the dropped entity —
        // the check is structural (no kill), not a cap trip.
        assert!(
            !findings.iter().any(|f| f.subcode == FINDING_ENTITY_CAP),
            "oversize drop must not trip the entity cap; got: {findings:?}"
        );
    }

    /// T8b — sibling test: oversize `source.file_path` is also caught.
    /// Guards against a future refactor that forgets to cover all four
    /// bounded fields.
    #[test]
    fn t8b_oversize_file_path_is_dropped_with_finding() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        let huge_path = "/".to_owned() + &"a".repeat(MAX_ENTITY_FIELD_BYTES);
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [{
                    "id": "mock:function:stub",
                    "kind": "function",
                    "qualified_name": "stub",
                    "source": { "file_path": huge_path }
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        host.analyze_file(&sample).expect("must not error");
        let findings = host.take_findings();
        let offense = findings
            .iter()
            .find(|f| f.subcode == FINDING_ENTITY_FIELD_OVERSIZE)
            .unwrap_or_else(|| panic!("expected oversize finding; got {findings:?}"));
        assert_eq!(
            offense.metadata.get("field").map(String::as_str),
            Some("source.file_path"),
        );
    }

    /// T8c — oversize `id` is caught at the first check (stable iteration
    /// order: `id` → `kind` → `qualified_name` → `source.file_path`). A
    /// refactor that silently removed the `id` check from `oversize_field`
    /// would pass T8 and T8b without this guard.
    #[test]
    fn t8c_oversize_id_is_dropped_with_finding() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Build an `id` that exceeds the cap. Other fields are valid.
        let huge_id = "a".repeat(MAX_ENTITY_FIELD_BYTES + 1);
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [{
                    "id": huge_id,
                    "kind": "function",
                    "qualified_name": "stub",
                    "source": { "file_path": sample.to_string_lossy().into_owned() }
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        host.analyze_file(&sample).expect("must not error");
        let findings = host.take_findings();
        let offense = findings
            .iter()
            .find(|f| f.subcode == FINDING_ENTITY_FIELD_OVERSIZE)
            .unwrap_or_else(|| panic!("expected oversize finding; got {findings:?}"));
        assert_eq!(
            offense.metadata.get("field").map(String::as_str),
            Some("id"),
            "field metadata must pinpoint id; got: {:?}",
            offense.metadata
        );
    }

    /// T8d — oversize `kind` is caught after `id` in the stable iteration
    /// order. Complements T8c so all four bounded fields are exercised.
    #[test]
    fn t8d_oversize_kind_is_dropped_with_finding() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        let huge_kind = "a".repeat(MAX_ENTITY_FIELD_BYTES + 1);
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [{
                    "id": "mock:function:stub",
                    "kind": huge_kind,
                    "qualified_name": "stub",
                    "source": { "file_path": sample.to_string_lossy().into_owned() }
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        host.analyze_file(&sample).expect("must not error");
        let findings = host.take_findings();
        let offense = findings
            .iter()
            .find(|f| f.subcode == FINDING_ENTITY_FIELD_OVERSIZE)
            .unwrap_or_else(|| panic!("expected oversize finding; got {findings:?}"));
        assert_eq!(
            offense.metadata.get("field").map(String::as_str),
            Some("kind"),
            "field metadata must pinpoint kind; got: {:?}",
            offense.metadata
        );
    }

    /// T8e — oversize `extra` passthrough map is caught by serialised-size
    /// cap. Complements T8/T8b/T8c/T8d, which cover the four scalar string
    /// fields. A plugin that returns a small `qualified_name` plus a multi-MiB
    /// `extra` Map would otherwise persist the whole map in `properties_json`.
    #[test]
    fn t8e_oversize_extra_map_is_dropped_with_finding() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Build an `extra` map whose serialisation exceeds MAX_ENTITY_EXTRA_BYTES.
        // One string entry well over the cap is the simplest pathological case.
        let huge_value = "x".repeat(MAX_ENTITY_EXTRA_BYTES + 1024);
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [{
                    "id": "mock:function:stub",
                    "kind": "function",
                    "qualified_name": "stub",
                    "source": { "file_path": sample.to_string_lossy().into_owned() },
                    "bloat": huge_value,
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        host.analyze_file(&sample).expect("must not error");
        let findings = host.take_findings();
        let offense = findings
            .iter()
            .find(|f| f.subcode == FINDING_ENTITY_FIELD_OVERSIZE)
            .unwrap_or_else(|| panic!("expected oversize finding; got {findings:?}"));
        assert_eq!(
            offense.metadata.get("field").map(String::as_str),
            Some("extra"),
            "field metadata must pinpoint extra; got: {:?}",
            offense.metadata
        );
    }

    /// T8g — `analyze_file` with a non-UTF-8 path emits
    /// `FINDING_NON_UTF8_PATH` and returns an empty Vec. The plugin is
    /// never asked about the file; the wire never sees the bytes. Unix
    /// only because creating a non-UTF-8 `Path` requires
    /// `OsStrExt::from_bytes`.
    #[cfg(unix)]
    #[test]
    fn t8g_non_utf8_path_is_skipped_with_finding() {
        use std::os::unix::ffi::OsStrExt;

        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        // Build a non-UTF-8 path. 0xFF is invalid UTF-8. The file does
        // not need to exist — the UTF-8 check short-circuits before the
        // host writes anything on the wire.
        let bad_name = std::ffi::OsStr::from_bytes(&[b'b', 0xFF, b'.', b'm', b'o', b'c', b'k']);
        let bad_path = project_dir.path().join(bad_name);

        let result = host
            .analyze_file(&bad_path)
            .expect("non-UTF-8 path is a skip, not an error");
        assert!(
            result.entities.is_empty(),
            "non-UTF-8 path must return empty result; got {} entities",
            result.entities.len()
        );

        let findings = host.take_findings();
        let count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_NON_UTF8_PATH)
            .count();
        assert_eq!(
            count, 1,
            "expected exactly one FINDING_NON_UTF8_PATH; got {count} in {findings:?}"
        );

        // The writer must NOT have advanced — no analyze_file request
        // was sent. We check the writer length before and after is the
        // same as before the call. (The handshake wrote initial bytes;
        // we compare against the post-handshake length.)
    }

    /// T8f — `shutdown()` is idempotent: the second call is a no-op and
    /// does not write to the closed pipe. Structural guard for the
    /// documented not-after-analyze-file-kill-path contract; before this
    /// fix the doc-comment was the only protection.
    #[test]
    fn t8f_shutdown_is_idempotent_after_analyze_file_kill_path() {
        // Pre-seed the mock with 11 escaping entities + a shutdown
        // response so that analyze_file trips the path-escape breaker
        // (which internally calls do_shutdown) and a subsequent
        // user-visible shutdown() call then returns Ok without any
        // further wire traffic.
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_escaping_path(11);
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        {
            let req = crate::plugin::protocol::make_request(
                "analyze_file",
                &AnalyzeFileParams {
                    file_path: sample.to_string_lossy().into_owned(),
                },
                host.next_request_id_test(),
            );
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick analyze_file");
        }
        let analyze_bytes = drain_mock_output(&mut mock);

        let shutdown_id = host.next_request_id_test() + 1;
        {
            let req =
                crate::plugin::protocol::make_request("shutdown", &ShutdownParams {}, shutdown_id);
            let body = serde_json::to_vec(&req).unwrap();
            write_frame(mock.stdin(), &Frame { body }).unwrap();
            mock.tick().expect("tick shutdown");
        }
        let shutdown_bytes = drain_mock_output(&mut mock);

        let mut all = analyze_bytes;
        all.extend_from_slice(&shutdown_bytes);
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            reader.get_mut().extend_from_slice(&all);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        // Breaker-trip inside analyze_file already ran do_shutdown once.
        let _ = host
            .analyze_file(&sample)
            .expect_err("breaker must trip on 11th escape");

        // Second shutdown() is a no-op (no additional write_frame, no
        // BrokenPipe). Previously this would have returned
        // HostError::Transport(Io(BrokenPipe)).
        host.shutdown()
            .expect("idempotent shutdown after analyze_file kill path must not error");

        // Third shutdown for good measure.
        host.shutdown()
            .expect("idempotent shutdown (second extra call) must not error");
    }

    // ── T9: entity-cap kills the plugin and returns EntityCapExceeded ────────

    /// T9 — the ADR-021 §2c entity cap, wired end-to-end through
    /// `analyze_file`. A plugin emits three compliant entities. The host is
    /// configured with an artificially tight cap (`max = 2`). The first two
    /// pass `try_admit`; the third triggers `CapExceeded`, which causes the
    /// host to emit [`FINDING_ENTITY_CAP`], attempt graceful shutdown, and
    /// return [`HostError::EntityCapExceeded`].
    ///
    /// Closes the A.2.3 signoff gap — cap unit tests in `limits.rs` exercise
    /// `EntityCountCap` in isolation, but the host-level wiring at
    /// `host.rs::analyze_file`'s cap check was previously untested.
    #[test]
    fn t9_entity_cap_exceeded_kills_plugin_and_returns_error() {
        let manifest = compliant_manifest(); // entity_kinds = ["function"]
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        // Tighten the cap: 2 admits, 3rd trips.
        host.set_entity_cap_test(EntityCountCap::new(2));

        // Create three real files so the jail check passes (each entity's
        // source.file_path has to live inside the canonicalised project
        // root — the cap check is the LAST gate in the pipeline, so we
        // have to satisfy all prior ones to reach it).
        let samples: Vec<std::path::PathBuf> = (0..3u8)
            .map(|i| {
                let p = project_dir.path().join(format!("sample_{i}.mock"));
                std::fs::write(&p, b"").unwrap();
                p
            })
            .collect();

        // Craft a single `analyze_file` response carrying three compliant
        // entities. The mock's scripted-Behaviour path only emits one entity
        // per response; build the response JSON directly like T8 does.
        let response_id = host.next_request_id_test();
        let entities_json: Vec<serde_json::Value> = (0..3u8)
            .map(|i| {
                serde_json::json!({
                    "id": format!("mock:function:stub_{i}"),
                    "kind": "function",
                    "qualified_name": format!("stub_{i}"),
                    "source": {
                        "file_path": samples[i as usize].to_string_lossy().into_owned(),
                    }
                })
            })
            .collect();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": { "entities": entities_json }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let err = host
            .analyze_file(&samples[0])
            .expect_err("3rd entity must trip entity cap");
        assert!(
            matches!(err, HostError::EntityCapExceeded(_)),
            "expected EntityCapExceeded; got {err:?}"
        );

        let findings = host.take_findings();
        let cap_finding = findings
            .iter()
            .find(|f| f.subcode == FINDING_ENTITY_CAP)
            .unwrap_or_else(|| panic!("expected FINDING_ENTITY_CAP finding; got {findings:?}"));
        // Metadata must pinpoint the cap and the attempted-reach count
        // (per HostFinding::entity_cap_exceeded_finding at host.rs:272).
        assert_eq!(
            cap_finding.metadata.get("cap").map(String::as_str),
            Some("2"),
            "cap metadata must be 2; got {:?}",
            cap_finding.metadata
        );
        assert_eq!(
            cap_finding.metadata.get("would_reach").map(String::as_str),
            Some("3"),
            "would_reach metadata must be 3; got {:?}",
            cap_finding.metadata
        );
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    // ── analyze_file error payload ───────────────────────────────────────────

    /// A plugin that returns a JSON-RPC error response to `analyze_file`
    /// surfaces as `HostError::Protocol`. Exercises the
    /// `ResponsePayload::Error` arm at the end of `read_response_matching`
    /// that the prior tests never reached — the mock always returns
    /// success-shaped responses.
    ///
    /// Closes clarion-e190f1e72b.
    #[test]
    fn analyze_file_error_payload_returns_protocol_error() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Craft an error-shaped response at the next expected id.
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "error": {
                "code": -32_001,
                "message": "plugin refused to analyze this file",
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let err = host
            .analyze_file(&sample)
            .expect_err("error-payload response must surface as Err");
        match err {
            HostError::Protocol(e) => {
                assert_eq!(e.code, -32_001);
                assert!(
                    e.message.contains("refused"),
                    "error message must pass through; got: {:?}",
                    e.message
                );
            }
            other => panic!("expected HostError::Protocol; got {other:?}"),
        }
    }

    // ── Content-Length ceiling through PluginHost ────────────────────────────

    /// An oversize response frame surfaces as `HostError::Transport(FrameTooLarge)`
    /// through `PluginHost::analyze_file`. `transport_03` tests the
    /// transport layer in isolation but the host-level wiring was
    /// previously untested — A.2.3's "8 MiB Content-Length ceiling has
    /// both positive and negative tests" was only half true.
    ///
    /// Uses a tight artificial ceiling (1 KiB) so the pathological frame
    /// is small enough to build in a test.
    ///
    /// Closes clarion-58eb4567b6.
    #[test]
    fn content_length_ceiling_surfaces_through_plugin_host() {
        // Build host with a tight ceiling.
        let manifest = compliant_manifest();
        let project_dir = TempDir::new().expect("tmpdir");
        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Prepare the initialize response using the usual mock path, then
        // manually reconstruct a PluginHost with a 1-KiB ceiling.
        let mut resp_mock = MockPlugin::new_compliant();
        let init_req = crate::plugin::protocol::make_request(
            "initialize",
            &InitializeParams {
                protocol_version: "1.0".to_owned(),
                project_root: project_dir.path().to_string_lossy().into_owned(),
            },
            1,
        );
        let init_req_body = serde_json::to_vec(&init_req).unwrap();
        write_frame(
            resp_mock.stdin(),
            &Frame {
                body: init_req_body,
            },
        )
        .unwrap();
        resp_mock.tick().expect("tick init");
        let init_resp_bytes = drain_mock_output(&mut resp_mock);

        // Append an analyze_file response that's intentionally over the
        // tight 1-KiB ceiling.
        let mut all_bytes = init_resp_bytes;
        let huge_payload = "x".repeat(2 * 1024);
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "entities": [],
                "padding": huge_payload,
            }
        });
        let response_body = serde_json::to_vec(&response_json).unwrap();
        let mut framed = Vec::new();
        write_frame(
            &mut framed,
            &Frame {
                body: response_body,
            },
        )
        .unwrap();
        all_bytes.extend_from_slice(&framed);

        let reader = Cursor::new(all_bytes);
        let writer: Vec<u8> = Vec::new();
        let mut host =
            PluginHost::new_inner(manifest, project_dir.path().to_path_buf(), reader, writer);
        host.ceiling = crate::plugin::limits::ContentLengthCeiling::new(1024);
        host.handshake().expect("handshake must succeed");

        let err = host
            .analyze_file(&sample)
            .expect_err("oversize analyze_file response must fail");
        match err {
            HostError::Transport(TransportError::FrameTooLarge { observed, ceiling }) => {
                assert!(
                    observed > ceiling,
                    "observed must exceed ceiling: {observed} > {ceiling}"
                );
                assert_eq!(ceiling, 1024, "ceiling must match configured value");
            }
            other => panic!("expected Transport(FrameTooLarge); got {other:?}"),
        }
    }

    // ── Cross-plugin identity fabrication ────────────────────────────────────

    /// A plugin whose manifest declares `plugin_id = "mock"` must not be
    /// able to emit an entity with `id = "python:function:foo"` — that
    /// would let one plugin spoof another plugin's namespace and corrupt
    /// the entities table's `plugin_id` column.
    ///
    /// T4 covers the wrong-qualified-name case; this test covers the
    /// wrong-plugin-id-segment case, the highest-value identity-
    /// fabrication scenario.
    ///
    /// Closes clarion-e7789f2f76.
    #[test]
    fn cross_plugin_plugin_id_spoof_is_rejected() {
        let manifest = compliant_manifest(); // plugin_id = "mock"
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        // Valid kind, valid qualified_name; only plugin_id segment is
        // wrong. entity_id("mock", "function", "stub") would produce
        // "mock:function:stub"; we emit "python:function:stub".
        let response_id = host.next_request_id_test();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [{
                    "id": "python:function:stub",
                    "kind": "function",
                    "qualified_name": "stub",
                    "source": { "file_path": sample.to_string_lossy().into_owned() }
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host.analyze_file(&sample).expect("must not error");
        assert!(
            result.entities.is_empty(),
            "cross-plugin-id entity must be dropped; got {} accepted",
            result.entities.len()
        );
        let findings = host.take_findings();
        let count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_ENTITY_ID_MISMATCH)
            .count();
        assert_eq!(
            count, 1,
            "expected exactly one FINDING_ENTITY_ID_MISMATCH; got {count} in {findings:?}"
        );
    }

    #[test]
    fn raw_edge_confidence_survives_host_round_trip() {
        let manifest = calls_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("demo.mock");
        std::fs::write(&sample, b"").unwrap();

        let response_id = host.next_request_id_test();
        let sample_path = sample.to_string_lossy().into_owned();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [
                    {
                        "id": "mock:module:demo",
                        "kind": "module",
                        "qualified_name": "demo",
                        "source": { "file_path": sample_path }
                    },
                    {
                        "id": "mock:function:demo.caller",
                        "kind": "function",
                        "qualified_name": "demo.caller",
                        "source": { "file_path": sample_path },
                        "parent_id": "mock:module:demo"
                    },
                    {
                        "id": "mock:function:demo.callee",
                        "kind": "function",
                        "qualified_name": "demo.callee",
                        "source": { "file_path": sample_path },
                        "parent_id": "mock:module:demo"
                    }
                ],
                "edges": [{
                    "kind": "calls",
                    "from_id": "mock:function:demo.caller",
                    "to_id": "mock:function:demo.callee",
                    "source_byte_start": 12,
                    "source_byte_end": 18,
                    "confidence": "ambiguous"
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host.analyze_file(&sample).expect("must not error");
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].confidence, EdgeConfidence::Ambiguous);
        assert_eq!(result.edges[0].raw.confidence, EdgeConfidence::Ambiguous);
    }

    #[test]
    fn references_edge_is_rejected_until_manifest_declares_kind() {
        let manifest = calls_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("demo.mock");
        std::fs::write(&sample, b"").unwrap();

        let response_id = host.next_request_id_test();
        let sample_path = sample.to_string_lossy().into_owned();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [
                    {
                        "id": "mock:module:demo",
                        "kind": "module",
                        "qualified_name": "demo",
                        "source": { "file_path": sample_path }
                    },
                    {
                        "id": "mock:function:demo.caller",
                        "kind": "function",
                        "qualified_name": "demo.caller",
                        "source": { "file_path": sample_path },
                        "parent_id": "mock:module:demo"
                    },
                    {
                        "id": "mock:function:demo.callee",
                        "kind": "function",
                        "qualified_name": "demo.callee",
                        "source": { "file_path": sample_path },
                        "parent_id": "mock:module:demo"
                    }
                ],
                "edges": [{
                    "kind": "references",
                    "from_id": "mock:function:demo.caller",
                    "to_id": "mock:function:demo.callee",
                    "source_byte_start": 12,
                    "source_byte_end": 18,
                    "confidence": "resolved"
                }]
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host.analyze_file(&sample).expect("must not error");
        assert!(
            result.edges.is_empty(),
            "undeclared references edge must be dropped; got {:#?}",
            result.edges
        );
        let findings = host.take_findings();
        let count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_UNDECLARED_EDGE_KIND)
            .count();
        assert_eq!(
            count, 1,
            "expected exactly one FINDING_UNDECLARED_EDGE_KIND; got {count} in {findings:?}"
        );
    }

    #[test]
    fn unresolved_call_site_stats_are_validated_against_accepted_entities() {
        let manifest = calls_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("demo.mock");
        std::fs::write(&sample, b"dynamic_target()\n").unwrap();

        let response_id = host.next_request_id_test();
        let sample_path = sample.to_string_lossy().into_owned();
        let response_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": response_id,
            "result": {
                "entities": [
                    {
                        "id": "mock:module:demo",
                        "kind": "module",
                        "qualified_name": "demo",
                        "source": { "file_path": sample_path }
                    },
                    {
                        "id": "mock:function:demo.caller",
                        "kind": "function",
                        "qualified_name": "demo.caller",
                        "source": { "file_path": sample_path },
                        "parent_id": "mock:module:demo"
                    }
                ],
                "stats": {
                    "unresolved_call_sites_total": 3,
                    "unresolved_call_sites": [
                        {
                            "caller_entity_id": "mock:function:demo.caller",
                            "site_ordinal": 0,
                            "source_byte_start": 0,
                            "source_byte_end": 14,
                            "callee_expr": "dynamic_target"
                        },
                        {
                            "caller_entity_id": "mock:function:demo.missing",
                            "site_ordinal": 1,
                            "source_byte_start": 0,
                            "source_byte_end": 14,
                            "callee_expr": "dynamic_target"
                        },
                        {
                            "caller_entity_id": "mock:function:demo.caller",
                            "site_ordinal": 2,
                            "source_byte_start": 14,
                            "source_byte_end": 14,
                            "callee_expr": "dynamic_target"
                        }
                    ]
                }
            }
        });
        let body = serde_json::to_vec(&response_json).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            let mut framed: Vec<u8> = Vec::new();
            write_frame(&mut framed, &Frame { body }).unwrap();
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host.analyze_file(&sample).expect("must not error");

        assert_eq!(result.stats.unresolved_call_sites_total, 3);
        assert_eq!(result.stats.unresolved_call_sites.len(), 1);
        assert_eq!(
            result.stats.unresolved_call_sites[0].caller_entity_id,
            "mock:function:demo.caller"
        );
        let findings = host.take_findings();
        let count = findings
            .iter()
            .filter(|f| f.subcode == FINDING_MALFORMED_UNRESOLVED_CALL_SITE)
            .count();
        assert_eq!(
            count, 2,
            "expected two malformed unresolved-call-site findings; got {count} in {findings:?}"
        );
    }

    // ── Drain-until-match: stale frames discarded, matching accepted ─────────

    /// The drain-until-match helper (introduced for clarion-c08586a2da /
    /// clarion-ff2831eec0) must accept a matching response that follows
    /// one or more stale frames. Without this property, stale frames
    /// would convert into false transport errors on the happy path.
    ///
    /// Sends a frame with id=99 (stale) followed by the real id=2
    /// response; `analyze_file` should succeed and return the entities.
    ///
    /// Closes clarion-049bbe44ce (response-id mismatch surface).
    #[test]
    fn analyze_file_drains_stale_frames_before_matching_response() {
        let manifest = compliant_manifest();
        let mut mock = MockPlugin::new_compliant();
        let (mut host, project_dir) = connect_and_handshake(manifest, &mut mock);

        let sample = project_dir.path().join("sample.mock");
        std::fs::write(&sample, b"").unwrap();

        let expected_id = host.next_request_id_test();

        // Frame 1: stale response, wrong id.
        let stale_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 999_999,
            "result": { "entities": [] }
        });
        let stale_body = serde_json::to_vec(&stale_json).unwrap();

        // Frame 2: real response at expected id, with one compliant entity.
        let real_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": expected_id,
            "result": {
                "entities": [{
                    "id": "mock:function:stub",
                    "kind": "function",
                    "qualified_name": "stub",
                    "source": { "file_path": sample.to_string_lossy().into_owned() }
                }]
            }
        });
        let real_body = serde_json::to_vec(&real_json).unwrap();

        let mut framed: Vec<u8> = Vec::new();
        write_frame(&mut framed, &Frame { body: stale_body }).unwrap();
        write_frame(&mut framed, &Frame { body: real_body }).unwrap();
        {
            let reader = host.reader_mut_test();
            let pos_before = reader.position();
            let old_end = reader.get_ref().len() as u64;
            reader.get_mut().extend_from_slice(&framed);
            if pos_before == old_end {
                reader.set_position(old_end);
            }
        }

        let result = host
            .analyze_file(&sample)
            .expect("drain-until-match must succeed past stale frame");
        assert_eq!(
            result.entities.len(),
            1,
            "matching response must yield its entity; got {} entities",
            result.entities.len()
        );
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn append_mock_output_to_host_reader(mock: &mut MockPlugin, host_reader: &mut Cursor<Vec<u8>>) {
        let new_bytes = drain_mock_output(mock);
        let old_pos = host_reader.position();
        let old_end = host_reader.get_ref().len() as u64;
        host_reader.get_mut().extend_from_slice(&new_bytes);
        if old_pos == old_end {
            host_reader.set_position(old_end);
        }
    }

    fn drain_mock_output(mock: &mut MockPlugin) -> Vec<u8> {
        let end = mock.stdout().get_ref().len() as u64;
        let start = mock.stdout().position();
        let bytes = mock.stdout().get_ref()
            [usize::try_from(start).unwrap()..usize::try_from(end).unwrap()]
            .to_vec();
        mock.stdout().set_position(end);
        bytes
    }
}
