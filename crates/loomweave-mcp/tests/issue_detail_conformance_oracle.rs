//! Filigree → Loomweave issue-detail wire conformance oracle.
//!
//! The CONSUMER side of the cross-repo `filigree<->loomweave` issue-detail seam.
//! FILIGREE is the PRODUCER/authority of the `GET /api/weft/issues/{issue_id}`
//! response (weft generation, ADR-002 / Phase C3): the route handler
//! `api_weft_get_issue` projects a stored issue through `issue_to_weft` into the
//! `IssueWeft` shape, renaming the entity's OWN primary key `id` → `issue_id`
//! while reference fields keep their classic names. LOOMWEAVE (this crate) is the
//! CONSUMER: its `issues_for` reverse-join enriches a matched association row by
//! deserializing that body through
//! `loomweave_federation::filigree::parse_issue_detail_response` (re-exported as
//! `loomweave_mcp::filigree::parse_issue_detail_response`) into the four-field
//! `IssueDetail` stub.
//!
//! This oracle pins that the bytes Filigree produces are accepted + correctly
//! deserialized by Loomweave's REAL parse code path. Mirrors the proven layering
//! of `entity_associations_conformance_oracle.rs`:
//!
//!   * Layer 1 — a byte-pin: a `blake3` digest over the vendored golden bytes,
//!     asserted against a const. A single-byte drift reds the pin. (Proven to red
//!     on tamper — see `golden_bytes_match_layer1_pin_rejects_a_mutated_byte`.)
//!
//!   * A NON-CIRCULAR consumer oracle: the golden's 200 response body is fed
//!     through Loomweave's REAL `parse_issue_detail_response`, and the parsed
//!     `IssueDetail` is asserted off the REAL parser's output (NOT the golden
//!     restated against itself). Crucially, the load-bearing property of THIS seam
//!     is the OPPOSITE of the entity-associations seam: there the contract was
//!     graceful degradation (defaults, ignore-unknown); HERE `title`, `status`,
//!     and `priority` are REQUIRED (no `serde(default)`) and `priority` is `i64` —
//!     dropping or retyping any of them MUST hard-fail. A positive parse alone
//!     cannot prove that (required and default fields parse a full body
//!     identically), so the core carries NEGATIVE cases: drop/retype each required
//!     field via a mutated `Value` and assert the REAL parser returns `Err`.
//!
//!     The `id` field is the one exception — its source carries
//!     `#[serde(alias = "issue_id", default)]`, so `id` deserializes from the
//!     route's `issue_id` AND is itself default. Dropping `issue_id` therefore
//!     does NOT fail (it degrades to `""`); the alias is proven POSITIVELY
//!     instead (golden carries `issue_id`, no `id`; parser yields the id).
//!
//!   * Layer 2 — a drift recheck: the vendored fixture bytes are byte-compared
//!     against the authority golden in the Filigree repo
//!     (`$FILIGREE_REPO/tests/fixtures/contracts/weft/issues-get.json`,
//!     `FILIGREE_REPO` defaulting to `/home/john/filigree`). Filigree is the
//!     PRODUCER, so its copy is the authority. When the sibling repo is absent the
//!     recheck is SKIP-CLEAN *by default* — Layer-1 + the vendored copy still gate
//!     — but a release-gate job can ARM it via `LOOMWEAVE_DRIFT_REQUIRED=1`
//!     (`1`/`true`/`yes`/`on`), turning the absent-sibling skip into a hard
//!     FAILURE so a release cut never silently ships the authority check un-run.
//!     The arming decision is a pure helper (`drift_check_action`) so all three
//!     branches are exercised deterministically without mutating process-global
//!     env.
//!
//! ── Scope / honesty caveats ──
//!
//! * Parse-surface, not envelope-surface. `parse_issue_detail_response` and
//!   `IssueDetail` are `pub` (reachable from this external test crate via the
//!   `pub use loomweave_federation::filigree::*` re-export), so this oracle drives
//!   them directly. The downstream `issues_for` enrichment ENVELOPE (how an
//!   `IssueDetail` is folded into a match row) is crate-private and is NOT
//!   re-proven here; it is pinned in-crate by `storage_tools.rs`. This oracle
//!   proves the precondition: the real parser yields PRECISELY the (id, title,
//!   status, priority) tuple the enrichment consumes off the producer's wire.
//!
//! * Only the `live_v_issue_detail_200` example is fed to the parser. The
//!   `error_not_found` example is a different (`error`/`code`) shape handled at the
//!   HTTP-status layer (`get_json_or_none` → `None`), already covered in-crate by
//!   `issue_detail_http_client_maps_404_to_none`; it is NOT parsed here.
//!
//! * Layer 2 is fixture-to-fixture, not producer-source. It byte-compares the
//!   vendored copy against Filigree's vendored fixture FILE — it does NOT re-invoke
//!   Filigree's Python route handler from Rust. The authority-vs-real-producer gap
//!   is closed on Filigree's OWN side, by its producer-source wire oracle
//!   (`filigree/tests/federation/test_weft_issue_detail_wire_conformance_oracle.py`,
//!   `test_real_handler_produces_golden_issue_detail_shape`), which drives the live
//!   `GET /api/weft/issues/{issue_id}` route over its ASGI app with self-owned
//!   seeds and ties the produced key shape to the golden. This Rust test cannot
//!   invoke that Python oracle, so Layer-2 from this side stays fixture-to-fixture
//!   by design; the producer-source assurance lives in (and is owned by) Filigree.

use std::path::PathBuf;

use loomweave_mcp::filigree::{IssueDetail, parse_issue_detail_response};
use serde_json::Value;

/// The Filigree authority golden, vendored BYTE-IDENTICAL from
/// `filigree/tests/fixtures/contracts/weft/issues-get.json` (confirmed via `cmp`).
/// `include_str!` embeds the exact on-disk bytes.
const GOLDEN: &str = include_str!("../../../docs/federation/fixtures/filigree-issues-get.json");

/// Layer-1 byte-pin: lowercase-hex `blake3` of the vendored golden's exact bytes.
/// Pins the fixture so a silent edit/re-vendor reds here.
///
/// Tamper proof: perturbing one hex char of this const (or one byte of the
/// fixture) makes `golden_bytes_match_layer1_pin` fail with a `left != right`
/// mismatch — the pin is load-bearing, not decorative. The
/// `golden_bytes_match_layer1_pin_rejects_a_mutated_byte` test additionally
/// demonstrates that a single mutated input byte produces a DIFFERENT digest.
const GOLDEN_BLAKE3: &str = "94f2659a44f6e0a976fc6bc3f7957aee7f4bc613c07473ef40c9b0dba7134e03";

/// The 200-response example name in the fixture's `examples` array. The other
/// example (`error_not_found`) is a different shape handled at the HTTP layer and
/// is deliberately NOT fed to the parser here.
const EXAMPLE_NAME: &str = "live_v_issue_detail_200";

/// Pull the `response.body` of the named example straight out of the vendored
/// golden, re-serialized to the bytes the producer puts on the wire. This is the
/// exact body Filigree's route returns; we feed it to the REAL consumer parser.
fn golden_response_body(example_name: &str) -> Value {
    let fixture: Value =
        serde_json::from_str(GOLDEN).expect("vendored issue-detail golden parses as JSON");
    fixture
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| {
            examples
                .iter()
                .find(|example| example.get("name").and_then(Value::as_str) == Some(example_name))
        })
        .and_then(|example| example.pointer("/response/body"))
        .unwrap_or_else(|| panic!("missing fixture example body {example_name}"))
        .clone()
}

fn golden_body_string(example_name: &str) -> String {
    serde_json::to_string(&golden_response_body(example_name)).expect("re-serialize golden body")
}

// ── Layer 1 — byte-pin ───────────────────────────────────────────────────────

#[test]
fn golden_bytes_match_layer1_pin() {
    let actual = blake3::hash(GOLDEN.as_bytes()).to_hex().to_string();
    assert_eq!(
        actual, GOLDEN_BLAKE3,
        "vendored filigree issue-detail golden drifted from its byte-pin; \
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
fn real_parser_accepts_the_golden_and_extracts_the_modeled_tuple() {
    // Drive Loomweave's REAL parser on the producer's full 25-field wire body.
    let body = golden_body_string(EXAMPLE_NAME);
    let parsed: IssueDetail = parse_issue_detail_response(&body)
        .expect("Loomweave must accept Filigree's canonical issue-detail 200 body");

    // The four modeled fields land off the REAL parser's output, asserted against
    // the producer's source-of-truth literals.
    assert_eq!(parsed.title, "golden issue detail");
    assert_eq!(parsed.status, "open");
    assert_eq!(parsed.priority, 2);
}

#[test]
fn real_parser_routes_issue_id_into_id_via_the_weft_rename() {
    // The contract's whole reason to exist: the weft vocabulary renames the
    // entity's OWN primary key `id` → `issue_id`. The consumer's `IssueDetail.id`
    // carries `#[serde(alias = "issue_id", default)]`, so it deserializes FROM the
    // route's `issue_id`. Prove the alias is wired off the REAL parser's output.
    let body_value = golden_response_body(EXAMPLE_NAME);

    // Sanity: the golden body carries `issue_id` and NOT the classic `id` (else
    // this would assert the alias vacuously).
    assert!(
        body_value.get("issue_id").is_some(),
        "golden body must carry the weft-renamed issue_id key"
    );
    assert!(
        body_value.get("id").is_none(),
        "golden body must NOT carry the classic id key (it is renamed to issue_id)"
    );

    let parsed: IssueDetail =
        parse_issue_detail_response(&body_value.to_string()).expect("golden 200 body must parse");
    assert_eq!(
        parsed.id, "genp-0123456789",
        "id deserializes from the route's issue_id field (weft-4a46553503)"
    );
}

#[test]
fn real_parser_ignores_the_unmodelled_producer_fields() {
    // `IssueDetail` models only 4 of the 25 IssueWeft fields. The rest
    // (status_category, type, parent_id, assignee, the lifecycle/commit anchors,
    // description, notes, fields, labels, blocks, blocked_by, is_ready, children,
    // data_warnings, the four *_at timestamps) are present in the golden body but
    // NOT modelled. The consumer must IGNORE them additively (no
    // `deny_unknown_fields`) so it keeps parsing as Filigree grows the route.
    let body_value = golden_response_body(EXAMPLE_NAME);

    // Sanity: the golden body really does carry those unmodelled keys (otherwise
    // this test would pass vacuously).
    for unmodelled in [
        "status_category",
        "type",
        "parent_id",
        "assignee",
        "claimed_at",
        "created_at",
        "is_ready",
        "children",
        "data_warnings",
    ] {
        assert!(
            body_value.get(unmodelled).is_some(),
            "golden body must carry the unmodelled producer field {unmodelled} \
             (else the ignore-unknown assertion is vacuous)"
        );
    }

    // The REAL parser accepts the body despite the 21 extra fields.
    let parsed: IssueDetail = parse_issue_detail_response(&body_value.to_string())
        .expect("consumer must tolerate the unmodelled additive producer fields");
    assert_eq!(parsed.id, "genp-0123456789");
}

// ── NON-CIRCULAR negative cases — the discriminator for THIS seam ─────────────
//
// `title`/`status`/`priority` carry NO `#[serde(default)]`, and `priority` is
// typed `i64`. A positive parse cannot prove that (a defaulted or string-typed
// field would parse the full golden identically). These negatives drive the REAL
// parser on a MUTATED golden body and assert it hard-fails — making "dropping or
// retyping any required field hard-fails" executable, not aspirational.

/// Take the golden 200 body, apply `mutate`, re-serialize, feed to the REAL
/// parser, and assert it returns `Err`. The base body parses (asserted above), so
/// any `Err` here is attributable to the mutation alone.
fn assert_mutation_rejected(mutate: impl FnOnce(&mut serde_json::Map<String, Value>), why: &str) {
    let mut body_value = golden_response_body(EXAMPLE_NAME);
    let obj = body_value
        .as_object_mut()
        .expect("golden 200 body is a JSON object");
    mutate(obj);
    let result = parse_issue_detail_response(&Value::Object(obj.clone()).to_string());
    assert!(result.is_err(), "REAL parser must reject {why}: {result:?}");
}

#[test]
fn real_parser_rejects_a_dropped_title() {
    // `title` is required (no serde default): a producer that stopped emitting it
    // is a breaking shape regression, not a tolerable degrade.
    assert_mutation_rejected(
        |obj| {
            obj.remove("title");
        },
        "a body missing the required `title` field",
    );
}

#[test]
fn real_parser_rejects_a_dropped_status() {
    // `status` is required (no serde default).
    assert_mutation_rejected(
        |obj| {
            obj.remove("status");
        },
        "a body missing the required `status` field",
    );
}

#[test]
fn real_parser_rejects_a_dropped_priority() {
    // `priority` is required (no serde default).
    assert_mutation_rejected(
        |obj| {
            obj.remove("priority");
        },
        "a body missing the required `priority` field",
    );
}

#[test]
fn real_parser_rejects_a_string_typed_priority() {
    // `priority` is typed `i64`. A producer that emitted it as a JSON string
    // (`"2"`) is a retyping regression the consumer must reject, not coerce.
    assert_mutation_rejected(
        |obj| {
            obj.insert("priority".to_owned(), Value::String("2".to_owned()));
        },
        "a `priority` retyped to a JSON string",
    );
}

#[test]
fn real_parser_rejects_a_null_priority() {
    // `priority` is a non-optional `i64`; a null is a retyping/drop regression.
    assert_mutation_rejected(
        |obj| {
            obj.insert("priority".to_owned(), Value::Null);
        },
        "a `priority` retyped to JSON null",
    );
}

// ── Layer 2 — drift recheck vs the Filigree producer source of truth ─────────

/// Env var that ARMS the cross-repo drift recheck: when set truthy
/// (`1`/`true`/`yes`/`on`), an absent sibling Filigree repo becomes a hard
/// FAILURE instead of a skip-clean, so a release-gate run can require the
/// cross-repo authority recheck to actually execute rather than silently skip.
///
/// NOT YET WIRED INTO CI: no job in `.github/workflows/{ci,release,verify}.yml`
/// currently sets this env or checks out the sibling Filigree repo (`FILIGREE_REPO`),
/// so in automation `vendored_golden_matches_filigree_authority` skips clean —
/// the byte-drift binding is developer-local / release-gate-manual, not
/// CI-enforced. The mechanism below is verified (the 2x2 `drift_check_action`
/// unit tests); only the trigger is absent. Wiring a release-gate job that sets
/// `LOOMWEAVE_DRIFT_REQUIRED=1` with `FILIGREE_REPO` pointed at a sibling checkout
/// is the remaining step to make this gate non-decorative in CI.
const DRIFT_REQUIRED_ENV: &str = "LOOMWEAVE_DRIFT_REQUIRED";

/// The action the Layer-2 recheck must take, given (a) whether the recheck is
/// armed as REQUIRED and (b) whether the sibling Filigree authority fixture is
/// present. Pure + total over the 2×2 so all three outcomes are unit-testable
/// without touching process-global env (which `cargo`'s parallel test threads
/// share) or the filesystem.
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
        .join("weft")
        .join("issues-get.json");

    match drift_check_action(drift_required(), authority.exists()) {
        DriftCheck::SkipClean => {
            eprintln!(
                "filigree authority fixture not found at {} — skipping Layer-2 drift recheck \
                 (set FILIGREE_REPO to enable, or {DRIFT_REQUIRED_ENV}=1 to make absence a failure)",
                authority.display()
            );
        }
        DriftCheck::FailRequired => {
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
    assert_eq!(drift_check_action(false, true), DriftCheck::Compare);
    assert_eq!(drift_check_action(true, true), DriftCheck::Compare);
}

#[test]
fn drift_check_action_skips_clean_when_absent_and_unarmed() {
    assert_eq!(drift_check_action(false, false), DriftCheck::SkipClean);
}

#[test]
fn drift_check_action_fails_when_absent_but_required() {
    assert_eq!(drift_check_action(true, false), DriftCheck::FailRequired);
}
