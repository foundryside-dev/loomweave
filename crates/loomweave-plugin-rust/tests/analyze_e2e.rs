//! Task 1 (Phase 1b) — front-loaded writer-proving end-to-end test.
//!
//! Converts the Phase-1a `contains`/`parent_id` fix from *in-memory-proven*
//! (`host_integration.rs` never constructs a `Writer`) to **writer-proven**:
//! it drives the real `loomweave analyze` CLI over a vendored fixture crate and
//! reads the stored entity-id set back out of the index DB.
//!
//! The fixture deliberately carries out-of-`src/` files (`tests/it.rs`,
//! `build.rs`) that `analyze_one_file` attributes to the crate with no scope
//! guard. Each mints a bare `rust:module:e2e_crate`, and `tests/it.rs` /
//! `build.rs` additionally leak `rust:function:e2e_crate.test_only_helper` /
//! `rust:function:e2e_crate.main`.
//!
//! EMPIRICAL FAILURE MODE (diverges from the Phase-1b plan's prediction): the
//! collision is NOT a silent `ON CONFLICT(id) DO UPDATE` merge. The plugin
//! emits the same `rust:module:e2e_crate` id once per file (lib.rs, it.rs,
//! build.rs) with a DIFFERENT `parent_id` each time (`core:file:src/lib.rs`,
//! `core:file:tests/it.rs`, `core:file:build.rs`). The writer's
//! parent/contains validation (`writer.rs`,
//! `LMWV-INFRA-PARENT-CONTAINS-MISMATCH`) catches the duplicate module whose
//! `parent_id` has no matching `contains` edge and HARD-FailsRun the batch:
//! `runs.status = 'failed'` and ZERO rust entities are committed.
//!
//! This test therefore asserts BOTH (a) run status `completed` and (b) the
//! exact stored rust id SET == the 6 in-`src/` entities. Both are red today.
//! The later "Shared crate-scope helper" task removes out-of-`src/`
//! attribution, after which the run completes and the set narrows to exactly
//! the 6. Keeping the SET assertion also guards a wrong fix path: were the
//! collision instead "fixed" by relaxing the writer to merge silently, the run
//! would complete but `test_only_helper` / `main` would leak — and the SET
//! assertion would still catch it.
//!
//! Written first; it goes **RED**. A later task makes it GREEN.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::{env, fs};

use rusqlite::Connection;
use tempfile::TempDir;

/// Locate the off-glob `loomweave-rust-plugin` binary (this crate's bin).
///
/// Cargo sets `CARGO_BIN_EXE_loomweave-rust-plugin` for integration tests of
/// this crate; fall back to a `target/{debug,release}/` search, mirroring
/// `wp2_e2e.rs::fixture_binary_path`.
fn rust_plugin_binary_path() -> PathBuf {
    if let Ok(path) = env::var("CARGO_BIN_EXE_loomweave-rust-plugin") {
        return PathBuf::from(path);
    }
    let target_dir = target_dir();
    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join("loomweave-rust-plugin");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!(
        "loomweave-rust-plugin binary not found. \
         Run `cargo build --workspace` before running this test. \
         Searched: {}",
        target_dir.display()
    );
}

/// Locate the `loomweave` driver binary. It lives in a DIFFERENT crate
/// (`loomweave-cli`), so cargo does NOT set a `CARGO_BIN_EXE_*` for it here;
/// search the workspace target dir directly (built by `cargo build
/// --workspace`).
fn loomweave_driver_path() -> PathBuf {
    let target_dir = target_dir();
    for profile in &["debug", "release"] {
        let candidate = target_dir.join(profile).join("loomweave");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!(
        "loomweave driver binary not found. \
         Run `cargo build --workspace` before running this test. \
         Searched: {}",
        target_dir.display()
    );
}

/// Resolve the workspace target dir from `CARGO_MANIFEST_DIR`
/// (`crates/loomweave-plugin-rust` -> `crates` -> workspace root).
fn target_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must exist");
    env::var("CARGO_TARGET_DIR").map_or_else(|_| workspace_root.join("target"), PathBuf::from)
}

/// Stage the off-glob rust plugin binary under its discovery name
/// (`loomweave-plugin-rust`) plus the neighbour `plugin.toml`, returning the
/// staging `TempDir` (keep it alive for the test). Mirrors
/// `wp2_e2e.rs::setup_plugin_dir`.
fn setup_plugin_dir() -> TempDir {
    let plugin_dir = TempDir::new().expect("create plugin tempdir");

    let dest = plugin_dir.path().join("loomweave-plugin-rust");
    std::os::unix::fs::symlink(rust_plugin_binary_path(), &dest)
        .expect("symlink loomweave-plugin-rust");

    // Neighbour-discovery convention: the manifest sits beside the binary. The
    // shipped manifest is at CARGO_MANIFEST_DIR/plugin.toml.
    let toml_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugin.toml");
    fs::copy(&toml_src, plugin_dir.path().join("plugin.toml")).expect("copy plugin.toml");

    plugin_dir
}

/// Recursively copy a directory tree.
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file_type").is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// Copy the vendored fixture crate into a fresh project dir (isolated so
/// `analyze` writes `.weft/` here, not into the source tree).
fn staged_fixture_project() -> TempDir {
    let project = TempDir::new().expect("create project tempdir");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("e2e_crate");
    copy_tree(&fixture, project.path());
    project
}

#[test]
fn analyze_e2e_stored_rust_entity_set_excludes_out_of_src_files() {
    let plugin_dir = setup_plugin_dir();
    let driver = loomweave_driver_path();
    let project = staged_fixture_project();
    let project_path = project.path();

    // `loomweave install` initialises `.weft/loomweave/loomweave.db`.
    let install = std::process::Command::new(&driver)
        .args(["install", "--path"])
        .arg(project_path)
        .status()
        .expect("spawn loomweave install");
    assert!(install.success(), "loomweave install must succeed");

    // Synthetic PATH containing ONLY the staging dir — do NOT inherit the
    // runner PATH (world-writable dirs trip discovery refusal; see wp2_e2e.rs).
    let synthetic_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");

    let analyze = std::process::Command::new(&driver)
        .args(["analyze"])
        .arg(project_path)
        .env("PATH", &synthetic_path)
        .status()
        .expect("spawn loomweave analyze");
    // analyze's exit status is not itself the assertion — we read the run row
    // and the stored id SET from the DB below.
    let _ = analyze;

    let db_path = project_path.join(".weft/loomweave/loomweave.db");
    let conn = Connection::open(&db_path).expect("open index db");

    // Target (green-state) run status. Red today: the out-of-`src/` duplicate
    // `rust:module:e2e_crate` trips LMWV-INFRA-PARENT-CONTAINS-MISMATCH and the
    // writer FailsRun, so this is currently 'failed'. The scope-guard task
    // makes it 'completed'.
    let run_status: String = conn
        .query_row("SELECT COALESCE(MAX(status), '') FROM runs", [], |row| {
            row.get(0)
        })
        .expect("query runs status");
    assert_eq!(
        run_status, "completed",
        "run must complete once out-of-src/ attribution is removed; \
         red today because the duplicate module trips \
         LMWV-INFRA-PARENT-CONTAINS-MISMATCH -> FailRun; got {run_status:?}"
    );

    // The stored rust-plugin entity-id set. Filter to plugin_id='rust' to
    // exclude the core's `core:file:*` identity rows.
    let mut stmt = conn
        .prepare("SELECT id FROM entities WHERE plugin_id = 'rust' ORDER BY id")
        .expect("prepare entities query");
    let mut got: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query entity ids")
        .map(|r| r.expect("row id"))
        .collect();
    got.sort();

    // Expected (`want`) set — only the in-`src/` entities. The `impl Widget`
    // block is its own `impl` entity (Task 5), and `bump` re-parents onto it.
    // The method locator carries NO source-order ordinal (`impl#<>`, ADR-049
    // amend, Option b).
    let mut want: Vec<String> = vec![
        "rust:module:e2e_crate".to_owned(),
        "rust:module:e2e_crate.sub".to_owned(),
        "rust:struct:e2e_crate.Widget".to_owned(),
        "rust:function:e2e_crate.make".to_owned(),
        "rust:function:e2e_crate.sub.helper".to_owned(),
        "rust:impl:e2e_crate.Widget.impl#<>".to_owned(),
        "rust:function:e2e_crate.Widget.impl#<>.bump".to_owned(),
    ];
    want.sort();

    assert_eq!(
        got, want,
        "stored rust entity-id set must contain only in-src/ entities.\n\
         The out-of-src/ collision leaks tests/it.rs + build.rs items.\n\
           got:  {got:#?}\n\
           want: {want:#?}"
    );
}
