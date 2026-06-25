//! Filigree → Loomweave entity-associations (ADR-029) wire conformance oracle.
//!
//! The CONSUMER side of the cross-repo `filigree<->loomweave` entity-associations
//! seam. FILIGREE is the PRODUCER: it stores Loomweave entity IDs OPAQUELY on its
//! `entity_associations` rows (it never parses the `{plugin}:{kind}:{qualname}` /
//! `loomweave:eid:*` grammar) and serves them back over
//! `GET /api/entity-associations?entity_id={entity_id}`
//! (`dashboard_routes.entities.api_list_associations_by_entity`). LOOMWEAVE (this
//! crate) is the CONSUMER: its `issues_for` reverse-join deserializes that body
//! through `loomweave_federation::filigree::parse_entity_associations_response`
//! (re-exported as `loomweave_mcp::filigree::parse_entity_associations_response`)
//! and joins each row's opaque `loomweave_entity_id` back to a current entity.
//!
//! This oracle pins that the bytes Filigree produces are accepted + correctly
//! deserialized by Loomweave's REAL parse code path, with the opaque bindings
//! round-tripped VERBATIM (never grammar-parsed) and exposing exactly the fields
//! the reverse-join consumes.
//!
//! Mirrors the proven layering of `wardline_taint_fact_conformance_oracle.rs`
//! and `sei_conformance_oracle.rs`:
//!
//!   * Layer 1 — a byte-pin: a `blake3` digest over the vendored golden bytes,
//!     asserted against a const. If the vendored fixture drifts by a single byte
//!     the pin reds. (Proven to red on tamper — see `golden_bytes_match_layer1_pin`.)
//!
//!   * A NON-CIRCULAR consumer oracle: the golden's response body is fed through
//!     Loomweave's REAL `parse_entity_associations_response` and the parsed
//!     `EntityAssociation` rows are asserted — the opaque `loomweave:eid:*`
//!     binding lands VERBATIM in `loomweave_entity_id` (un-parsed), and the row
//!     carries exactly the three fields the `issues_for` reverse-join joins on
//!     (`issue_id` for global dedup, `loomweave_entity_id` for the alias→current
//!     entity join, `content_hash_at_attach` for drift classification). The
//!     assertions are driven off the REAL parser's output, NOT off the golden
//!     restated against itself. The `clarion_entity_id` pre-v26 alias is also
//!     exercised — that is live parse behaviour the consumer must preserve.
//!
//!   * Layer 2 — a drift recheck: the vendored fixture bytes are compared against
//!     the authority golden in the Filigree repo
//!     (`$FILIGREE_REPO/tests/fixtures/contracts/entity-associations-response.json`,
//!     `FILIGREE_REPO` defaulting to `/home/john/filigree`). Filigree is the
//!     PRODUCER, so its copy is the authority. When the sibling repo is absent
//!     (CI / detached checkout) the recheck is SKIP-CLEAN *by default* — the
//!     oracle still passes on the vendored copy + Layer-1 pin — but a release-gate
//!     job can ARM the recheck via `LOOMWEAVE_DRIFT_REQUIRED=1` (`1`/`true`/`yes`/
//!     `on`), which turns the absent-sibling skip into a hard FAILURE so a release
//!     cut never silently ships the cross-repo authority check un-run. The
//!     arming decision is a pure helper (`drift_check_action`) so all three
//!     branches — compare / skip-clean / fail-required — are exercised
//!     deterministically without mutating process-global env (see the
//!     `drift_check_action_*` unit tests). Mirrors wardline's `_live_oracle.py`
//!     §5c SKIP→FAILURE primitive.
//!
//! ── Scope / honesty caveats ──
//!
//! * Parse-surface, not envelope-surface. `parse_entity_associations_response`,
//!   `EntityAssociation`, and `EntityAssociationsResponse` are `pub` (reachable
//!   from this external test crate via the `pub use loomweave_federation::filigree::*`
//!   re-export), so this oracle drives them directly. The downstream reverse-join
//!   ENVELOPE shape (`matched`/`drifted`/`not_found`/`result_kind`, built by the
//!   crate-PRIVATE `IssuesForAccumulator::{add_response,into_envelope}` +
//!   `association_json`) is NOT reachable from `tests/` and is NOT re-proven here.
//!   It is already pinned end-to-end in-crate by the `tool_issues_for` integration
//!   tests in `crates/loomweave-mcp/tests/storage_tools.rs` (e.g.
//!   `issues_for_includes_contained_entities_and_flags_drift`,
//!   `issues_for_reports_resolved_endpoint_and_result_kind`). This oracle proves
//!   the precondition those tests assume: the real parser yields PRECISELY the
//!   per-row inputs the reverse-join joins on.
//!
//! * Layer 2 is fixture-to-fixture, not producer-source. It byte-compares the
//!   vendored copy against Filigree's vendored fixture FILE — it does NOT
//!   re-invoke Filigree's Python route handler from Rust. It therefore catches
//!   vendored-vs-authority drift, not authority-vs-real-producer drift. The
//!   authority-vs-real-producer gap is now closed on Filigree's OWN side: it
//!   ships a real producer-source wire oracle
//!   (`filigree/tests/federation/test_entity_associations_wire_conformance_oracle.py`,
//!   `test_real_handler_produces_golden_row_shape`) that drives the live
//!   `GET /api/entity-associations` route over its ASGI app with self-owned seeds
//!   and ties the produced per-row key shape + derived enrichment to the golden.
//!   The only honest limitation HERE is that this Rust test cannot invoke that
//!   Python oracle, so Layer-2 from this side stays fixture-to-fixture by design;
//!   the producer-source assurance lives in (and is owned by) Filigree's suite.

use std::path::PathBuf;

use loomweave_mcp::filigree::{EntityAssociation, parse_entity_associations_response};
use serde_json::Value;

/// The Filigree authority golden, vendored BYTE-IDENTICAL from
/// `filigree/tests/fixtures/contracts/entity-associations-response.json`
/// (confirmed via `cmp`). `include_str!` embeds the exact on-disk bytes.
const GOLDEN: &str =
    include_str!("../../../docs/federation/fixtures/filigree-entity-associations-response.json");

/// Layer-1 byte-pin: lowercase-hex `blake3` of the vendored golden's exact
/// bytes. Pins the fixture so a silent edit/re-vendor reds here.
///
/// Tamper proof: perturbing one hex char of this const (or one byte of the
/// fixture) makes `golden_bytes_match_layer1_pin` fail with a `left != right`
/// mismatch — the pin is load-bearing, not decorative. The
/// `golden_bytes_match_layer1_pin_rejects_a_mutated_byte` test additionally
/// demonstrates that a single mutated input byte produces a DIFFERENT digest.
const GOLDEN_BLAKE3: &str = "9234531b1ec3ff5ee1edc77a0a6e2b61e367d9d96d5c8e9b4e99f5b4a47cf6d4";

/// The single canonical example name in the fixture's `examples` array.
const EXAMPLE_NAME: &str = "live_v27_reverse_lookup_200";

/// The opaque Loomweave entity binding the canonical row carries — an SEI token.
/// Filigree stores this VERBATIM and never parses its grammar.
const OPAQUE_SEI_BINDING: &str = "loomweave:eid:0123456789abcdef0123456789abcdef";

/// Pull the `response.body` of the named example straight out of the vendored
/// golden, re-serialized to the bytes the producer puts on the wire. This is the
/// exact body Filigree's route returns; we feed it to the REAL consumer parser.
fn golden_response_body(example_name: &str) -> String {
    let fixture: Value =
        serde_json::from_str(GOLDEN).expect("vendored entity-associations golden parses as JSON");
    let body = fixture
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| {
            examples
                .iter()
                .find(|example| example.get("name").and_then(Value::as_str) == Some(example_name))
        })
        .and_then(|example| example.pointer("/response/body"))
        .unwrap_or_else(|| panic!("missing fixture example body {example_name}"))
        .clone();
    serde_json::to_string(&body).expect("re-serialize golden response body")
}

// ── Layer 1 — byte-pin ───────────────────────────────────────────────────────

#[test]
fn golden_bytes_match_layer1_pin() {
    let actual = blake3::hash(GOLDEN.as_bytes()).to_hex().to_string();
    assert_eq!(
        actual, GOLDEN_BLAKE3,
        "vendored filigree entity-associations golden drifted from its byte-pin; \
         re-vendor BYTE-IDENTICAL from filigree and update GOLDEN_BLAKE3"
    );
}

#[test]
fn golden_bytes_match_layer1_pin_rejects_a_mutated_byte() {
    // Tamper proof: flipping one byte of the vendored golden produces a digest
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

// ── NON-CIRCULAR consumer oracle ─────────────────────────────────────────────

#[test]
fn real_parser_accepts_the_golden_and_round_trips_the_opaque_binding() {
    // Drive Loomweave's REAL parser on the producer's wire body.
    let body = golden_response_body(EXAMPLE_NAME);
    let parsed = parse_entity_associations_response(&body)
        .expect("Loomweave must accept Filigree's canonical entity-associations body");

    // The envelope's `associations` array deserialized to exactly one row — the
    // canonical g15-oracle binding.
    assert_eq!(
        parsed.associations.len(),
        1,
        "golden carries exactly one association row"
    );
    let row: &EntityAssociation = &parsed.associations[0];

    // (1) OPACITY — the opaque Loomweave entity ID lands in `loomweave_entity_id`
    //     BYTE-VERBATIM. The parser must NOT split/normalize the SEI grammar;
    //     it is an opaque join key (Filigree stored it opaquely, Loomweave
    //     resolves it via its alias map downstream).
    assert_eq!(
        row.loomweave_entity_id, OPAQUE_SEI_BINDING,
        "the opaque SEI binding must round-trip verbatim into loomweave_entity_id"
    );

    // (2) REVERSE-JOIN INPUTS — the parsed row carries exactly the three fields
    //     the `issues_for` reverse-join consumes (read off the REAL parser's
    //     output, asserted against the producer's source-of-truth literals):
    //       * issue_id              → global dedup key (`seen_issue_ids`)
    //       * loomweave_entity_id   → alias→current-entity join key
    //       * content_hash_at_attach → drift classification (matched vs drifted)
    assert_eq!(
        row.issue_id, "test-045076e30f",
        "issue_id (reverse-join dedup key) must parse"
    );
    assert_eq!(
        row.content_hash_at_attach, "hash-g15-oracle",
        "content_hash_at_attach (drift-classification key) must parse"
    );

    // (3) DISPLAY ENRICHMENT — the optional display-only fields parse when present
    //     (they `default` to empty when the producer omits them).
    assert_eq!(row.attached_at, "2026-06-13T00:00:00+00:00");
    assert_eq!(row.attached_by, "g15-oracle");
}

#[test]
fn real_parser_ignores_the_unmodelled_producer_fields() {
    // The v2/v3 warpline-seam fields (claimed_at, closed_at, claim_commit,
    // close_commit, status, status_category, orphan_status, signature, …) and the
    // co-emitted canonical `entity_id` are present in the golden body but NOT
    // modelled by `EntityAssociation`. The consumer must IGNORE them additively
    // (no `deny_unknown_fields`), so a v1/v2 consumer still parses a v3 body.
    let body = golden_response_body(EXAMPLE_NAME);

    // Sanity: the golden body really does carry those unmodelled keys (otherwise
    // this test would pass vacuously).
    let body_value: Value = serde_json::from_str(&body).expect("body parses");
    let first_row = &body_value["associations"][0];
    for unmodelled in [
        "entity_id",
        "entity_kind",
        "orphan_status",
        "freshness_status",
        "claimed_at",
        "closed_at",
        "claim_commit",
        "close_commit",
        "status",
        "status_category",
    ] {
        assert!(
            first_row.get(unmodelled).is_some(),
            "golden row must carry the unmodelled producer field {unmodelled} \
             (else the ignore-unknown assertion is vacuous)"
        );
    }

    // The REAL parser accepts the body despite those extra fields.
    let parsed = parse_entity_associations_response(&body)
        .expect("consumer must tolerate unmodelled additive producer fields");
    assert_eq!(parsed.associations.len(), 1);
}

#[test]
fn real_parser_tolerates_the_prev26_clarion_entity_id_alias() {
    // Live parse behaviour the consumer must preserve: a pre-v26 producer (or a
    // JSONL export) emits `clarion_entity_id` instead of `loomweave_entity_id`.
    // The `#[serde(alias = "clarion_entity_id")]` on the field must map it onto
    // `loomweave_entity_id`. Drive the REAL parser to prove the alias is wired.
    let legacy_body = r#"{"associations":[{
        "issue_id":"filigree-legacy",
        "clarion_entity_id":"loomweave:eid:0123456789abcdef0123456789abcdef",
        "content_hash_at_attach":"hash-legacy"
    }]}"#;
    let parsed = parse_entity_associations_response(legacy_body)
        .expect("consumer must tolerate the pre-v26 clarion_entity_id field name");
    assert_eq!(parsed.associations.len(), 1);
    assert_eq!(
        parsed.associations[0].loomweave_entity_id, OPAQUE_SEI_BINDING,
        "the pre-v26 clarion_entity_id alias must deserialize into loomweave_entity_id"
    );
}

#[test]
fn real_parser_degrades_absent_associations_to_empty_list() {
    // Enrich-only degrade: an empty/absent envelope key yields an empty list
    // (`#[serde(default)]`), NOT a hard `missing field associations` failure —
    // the reverse-join then reports `no_matches`, never an error. Drive the REAL
    // parser to prove the `default` is wired.
    let parsed = parse_entity_associations_response("{}")
        .expect("an empty envelope must degrade to an empty association list");
    assert!(
        parsed.associations.is_empty(),
        "absent associations key degrades to an empty list (enrich-only)"
    );
}

// ── Layer 2 — drift recheck vs the Filigree producer source of truth ─────────

/// Env var that ARMS the cross-repo drift recheck: when set truthy
/// (`1`/`true`/`yes`/`on`), an absent sibling Filigree repo becomes a hard
/// FAILURE instead of a skip-clean. A release-gate CI job sets this so a release
/// cut can never silently ship with the authority recheck un-run. Mirrors
/// wardline's `WARDLINE_LIVE_ORACLE_REQUIRED` SKIP→FAILURE primitive.
const DRIFT_REQUIRED_ENV: &str = "LOOMWEAVE_DRIFT_REQUIRED";

/// The action the Layer-2 recheck must take, given (a) whether the recheck is
/// armed as REQUIRED and (b) whether the sibling Filigree authority fixture is
/// present. Pure + total over the 2×2 so all three outcomes are unit-testable
/// without touching process-global env (which `cargo`'s parallel test threads
/// share — mutating it would race the real recheck) or the filesystem.
#[derive(Debug, PartialEq, Eq)]
enum DriftCheck {
    /// Sibling present — byte-compare the vendored golden against the authority.
    Compare,
    /// Sibling absent and the recheck is NOT armed — skip cleanly (CI / detached
    /// checkout). Layer-1 byte-pin + the vendored copy still gate the run.
    SkipClean,
    /// Sibling absent but the recheck IS armed (`LOOMWEAVE_DRIFT_REQUIRED=1`) —
    /// fail: a release-gate run must not skip the cross-repo authority check.
    FailRequired,
}

fn drift_check_action(required: bool, authority_exists: bool) -> DriftCheck {
    match (authority_exists, required) {
        (true, _) => DriftCheck::Compare,
        (false, false) => DriftCheck::SkipClean,
        (false, true) => DriftCheck::FailRequired,
    }
}

fn drift_required() -> bool {
    matches!(
        std::env::var(DRIFT_REQUIRED_ENV)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[test]
fn vendored_golden_matches_filigree_authority() {
    let repo = std::env::var("FILIGREE_REPO").unwrap_or_else(|_| "/home/john/filigree".to_owned());
    let authority: PathBuf = PathBuf::from(repo)
        .join("tests")
        .join("fixtures")
        .join("contracts")
        .join("entity-associations-response.json");

    match drift_check_action(drift_required(), authority.exists()) {
        DriftCheck::SkipClean => {
            // Sibling Filigree repo absent (CI / detached checkout) and the recheck
            // is NOT armed. The vendored copy + Layer-1 pin still hold; we just
            // can't recheck against the upstream producer source here.
            eprintln!(
                "filigree authority fixture not found at {} — skipping Layer-2 drift recheck \
                 (set FILIGREE_REPO to enable, or {DRIFT_REQUIRED_ENV}=1 to make absence a failure)",
                authority.display()
            );
        }
        DriftCheck::FailRequired => {
            // Armed for a release gate: an absent sibling is a HARD failure, not a
            // silent skip, so the cross-repo authority check is never un-run at a
            // release cut.
            panic!(
                "filigree authority fixture not found at {} but {DRIFT_REQUIRED_ENV} is set — \
                 the cross-repo drift recheck is REQUIRED; make the sibling Filigree repo \
                 available (FILIGREE_REPO) or unset {DRIFT_REQUIRED_ENV}",
                authority.display()
            );
        }
        DriftCheck::Compare => {
            let authority_bytes =
                std::fs::read(&authority).expect("read filigree authority fixture");
            assert_eq!(
                authority_bytes,
                GOLDEN.as_bytes(),
                "vendored golden has DRIFTED from the Filigree authority at {} (filigree is the \
                 PRODUCER — its copy is the authority); re-vendor BYTE-IDENTICAL",
                authority.display()
            );
        }
    }
}

#[test]
fn drift_check_action_compares_when_authority_present() {
    // Sibling present → byte-compare, regardless of the arming flag.
    assert_eq!(drift_check_action(false, true), DriftCheck::Compare);
    assert_eq!(drift_check_action(true, true), DriftCheck::Compare);
}

#[test]
fn drift_check_action_skips_clean_when_absent_and_unarmed() {
    // Sibling absent and recheck NOT armed → skip cleanly (default CI posture).
    assert_eq!(drift_check_action(false, false), DriftCheck::SkipClean);
}

#[test]
fn drift_check_action_fails_when_absent_but_required() {
    // Sibling absent but recheck ARMED → hard failure (release-gate posture). This
    // is the load-bearing arming the MEDIUM finding required: an absent-sibling run
    // under LOOMWEAVE_DRIFT_REQUIRED no longer shows green.
    assert_eq!(drift_check_action(true, false), DriftCheck::FailRequired);
}
