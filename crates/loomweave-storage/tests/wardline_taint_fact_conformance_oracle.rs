//! Wardline → Loomweave taint-fact wire conformance oracle.
//!
//! The CONSUMER side of the cross-repo "wardline-taint-1" taint-fact seam.
//! Wardline AUTHORS per-entity taint-fact blobs (`build_taint_facts`, in
//! `wardline/src/wardline/loomweave/facts.py`) and ships them over
//! `POST /api/wardline/taint-facts`; Loomweave (this crate) CONSUMES them:
//! resolves the dotted qualname to an entity, stores the `wardline_json` blob
//! VERBATIM keyed by that entity, and serves it back on the read paths. This
//! oracle pins that the bytes Wardline produces are accepted + round-tripped by
//! Loomweave's REAL storage code path.
//!
//! Mirrors the proven layering of `sei_conformance_oracle.rs`:
//!
//!   * Layer 1 — a byte-pin: a `blake3` digest over the vendored golden bytes,
//!     asserted against a const. If the vendored fixture drifts by a single
//!     byte the pin reds. (Proven to red on tamper — see the module note below.)
//!
//!   * A NON-CIRCULAR consumer oracle: the golden's taint-fact blobs are fed
//!     through Loomweave's REAL deserialize/store/read API
//!     (`resolve_wardline_qualname` → `upsert_taint_fact` → `get_taint_facts`
//!     plus a direct read of the queryable `content_hash_at_compute` column),
//!     asserting Loomweave ACCEPTS them: the entity qualname resolves `Exact`,
//!     the `wardline_json` blob is stored + read back BYTE-VERBATIM, and the
//!     content-hash is preserved (both the in-blob copy and the queryable
//!     column). The assertions are driven off Loomweave's stored/returned bytes,
//!     NOT off the golden restated against itself.
//!
//!   * Layer 2 — a drift recheck: the vendored fixture bytes are compared
//!     against the authority golden in the Wardline repo
//!     (`$WARDLINE_REPO/tests/conformance/fixtures/wardline-taint-fact-wire.golden.json`,
//!     `WARDLINE_REPO` defaulting to `/home/john/wardline`). Skip-clean when the
//!     sibling repo is absent (CI / detached checkout): the oracle still passes
//!     on the vendored copy + Layer-1 pin.
//!
//! ── Scope / honesty caveat ──
//! This is a STORAGE-crate oracle. It drives the storage ingest/read API
//! (`loomweave_storage::{resolve_wardline_qualname, upsert_taint_fact,
//! get_taint_facts, TaintFact}`) — the same functions the cli HTTP handlers in
//! `loomweave-cli/src/http_read/wardline.rs` call. It does NOT exercise the cli
//! HTTP layer itself: the wire structs (`TaintFactInput`, `WriteTaintFactsRequest`)
//! and route handlers there are `pub(crate)` and unreachable from this crate, so
//! the oracle re-declares the minimal wire shape (`qualname` +
//! `content_hash_at_compute` + `wardline_json` as a byte-verbatim `RawValue`) and
//! drives the storage API directly. The verbatim-bytes contract that the cli
//! handler relies on (`RawValue::get()` → `upsert_taint_fact`) is reproduced here
//! faithfully; end-to-end HTTP coverage lives in
//! `loomweave-cli/src/http_read/wardline.rs`'s own tests.

use std::path::PathBuf;

use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::value::RawValue;

use loomweave_storage::schema::apply_migrations;
use loomweave_storage::{
    Resolution, TaintFact, get_taint_facts, resolve_wardline_qualname, upsert_taint_fact,
};

/// The Wardline authority golden, vendored BYTE-IDENTICAL from
/// `wardline/tests/conformance/fixtures/wardline-taint-fact-wire.golden.json`
/// (confirmed via `cmp`). `include_str!` embeds the exact on-disk bytes.
const GOLDEN: &str = include_str!("fixtures/wardline-taint-fact-wire.golden.json");

/// Layer-1 byte-pin: lowercase-hex `blake3` of the vendored golden's exact
/// bytes. Pins the fixture so a silent edit/re-vendor reds here.
///
/// Tamper proof: perturbing one hex char of this const (or one byte of the
/// fixture) makes `golden_bytes_match_layer1_pin` fail with a
/// `left != right` mismatch — the pin is load-bearing, not decorative.
const GOLDEN_BLAKE3: &str = "5ecabddd14bfb6a1c245c62bfa7b34e2cb4a5c9209c0f7da0250e7293f91ca6a";

/// The plugin under which Wardline's Python-frontend qualnames resolve. The
/// golden is a Python scan (`svc.py`), so its qualnames live under
/// `python:function:<qualname>` (ADR-036; Wardline pre-composes the dotted
/// qualname to byte-match Loomweave's `canonical_qualified_name`).
const PLUGIN: &str = "python";

/// One taint fact AS WARDLINE PUTS IT ON THE WIRE. This re-declares the cli's
/// `pub(crate)` `TaintFactInput` shape (`loomweave-cli/src/http_read/wardline.rs`):
/// `wardline_json` is a `Box<RawValue>` so the ORIGINAL bytes of the blob are
/// captured verbatim — a `serde_json::Value` would reorder object keys
/// (`BTreeMap`) and recompact, destroying the byte-verbatim contract the seam
/// guarantees.
#[derive(Debug, Deserialize)]
struct GoldenFact {
    qualname: String,
    content_hash_at_compute: String,
    wardline_json: Box<RawValue>,
}

fn golden_facts() -> Vec<GoldenFact> {
    serde_json::from_str(GOLDEN).expect("vendored golden parses as a taint-fact list")
}

/// Fresh in-memory DB with the REAL schema applied (migrations 0001..0010,
/// incl. `0003_wardline_taint_facts`). `foreign_keys` ON via the read pragmas
/// so the `entity_id → entities.id` FK is live (production parity).
fn migrated_conn() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open in-memory db");
    apply_migrations(&mut conn).expect("apply migrations");
    loomweave_storage::pragma::apply_read_pragmas(&conn).expect("apply read pragmas");
    conn
}

/// Seed a full `entities` row for `python:function:<qualname>`. Both legs of the
/// real consumer path need it: `resolve_wardline_qualname` returns `Exact` only
/// when the row exists, and `get_taint_facts` JOINs `entities`. Column list
/// mirrors `wardline_taint.rs`'s `insert_entity` test helper.
fn seed_entity(conn: &Connection, qualname: &str) -> String {
    let id = format!("{PLUGIN}:function:{qualname}");
    conn.execute(
        "INSERT INTO entities ( \
            id, plugin_id, kind, name, short_name, properties, \
            content_hash, source_file_path, created_at, updated_at \
         ) VALUES (?1, ?2, 'function', ?3, ?4, '{}', 'deadbeef', ?5, ?6, ?6)",
        params![
            id,
            PLUGIN,
            qualname,
            qualname.rsplit('.').next().unwrap_or(qualname),
            "svc.py",
            "2026-06-24T00:00:00.000Z",
        ],
    )
    .expect("seed entity row");
    id
}

/// Read the queryable `content_hash_at_compute` column directly from the real
/// `wardline_taint_facts` table — the column `TaintFactRow`/`get_taint_facts`
/// do NOT surface (they expose only `wardline_json` + `source_file_path` + `sei`).
fn stored_content_hash(conn: &Connection, entity_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT content_hash_at_compute FROM wardline_taint_facts WHERE entity_id = ?1",
        params![entity_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .expect("query stored content_hash")
}

// ── Layer 1 — byte-pin ───────────────────────────────────────────────────────

#[test]
fn golden_bytes_match_layer1_pin() {
    let actual = blake3::hash(GOLDEN.as_bytes()).to_hex().to_string();
    assert_eq!(
        actual, GOLDEN_BLAKE3,
        "vendored wardline-taint-fact golden drifted from its byte-pin; \
         re-vendor BYTE-IDENTICAL from wardline and update GOLDEN_BLAKE3"
    );
}

#[test]
fn golden_is_the_expected_three_fact_shape() {
    // A cheap structural floor so the consumer oracle below isn't silently
    // exercising an empty list (a regression that vendored a `[]` would pass the
    // round-trip vacuously). The authority golden is the three svc.py facts.
    let facts = golden_facts();
    assert_eq!(facts.len(), 3, "golden carries exactly three taint facts");
    let qualnames: Vec<&str> = facts.iter().map(|f| f.qualname.as_str()).collect();
    assert_eq!(qualnames, ["svc.read_raw", "svc.helper", "svc.leaky"]);
}

// ── NON-CIRCULAR consumer oracle ─────────────────────────────────────────────

#[test]
fn loomweave_accepts_and_roundtrips_every_golden_fact_verbatim() {
    let conn = migrated_conn();
    let facts = golden_facts();

    for fact in &facts {
        // The exact bytes Wardline ships for this fact's blob (no key reorder).
        let wire_blob: &str = fact.wardline_json.get();

        // 1. RESOLVE — Loomweave's real exact-tier resolver maps the dotted
        //    qualname to its entity id. Seed the entity first (an analyze run
        //    would have placed it); then assert resolution is Exact to the
        //    `python:function:<qualname>` id. This is the "qualname resolves"
        //    leg, driven through `resolve_wardline_qualname`.
        let expected_id = seed_entity(&conn, &fact.qualname);
        let resolution =
            resolve_wardline_qualname(&conn, &fact.qualname).expect("resolve qualname");
        assert_eq!(
            resolution,
            Resolution::Exact {
                entity_id: expected_id.clone(),
            },
            "golden qualname {} must resolve Exact to {expected_id}",
            fact.qualname
        );
        let entity_id = resolution.into_entity_id().expect("exact has an id");

        // 2. STORE — feed the fact through the REAL writer. `TaintFact.wardline_json`
        //    takes the verbatim blob bytes exactly as the cli handler does
        //    (`RawValue::get().to_owned()`); `content_hash_at_compute` is the
        //    top-level queryable column Wardline ships alongside the blob.
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: entity_id.clone(),
                wardline_json: wire_blob.to_owned(),
                scan_id: Some("conformance-scan".to_owned()),
                content_hash_at_compute: Some(fact.content_hash_at_compute.clone()),
                updated_at: "2026-06-24T00:00:00.000Z".to_owned(),
                sei: None,
            },
        )
        .expect("upsert golden taint fact");

        // 3. READ BACK — Loomweave's real reader returns the blob VERBATIM.
        let rows =
            get_taint_facts(&conn, std::slice::from_ref(&entity_id)).expect("get taint facts");
        assert_eq!(rows.len(), 1, "exactly one stored fact for {entity_id}");
        let row = &rows[0];
        assert_eq!(row.entity_id, entity_id);
        assert_eq!(
            row.wardline_json, wire_blob,
            "Loomweave must store + return the wardline_json blob byte-verbatim \
             (key order included) for {}",
            fact.qualname
        );

        // 4. CONTENT-HASH PRESERVED — two ways, both read off Loomweave's store:
        //    (a) the queryable column, read directly from the real table;
        //    (b) the in-blob copy, parsed back out of the verbatim bytes the
        //        reader returned. Both must equal the golden's content hash.
        assert_eq!(
            stored_content_hash(&conn, &entity_id).as_deref(),
            Some(fact.content_hash_at_compute.as_str()),
            "queryable content_hash_at_compute column must be preserved for {}",
            fact.qualname
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&row.wardline_json).expect("stored blob is valid JSON");
        assert_eq!(
            parsed["content_hash_at_compute"].as_str(),
            Some(fact.content_hash_at_compute.as_str()),
            "in-blob content_hash_at_compute must survive the round-trip for {}",
            fact.qualname
        );
        // The blob self-identifies as the wardline-taint-1 schema and echoes the
        // qualname — proves we round-tripped the RIGHT fact, not an empty stub.
        assert_eq!(parsed["schema_version"].as_str(), Some("wardline-taint-1"));
        assert_eq!(parsed["qualname"].as_str(), Some(fact.qualname.as_str()));
    }
}

#[test]
fn golden_facts_cover_the_blob_variants() {
    // Beyond the per-fact round-trip, assert the three facts exercise the
    // structural variants of the wardline-taint-1 blob the consumer must accept:
    // an entry-point root with no findings, a non-root fallback, and a leaky
    // entry-point WITH a finding + a resolved contributing callee. This keeps
    // the oracle honest if the golden is ever re-vendored to a thinner shape.
    let facts = golden_facts();
    let parsed: Vec<serde_json::Value> = facts
        .iter()
        .map(|f| serde_json::from_str(f.wardline_json.get()).expect("blob json"))
        .collect();

    // svc.read_raw — root entry-point, no findings, anchored EXTERNAL_RAW.
    assert_eq!(parsed[0]["dead_code_root"]["is_root"], true);
    assert_eq!(parsed[0]["findings"].as_array().unwrap().len(), 0);

    // svc.helper — non-root fallback, no findings.
    assert_eq!(parsed[1]["dead_code_root"]["is_root"], false);
    assert_eq!(parsed[1]["taint"]["source"], "fallback");

    // svc.leaky — root entry-point WITH a finding + a contributing callee.
    assert_eq!(parsed[2]["dead_code_root"]["is_root"], true);
    let findings = parsed[2]["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["rule_id"], "PY-WL-101");
    assert_eq!(
        parsed[2]["taint"]["contributing_callee_qualname"], "svc.read_raw",
        "the leaky entity's contributing callee must round-trip"
    );
}

// ── Layer 2 — drift recheck vs the Wardline source of truth ──────────────────

#[test]
fn vendored_golden_matches_wardline_authority() {
    let repo = std::env::var("WARDLINE_REPO").unwrap_or_else(|_| "/home/john/wardline".to_owned());
    let authority: PathBuf = PathBuf::from(repo)
        .join("tests")
        .join("conformance")
        .join("fixtures")
        .join("wardline-taint-fact-wire.golden.json");

    if !authority.exists() {
        // Skip-clean: the sibling Wardline repo is absent (CI / detached
        // checkout). The vendored copy + Layer-1 pin still hold; we just can't
        // recheck against the upstream source here.
        eprintln!(
            "wardline authority golden not found at {} — skipping Layer-2 drift recheck \
             (set WARDLINE_REPO to enable)",
            authority.display()
        );
        return;
    }

    let authority_bytes = std::fs::read(&authority).expect("read wardline authority golden");
    assert_eq!(
        authority_bytes,
        GOLDEN.as_bytes(),
        "vendored golden has DRIFTED from the Wardline authority at {}; \
         re-vendor BYTE-IDENTICAL",
        authority.display()
    );
}
