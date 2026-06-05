//! JSON-RPC 2.0 protocol types for the Loomweave plugin host.
//!
//! # Design choice: Option B — struct-per-method with manual dispatch
//!
//! Each method has its own typed `Params` and `Result` struct. A top-level
//! `IncomingMessage` enum dispatches on the presence of `"id"` vs `"method"` in
//! the raw JSON. This keeps the JSON-RPC 2.0 envelope (`jsonrpc`, `id`) cleanly
//! separate from the method-specific payload, matching the wire spec exactly:
//!
//! ```json
//! {"jsonrpc":"2.0","method":"initialize","params":{...},"id":1}
//! ```
//!
//! Option A (tagged enum) would conflict with the outer envelope because
//! `#[serde(tag = "method", content = "params")]` embeds `method` inside the
//! `params` layer rather than at the top level where JSON-RPC 2.0 requires it.
//!
//! # Sprint 1 scope
//!
//! Only the five L4 methods are typed here:
//! - `initialize` (request/response)
//! - `initialized` (notification — no id, no response expected)
//! - `analyze_file` (request/response)
//! - `shutdown` (request/response — empty params and result)
//! - `exit` (notification — no id, no response expected)
//!
//! Entity typing for `AnalyzeFileResult.entities` is deferred to Task 6
//! (ontology boundary enforcement). The field is `Vec<serde_json::Value>` as a
//! deliberate placeholder.
//!
//! `IncomingMessage` covers only plugin-to-core traffic (responses and
//! notifications) because Sprint 1's walking skeleton is core-initiated only.
//! Full bidirectional dispatch is Task 6's concern.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// Edge-confidence tier carried on edge rows and wire records (ADR-028).
///
/// Ordering is permissiveness, not trust strength: `Resolved < Ambiguous <
/// Inferred`, so a consumer asking for `confidence >= Ambiguous` opts into
/// ambiguous and inferred tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EdgeConfidence {
    /// Static analysis resolved the edge unambiguously.
    #[default]
    Resolved,
    /// Static analysis found more than one in-project candidate.
    Ambiguous,
    /// Query-time inference produced the edge.
    Inferred,
}

impl EdgeConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Ambiguous => "ambiguous",
            Self::Inferred => "inferred",
        }
    }
}

// ── JSON-RPC version wrapper ──────────────────────────────────────────────────

/// Wrapper that (de)serialises to/from the literal string `"2.0"`.
///
/// Serde impl rejects any other value with a descriptive error, catching
/// wire-format corruption early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(de::Error::custom(format!(
                "unsupported JSON-RPC version {s:?}; expected \"2.0\""
            )))
        }
    }
}

// ── Envelope types ────────────────────────────────────────────────────────────

/// JSON-RPC 2.0 request envelope (core → plugin).
///
/// Wire shape: `{"jsonrpc":"2.0","method":"...","params":{...},"id":1}`
///
/// JSON-RPC 2.0 permits string, number, or null request IDs. Loomweave narrows
/// this to `i64` as an application-level policy because the plugin transport is
/// core-initiated and core-controlled. Revisit this type before exposing the
/// envelope to external JSON-RPC peers or plugins that originate arbitrary IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestEnvelope {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    pub params: Value,
    pub id: i64,
}

/// JSON-RPC 2.0 notification envelope (no `id`, no response expected).
///
/// Wire shape: `{"jsonrpc":"2.0","method":"...","params":{...}}`
///
/// Used for `initialized` and `exit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotificationEnvelope {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    pub params: Value,
}

/// JSON-RPC 2.0 response envelope (plugin → core).
///
/// Wire shape (success): `{"jsonrpc":"2.0","result":{...},"id":1}`
/// Wire shape (error):   `{"jsonrpc":"2.0","error":{...},"id":1}`
///
/// Like [`RequestEnvelope`], this intentionally accepts only `i64` IDs. That is
/// narrower than JSON-RPC 2.0 but matches Loomweave's current core-issued request
/// IDs; a plugin response with a string or null ID is treated as malformed
/// plugin traffic rather than general JSON-RPC interop.
///
/// The spec (§5) requires **exactly one** of `result`/`error`. This type
/// enforces that invariant during deserialisation:
/// - both present → error (so a misbehaving plugin can't hide an error by
///   also sending a result).
/// - neither present → error (so a malformed response doesn't silently
///   become an empty success).
///
/// Serialisation emits only the matching key (`result` or `error`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseEnvelope {
    pub jsonrpc: JsonRpcVersion,
    pub id: i64,
    pub payload: ResponsePayload,
}

/// The result-or-error payload inside a [`ResponseEnvelope`].
///
/// Serialisation/deserialisation of the enclosing envelope is custom
/// (see [`ResponseEnvelope`]) — do not add serde attributes here.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ResponsePayload {
    Result(Value),
    Error(ProtocolError),
}

impl Serialize for ResponseEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        // 3 fields: jsonrpc, id, and either "result" or "error".
        let mut s = serializer.serialize_struct("ResponseEnvelope", 3)?;
        s.serialize_field("jsonrpc", &self.jsonrpc)?;
        s.serialize_field("id", &self.id)?;
        match &self.payload {
            ResponsePayload::Result(v) => s.serialize_field("result", v)?,
            ResponsePayload::Error(e) => s.serialize_field("error", e)?,
        }
        s.end()
    }
}

impl<'de> Deserialize<'de> for ResponseEnvelope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Read into a generic JSON object, then enforce JSON-RPC 2.0 §5
        // (exactly one of `result`/`error`). Going via `Map<String, Value>`
        // lets us give clear errors for both the "both present" and "neither
        // present" cases — `#[serde(flatten)]` over an externally-tagged
        // enum silently drops the `error` branch when both are present.
        let mut obj = Map::<String, Value>::deserialize(deserializer)?;

        // Required fields.
        let jsonrpc_val = obj
            .remove("jsonrpc")
            .ok_or_else(|| de::Error::missing_field("jsonrpc"))?;
        let jsonrpc: JsonRpcVersion =
            serde_json::from_value(jsonrpc_val).map_err(de::Error::custom)?;

        let id_val = obj
            .remove("id")
            .ok_or_else(|| de::Error::missing_field("id"))?;
        let id: i64 = serde_json::from_value(id_val).map_err(de::Error::custom)?;

        // Enforce exactly-one-of.
        let result = obj.remove("result");
        let error = obj.remove("error");
        let payload = match (result, error) {
            (Some(_), Some(_)) => {
                return Err(de::Error::custom(
                    "response envelope must have exactly one of `result` or `error`, \
                     but both were present",
                ));
            }
            (None, None) => {
                return Err(de::Error::custom(
                    "response envelope must have exactly one of `result` or `error`, \
                     but neither was present",
                ));
            }
            (Some(v), None) => ResponsePayload::Result(v),
            (None, Some(e)) => ResponsePayload::Error(
                serde_json::from_value::<ProtocolError>(e).map_err(de::Error::custom)?,
            ),
        };

        Ok(ResponseEnvelope {
            jsonrpc,
            id,
            payload,
        })
    }
}

/// JSON-RPC 2.0 error object.
///
/// `message` and `data` are truncated at deserialisation time — a plugin
/// returning multi-MiB error strings inside an 8 MiB frame would otherwise
/// balloon host RSS as the `ProtocolError` value is cloned through
/// `HostError::Protocol` and every `?`-propagation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Maximum accepted size of [`ProtocolError::message`] and the serialized
/// form of [`ProtocolError::data`] at the wire boundary. A well-behaved
/// plugin produces error messages well under 1 KiB; the 4 KiB cap matches
/// [`crate::plugin::host::MAX_ENTITY_FIELD_BYTES`] and lets a host-emitted
/// finding safely embed the message without further truncation.
pub const MAX_PROTOCOL_ERROR_FIELD_BYTES: usize = 4 * 1024;

/// Truncate a string field to [`MAX_PROTOCOL_ERROR_FIELD_BYTES`]. Called on
/// every deserialisation of a plugin-originated `ProtocolError`.
fn truncate_for_display(s: String) -> String {
    if s.len() <= MAX_PROTOCOL_ERROR_FIELD_BYTES {
        return s;
    }
    // Slice on a char boundary at-or-before the cap. UTF-8 safety: walk
    // backwards from the cap index until we find a boundary.
    let mut end = MAX_PROTOCOL_ERROR_FIELD_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 4);
    out.push_str(&s[..end]);
    out.push_str("...");
    out
}

impl<'de> Deserialize<'de> for ProtocolError {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Go via an intermediate raw form so we can post-process each field
        // before holding the full-size copy in the `ProtocolError` value.
        #[derive(Deserialize)]
        struct Raw {
            code: i64,
            message: String,
            #[serde(default)]
            data: Option<Value>,
        }
        let raw = Raw::deserialize(deserializer)?;

        // Truncate `message` to the cap.
        let message = truncate_for_display(raw.message);

        // For `data`: if it's a string, truncate in place. Otherwise, cap
        // its serialised byte length — a deeply-nested or wide `Value`
        // tree at 8 MiB would otherwise survive past the frame ceiling.
        let data = match raw.data {
            None => None,
            Some(Value::String(s)) => Some(Value::String(truncate_for_display(s))),
            Some(v) => {
                let bytes = serde_json::to_vec(&v).map_err(de::Error::custom)?;
                if bytes.len() <= MAX_PROTOCOL_ERROR_FIELD_BYTES {
                    Some(v)
                } else {
                    // Replace with a string-shaped marker preserving the
                    // shape-kind so operators can see "there was a data
                    // field but it was too large."
                    Some(Value::String(format!(
                        "<truncated: {} bytes exceeded {} byte cap>",
                        bytes.len(),
                        MAX_PROTOCOL_ERROR_FIELD_BYTES,
                    )))
                }
            }
        };

        Ok(ProtocolError {
            code: raw.code,
            message,
            data,
        })
    }
}

// ── Method-level param and result structs ─────────────────────────────────────

// ── initialize ────────────────────────────────────────────────────────────────

/// Params for `initialize` (core → plugin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InitializeParams {
    /// Protocol version the core speaks, e.g. `"1.0"`.
    pub protocol_version: String,
    /// Absolute path to the project root being analysed, as a UTF-8 string.
    ///
    /// Using `String` rather than `PathBuf` makes the wire format statically
    /// UTF-8 safe. Task 4's jail owns the `PathBuf → String` conversion and
    /// UTF-8 validation at the boundary.
    pub project_root: String,
}

/// Result for `initialize` (plugin → core).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InitializeResult {
    /// Plugin display name.
    pub name: String,
    /// Plugin version (semver).
    pub version: String,
    /// Ontology version (semver); used in ADR-007 cache keying.
    pub ontology_version: String,
    /// Opaque capability advertisement. Shape is left to the plugin;
    /// the core forwards it without interpretation in Sprint 1.
    pub capabilities: Value,
}

// ── initialized ───────────────────────────────────────────────────────────────

/// Notification params for `initialized` (core → plugin).
///
/// Deliberately empty: the notification carries no payload. The empty-braced
/// form (`struct Foo {}`) is intentional — it serialises to the JSON object
/// `{}` as JSON-RPC 2.0 §4.2 requires. A unit struct (`struct Foo;`) would
/// serialise to `null` and be rejected by strict decoders.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InitializedNotification {}

// ── analyze_file ──────────────────────────────────────────────────────────────

/// Params for `analyze_file` (core → plugin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyzeFileParams {
    /// Path to the file to analyse (relative or absolute; plugin resolves),
    /// as a UTF-8 string. See [`InitializeParams::project_root`] for rationale.
    pub file_path: String,
}

/// Run-observability counters emitted per `analyze_file` result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyzeFileStats {
    /// Call sites where the plugin found no in-project candidate.
    #[serde(default)]
    pub unresolved_call_sites_total: u64,
    /// Concrete unresolved call sites available for query-time inference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_call_sites: Vec<UnresolvedCallSite>,
    /// Reference sites enumerated by the plugin before resolver filtering.
    #[serde(default)]
    pub reference_sites_total: u64,
    /// Reference sites resolved to at least one in-project target.
    #[serde(default)]
    pub references_resolved_total: u64,
    /// Reference sites skipped because pyright resolved only external targets.
    #[serde(default)]
    pub references_skipped_external_total: u64,
    /// Reference sites skipped because the per-file site cap was exceeded.
    #[serde(default)]
    pub references_skipped_cap_total: u64,
    /// Reference sites where no in-project target was emitted.
    #[serde(default)]
    pub unresolved_reference_sites_total: u64,
    /// Raw latency samples (milliseconds) for per-file Pyright LSP queries.
    #[serde(default)]
    pub pyright_query_latency_ms: Vec<u64>,
    /// Raw latency samples (milliseconds) for Pyright-side AST index builds.
    #[serde(default)]
    pub pyright_index_parse_latency_ms: Vec<u64>,
    /// Extractor-side `ast.parse` latency for this file in milliseconds.
    #[serde(default)]
    pub extractor_parse_latency_ms: u64,
}

impl AnalyzeFileStats {
    fn is_empty(&self) -> bool {
        self.unresolved_call_sites_total == 0
            && self.reference_sites_total == 0
            && self.references_resolved_total == 0
            && self.references_skipped_external_total == 0
            && self.references_skipped_cap_total == 0
            && self.unresolved_reference_sites_total == 0
            && self.unresolved_call_sites.is_empty()
            && self.pyright_query_latency_ms.is_empty()
            && self.pyright_index_parse_latency_ms.is_empty()
            && self.extractor_parse_latency_ms == 0
    }
}

/// One plugin-reported diagnostic emitted with an `analyze_file` result.
///
/// The host validates the prefix, field sizes, and metadata shape before
/// accepting this into its run-level finding queue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalyzeFileFinding {
    /// Plugin-owned rule id. Must be under the manifest's `rule_id_prefix`.
    pub subcode: String,
    /// Plugin severity label. Stored as metadata by the host; ADR-017
    /// persistence severity remains host-controlled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Plugin-provided context. Values are validated and stringified by the host.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

/// One unresolved call site emitted by a plugin for lazy inferred-call dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnresolvedCallSite {
    pub caller_entity_id: String,
    pub site_ordinal: i64,
    pub source_byte_start: i64,
    pub source_byte_end: i64,
    pub callee_expr: String,
}

/// Result for `analyze_file` (plugin → core).
///
/// `entities` is `Vec<serde_json::Value>` as a Sprint 1 placeholder; per-element
/// deserialisation into `RawEntity` happens in `plugin::host::analyze_file`,
/// so a single malformed entity drops with a finding rather than failing the
/// whole response. B.3 added `edges` under the same per-element pattern.
///
/// `edges` defaults to empty for Sprint-1 plugins that pre-date B.3 — the
/// `#[serde(default)]` makes the new field non-breaking on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnalyzeFileResult {
    /// Extracted entities. Element shape is the plugin's concern; the core
    /// stores them opaquely until Task 6 introduces the typed ontology layer.
    pub entities: Vec<Value>,
    /// Edges emitted by the plugin (B.3, ADR-026). Per-element
    /// deserialisation into `plugin::host::RawEdge` happens in the host;
    /// malformed edges drop with a finding. `serde(default)` keeps the
    /// addition non-breaking for Sprint-1 plugins.
    #[serde(default)]
    pub edges: Vec<Value>,
    /// Observability counters and samples. Defaults keep pre-B.4 plugins valid.
    #[serde(default, skip_serializing_if = "AnalyzeFileStats::is_empty")]
    pub stats: AnalyzeFileStats,
    /// Plugin-reported diagnostics. Defaults keep older plugins valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<AnalyzeFileFinding>,
}

// ── shutdown ──────────────────────────────────────────────────────────────────

/// Params for `shutdown` (core → plugin).
///
/// Empty by design — the message carries no payload. Empty-braced form is
/// intentional so the type serialises to JSON `{}` (an object), not `null`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShutdownParams {}

/// Result for `shutdown` (plugin → core).
///
/// JSON-RPC 2.0 requires a non-null response to a request, so we use an empty
/// result object `{}` rather than `null`. The empty-braced form ensures serde
/// emits `{}` (object) rather than `null` (which a unit struct would produce).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShutdownResult {}

// ── exit ──────────────────────────────────────────────────────────────────────

/// Notification params for `exit` (core → plugin).
///
/// Deliberately empty: this is a forceful termination signal sent after the
/// plugin has replied to `shutdown`. No response is expected. Empty-braced
/// form ensures `{}` is emitted on the wire rather than `null`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExitNotification {}

// ── Helpers for building common envelopes ─────────────────────────────────────

/// Convenience: build a `RequestEnvelope` from typed params.
///
/// Kept `pub(crate)` because the body panics on `serde_json::to_value` failure
/// and the panic condition ("`params` serialises to a valid JSON value") is a
/// property of the internally-defined `ShutdownParams` / `InitializeParams`
/// / etc. structs, not something an arbitrary external caller could rely on.
/// External crates that need a `RequestEnvelope` should construct one
/// directly and surface the serde error themselves.
///
/// # Panics
///
/// Panics if serialisation of `params` fails. This should never happen for the
/// well-formed Sprint 1 param types. Callers that need explicit error handling
/// should construct [`RequestEnvelope`] directly.
pub(crate) fn make_request<P: Serialize>(method: &str, params: &P, id: i64) -> RequestEnvelope {
    RequestEnvelope {
        jsonrpc: JsonRpcVersion,
        method: method.to_owned(),
        params: serde_json::to_value(params).expect("params serialisation must not fail"),
        id,
    }
}

/// Convenience: build a `NotificationEnvelope` from typed params.
///
/// Kept `pub(crate)` for the same reason as [`make_request`] — the panic
/// condition is an internal-types property; external callers that want
/// a `NotificationEnvelope` should construct one directly.
///
/// # Panics
///
/// Panics if serialisation of `params` fails. This should never happen for the
/// well-formed Sprint 1 param types. Callers that need explicit error handling
/// should construct [`NotificationEnvelope`] directly.
pub(crate) fn make_notification<P: Serialize>(method: &str, params: &P) -> NotificationEnvelope {
    NotificationEnvelope {
        jsonrpc: JsonRpcVersion,
        method: method.to_owned(),
        params: serde_json::to_value(params).expect("params serialisation must not fail"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Protocol test 1: round-trip InitializeParams ──────────────────────────

    #[test]
    fn proto_01_initialize_params_round_trips() {
        let params = InitializeParams {
            protocol_version: "1.0".to_owned(),
            project_root: "/home/user/project".to_owned(),
        };
        let json = serde_json::to_string(&params).expect("serialise");
        let back: InitializeParams = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(params, back);
    }

    // ── Protocol test 2: JsonRpcVersion rejects wrong versions ────────────────

    #[test]
    fn proto_02_json_rpc_version_rejects_non_2_0() {
        // "3.0"
        let bad_30 = r#"{"jsonrpc":"3.0","method":"initialize","params":{},"id":1}"#;
        let err = serde_json::from_str::<RequestEnvelope>(bad_30);
        assert!(err.is_err(), "should reject version 3.0");

        // "1.0"
        let bad_10 = r#"{"jsonrpc":"1.0","method":"initialize","params":{},"id":1}"#;
        let err = serde_json::from_str::<RequestEnvelope>(bad_10);
        assert!(err.is_err(), "should reject version 1.0");

        // "" (empty)
        let bad_empty = r#"{"jsonrpc":"","method":"initialize","params":{},"id":1}"#;
        let err = serde_json::from_str::<RequestEnvelope>(bad_empty);
        assert!(err.is_err(), "should reject empty version string");
    }

    // ── Protocol test 3: request envelope serialises to expected shape ─────────

    #[test]
    fn proto_03_request_envelope_serialises_correctly() {
        let params = InitializeParams {
            protocol_version: "1.0".to_owned(),
            project_root: "/proj".to_owned(),
        };
        let env = make_request("initialize", &params, 1);
        let json = serde_json::to_string(&env).expect("serialise");
        let v: Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(v["jsonrpc"], "2.0", "jsonrpc field must be \"2.0\"");
        assert_eq!(v["method"], "initialize", "method field");
        assert_eq!(v["id"], 1, "id field");
        // params object is present and contains protocol_version
        assert_eq!(v["params"]["protocol_version"], "1.0");
    }

    // ── Protocol test 4: response envelope — both payload variants ────────────

    #[test]
    fn proto_04_response_envelope_result_variant_round_trips() {
        let env = ResponseEnvelope {
            jsonrpc: JsonRpcVersion,
            id: 7,
            payload: ResponsePayload::Result(serde_json::json!({"ok": true})),
        };
        let json = serde_json::to_string(&env).expect("serialise");
        let v: Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["result"]["ok"], true);
        assert!(v.get("error").is_none(), "no error key on success response");

        // Full round-trip
        let back: ResponseEnvelope = serde_json::from_str(&json).expect("deserialise back");
        assert_eq!(env, back);
    }

    #[test]
    fn proto_04_response_envelope_error_variant_round_trips() {
        let env = ResponseEnvelope {
            jsonrpc: JsonRpcVersion,
            id: 7,
            payload: ResponsePayload::Error(ProtocolError {
                code: -32_600,
                message: "Invalid Request".to_owned(),
                data: None,
            }),
        };
        let json = serde_json::to_string(&env).expect("serialise");
        let v: Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["error"]["code"], -32_600);
        assert_eq!(v["error"]["message"], "Invalid Request");
        assert!(v.get("result").is_none(), "no result key on error response");

        // Full round-trip
        let back: ResponseEnvelope = serde_json::from_str(&json).expect("deserialise back");
        assert_eq!(env, back);
    }

    // ── Protocol test 5: notification envelope has no id field ────────────────

    #[test]
    fn proto_05_notification_envelope_has_no_id_field() {
        let env = make_notification("initialized", &serde_json::json!({}));
        let json = serde_json::to_string(&env).expect("serialise");
        let v: Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "initialized");
        assert!(
            v.get("id").is_none(),
            "notification must not carry an id field"
        );
    }

    // ── Protocol test 6: round-trip for all 5 methods' param+result structs ───

    #[test]
    fn proto_06_all_method_param_result_structs_round_trip() {
        // initialize params
        let p = InitializeParams {
            protocol_version: "1.0".to_owned(),
            project_root: "/proj".to_owned(),
        };
        let back: InitializeParams =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);

        // initialize result
        let r = InitializeResult {
            name: "loomweave-plugin-python".to_owned(),
            version: "0.1.0".to_owned(),
            ontology_version: "0.1.0".to_owned(),
            capabilities: serde_json::json!({"wardline_aware": true}),
        };
        let back: InitializeResult =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);

        // initialized notification — round-trips through the derived serde impl
        let n = InitializedNotification {};
        let back: InitializedNotification =
            serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(n, back);

        // analyze_file params
        let p = AnalyzeFileParams {
            file_path: "src/main.py".to_owned(),
        };
        let back: AnalyzeFileParams =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);

        // analyze_file result
        let r = AnalyzeFileResult {
            entities: vec![serde_json::json!({"kind": "function", "name": "main"})],
            edges: vec![serde_json::json!({
                "kind": "contains",
                "from_id": "python:module:m",
                "to_id": "python:function:m.main",
            })],
            stats: AnalyzeFileStats {
                unresolved_call_sites_total: 2,
                unresolved_call_sites: vec![UnresolvedCallSite {
                    caller_entity_id: "python:function:m.main".to_owned(),
                    site_ordinal: 1,
                    source_byte_start: 10,
                    source_byte_end: 14,
                    callee_expr: "dynamic_target".to_owned(),
                }],
                reference_sites_total: 3,
                references_resolved_total: 4,
                references_skipped_external_total: 5,
                references_skipped_cap_total: 6,
                unresolved_reference_sites_total: 7,
                pyright_query_latency_ms: vec![10, 20, 30],
                pyright_index_parse_latency_ms: vec![8, 13],
                extractor_parse_latency_ms: 5,
            },
            findings: vec![AnalyzeFileFinding {
                subcode: "LMWV-PY-PYRIGHT-RESTART".to_owned(),
                severity: Some("warning".to_owned()),
                message: "pyright subprocess died and was restarted".to_owned(),
                metadata: BTreeMap::new(),
            }],
        };
        let back: AnalyzeFileResult =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);

        // analyze_file result with omitted `edges` field (Sprint-1 plugins
        // pre-date B.3); serde(default) means it deserialises to empty.
        let pre_b3 = r#"{"entities": []}"#;
        let back: AnalyzeFileResult =
            serde_json::from_str(pre_b3).expect("pre-B.3 wire shape must remain accepted");
        assert!(
            back.edges.is_empty(),
            "missing edges field must default to empty vec"
        );
        assert_eq!(
            back.stats.unresolved_call_sites_total, 0,
            "missing stats field must default unresolved-call-site counter to zero"
        );
        assert_eq!(
            back.stats.reference_sites_total, 0,
            "missing stats field must default reference-site counter to zero"
        );
        assert!(
            back.findings.is_empty(),
            "missing findings field must default to empty vec"
        );
        assert_eq!(
            back.stats.references_resolved_total, 0,
            "missing stats field must default resolved-reference counter to zero"
        );
        assert_eq!(
            back.stats.references_skipped_external_total, 0,
            "missing stats field must default external-reference counter to zero"
        );
        assert_eq!(
            back.stats.references_skipped_cap_total, 0,
            "missing stats field must default reference-cap counter to zero"
        );
        assert_eq!(
            back.stats.unresolved_reference_sites_total, 0,
            "missing stats field must default unresolved-reference counter to zero"
        );
        assert!(
            back.stats.pyright_query_latency_ms.is_empty(),
            "missing stats field must default pyright latency samples to empty"
        );
        assert!(
            back.stats.pyright_index_parse_latency_ms.is_empty(),
            "missing stats field must default pyright index parse samples to empty"
        );
        assert_eq!(
            back.stats.extractor_parse_latency_ms, 0,
            "missing stats field must default extractor parse latency to zero"
        );

        // shutdown params (empty)
        let p = ShutdownParams {};
        let back: ShutdownParams =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);

        // shutdown result (empty)
        let r = ShutdownResult {};
        let back: ShutdownResult =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);

        // exit notification — round-trips through the derived serde impl
        let n = ExitNotification {};
        let back: ExitNotification =
            serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(n, back);
    }

    // ── C1 regression: unit-like params serialise as `{}`, not `null` ─────────

    #[test]
    fn proto_07_unit_notifications_serialise_as_empty_object_not_null() {
        // Every empty-braced param/result struct must serialise to the JSON
        // object `{}` (JSON-RPC 2.0 §4.2 requires params to be a structured
        // value; `null` violates the spec and strict Python decoders reject
        // it).
        let v = serde_json::to_value(InitializedNotification {}).unwrap();
        assert_eq!(v, serde_json::json!({}), "InitializedNotification");

        let v = serde_json::to_value(ShutdownParams {}).unwrap();
        assert_eq!(v, serde_json::json!({}), "ShutdownParams");

        let v = serde_json::to_value(ShutdownResult {}).unwrap();
        assert_eq!(v, serde_json::json!({}), "ShutdownResult");

        let v = serde_json::to_value(ExitNotification {}).unwrap();
        assert_eq!(v, serde_json::json!({}), "ExitNotification");
    }

    // ── C2 regression: exactly-one-of result/error enforced at deserialise ────

    #[test]
    fn proto_08_response_with_both_result_and_error_rejected() {
        // A plugin sending both `result` and `error` in the same response is
        // malformed (JSON-RPC 2.0 §5 requires exactly one). The supervisor
        // must see a hard error, not a silent drop of the `error` half.
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"ok": true},
            "error": {"code": -32600, "message": "oops"}
        });
        let err = serde_json::from_value::<ResponseEnvelope>(raw).expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("both"),
            "error message should mention `both` fields present; got: {msg}"
        );
    }

    #[test]
    fn proto_protocol_error_message_truncated_at_deserialise() {
        // A plugin returning a pathological multi-MiB message must not
        // balloon host RSS via the ProtocolError clone path.
        let huge = "x".repeat(MAX_PROTOCOL_ERROR_FIELD_BYTES * 4);
        let raw = serde_json::json!({
            "code": -32_600,
            "message": huge,
        });
        let err: ProtocolError = serde_json::from_value(raw).expect("must deserialise");
        assert!(
            err.message.len() <= MAX_PROTOCOL_ERROR_FIELD_BYTES + 4,
            "message must be truncated; got {} bytes",
            err.message.len()
        );
        assert!(
            err.message.ends_with("..."),
            "truncated message must end with ellipsis; got: {:?}",
            &err.message[err.message.len().saturating_sub(10)..]
        );
    }

    #[test]
    fn proto_protocol_error_data_replaced_when_over_cap() {
        // `data` is an arbitrary JSON Value; when its serialised form
        // exceeds the cap, it's replaced with a marker string rather
        // than the full payload.
        let big_array: Vec<String> = (0..2000).map(|i| format!("entry_{i}")).collect();
        let raw = serde_json::json!({
            "code": -32_600,
            "message": "err",
            "data": big_array,
        });
        let err: ProtocolError = serde_json::from_value(raw).expect("must deserialise");
        match err.data {
            Some(Value::String(s)) => {
                assert!(s.starts_with("<truncated:"), "got: {s}");
            }
            other => panic!("data must be replaced with truncation marker; got {other:?}"),
        }
    }

    #[test]
    fn proto_08_response_with_neither_result_nor_error_rejected() {
        // A response with neither `result` nor `error` is structurally invalid.
        // This used to silently become `ResponsePayload::Result(Null)` via the
        // flatten-enum approach; now it produces a loud error.
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1
        });
        let err = serde_json::from_value::<ResponseEnvelope>(raw).expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("neither"),
            "error message should mention neither field present; got: {msg}"
        );
    }
}
