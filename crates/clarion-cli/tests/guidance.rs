//! `clarion guidance` authoring CLI integration tests (WS6 / REQ-GUIDANCE-03).
//!
//! Drives the real binary end-to-end against a seeded `.clarion/clarion.db`:
//! create (via `--content`), show, list (incl. `--for-entity`), edit (via a
//! fake `$EDITOR`), and delete. Verifies the written `properties` JSON matches
//! the shape the MCP read path consumes.

use assert_cmd::Command;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

/// Seed a real `.clarion/clarion.db` with the schema and one code entity (so
/// `--for-entity` has a target to match).
fn seed_db(root: &std::path::Path) {
    let clarion_dir = root.join(".clarion");
    std::fs::create_dir_all(&clarion_dir).expect("mkdir .clarion");
    let db_path = clarion_dir.join("clarion.db");
    let mut conn = Connection::open(&db_path).expect("open db");
    clarion_storage::pragma::apply_write_pragmas(&conn).expect("write pragmas");
    clarion_storage::schema::apply_migrations(&mut conn).expect("migrate");
    // A function entity under src/auth/ so path + kind rules can match it.
    // `analyze` stores `source_file_path` as a *canonicalized* absolute path
    // (clarion_storage::query::normalize_source_path canonicalizes both root and
    // file), and `serve` / the CLI canonicalize project_root the same way. The
    // file must exist on disk for canonicalize to resolve symlinks (e.g. macOS
    // /tmp → /private/tmp), so create it before seeding — this makes the seeded
    // path identical to what the real write path produces, so path match-rules
    // are genuinely exercised through symlinked tempdirs.
    let src_dir = root.join("src").join("auth");
    std::fs::create_dir_all(&src_dir).expect("mkdir src/auth");
    let src = src_dir.join("tokens.py");
    std::fs::write(&src, "def refresh(): ...\n").expect("write source file");
    let canonical_src = src.canonicalize().expect("canonicalize source file");
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         source_file_path, created_at, updated_at) VALUES \
         (?1, 'python', 'function', 'auth.tokens.refresh', 'refresh', '{}', ?2, \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![
            "python:function:auth.tokens.refresh",
            canonical_src.to_str().unwrap()
        ],
    )
    .expect("seed entity");
}

fn properties(root: &std::path::Path, id: &str) -> Value {
    let db_path = root.join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).expect("reopen db");
    let raw: String = conn
        .query_row(
            "SELECT properties FROM entities WHERE id = ?1 AND kind = 'guidance'",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .expect("sheet row present");
    serde_json::from_str(&raw).expect("properties parse")
}

/// Insert a guidance sheet directly with a fully-controlled `properties` object
/// (so `expires` / `authored_at` / `reviewed_at` can be pinned to fixed instants
/// for the `--expired` / `--stale` filter tests). Bypasses the CLI `create` path
/// deliberately — these tests exercise `list`, not authoring.
fn seed_sheet(root: &std::path::Path, slug: &str, properties: &Value) {
    let db_path = root.join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).expect("open db for seed_sheet");
    let id = format!("core:guidance:{slug}");
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) VALUES \
         (?1, 'core', 'guidance', ?2, ?2, ?3, \
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![id, slug, serde_json::to_string(&properties).unwrap()],
    )
    .expect("seed guidance sheet");
}

/// Run `guidance list` with the given extra args and return stdout.
fn list_stdout(root: &std::path::Path, extra: &[&str]) -> String {
    let assert = clarion_bin()
        .args(["guidance", "list"])
        .args(["--path"])
        .arg(root)
        .args(extra)
        .assert()
        .success();
    String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
}

#[test]
fn list_expired_shows_only_past_expiry_sheets() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    // Past expiry → expired; future expiry → not; no expiry → not.
    seed_sheet(
        dir.path(),
        "past",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
            "expires": "2026-01-02T00:00:00.000Z",
        }),
    );
    seed_sheet(
        dir.path(),
        "future",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
            "expires": "2999-01-01T00:00:00.000Z",
        }),
    );
    seed_sheet(
        dir.path(),
        "noexpiry",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );

    let out = list_stdout(dir.path(), &["--expired"]);
    assert!(
        out.contains("core:guidance:past"),
        "expired list missing past-expiry sheet: {out}"
    );
    assert!(
        !out.contains("core:guidance:future"),
        "expired list should not include future-expiry sheet: {out}"
    );
    assert!(
        !out.contains("core:guidance:noexpiry"),
        "expired list should not include no-expiry sheet: {out}"
    );
}

#[test]
fn list_stale_shows_only_sheets_untouched_within_window() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    // Authored long ago → stale at 90 days.
    seed_sheet(
        dir.path(),
        "old",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2025-01-01T00:00:00.000Z",
        }),
    );
    // Old authored_at but recently reviewed → max(reviewed,authored) is fresh →
    // NOT stale. The reviewed_at-wins TDD target, exercised through the binary.
    seed_sheet(
        dir.path(),
        "reviewed",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2025-01-01T00:00:00.000Z",
            "reviewed_at": "2999-01-01T00:00:00.000Z",
        }),
    );

    let out = list_stdout(dir.path(), &["--stale", "--days", "90"]);
    assert!(
        out.contains("core:guidance:old"),
        "stale list missing old sheet: {out}"
    );
    assert!(
        !out.contains("core:guidance:reviewed"),
        "stale list should exclude recently-reviewed sheet: {out}"
    );
}

#[test]
fn list_expired_and_stale_intersect() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    // Expired AND stale (old authored_at, past expiry) → shown.
    seed_sheet(
        dir.path(),
        "both",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2025-01-01T00:00:00.000Z",
            "expires": "2025-06-01T00:00:00.000Z",
        }),
    );
    // Expired but fresh (recent authored_at) → excluded by --stale.
    seed_sheet(
        dir.path(),
        "expired-fresh",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2999-01-01T00:00:00.000Z",
            "expires": "2025-06-01T00:00:00.000Z",
        }),
    );
    // Stale but not expired (future expiry) → excluded by --expired.
    seed_sheet(
        dir.path(),
        "stale-unexpired",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2025-01-01T00:00:00.000Z",
            "expires": "2999-01-01T00:00:00.000Z",
        }),
    );

    let out = list_stdout(dir.path(), &["--expired", "--stale", "--days", "90"]);
    assert!(
        out.contains("core:guidance:both"),
        "intersection list missing expired+stale sheet: {out}"
    );
    assert!(
        !out.contains("core:guidance:expired-fresh"),
        "intersection should exclude fresh sheet: {out}"
    );
    assert!(
        !out.contains("core:guidance:stale-unexpired"),
        "intersection should exclude unexpired sheet: {out}"
    );
}

#[test]
fn create_show_list_delete_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    // create with explicit content + two match rules.
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "auth-tokens"])
        .args(["--match", "path:src/auth/**"])
        .args(["--match", "kind:function"])
        .args(["--content", "Refresh tokens carefully."])
        .assert()
        .success();

    let id = "core:guidance:auth-tokens";
    let props = properties(dir.path(), id);
    assert_eq!(props["content"], "Refresh tokens carefully.");
    assert_eq!(props["scope_level"], "module");
    assert_eq!(props["provenance"], "manual");
    assert_eq!(props["pinned"], false);
    assert!(props["authored_at"].is_string());
    // Match-rules in the read-path-consumed `{"type":…}` shape.
    let rules = props["match_rules"].as_array().unwrap();
    assert_eq!(
        rules[0],
        serde_json::json!({"type":"path","pattern":"src/auth/**"})
    );
    assert_eq!(
        rules[1],
        serde_json::json!({"type":"kind","value":"function"})
    );

    // show prints the id + content.
    let show = clarion_bin()
        .args(["guidance", "show", id])
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .success();
    let show_out = String::from_utf8_lossy(&show.get_output().stdout).into_owned();
    assert!(show_out.contains(id), "show missing id: {show_out}");
    assert!(
        show_out.contains("Refresh tokens carefully."),
        "show missing content: {show_out}"
    );

    // list (no filter) shows the sheet.
    let list = clarion_bin()
        .args(["guidance", "list"])
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&list.get_output().stdout).contains(id),
        "list missing sheet"
    );

    // list --for-entity matches via path/kind rule.
    let filtered = clarion_bin()
        .args(["guidance", "list"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--for-entity", "python:function:auth.tokens.refresh"])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&filtered.get_output().stdout).contains(id),
        "for-entity list should match via path/kind rule"
    );

    // delete removes it.
    clarion_bin()
        .args(["guidance", "delete", id])
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .success();

    // show now fails (not found).
    clarion_bin()
        .args(["guidance", "show", id])
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .failure();
}

#[test]
fn list_for_entity_matches_via_path_rule_only() {
    // A path-only sheet (no kind rule to mask it) must match the seeded entity
    // through the canonicalized project_root / source_file_path symmetry — this
    // is the case that silently degrades if the CLI's path treatment diverges
    // from what `analyze` writes and `serve` reads.
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "path-only"])
        .args(["--match", "path:src/auth/**"])
        .args(["--content", "auth guidance"])
        .assert()
        .success();

    let matched = clarion_bin()
        .args(["guidance", "list"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--for-entity", "python:function:auth.tokens.refresh"])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&matched.get_output().stdout).contains("core:guidance:path-only"),
        "path-only sheet should match via path rule (root/source canonicalization symmetry)"
    );

    // A non-matching path must NOT list for this entity.
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "other-path"])
        .args(["--match", "path:src/billing/**"])
        .args(["--content", "billing guidance"])
        .assert()
        .success();
    let filtered = clarion_bin()
        .args(["guidance", "list"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--for-entity", "python:function:auth.tokens.refresh"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&filtered.get_output().stdout);
    assert!(
        out.contains("core:guidance:path-only"),
        "auth path still matches"
    );
    assert!(
        !out.contains("core:guidance:other-path"),
        "billing path must not match the auth entity"
    );
}

#[test]
fn create_rejects_duplicate_id() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    let make = || {
        clarion_bin()
            .args(["guidance", "create"])
            .args(["--path"])
            .arg(dir.path())
            .args(["--scope-level", "project"])
            .args(["--name", "dup"])
            .args(["--match", "kind:function"])
            .args(["--content", "x"])
            .assert()
    };
    make().success();
    make().failure(); // second create on same id errors, not silent overwrite.
}

#[test]
fn create_rejects_bad_scope_level_and_bad_match() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "galaxy"])
        .args(["--match", "kind:function"])
        .args(["--content", "x"])
        .assert()
        .failure();

    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--match", "bogus-no-colon"])
        .args(["--content", "x"])
        .assert()
        .failure();
}

#[test]
fn create_normalizes_and_validates_expires() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    // A bare date is accepted and normalized to start-of-day UTC in the same
    // 24-char `…Z` shape the read path's lexical expiry compare expects.
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "expiring"])
        .args(["--match", "kind:function"])
        .args(["--content", "x"])
        .args(["--expires", "2999-12-31"])
        .assert()
        .success();

    let props = properties(dir.path(), "core:guidance:expiring");
    let stored = props["expires"].as_str().expect("expires stored");
    assert_eq!(
        stored, "2999-12-31T00:00:00.000Z",
        "bare date normalized to start-of-day UTC"
    );

    // Proxy the read path: a future expiry must NOT be lexically < now, i.e. the
    // sheet is not treated as already expired.
    let db_path = dir.path().join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).unwrap();
    let now: String = conn
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(stored > now.as_str(), "future expiry must sort after now");

    // Garbage `--expires` is rejected up front (no sheet written).
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "bad-expiry"])
        .args(["--match", "kind:function"])
        .args(["--content", "x"])
        .args(["--expires", "tomorrow"])
        .assert()
        .failure();
}

#[test]
fn edit_preserves_authored_at_and_provenance_changes_only_content() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    let id = "core:guidance:edit-me";

    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "edit-me"])
        .args(["--match", "kind:function"])
        .args(["--pinned"])
        .args(["--content", "original"])
        .assert()
        .success();

    let before = properties(dir.path(), id);
    let authored_at = before["authored_at"].as_str().unwrap().to_owned();

    // A fake editor: a shell script that overwrites the file with new content.
    let editor = dir.path().join("fake-editor.sh");
    std::fs::write(&editor, "#!/bin/sh\nprintf 'rewritten content' > \"$1\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&editor).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&editor, perms).unwrap();
    }

    clarion_bin()
        .args(["guidance", "edit", id])
        .args(["--path"])
        .arg(dir.path())
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .assert()
        .success();

    let after = properties(dir.path(), id);
    assert_eq!(after["content"], "rewritten content", "content updated");
    assert_eq!(
        after["authored_at"].as_str().unwrap(),
        authored_at,
        "authored_at preserved across edit"
    );
    assert_eq!(after["provenance"], "manual", "provenance unchanged");
    assert_eq!(after["pinned"], true, "pinned preserved");
    assert_eq!(after["scope_level"], "module", "scope_level preserved");
}

#[test]
fn edit_without_editor_set_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "noeditor"])
        .args(["--match", "kind:function"])
        .args(["--content", "x"])
        .assert()
        .success();

    clarion_bin()
        .args(["guidance", "edit", "core:guidance:noeditor"])
        .args(["--path"])
        .arg(dir.path())
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .assert()
        .failure();
}

/// Seed one `summary_cache` row for the given entity (the column shape
/// `analyze` and the cache writer use).
fn seed_summary_cache(root: &std::path::Path, entity_id: &str) {
    let db_path = root.join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).expect("open db");
    conn.execute(
        "INSERT INTO summary_cache \
         (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint, \
          summary_json, cost_usd, tokens_input, tokens_output, created_at, last_accessed_at, \
          caller_count, fan_out) \
         VALUES (?1, 'h', 'tmpl', 'tier', 'fp', '{}', 0.0, 0, 0, \
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 0, 0)",
        rusqlite::params![entity_id],
    )
    .expect("seed summary_cache row");
}

fn summary_cache_count(root: &std::path::Path, entity_id: &str) -> i64 {
    let db_path = root.join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).expect("open db");
    conn.query_row(
        "SELECT COUNT(*) FROM summary_cache WHERE entity_id = ?1",
        rusqlite::params![entity_id],
        |row| row.get(0),
    )
    .expect("count cache rows")
}

#[test]
fn create_invalidates_cached_summary_for_matched_entity() {
    // ADR-007 / T-cache: authoring a sheet that matches a seeded entity drops
    // that entity's cached summary, so the new guidance can reach future prompts.
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    seed_summary_cache(dir.path(), "python:function:auth.tokens.refresh");
    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        1
    );

    let assert = clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "auth-sheet"])
        .args(["--match", "path:src/auth/**"])
        .args(["--content", "auth guidance"])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&assert.get_output().stdout).contains("Invalidated 1 cached"),
        "create should report the invalidation"
    );

    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        0,
        "matched entity's cached summary must be invalidated on authoring"
    );
}

#[test]
fn create_non_matching_sheet_leaves_cache_intact() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    seed_summary_cache(dir.path(), "python:function:auth.tokens.refresh");

    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "billing-sheet"])
        .args(["--match", "path:src/billing/**"])
        .args(["--content", "billing guidance"])
        .assert()
        .success();

    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        1,
        "a sheet that matches nothing must not touch any cache row"
    );
}

#[test]
fn delete_invalidates_cached_summary_for_matched_entity() {
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());

    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "auth-sheet"])
        .args(["--match", "kind:function"])
        .args(["--content", "auth guidance"])
        .assert()
        .success();

    // Simulate a summary cached *after* the sheet was created (e.g. the next
    // briefing query); deleting the sheet must drop it so the now-removed
    // guidance can't linger in a cached summary.
    seed_summary_cache(dir.path(), "python:function:auth.tokens.refresh");
    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        1
    );

    let assert = clarion_bin()
        .args(["guidance", "delete", "core:guidance:auth-sheet"])
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&assert.get_output().stdout).contains("Invalidated 1 cached"),
        "delete should report the invalidation"
    );

    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        0,
        "deleting a matching sheet must invalidate the matched entity's cache"
    );
}

#[test]
fn edit_invalidates_cached_summary_for_matched_entity() {
    // An edit changes `content`, so the composed guidance for every matched
    // entity changed; the cached summary must be dropped.
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path());
    let id = "core:guidance:edit-cache";

    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "edit-cache"])
        .args(["--match", "kind:function"])
        .args(["--content", "original"])
        .assert()
        .success();

    // Cache a summary *after* creation; the edit below must invalidate it.
    seed_summary_cache(dir.path(), "python:function:auth.tokens.refresh");

    let editor = dir.path().join("fake-editor.sh");
    std::fs::write(&editor, "#!/bin/sh\nprintf 'revised guidance' > \"$1\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&editor).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&editor, perms).unwrap();
    }

    let assert = clarion_bin()
        .args(["guidance", "edit", id])
        .args(["--path"])
        .arg(dir.path())
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&assert.get_output().stdout).contains("Invalidated 1 cached"),
        "edit should report the invalidation"
    );

    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        0,
        "editing a matching sheet must invalidate the matched entity's cache"
    );
}

// ── Export / import (WS6 / T5, REQ-GUIDANCE-06) ───────────────────────────────

/// Run `guidance export --to <to_dir>`.
fn export_to(root: &std::path::Path, to_dir: &std::path::Path) {
    clarion_bin()
        .args(["guidance", "export"])
        .args(["--path"])
        .arg(root)
        .args(["--to"])
        .arg(to_dir)
        .assert()
        .success();
}

/// Run `guidance import <from_dir>`.
fn import_from(root: &std::path::Path, from_dir: &std::path::Path) {
    clarion_bin()
        .args(["guidance", "import"])
        .args(["--path"])
        .arg(root)
        .arg(from_dir)
        .assert()
        .success();
}

/// Fetch a guidance sheet's (name, properties) tuple, or None if absent.
fn sheet_fields(root: &std::path::Path, id: &str) -> Option<(String, Value)> {
    let db_path = root.join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).expect("reopen db");
    conn.query_row(
        "SELECT name, properties FROM entities WHERE id = ?1 AND kind = 'guidance'",
        rusqlite::params![id],
        |row| {
            let name: String = row.get(0)?;
            let raw: String = row.get(1)?;
            Ok((name, raw))
        },
    )
    .optional()
    .expect("query sheet")
    .map(|(name, raw)| (name, serde_json::from_str(&raw).expect("props parse")))
}

#[test]
fn export_import_round_trips_all_fields() {
    // Headline test: seed varied sheets → export → import into a FRESH empty DB →
    // every sheet equals the original field-for-field (id, name, every property
    // incl. match_rules / pinned / expires / authored_at / content).
    let src = tempfile::tempdir().unwrap();
    seed_db(src.path());

    let sheets = [
        (
            "alpha",
            serde_json::json!({
                "content": "Refresh tokens carefully.",
                "scope_level": "module",
                "match_rules": [
                    { "type": "path", "pattern": "src/auth/**" },
                    { "type": "kind", "value": "function" },
                ],
                "pinned": true,
                "provenance": "manual",
                "authored_at": "2026-01-01T00:00:00.000Z",
                "expires": "2027-12-31T00:00:00.000Z",
            }),
        ),
        (
            "beta.nested.name",
            serde_json::json!({
                "content": "Project-wide invariant.",
                "scope_level": "project",
                "match_rules": [],
                "pinned": false,
                "provenance": "manual",
                "authored_at": "2025-06-01T12:34:56.789Z",
            }),
        ),
        (
            "gamma",
            serde_json::json!({
                "content": "multi\nline\ncontent with \"quotes\" and, commas",
                "scope_level": "subsystem",
                "match_rules": [
                    { "type": "subsystem", "id": "core:subsystem:abcd" },
                ],
                "pinned": false,
                "provenance": "manual",
                "authored_at": "2026-03-15T08:00:00.000Z",
                "reviewed_at": "2026-04-01T09:00:00.000Z",
            }),
        ),
    ];
    for (slug, props) in &sheets {
        seed_sheet(src.path(), slug, props);
    }

    let export_dir = tempfile::tempdir().unwrap();
    export_to(src.path(), export_dir.path());

    // One file per sheet, colons sanitized.
    assert!(
        export_dir
            .path()
            .join("core__guidance__alpha.json")
            .exists(),
        "expected per-sheet file with sanitized name"
    );

    // Import into a fresh, empty DB.
    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path()); // schema only; no guidance sheets yet.
    import_from(dst.path(), export_dir.path());

    for (slug, props) in &sheets {
        let id = format!("core:guidance:{slug}");
        let (orig_name, orig_props) =
            sheet_fields(src.path(), &id).expect("original sheet present");
        let (imp_name, imp_props) = sheet_fields(dst.path(), &id).expect("imported sheet present");
        assert_eq!(imp_name, orig_name, "name round-trips for {id}");
        // Field-for-field: properties equal the original (excludes created_at /
        // updated_at, which are NOT stored in properties).
        assert_eq!(imp_props, *props, "properties round-trip for {id}");
        assert_eq!(imp_props, orig_props, "imported == original for {id}");
    }
}

#[test]
fn export_is_byte_deterministic() {
    // Export the same DB to two dirs → byte-identical files.
    let src = tempfile::tempdir().unwrap();
    seed_db(src.path());
    seed_sheet(
        src.path(),
        "det",
        // Properties authored with keys in non-sorted order on purpose.
        &serde_json::json!({
            "zeta": "z", "content": "x", "alpha": "a",
            "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );

    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    export_to(src.path(), a.path());
    export_to(src.path(), b.path());

    let fname = "core__guidance__det.json";
    let bytes_a = std::fs::read(a.path().join(fname)).unwrap();
    let bytes_b = std::fs::read(b.path().join(fname)).unwrap();
    assert_eq!(bytes_a, bytes_b, "two exports must be byte-identical");

    // Sanity: keys are actually sorted (alpha before zeta) and there is a
    // trailing newline.
    let text = String::from_utf8(bytes_a).unwrap();
    assert!(text.ends_with('\n'), "trailing newline: {text:?}");
    assert!(
        text.find("alpha").unwrap() < text.find("zeta").unwrap(),
        "keys sorted for diff-friendliness: {text}"
    );
}

#[test]
fn import_is_idempotent() {
    let src = tempfile::tempdir().unwrap();
    seed_db(src.path());
    seed_sheet(
        src.path(),
        "idem",
        &serde_json::json!({
            "content": "stable", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );
    let export_dir = tempfile::tempdir().unwrap();
    export_to(src.path(), export_dir.path());

    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path());
    import_from(dst.path(), export_dir.path());
    let first = sheet_fields(dst.path(), "core:guidance:idem").expect("present after import 1");

    // Second import of the same dir changes nothing in content.
    import_from(dst.path(), export_dir.path());
    let second = sheet_fields(dst.path(), "core:guidance:idem").expect("present after import 2");

    assert_eq!(first, second, "re-import is a content no-op");

    // Exactly one sheet, not duplicated.
    let db_path = dst.path().join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE kind = 'guidance'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "re-import must not duplicate sheets");
}

#[test]
fn import_is_additive_not_a_mirror() {
    // A local sheet absent from the import dir must survive the import.
    let src = tempfile::tempdir().unwrap();
    seed_db(src.path());
    seed_sheet(
        src.path(),
        "incoming",
        &serde_json::json!({
            "content": "from-team", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );
    let export_dir = tempfile::tempdir().unwrap();
    export_to(src.path(), export_dir.path());

    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path());
    seed_sheet(
        dst.path(),
        "local-only",
        &serde_json::json!({
            "content": "mine", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );
    import_from(dst.path(), export_dir.path());

    assert!(
        sheet_fields(dst.path(), "core:guidance:incoming").is_some(),
        "imported sheet present"
    );
    assert!(
        sheet_fields(dst.path(), "core:guidance:local-only").is_some(),
        "local-only sheet must NOT be deleted by an additive import"
    );
}

#[test]
fn import_fails_loudly_on_malformed_file() {
    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path());
    let import_dir = tempfile::tempdir().unwrap();
    // A junk .json file in the import set.
    std::fs::write(import_dir.path().join("broken.json"), "{ not valid json").unwrap();

    let assert = clarion_bin()
        .args(["guidance", "import"])
        .args(["--path"])
        .arg(dst.path())
        .arg(import_dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("broken.json"),
        "import error must name the offending file: {stderr}"
    );
}

#[test]
fn import_rejects_code_entity_id_and_leaves_entity_intact() {
    // FINDING 1(c): an import file whose JSON `id` is a CODE entity id must fail
    // loudly (naming the file) and must NOT mutate the existing code entity.
    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path()); // seeds python:function:auth.tokens.refresh

    let target = "python:function:auth.tokens.refresh";
    let before = sheet_props_raw(dst.path(), target).expect("code entity present");

    let import_dir = tempfile::tempdir().unwrap();
    let evil = serde_json::json!({
        "id": target,
        "name": "pwned",
        "properties": { "content": "overwrite", "scope_level": "module", "match_rules": [] },
    });
    std::fs::write(
        import_dir.path().join("evil.json"),
        serde_json::to_string_pretty(&evil).unwrap(),
    )
    .unwrap();

    let assert = clarion_bin()
        .args(["guidance", "import"])
        .args(["--path"])
        .arg(dst.path())
        .arg(import_dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("evil.json"),
        "import error must name the offending file: {stderr}"
    );

    let after = sheet_props_raw(dst.path(), target).expect("code entity still present");
    assert_eq!(
        after, before,
        "the code entity must be byte-identical after a rejected import"
    );
}

/// Fetch the raw (name, kind, `plugin_id`, properties) tuple for ANY entity (not
/// just guidance), or None.
fn sheet_props_raw(root: &std::path::Path, id: &str) -> Option<(String, String, String, String)> {
    let db_path = root.join(".clarion").join("clarion.db");
    let conn = Connection::open(&db_path).expect("reopen db");
    conn.query_row(
        "SELECT name, kind, plugin_id, properties FROM entities WHERE id = ?1",
        rusqlite::params![id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .optional()
    .expect("query entity")
}

#[test]
fn import_invalidates_union_of_old_and_new_matches() {
    // FINDING 2: when import UPDATES an existing sheet whose match_rules changed,
    // the OLD-matched entities' cached summaries must also be invalidated (not
    // just the NEW-matched ones). kind:class → kind:function is the reliable
    // discriminator (no on-disk file needed for kind rules).
    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path()); // seeds a `function` entity: auth.tokens.refresh

    // Seed a `class` entity too, so an OLD `kind:class` rule has a target.
    {
        let db_path = dst.path().join(".clarion").join("clarion.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
             created_at, updated_at) VALUES \
             (?1, 'python', 'class', 'pkg.mod.C', 'C', '{}', \
              strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            rusqlite::params!["python:class:pkg.mod.C"],
        )
        .unwrap();
    }

    // Pre-existing sheet matching the CLASS entity (OLD rule: kind:class).
    seed_sheet(
        dst.path(),
        "shifting",
        &serde_json::json!({
            "content": "old", "scope_level": "module",
            "match_rules": [{ "type": "kind", "value": "class" }],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );

    // Cache rows for BOTH the old-matched (class) and new-matched (function).
    seed_summary_cache(dst.path(), "python:class:pkg.mod.C");
    seed_summary_cache(dst.path(), "python:function:auth.tokens.refresh");

    // Import a NEW version of the SAME sheet id, with match_rules flipped to
    // kind:function (so the OLD class match no longer applies).
    let import_dir = tempfile::tempdir().unwrap();
    let updated = serde_json::json!({
        "id": "core:guidance:shifting",
        "name": "shifting",
        "properties": {
            "content": "new", "scope_level": "module",
            "match_rules": [{ "type": "kind", "value": "function" }],
            "authored_at": "2026-01-01T00:00:00.000Z",
        },
    });
    std::fs::write(
        import_dir.path().join("core__guidance__shifting.json"),
        serde_json::to_string_pretty(&updated).unwrap(),
    )
    .unwrap();

    import_from(dst.path(), import_dir.path());

    // BOTH cache rows must be gone: the NEW match (function) AND — the regression
    // this fixes — the OLD match (class) that no longer applies.
    assert_eq!(
        summary_cache_count(dst.path(), "python:function:auth.tokens.refresh"),
        0,
        "new-matched entity invalidated"
    );
    assert_eq!(
        summary_cache_count(dst.path(), "python:class:pkg.mod.C"),
        0,
        "OLD-matched entity must also be invalidated on a match_rules change"
    );
}

#[test]
fn delete_invalidates_guides_edge_target() {
    // FINDING 3 (through the real delete path): a sheet that applies SOLELY via a
    // `guides` edge must invalidate the guided entity's cache on delete. This is
    // the FK-cascade trap: delete must invalidate BEFORE removing the sheet row,
    // or the CASCADE removes the guides edge first and invalidation sees nothing.
    let dir = tempfile::tempdir().unwrap();
    seed_db(dir.path()); // seeds python:function:auth.tokens.refresh

    // Author a sheet with NO match_rules (so only the guides edge can match).
    clarion_bin()
        .args(["guidance", "create"])
        .args(["--path"])
        .arg(dir.path())
        .args(["--scope-level", "module"])
        .args(["--name", "guides-sheet"])
        .args(["--content", "guides-edge guidance"])
        .assert()
        .success();

    // Manually wire a `guides` edge (no authoring path creates one today) and a
    // cache row on the target.
    {
        let db_path = dir.path().join(".clarion").join("clarion.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) VALUES \
             ('guides', ?1, ?2, 'resolved')",
            rusqlite::params![
                "core:guidance:guides-sheet",
                "python:function:auth.tokens.refresh"
            ],
        )
        .unwrap();
    }
    seed_summary_cache(dir.path(), "python:function:auth.tokens.refresh");

    let assert = clarion_bin()
        .args(["guidance", "delete", "core:guidance:guides-sheet"])
        .args(["--path"])
        .arg(dir.path())
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&assert.get_output().stdout).contains("Invalidated 1 cached"),
        "delete should report invalidating the guides-edge target"
    );

    assert_eq!(
        summary_cache_count(dir.path(), "python:function:auth.tokens.refresh"),
        0,
        "guides-edge target's cache must be invalidated on delete (before FK cascade)"
    );
}

#[test]
fn import_ignores_non_json_files() {
    // A README committed alongside the sheets must not crash import.
    let src = tempfile::tempdir().unwrap();
    seed_db(src.path());
    seed_sheet(
        src.path(),
        "ok",
        &serde_json::json!({
            "content": "x", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        }),
    );
    let export_dir = tempfile::tempdir().unwrap();
    export_to(src.path(), export_dir.path());
    std::fs::write(export_dir.path().join("README.md"), "# team guidance\n").unwrap();

    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path());
    import_from(dst.path(), export_dir.path());
    assert!(
        sheet_fields(dst.path(), "core:guidance:ok").is_some(),
        "the json sheet imports despite a non-json sibling"
    );
}

#[test]
fn import_is_partial_but_safe_when_a_later_file_is_malformed() {
    // Import is not atomic across the file set (each upsert is its own txn, files
    // processed in sorted name order). A malformed file aborts loudly, but any
    // sheet already committed before it survives — and re-import is idempotent, so
    // partial progress is safe to retry. This locks that property: a good "aaa"
    // sheet sorts before the bad "zzz" file, so it is committed before the abort.
    let dst = tempfile::tempdir().unwrap();
    seed_db(dst.path());

    let import_dir = tempfile::tempdir().unwrap();
    let good = serde_json::json!({
        "id": "core:guidance:aaa-good",
        "name": "aaa-good",
        "properties": {
            "content": "valid", "scope_level": "module", "match_rules": [],
            "authored_at": "2026-01-01T00:00:00.000Z",
        },
    });
    std::fs::write(
        import_dir.path().join("aaa-good.json"),
        serde_json::to_string_pretty(&good).unwrap(),
    )
    .unwrap();
    std::fs::write(import_dir.path().join("zzz-bad.json"), "{ not valid json").unwrap();

    // The whole import fails loudly naming the bad file...
    let assert = clarion_bin()
        .args(["guidance", "import"])
        .args(["--path"])
        .arg(dst.path())
        .arg(import_dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("zzz-bad.json"),
        "names the bad file: {stderr}"
    );

    // ...but the earlier good sheet was already committed (non-atomic, idempotent
    // on retry).
    assert!(
        sheet_fields(dst.path(), "core:guidance:aaa-good").is_some(),
        "a sheet committed before the malformed file survives the abort"
    );
}
