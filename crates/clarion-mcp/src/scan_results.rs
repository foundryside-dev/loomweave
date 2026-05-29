//! Filigree-native scan-results emission (WP9-B, REQ-FINDING-03).
//!
//! Maps Clarion's persisted findings onto Filigree's `POST /api/v1/scan-results`
//! intake schema (ADR-004 + detailed-design §7) and models the response. This
//! module is pure — request building and response parsing only; the HTTP POST
//! lives on [`crate::filigree::FiligreeHttpClient::post_scan_results`].
//!
//! Emission is enrich-only: a one-way Clarion→Filigree push that adds no
//! Filigree-side routes and never gates Clarion's own semantics. Clarion's
//! richer fields nest under `metadata.clarion.*` so Filigree's silent
//! top-level-key drop (verified against the live intake) cannot lose them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use clarion_storage::FindingForEmitRow;

/// The `scan_source` Clarion stamps on every emitted finding. Filigree's dedup
/// key includes `scan_source`, so this is stable across runs.
pub const CLARION_SCAN_SOURCE: &str = "clarion";

/// Map Clarion's internal severity vocabulary (`INFO` | `WARN` | `ERROR` |
/// `CRITICAL` | `NONE`) to Filigree's wire vocabulary (detailed-design §7
/// table). Anything unrecognised — including `NONE` (facts) and `INFO` — maps
/// to `info`, mirroring the coercion Filigree applies server-side, except done
/// here so the original survives in `metadata.clarion.internal_severity`.
///
/// This mapping is load-bearing: a live probe confirmed Filigree coerces an
/// unmapped uppercase `WARN` to `info` (with a response warning), so emitting
/// the internal vocabulary verbatim would silently flatten every defect to
/// `info`.
#[must_use]
pub fn severity_to_wire(internal: &str) -> &'static str {
    match internal {
        "CRITICAL" => "critical",
        "ERROR" => "high",
        "WARN" => "medium",
        _ => "info",
    }
}

/// Knobs the emitter sets per `clarion analyze` invocation. `create_observations`
/// is always `false` (Clarion emits findings, not observations).
#[derive(Debug, Clone)]
pub struct EmitOptions {
    /// Filigree's `scan_run_id`; Clarion passes its `run_id` here. An unknown
    /// id is tolerated by Filigree (it warns and proceeds), so this carries the
    /// REQ-FINDING-05 wire shape without a pre-create handshake.
    pub scan_run_id: Option<String>,
    /// `mark_unseen`: `true` for a normal full run so old-position findings for
    /// the same rule/file transition to `unseen_in_latest` (REQ-FINDING-06).
    pub mark_unseen: bool,
    /// `complete_scan_run`: `true` on the final (here: only) batch.
    pub complete_scan_run: bool,
}

/// The Filigree-native scan-results request body. Serializes to the exact wire
/// shape Filigree's intake accepts; any field outside its enumerated set is
/// silently dropped server-side, so the struct carries only known keys.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScanResultsRequest {
    pub scan_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_run_id: Option<String>,
    pub mark_unseen: bool,
    pub create_observations: bool,
    pub complete_scan_run: bool,
    pub findings: Vec<Value>,
}

/// A prepared batch plus the counts the emitter records in `stats.json`.
#[derive(Debug, Clone)]
pub struct PreparedBatch {
    pub request: ScanResultsRequest,
    /// Findings rendered into the request body.
    pub emitted: usize,
    /// Findings dropped because their anchor entity has no `source_file_path`
    /// (Filigree requires `path`; emitting a synthetic one would pollute its
    /// file registry). Surfaced so the skip is never silent.
    pub skipped_no_path: usize,
}

/// Build a scan-results batch from persisted findings. Findings whose anchor
/// entity has no source path are skipped and counted, not emitted.
#[must_use]
pub fn prepare_batch(rows: &[FindingForEmitRow], opts: &EmitOptions) -> PreparedBatch {
    let mut findings = Vec::with_capacity(rows.len());
    let mut skipped_no_path = 0;
    for row in rows {
        match wire_finding(row) {
            Some(finding) => findings.push(finding),
            None => skipped_no_path += 1,
        }
    }
    let emitted = findings.len();
    PreparedBatch {
        request: ScanResultsRequest {
            scan_source: CLARION_SCAN_SOURCE.to_owned(),
            scan_run_id: opts.scan_run_id.clone(),
            mark_unseen: opts.mark_unseen,
            create_observations: false,
            complete_scan_run: opts.complete_scan_run,
            findings,
        },
        emitted,
        skipped_no_path,
    }
}

/// Render one persisted finding as a Filigree-native wire finding, or `None`
/// when it has no usable `path` (Filigree rejects path-less findings with a
/// `400 VALIDATION`).
fn wire_finding(row: &FindingForEmitRow) -> Option<Value> {
    let path = row
        .source_file_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())?;
    let mut finding = Map::new();
    finding.insert("path".to_owned(), json!(path));
    finding.insert("rule_id".to_owned(), json!(row.rule_id));
    finding.insert("message".to_owned(), json!(row.message));
    finding.insert(
        "severity".to_owned(),
        json!(severity_to_wire(&row.severity)),
    );
    if let Some(line_start) = row.source_line_start {
        finding.insert("line_start".to_owned(), json!(line_start));
    }
    if let Some(line_end) = row.source_line_end {
        finding.insert("line_end".to_owned(), json!(line_end));
    }
    finding.insert("metadata".to_owned(), wire_metadata(row));
    Some(Value::Object(finding))
}

/// Nest Clarion's richer fields under `metadata` (top level) and
/// `metadata.clarion` (Clarion-owned slot), per ADR-004 + detailed-design §7.
fn wire_metadata(row: &FindingForEmitRow) -> Value {
    let mut meta = Map::new();
    meta.insert("kind".to_owned(), json!(row.kind));
    if let Some(confidence) = row.confidence {
        meta.insert("confidence".to_owned(), json!(confidence));
    }
    if let Some(basis) = &row.confidence_basis {
        meta.insert("confidence_basis".to_owned(), json!(basis));
    }

    let mut clarion = Map::new();
    clarion.insert("entity_id".to_owned(), json!(row.entity_id));
    clarion.insert(
        "related_entities".to_owned(),
        json_array_or_empty(&row.related_entities_json),
    );
    clarion.insert(
        "supports".to_owned(),
        json_array_or_empty(&row.supports_json),
    );
    clarion.insert(
        "supported_by".to_owned(),
        json_array_or_empty(&row.supported_by_json),
    );
    // Lossless round-trip: the wire `severity` is the mapped value, so the
    // internal vocabulary is preserved here for read-back.
    clarion.insert("internal_severity".to_owned(), json!(row.severity));
    clarion.insert("internal_status".to_owned(), json!("open"));
    meta.insert("clarion".to_owned(), Value::Object(clarion));
    Value::Object(meta)
}

/// Parse a stored JSON-array column; fall back to an empty array if the text is
/// malformed or not an array, so one bad row never derails a batch.
fn json_array_or_empty(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Array(_)) => value,
        _ => Value::Array(Vec::new()),
    }
}

/// Filigree's scan-results response. `#[serde(default)]` keeps the read
/// forward-compatible: Filigree may add fields without breaking Clarion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ScanResultsResponse {
    pub files_created: u64,
    pub files_updated: u64,
    pub findings_created: u64,
    pub findings_updated: u64,
    pub observations_created: u64,
    pub observations_failed: u64,
    pub new_finding_ids: Vec<String>,
    /// Per-finding intake warnings (e.g. coerced severity, unknown
    /// `scan_run_id`). REQ-FINDING-03 requires the emitter to parse these, not
    /// just count them.
    pub warnings: Vec<String>,
}

/// Parse a scan-results response body.
///
/// # Errors
///
/// Returns the underlying [`serde_json::Error`] if the body is not the expected
/// JSON object shape.
pub fn parse_scan_results_response(body: &str) -> Result<ScanResultsResponse, serde_json::Error> {
    serde_json::from_str(body)
}

/// The scan-results intake URL for a Filigree base URL.
#[must_use]
pub fn scan_results_url(base_url: &str) -> String {
    format!("{}/api/v1/scan-results", base_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defect_row() -> FindingForEmitRow {
        FindingForEmitRow {
            id: "core:finding:run-1:circular".to_owned(),
            rule_id: "CLA-PY-STRUCTURE-001".to_owned(),
            kind: "defect".to_owned(),
            severity: "WARN".to_owned(),
            confidence: Some(0.95),
            confidence_basis: Some("ast_match".to_owned()),
            message: "Circular import detected".to_owned(),
            entity_id: "python:class:auth.tokens::TokenManager".to_owned(),
            related_entities_json: r#"["python:class:auth.sessions::SessionStore"]"#.to_owned(),
            supports_json: "[]".to_owned(),
            supported_by_json: "[]".to_owned(),
            source_file_path: Some("src/auth/tokens.py".to_owned()),
            source_line_start: Some(12),
            source_line_end: Some(12),
        }
    }

    #[test]
    fn severity_table_matches_detailed_design() {
        assert_eq!(severity_to_wire("CRITICAL"), "critical");
        assert_eq!(severity_to_wire("ERROR"), "high");
        assert_eq!(severity_to_wire("WARN"), "medium");
        assert_eq!(severity_to_wire("INFO"), "info");
        assert_eq!(severity_to_wire("NONE"), "info");
        // Unknown values coerce to info, the same as Filigree's server-side rule.
        assert_eq!(severity_to_wire("bogus"), "info");
    }

    #[test]
    fn wire_finding_carries_mapped_severity_and_nested_clarion_metadata() {
        let finding = wire_finding(&defect_row()).expect("path present");

        assert_eq!(finding["path"], json!("src/auth/tokens.py"));
        assert_eq!(finding["rule_id"], json!("CLA-PY-STRUCTURE-001"));
        assert_eq!(finding["message"], json!("Circular import detected"));
        // Internal WARN maps to wire medium...
        assert_eq!(finding["severity"], json!("medium"));
        assert_eq!(finding["line_start"], json!(12));
        assert_eq!(finding["line_end"], json!(12));

        let meta = &finding["metadata"];
        assert_eq!(meta["kind"], json!("defect"));
        assert_eq!(meta["confidence"], json!(0.95));
        assert_eq!(meta["confidence_basis"], json!("ast_match"));

        let clarion = &meta["clarion"];
        assert_eq!(
            clarion["entity_id"],
            json!("python:class:auth.tokens::TokenManager")
        );
        assert_eq!(
            clarion["related_entities"],
            json!(["python:class:auth.sessions::SessionStore"])
        );
        assert_eq!(clarion["supports"], json!([]));
        assert_eq!(clarion["supported_by"], json!([]));
        // ...while the internal value round-trips under clarion.*.
        assert_eq!(clarion["internal_severity"], json!("WARN"));
        assert_eq!(clarion["internal_status"], json!("open"));
    }

    #[test]
    fn fact_finding_omits_confidence_basis_when_absent() {
        let mut row = defect_row();
        row.kind = "fact".to_owned();
        row.severity = "NONE".to_owned();
        row.confidence = None;
        row.confidence_basis = None;

        let finding = wire_finding(&row).expect("path present");
        assert_eq!(finding["severity"], json!("info"));
        let meta = &finding["metadata"];
        assert_eq!(meta["kind"], json!("fact"));
        assert!(
            meta.get("confidence").is_none(),
            "confidence omitted: {meta}"
        );
        assert!(
            meta.get("confidence_basis").is_none(),
            "confidence_basis omitted: {meta}"
        );
        assert_eq!(meta["clarion"]["internal_severity"], json!("NONE"));
    }

    #[test]
    fn path_less_finding_is_skipped_not_emitted() {
        let mut row = defect_row();
        row.source_file_path = None;
        assert!(wire_finding(&row).is_none());

        let mut blank = defect_row();
        blank.source_file_path = Some("   ".to_owned());
        assert!(wire_finding(&blank).is_none(), "blank path is skipped too");
    }

    #[test]
    fn malformed_related_entities_falls_back_to_empty_array() {
        let mut row = defect_row();
        row.related_entities_json = "not json".to_owned();
        let finding = wire_finding(&row).expect("path present");
        assert_eq!(
            finding["metadata"]["clarion"]["related_entities"],
            json!([])
        );
    }

    #[test]
    fn prepare_batch_counts_emitted_and_skipped() {
        let emitted = defect_row();
        let mut skipped = defect_row();
        skipped.id = "core:finding:run-1:weak-modularity".to_owned();
        skipped.entity_id = "core:subsystem:abcd".to_owned();
        skipped.source_file_path = None;

        let batch = prepare_batch(
            &[emitted, skipped],
            &EmitOptions {
                scan_run_id: Some("run-1".to_owned()),
                mark_unseen: true,
                complete_scan_run: true,
            },
        );

        assert_eq!(batch.emitted, 1);
        assert_eq!(batch.skipped_no_path, 1);
        assert_eq!(batch.request.findings.len(), 1);
        assert_eq!(batch.request.scan_source, "clarion");
        assert_eq!(batch.request.scan_run_id.as_deref(), Some("run-1"));
        assert!(batch.request.mark_unseen);
        assert!(batch.request.complete_scan_run);
        assert!(!batch.request.create_observations);
    }

    #[test]
    fn request_serializes_to_filigree_wire_shape() {
        let batch = prepare_batch(
            &[defect_row()],
            &EmitOptions {
                scan_run_id: Some("run-1".to_owned()),
                mark_unseen: true,
                complete_scan_run: true,
            },
        );
        let value = serde_json::to_value(&batch.request).expect("serialize request");

        assert_eq!(value["scan_source"], json!("clarion"));
        assert_eq!(value["scan_run_id"], json!("run-1"));
        assert_eq!(value["mark_unseen"], json!(true));
        assert_eq!(value["create_observations"], json!(false));
        assert_eq!(value["complete_scan_run"], json!(true));
        assert_eq!(
            value["findings"].as_array().expect("findings array").len(),
            1
        );
    }

    #[test]
    fn omitted_scan_run_id_is_absent_from_wire() {
        let batch = prepare_batch(
            &[defect_row()],
            &EmitOptions {
                scan_run_id: None,
                mark_unseen: true,
                complete_scan_run: true,
            },
        );
        let value = serde_json::to_value(&batch.request).expect("serialize request");
        assert!(
            value.get("scan_run_id").is_none(),
            "scan_run_id omitted when None: {value}"
        );
    }

    #[test]
    fn parses_live_response_shape() {
        // Pinned to the real Filigree response captured from a live probe POST.
        let response = parse_scan_results_response(
            r#"{
                "files_created": 1,
                "files_updated": 0,
                "findings_created": 1,
                "findings_updated": 0,
                "new_finding_ids": ["clarion-sf-2f4cf9ca1b"],
                "observations_created": 0,
                "observations_failed": 0,
                "warnings": ["Unknown severity 'WARN' for finding at probe/sev.py, mapped to 'info'"]
            }"#,
        )
        .expect("parse live response shape");

        assert_eq!(response.findings_created, 1);
        assert_eq!(response.files_created, 1);
        assert_eq!(response.new_finding_ids, vec!["clarion-sf-2f4cf9ca1b"]);
        assert_eq!(response.warnings.len(), 1);
        assert!(response.warnings[0].contains("Unknown severity"));
    }

    #[test]
    fn response_parse_tolerates_missing_and_extra_fields() {
        // Forward-compat: unknown fields ignored, missing fields default.
        let response = parse_scan_results_response(
            r#"{"findings_created": 2, "warnings": [], "some_future_field": 99}"#,
        )
        .expect("parse forward-compatible response");
        assert_eq!(response.findings_created, 2);
        assert!(response.warnings.is_empty());
        assert!(response.new_finding_ids.is_empty());
    }

    #[test]
    fn builds_scan_results_url() {
        assert_eq!(
            scan_results_url("http://127.0.0.1:8542/"),
            "http://127.0.0.1:8542/api/v1/scan-results"
        );
        assert_eq!(
            scan_results_url("http://127.0.0.1:8542"),
            "http://127.0.0.1:8542/api/v1/scan-results"
        );
    }
}
