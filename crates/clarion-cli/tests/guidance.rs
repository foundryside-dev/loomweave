//! `clarion guidance` authoring CLI integration tests (WS6 / REQ-GUIDANCE-03).
//!
//! Drives the real binary end-to-end against a seeded `.clarion/clarion.db`:
//! create (via `--content`), show, list (incl. `--for-entity`), edit (via a
//! fake `$EDITOR`), and delete. Verifies the written `properties` JSON matches
//! the shape the MCP read path consumes.

use assert_cmd::Command;
use rusqlite::Connection;
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
