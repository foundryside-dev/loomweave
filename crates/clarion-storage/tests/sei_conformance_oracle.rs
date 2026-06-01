//! SEI conformance oracle (Loom SEI standard §8) — the shared, fixtures-based
//! conformance suite, exercised against a reference Clarion (this crate's real
//! matcher + identity store + resolution surface). Wave 1 / WS1 (ADR-038).
//!
//! The six §8 scenarios, each asserted end-to-end through `rebind_or_mint`,
//! `orphaned_bindings`, the writer primitives, and the `resolve*` reads:
//!
//! 1. identity round-trip + opacity
//! 2. rename (unchanged body)        → carry, `locator_changed`
//! 3. move (body + signature stable) → carry, `moved`
//! 4. ambiguous (rename + body edit) → fail closed: new SEI, old `orphaned`
//! 5. delete                         → `orphaned`, `resolve_sei` alive:false + lineage
//! 6. capability-absent              → consumer degrades (no crash, honest absence)
//!
//! The oracle is token-format-agnostic by design (ADR-038 §1 / SEI spec §8): it
//! asserts BEHAVIOUR + OPACITY, never the SEI's internal form.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};

use clarion_storage::{
    GitRename, LineageEvent, NewEntityDescriptor, SeiBindingRecord, SeiLineageEntry,
    SeiLookupResult, alive_bindings_snapshot, append_sei_lineage, has_any_alive_binding,
    is_reserved_sei, mint_sei, orphan_sei_binding, orphaned_bindings, rebind_or_mint,
    resolve_locator, resolve_sei, schema::apply_migrations, sei::BindingStatus, sei_lineage,
    set_entity_signature, upsert_sei_binding,
};

fn fresh_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&mut conn).unwrap();
    conn
}

fn entity(locator: &str, body: Option<&str>, sig: Option<&str>) -> NewEntityDescriptor {
    NewEntityDescriptor {
        locator: locator.to_owned(),
        body_hash: body.map(str::to_owned),
        signature: sig.map(str::to_owned),
    }
}

fn upsert_entity_row(conn: &Connection, locator: &str, body_hash: Option<&str>) {
    conn.execute(
        "INSERT INTO entities \
         (id, plugin_id, kind, name, short_name, properties, content_hash, created_at, updated_at) \
         VALUES (?1, 'python', 'function', ?1, ?1, '{}', ?2, 't0', 't0') \
         ON CONFLICT(id) DO UPDATE SET content_hash = excluded.content_hash",
        params![locator, body_hash],
    )
    .unwrap();
}

/// Apply one analysis run's SEI pass against the storage primitives, mirroring
/// the analyze mint pass orchestration (decide → dedup → orphan-first writes).
/// Returns the per-locator decision SEI for assertions.
fn apply_run(
    conn: &Connection,
    run_id: &str,
    descriptors: &[NewEntityDescriptor],
    git_renames: &[GitRename],
) -> HashMap<String, String> {
    // Entities are cumulative: insert/update every current entity row so the
    // resolution join finds a content_hash (delete leaves the row but orphans
    // the binding).
    for d in descriptors {
        upsert_entity_row(conn, &d.locator, d.body_hash.as_deref());
    }

    let alive = alive_bindings_snapshot(conn).unwrap();
    let current_locators: HashSet<String> = descriptors.iter().map(|d| d.locator.clone()).collect();
    let sei_to_old: HashMap<String, String> = alive
        .iter()
        .map(|(loc, b)| (b.sei.clone(), loc.clone()))
        .collect();

    let mut ordered: Vec<NewEntityDescriptor> = descriptors.to_vec();
    ordered.sort_by(|a, b| a.locator.cmp(&b.locator));

    let mut claimed: HashSet<String> = HashSet::new();
    let mut rematched: HashSet<String> = HashSet::new();
    let mut sei_by_locator: HashMap<String, String> = HashMap::new();
    let mut planned: Vec<(NewEntityDescriptor, String, Option<LineageEvent>)> = Vec::new();

    for d in ordered {
        let decision = rebind_or_mint(&d, &alive, &current_locators, git_renames, run_id);
        let (sei, event, is_carry) = match decision {
            clarion_storage::SeiDecision::Carry { sei, event } => (sei, event, true),
            clarion_storage::SeiDecision::Mint { sei } => (sei, Some(LineageEvent::Born), false),
        };
        // Dedup carries of the same SEI (fail-closed re-mint).
        let (sei, event) = if is_carry && !claimed.insert(sei.clone()) {
            (mint_sei(&d.locator, run_id), Some(LineageEvent::Born))
        } else {
            (sei, event)
        };
        if is_carry
            && matches!(
                event,
                Some(LineageEvent::LocatorChanged | LineageEvent::Moved)
            )
            && let Some(old) = sei_to_old.get(&sei)
        {
            rematched.insert(old.clone());
        }
        sei_by_locator.insert(d.locator.clone(), sei.clone());
        planned.push((d, sei, event));
    }

    let orphans = orphaned_bindings(&alive, &current_locators, &rematched);
    for sei in &orphans {
        orphan_sei_binding(conn, sei, run_id, "t-orphan").unwrap();
        append_sei_lineage(
            conn,
            &SeiLineageEntry {
                sei: sei.clone(),
                event: LineageEvent::Orphaned,
                old_locator: sei_to_old.get(sei).cloned(),
                new_locator: None,
                run_id: run_id.to_owned(),
                recorded_at: "t-orphan".to_owned(),
            },
        )
        .unwrap();
    }

    for (d, sei, event) in planned {
        set_entity_signature(conn, &d.locator, d.signature.as_deref()).unwrap();
        upsert_sei_binding(
            conn,
            &SeiBindingRecord {
                sei: sei.clone(),
                current_locator: Some(d.locator.clone()),
                body_hash: d.body_hash.clone(),
                signature: d.signature.clone(),
                status: BindingStatus::Alive,
                born_run_id: run_id.to_owned(),
                updated_run_id: run_id.to_owned(),
                updated_at: "t1".to_owned(),
            },
        )
        .unwrap();
        if let Some(event) = event {
            let (old_locator, new_locator) = match event {
                LineageEvent::LocatorChanged | LineageEvent::Moved => {
                    (sei_to_old.get(&sei).cloned(), Some(d.locator.clone()))
                }
                _ => (None, Some(d.locator.clone())),
            };
            append_sei_lineage(
                conn,
                &SeiLineageEntry {
                    sei: sei.clone(),
                    event,
                    old_locator,
                    new_locator,
                    run_id: run_id.to_owned(),
                    recorded_at: "t1".to_owned(),
                },
            )
            .unwrap();
        }
    }

    sei_by_locator
}

// ── §8.1 — identity round-trip + opacity ────────────────────────────────────
#[test]
fn oracle_identity_round_trip_and_opacity() {
    let conn = fresh_db();
    let seis = apply_run(
        &conn,
        "run-1",
        &[entity("python:function:m.f", Some("h1"), Some("s1"))],
        &[],
    );
    let sei = seis["python:function:m.f"].clone();

    // resolve(locator) → sei
    let by_locator = resolve_locator(&conn, "python:function:m.f")
        .unwrap()
        .unwrap();
    assert_eq!(by_locator.sei, sei);
    assert_eq!(
        by_locator.current_locator.as_deref(),
        Some("python:function:m.f")
    );
    assert_eq!(by_locator.content_hash.as_deref(), Some("h1"));

    // resolve_sei(sei) → locator (round-trip closes)
    match resolve_sei(&conn, &sei).unwrap() {
        SeiLookupResult::Alive(rec) => {
            assert_eq!(rec.current_locator.as_deref(), Some("python:function:m.f"));
        }
        SeiLookupResult::NotAlive { .. } => panic!("freshly minted SEI must be alive"),
    }

    // Opacity: the consumer treats the SEI as an opaque string. It carries the
    // reserved prefix and is NOT a locator (a colon-count check would be wrong).
    assert!(
        is_reserved_sei(&sei),
        "SEI must be opaque + reserved-prefixed"
    );
    assert_ne!(sei, "python:function:m.f");
}

// ── §8.2 — rename (unchanged body) → carry, locator_changed ──────────────────
#[test]
fn oracle_rename_carries_sei_with_locator_changed() {
    let conn = fresh_db();
    let r1 = apply_run(
        &conn,
        "run-1",
        &[entity("python:function:auth.login", Some("h1"), Some("s1"))],
        &[],
    );
    let original = r1["python:function:auth.login"].clone();

    // File/module rename: locator prefix changes, body identical, git signal present.
    let r2 = apply_run(
        &conn,
        "run-2",
        &[entity(
            "python:function:authn.login",
            Some("h1"),
            Some("s1"),
        )],
        &[GitRename {
            old_locator: "python:function:auth.login".to_owned(),
            new_locator: "python:function:authn.login".to_owned(),
        }],
    );
    assert_eq!(
        r2["python:function:authn.login"], original,
        "rename must CARRY the SEI (same token)"
    );
    // Old locator no longer resolves; new one does.
    assert!(
        resolve_locator(&conn, "python:function:auth.login")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        resolve_locator(&conn, "python:function:authn.login")
            .unwrap()
            .unwrap()
            .sei,
        original
    );
    // locator_changed lineage event recorded.
    let events: Vec<String> = sei_lineage(&conn, &original)
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(
        events,
        vec!["born".to_owned(), "locator_changed".to_owned()]
    );
}

// ── §8.3 — move (body + signature stable, new module) → carry, moved ─────────
#[test]
fn oracle_move_carries_sei_with_moved_event() {
    let conn = fresh_db();
    let r1 = apply_run(
        &conn,
        "run-1",
        &[entity("a.mod.f", Some("h1"), Some("s1"))],
        &[],
    );
    let original = r1["a.mod.f"].clone();

    // No git signal — identical body + signature at a new module carries (moved).
    let r2 = apply_run(
        &conn,
        "run-2",
        &[entity("b.mod.f", Some("h1"), Some("s1"))],
        &[],
    );
    assert_eq!(r2["b.mod.f"], original, "move must CARRY the SEI");
    let events: Vec<String> = sei_lineage(&conn, &original)
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(events, vec!["born".to_owned(), "moved".to_owned()]);
}

// ── §8.4 — ambiguous (rename WITH body edit) → fail closed ───────────────────
#[test]
fn oracle_ambiguous_fails_closed() {
    let conn = fresh_db();
    let r1 = apply_run(
        &conn,
        "run-1",
        &[entity("python:function:auth.login", Some("h1"), Some("s1"))],
        &[],
    );
    let original = r1["python:function:auth.login"].clone();

    // Rename WITH a body edit: even with a git signal, the body is not identical,
    // so the matcher cannot PROVE sameness → mint new, orphan old.
    let r2 = apply_run(
        &conn,
        "run-2",
        &[entity(
            "python:function:authn.login",
            Some("h2-edited"),
            Some("s1"),
        )],
        &[GitRename {
            old_locator: "python:function:auth.login".to_owned(),
            new_locator: "python:function:authn.login".to_owned(),
        }],
    );
    let minted = r2["python:function:authn.login"].clone();
    assert_ne!(
        minted, original,
        "ambiguous match must NOT carry (fail closed)"
    );
    // Old SEI is orphaned (never silently re-pointed).
    match resolve_sei(&conn, &original).unwrap() {
        SeiLookupResult::NotAlive { lineage } => {
            assert!(lineage.iter().any(|e| e.event == "orphaned"));
        }
        SeiLookupResult::Alive(_) => panic!("the old SEI must be orphaned"),
    }
    // The new SEI is alive at the new locator.
    assert_eq!(
        resolve_locator(&conn, "python:function:authn.login")
            .unwrap()
            .unwrap()
            .sei,
        minted
    );
}

// ── §8.5 — delete → orphaned, resolve_sei alive:false + lineage ──────────────
#[test]
fn oracle_delete_orphans_and_reports_not_alive() {
    let conn = fresh_db();
    let r1 = apply_run(
        &conn,
        "run-1",
        &[
            entity("keep.f", Some("h1"), Some("s1")),
            entity("gone.g", Some("h2"), Some("s2")),
        ],
        &[],
    );
    let gone_sei = r1["gone.g"].clone();

    // Run 2: gone.g removed (not in the current set), keep.f remains.
    apply_run(
        &conn,
        "run-2",
        &[entity("keep.f", Some("h1"), Some("s1"))],
        &[],
    );

    assert!(resolve_locator(&conn, "gone.g").unwrap().is_none());
    match resolve_sei(&conn, &gone_sei).unwrap() {
        SeiLookupResult::NotAlive { lineage } => {
            assert!(
                lineage.iter().any(|e| e.event == "orphaned"),
                "delete must record an orphaned lineage event"
            );
        }
        SeiLookupResult::Alive(_) => panic!("a deleted entity's SEI must be orphaned"),
    }
    // keep.f is untouched.
    assert!(resolve_locator(&conn, "keep.f").unwrap().is_some());
}

// ── §8.6 — capability-absent → consumer degrades ─────────────────────────────
#[test]
fn oracle_capability_absent_degrades_gracefully() {
    // A fresh DB before any SEI run models a pre-SEI / capability-absent Clarion:
    // the migration exists, but no bindings have been minted. A consumer MUST be
    // able to detect this and degrade — never crash.
    let conn = fresh_db();
    assert!(
        !has_any_alive_binding(&conn).unwrap(),
        "no bindings ⇒ capability effectively absent for this index"
    );
    // resolve over an empty store returns a clean negative, not an error.
    assert!(
        resolve_locator(&conn, "python:function:m.f")
            .unwrap()
            .is_none()
    );
    match resolve_sei(&conn, "clarion:eid:deadbeefdeadbeefdeadbeefdeadbeef").unwrap() {
        SeiLookupResult::NotAlive { lineage } => assert!(lineage.is_empty()),
        SeiLookupResult::Alive(_) => panic!("unknown SEI must resolve not-alive, not alive"),
    }

    // After a run, the capability is populated — the consumer's degrade check flips.
    apply_run(
        &conn,
        "run-1",
        &[entity("python:function:m.f", Some("h1"), Some("s1"))],
        &[],
    );
    assert!(has_any_alive_binding(&conn).unwrap());
}
