//! Loomweave → Filigree scan-results (`POST /api/v1/scan-results`) PRODUCER wire
//! conformance oracle.
//!
//! The PRODUCER side of the cross-repo `scan-results-lw` Loomweave→Filigree seam.
//! LOOMWEAVE (this crate) is the PRODUCER: `loomweave analyze` Phase 8 maps its
//! persisted findings onto Filigree's intake schema via
//! `loomweave_federation::scan_results::prepare_batch` / `wire_finding`
//! (`scan_source="loomweave"`) and POSTs the `ScanResultsRequest` body to
//! `POST /api/v1/scan-results`. Filigree is the CONSUMER.
//!
//! This module FREEZES Loomweave's produced wire (the assembled multi-finding
//! `ScanResultsRequest` body, for FIXED `FindingForEmit` inputs) to a committed
//! golden, plus a NON-CIRCULAR producer-source recheck that re-invokes the REAL
//! `prepare_batch` on those fixed inputs and asserts the produced body ties to the
//! golden. The golden lives in the federation authority dir
//! (`docs/federation/fixtures/loomweave-scan-results-wire.golden.json`), where
//! Filigree would later vendor a byte-identical consumer copy — mirroring the
//! entity-associations and wardline-taint-fact goldens.
//!
//! Mirrors the proven layering of the entity-associations / wardline-taint-fact
//! oracles, in the PRODUCER direction (no upstream to drift-check against —
//! Loomweave is the authority for this body):
//!
//!   * Layer 1 — a byte-pin: a `blake3` digest over the committed golden bytes,
//!     asserted against a const. A single-byte edit/re-freeze of the golden reds
//!     here. (Proven to red on tamper — see
//!     `golden_bytes_match_layer1_pin_rejects_a_mutated_byte`.)
//!
//!   * A NON-CIRCULAR producer-source recheck: re-invoke the REAL `prepare_batch`
//!     on the fixed inputs, serialize its `request` to a `serde_json::Value`, and
//!     assert it equals the golden parsed to a `Value` (semantic, key-order-immune
//!     compare). The body is built by REAL producer code, NOT restated against
//!     itself.
//!
//!   * The DISTINGUISHING contract this oracle adds over the in-module unit tests:
//!     it pins the ASSEMBLED multi-finding body AND affirmatively asserts the
//!     ABSENCE of wardline's per-finding `fingerprint` / `fingerprint_scheme` and
//!     the request-level `scanned_paths` (Loomweave's wire deliberately omits all
//!     three — Filigree computes the dedup fingerprint server-side). A regression
//!     that started emitting wardline-style fields would red here.
//!
//! ── Scope / honesty caveats ──
//!
//! * PRODUCER-ONLY. This proves Loomweave produces a STABLE, FROZEN wire. There is
//!   no Filigree consumer oracle for THIS fingerprint-less body — Filigree's
//!   `tests/federation/test_scan_results_wire_conformance_oracle.py` is
//!   wardline-specific (its golden carries `fingerprints` / `scanned_paths`). So
//!   cross-repo INGESTION of this exact shape is not proven by this oracle; only
//!   that Loomweave emits it deterministically.
//!
//! * SEMANTIC wire, not the literal reqwest byte stream. The recheck compares
//!   `serde_json::Value`s, so it freezes the produced wire's SEMANTIC shape
//!   (fields + values + structure), key-order-immune. The actual on-wire bytes are
//!   the compact `reqwest .json()` (`serde_json::to_vec`) encoding of the same
//!   struct; this oracle does not byte-compare that compact stream.

use loomweave_federation::scan_results::{
    EmitOptions, FindingForEmit, LOOMWEAVE_SCAN_SOURCE, PreparedBatch, prepare_batch,
};
use serde_json::Value;

/// The committed Loomweave authority golden: the frozen `ScanResultsRequest` wire
/// body for the fixed inputs in [`fixed_rows`] / [`fixed_opts`]. Loomweave is the
/// PRODUCER/authority for this body. `include_str!` embeds the exact on-disk bytes
/// (the `../../../` math reaches the workspace-root `docs/` dir, matching the
/// entity-associations oracle).
const GOLDEN: &str =
    include_str!("../../../docs/federation/fixtures/loomweave-scan-results-wire.golden.json");

/// Layer-1 byte-pin: lowercase-hex `blake3` of the committed golden's exact bytes.
/// Pins the fixture so a silent edit/re-freeze reds here.
///
/// Tamper proof: perturbing one hex char of this const (or one byte of the golden
/// file) makes `golden_bytes_match_layer1_pin` fail with a `left != right`
/// mismatch — the pin is load-bearing, not decorative.
const GOLDEN_BLAKE3: &str = "35eb440fae37afdcb3ea3ca04829a32eb587b2665d9d0694520137fa2e9be023";

// ── Fixed producer inputs (owned by this oracle) ─────────────────────────────

/// The fixed `FindingForEmit` rows the golden freezes. Three rows chosen to
/// exercise the structural variants of Loomweave's produced wire:
///
///   * a defect (WARN→`medium`, confidence + basis, real path + lines, nested
///     `metadata.loomweave.*` with `related`/`supports`/`supported_by`);
///   * a fact (NONE→`info`, no confidence/basis);
///   * a synthetic-anchor finding (subsystem entity, no `source_file_path`) that
///     emits against the `default_path` fallback, flagged `synthetic_anchor=true`
///     with NO line numbers.
fn fixed_rows() -> Vec<FindingForEmit> {
    vec![
        FindingForEmit {
            id: "core:finding:circular".to_owned(),
            rule_id: "LMWV-PY-STRUCTURE-001".to_owned(),
            kind: "defect".to_owned(),
            severity: "WARN".to_owned(),
            confidence: Some(0.95),
            confidence_basis: Some("ast_match".to_owned()),
            message: "Circular import detected".to_owned(),
            entity_id: "python:class:auth.tokens::TokenManager".to_owned(),
            related_entities_json: r#"["python:class:auth.sessions::SessionStore"]"#.to_owned(),
            supports_json: r#"["python:function:auth.tokens::mint"]"#.to_owned(),
            supported_by_json: "[]".to_owned(),
            source_file_path: Some("src/auth/tokens.py".to_owned()),
            source_line_start: Some(12),
            source_line_end: Some(20),
        },
        FindingForEmit {
            id: "core:finding:entrypoint-fact".to_owned(),
            rule_id: "LMWV-PY-FACT-ENTRYPOINT".to_owned(),
            kind: "fact".to_owned(),
            severity: "NONE".to_owned(),
            confidence: None,
            confidence_basis: None,
            message: "HTTP route entry point".to_owned(),
            entity_id: "python:function:api.routes::handle_request".to_owned(),
            related_entities_json: "[]".to_owned(),
            supports_json: "[]".to_owned(),
            supported_by_json: "[]".to_owned(),
            source_file_path: Some("src/api/routes.py".to_owned()),
            source_line_start: Some(42),
            source_line_end: Some(58),
        },
        FindingForEmit {
            id: "core:finding:weak-modularity".to_owned(),
            rule_id: "LMWV-SUBSYSTEM-COHESION".to_owned(),
            kind: "defect".to_owned(),
            severity: "ERROR".to_owned(),
            confidence: Some(0.5),
            confidence_basis: Some("coupling_metric".to_owned()),
            message: "Subsystem exhibits weak modularity".to_owned(),
            entity_id: "core:subsystem:abcd1234".to_owned(),
            related_entities_json: "[]".to_owned(),
            supports_json: "[]".to_owned(),
            supported_by_json: "[]".to_owned(),
            source_file_path: None,
            source_line_start: None,
            source_line_end: None,
        },
    ]
}

/// The fixed emit options the golden freezes: a normal full final batch with a
/// known `scan_run_id` and a `default_path` so the synthetic-anchor row emits
/// (rather than being skipped).
fn fixed_opts() -> EmitOptions {
    EmitOptions {
        scan_run_id: Some("run-conformance-1".to_owned()),
        mark_unseen: true,
        complete_scan_run: true,
        default_path: Some("/repo/root".to_owned()),
    }
}

/// Re-invoke the REAL producer on the fixed inputs. The single source of the
/// produced wire — both the golden (authored from this) and the recheck drive it.
fn produced_batch() -> PreparedBatch {
    prepare_batch(&fixed_rows(), &fixed_opts())
}

/// The produced `ScanResultsRequest` body as a `serde_json::Value`, exactly as the
/// HTTP client serializes it (`reqwest .json()` → `serde_json::to_*` over the same
/// `Serialize` impl).
fn produced_wire_value() -> Value {
    serde_json::to_value(&produced_batch().request).expect("serialize produced scan-results request")
}

fn golden_value() -> Value {
    serde_json::from_str(GOLDEN).expect("committed scan-results golden parses as JSON")
}

// ── Layer 1 — byte-pin ───────────────────────────────────────────────────────

#[test]
fn golden_bytes_match_layer1_pin() {
    let actual = blake3::hash(GOLDEN.as_bytes()).to_hex().to_string();
    assert_eq!(
        actual, GOLDEN_BLAKE3,
        "committed loomweave scan-results golden drifted from its byte-pin; \
         re-freeze from the producer and update GOLDEN_BLAKE3"
    );
}

#[test]
fn golden_bytes_match_layer1_pin_rejects_a_mutated_byte() {
    // Tamper proof: flipping one byte of the committed golden produces a digest
    // that does NOT equal the pin. This demonstrates the Layer-1 assertion is
    // load-bearing — it would catch a silent single-byte edit of the fixture.
    let mut tampered = GOLDEN.as_bytes().to_vec();
    tampered[0] ^= 0x01;
    let mutated = blake3::hash(&tampered).to_hex().to_string();
    assert_ne!(
        mutated, GOLDEN_BLAKE3,
        "a single mutated byte must NOT collide with the pinned digest"
    );
}

// ── NON-CIRCULAR producer-source recheck ─────────────────────────────────────

#[test]
fn real_producer_reproduces_the_golden_wire() {
    // Re-invoke the REAL `prepare_batch` on the fixed inputs and assert the
    // produced wire body equals the committed golden — semantic (key-order-immune)
    // Value compare. NON-CIRCULAR: the body is assembled by real producer code; the
    // golden is the frozen expectation, not the producer echoed at itself.
    assert_eq!(
        produced_wire_value(),
        golden_value(),
        "the real producer (prepare_batch) no longer reproduces the committed \
         scan-results golden; if this change is intended, re-freeze the golden and \
         update GOLDEN_BLAKE3"
    );
}

#[test]
fn producer_emits_the_loomweave_positive_contract() {
    // Affirmative positive-contract assertions read off the REAL producer's output
    // (not the golden against itself): the request-level knobs and the per-finding
    // severity mapping / nested loomweave axes / synthetic-anchor flagging that
    // define Loomweave's wire.
    let wire = produced_wire_value();

    // Request-level knobs.
    assert_eq!(wire["scan_source"], Value::from(LOOMWEAVE_SCAN_SOURCE));
    assert_eq!(wire["scan_source"], Value::from("loomweave"));
    assert_eq!(wire["scan_run_id"], Value::from("run-conformance-1"));
    assert_eq!(wire["mark_unseen"], Value::Bool(true));
    assert_eq!(
        wire["create_observations"],
        Value::Bool(false),
        "Loomweave emits findings, never observations"
    );
    assert_eq!(wire["complete_scan_run"], Value::Bool(true));

    let findings = wire["findings"].as_array().expect("findings array");
    assert_eq!(findings.len(), 3, "all three fixed rows emit (none skipped)");

    // Row 0 — defect: WARN→medium, real path + lines, nested loomweave.* round-trip.
    let defect = &findings[0];
    assert_eq!(defect["path"], Value::from("src/auth/tokens.py"));
    assert_eq!(defect["rule_id"], Value::from("LMWV-PY-STRUCTURE-001"));
    assert_eq!(defect["severity"], Value::from("medium"), "WARN→medium");
    assert_eq!(defect["line_start"], Value::from(12));
    assert_eq!(defect["line_end"], Value::from(20));
    assert_eq!(defect["metadata"]["kind"], Value::from("defect"));
    assert_eq!(defect["metadata"]["confidence"], Value::from(0.95));
    assert_eq!(defect["metadata"]["confidence_basis"], Value::from("ast_match"));
    let lw = &defect["metadata"]["loomweave"];
    assert_eq!(
        lw["entity_id"],
        Value::from("python:class:auth.tokens::TokenManager")
    );
    assert_eq!(
        lw["related_entities"],
        serde_json::json!(["python:class:auth.sessions::SessionStore"])
    );
    assert_eq!(
        lw["supports"],
        serde_json::json!(["python:function:auth.tokens::mint"])
    );
    assert_eq!(lw["supported_by"], serde_json::json!([]));
    assert_eq!(
        lw["internal_severity"],
        Value::from("WARN"),
        "internal vocabulary round-trips under loomweave.*"
    );
    assert_eq!(lw["internal_status"], Value::from("open"));

    // Row 1 — fact: NONE→info, no confidence/basis.
    let fact = &findings[1];
    assert_eq!(fact["severity"], Value::from("info"), "NONE→info");
    assert_eq!(fact["metadata"]["kind"], Value::from("fact"));
    assert!(
        fact["metadata"].get("confidence").is_none(),
        "fact omits confidence: {fact}"
    );
    assert!(
        fact["metadata"].get("confidence_basis").is_none(),
        "fact omits confidence_basis: {fact}"
    );
    assert_eq!(
        fact["metadata"]["loomweave"]["internal_severity"],
        Value::from("NONE")
    );

    // Row 2 — synthetic anchor: emits against default_path, flagged, no lines.
    let synthetic = &findings[2];
    assert_eq!(synthetic["path"], Value::from("/repo/root"));
    assert_eq!(synthetic["severity"], Value::from("high"), "ERROR→high");
    assert_eq!(
        synthetic["metadata"]["loomweave"]["synthetic_anchor"],
        Value::Bool(true)
    );
    assert!(
        synthetic.get("line_start").is_none() && synthetic.get("line_end").is_none(),
        "synthetic anchor carries no line position: {synthetic}"
    );
}

#[test]
fn producer_omits_wardlines_fingerprint_and_scanned_paths() {
    // The DISTINGUISHING contract: Loomweave's wire is fingerprint-LESS. Unlike
    // wardline's `POST /api/weft/scan-results` body (which carries a per-finding
    // `fingerprint` + `fingerprint_scheme` and a request-level `scanned_paths`),
    // Loomweave omits all three and relies on Filigree computing the dedup
    // fingerprint server-side. A regression that started emitting wardline-style
    // fields would red here.
    let wire = produced_wire_value();

    // Request-level: no `scanned_paths` (nor any of wardline's other request keys).
    let request_obj = wire.as_object().expect("request is a JSON object");
    assert!(
        !request_obj.contains_key("scanned_paths"),
        "Loomweave's scan-results request must NOT carry wardline's scanned_paths: {wire}"
    );

    // The request carries EXACTLY Loomweave's known keys — no un-sanctioned extra
    // (so a newly-added wardline-style top-level field reds here too).
    let request_keys: std::collections::BTreeSet<&str> =
        request_obj.keys().map(String::as_str).collect();
    let expected: std::collections::BTreeSet<&str> = [
        "scan_source",
        "scan_run_id",
        "mark_unseen",
        "create_observations",
        "complete_scan_run",
        "findings",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        request_keys, expected,
        "Loomweave's scan-results request key set drifted (extra/missing top-level field)"
    );

    // Per-finding: no `fingerprint` / `fingerprint_scheme` on ANY finding, at the
    // top level OR nested under metadata / metadata.loomweave.
    for (i, finding) in wire["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .enumerate()
    {
        for banned in ["fingerprint", "fingerprint_scheme"] {
            assert!(
                finding.get(banned).is_none(),
                "finding[{i}] must NOT carry wardline's top-level {banned}: {finding}"
            );
            assert!(
                finding["metadata"].get(banned).is_none(),
                "finding[{i}].metadata must NOT carry wardline's {banned}: {finding}"
            );
            assert!(
                finding["metadata"]["loomweave"].get(banned).is_none(),
                "finding[{i}].metadata.loomweave must NOT carry wardline's {banned}: {finding}"
            );
        }
    }
}
