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
#[allow(clippy::too_many_lines)] // one linear full-ontology snapshot assertion
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
        // Task 8 fixture additions: an in-project trait + its impl on Widget, and
        // an external-trait impl (`impl std::fmt::Display for Widget`). Each impl
        // is its own entity with its method re-parented (Task 5).
        "rust:trait:e2e_crate.Bumpable".to_owned(),
        "rust:impl:e2e_crate.Widget.impl[Bumpable]".to_owned(),
        "rust:function:e2e_crate.Widget.impl[Bumpable].bump_by".to_owned(),
        "rust:impl:e2e_crate.Widget.impl[Display]".to_owned(),
        "rust:function:e2e_crate.Widget.impl[Display].fmt".to_owned(),
        // Task 11 (exit gate): the remaining leaf kinds, so the analyzed crate
        // exercises EVERY entity kind in the 0.5.0 ontology. (`module`, `struct`,
        // `function`, `trait`, `impl` above; `enum`/`type_alias`/`const`/`static`/
        // `macro` here.) Enum VARIANTS do not emit as separate entities.
        "rust:enum:e2e_crate.Color".to_owned(),
        "rust:type_alias:e2e_crate.Count".to_owned(),
        "rust:const:e2e_crate.MAX".to_owned(),
        "rust:static:e2e_crate.NAME".to_owned(),
        "rust:macro:e2e_crate.twice".to_owned(),
        // Task 6 (Phase 2 edges): `sub::Gauge` exists to mint a cross-module
        // field-type `references` edge (Gauge -> Widget); see the edge-set
        // assertion below.
        "rust:struct:e2e_crate.sub.Gauge".to_owned(),
    ];
    want.sort();

    assert_eq!(
        got, want,
        "stored rust entity-id set must contain only in-src/ entities.\n\
         The out-of-src/ collision leaks tests/it.rs + build.rs items.\n\
           got:  {got:#?}\n\
           want: {want:#?}"
    );

    // ── Task 8: the `implements` edge + the seen-entity-set gate ──────────────
    //
    // The run COMPLETED (asserted above) even though the fixture carries an
    // external-trait impl (`impl std::fmt::Display for Widget`): the plugin drops
    // the External trait at emit, so no dangling `implements` edge reaches the
    // host force-flush to FK-HardFail the run. (The host seen-set gate is the
    // second line of defence for a Resolved-but-unstored target.)
    let mut edge_stmt = conn
        .prepare(
            "SELECT from_id, to_id FROM edges \
             WHERE kind = 'implements' ORDER BY from_id, to_id",
        )
        .expect("prepare implements-edge query");
    let implements: Vec<(String, String)> = edge_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query implements edges")
        .map(|r| r.expect("implements row"))
        .collect();

    // Exactly ONE `implements` edge: the in-project `impl Bumpable for Widget`.
    // The external `impl std::fmt::Display for Widget` yields no edge.
    assert_eq!(
        implements,
        vec![(
            "rust:impl:e2e_crate.Widget.impl[Bumpable]".to_owned(),
            "rust:trait:e2e_crate.Bumpable".to_owned(),
        )],
        "expected exactly one stored implements edge (impl Bumpable for Widget); \
         the external Display impl must NOT yield one, and the run must still \
         have completed. got: {implements:#?}"
    );

    // ── Task 11: the in-project `imports` edge ────────────────────────────────
    //
    // `pub use crate::sub;` at the crate root resolves (Resolved) to the
    // in-project MODULE `e2e_crate.sub`, anchored from the crate-root module. The
    // host stored that target, so the edge survives and is stored.
    //
    // HOST FINDING (DONE_WITH_CONCERNS): the import targets the MODULE, not the
    // function `sub::helper`, on purpose. The host's Python-era import filter
    // `filter_external_import_edges_by_module_refs` (analyze.rs) retains an
    // `imports` edge ONLY when its `to_id` is a file-scope MODULE; a Rust import
    // resolving to any non-module item (function/struct/const/trait) is dropped
    // as "external" and counted in `imports_skipped_external_total` — even though
    // it resolved in-project and its target was stored. The seen-set gate
    // (`plugin_edges_dropped_unseen_total`) would have kept it, but the
    // module-filter pre-empts that gate. Empirically confirmed: with a
    // function-target import the run completed with `imports_skipped_external_total
    // == 1` and ZERO stored imports edges. This is a pre-existing host limitation
    // surfaced by the full-ontology E2E, NOT a Rust-plugin defect (the plugin
    // emits the function-target edge correctly — see imports_edges.rs). Reported,
    // not fixed: the module-filter is plugin-agnostic and the Python plugin
    // depends on it; fixing it is out of scope for the exit-gate test task.
    let mut imports_stmt = conn
        .prepare(
            "SELECT from_id, to_id FROM edges \
             WHERE kind = 'imports' ORDER BY from_id, to_id",
        )
        .expect("prepare imports-edge query");
    let imports: Vec<(String, String)> = imports_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query imports edges")
        .map(|r| r.expect("imports row"))
        .collect();
    assert_eq!(
        imports,
        vec![(
            "rust:module:e2e_crate".to_owned(),
            "rust:module:e2e_crate.sub".to_owned(),
        )],
        "expected exactly one stored imports edge (use crate::sub -> module sub); \
         got: {imports:#?}"
    );

    // ── Task 11: the rust-scoped structural `contains` edge set ────────────────
    //
    // Filtered to rust-emitted rows (`from_id LIKE 'rust:%'`) so the host's
    // `core:file:*` -> `rust:module:*` attachment edges (unsorted readdir order)
    // do not flake the snapshot. Set-based / `ORDER BY` — readdir is unsorted.
    let mut contains_stmt = conn
        .prepare(
            "SELECT from_id, to_id FROM edges \
             WHERE kind = 'contains' AND from_id LIKE 'rust:%' \
             ORDER BY from_id, to_id",
        )
        .expect("prepare contains-edge query");
    let mut contains: Vec<(String, String)> = contains_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query contains edges")
        .map(|r| r.expect("contains row"))
        .collect();
    contains.sort();

    let mut want_contains: Vec<(String, String)> = vec![
        // module -> leaf/struct/fn/impl children
        ("rust:module:e2e_crate", "rust:struct:e2e_crate.Widget"),
        ("rust:module:e2e_crate", "rust:function:e2e_crate.make"),
        (
            "rust:module:e2e_crate",
            "rust:impl:e2e_crate.Widget.impl#<>",
        ),
        ("rust:module:e2e_crate", "rust:trait:e2e_crate.Bumpable"),
        (
            "rust:module:e2e_crate",
            "rust:impl:e2e_crate.Widget.impl[Bumpable]",
        ),
        (
            "rust:module:e2e_crate",
            "rust:impl:e2e_crate.Widget.impl[Display]",
        ),
        ("rust:module:e2e_crate", "rust:enum:e2e_crate.Color"),
        ("rust:module:e2e_crate", "rust:type_alias:e2e_crate.Count"),
        ("rust:module:e2e_crate", "rust:const:e2e_crate.MAX"),
        ("rust:module:e2e_crate", "rust:static:e2e_crate.NAME"),
        ("rust:module:e2e_crate", "rust:macro:e2e_crate.twice"),
        // impl -> re-parented methods (Task 5)
        (
            "rust:impl:e2e_crate.Widget.impl#<>",
            "rust:function:e2e_crate.Widget.impl#<>.bump",
        ),
        (
            "rust:impl:e2e_crate.Widget.impl[Bumpable]",
            "rust:function:e2e_crate.Widget.impl[Bumpable].bump_by",
        ),
        (
            "rust:impl:e2e_crate.Widget.impl[Display]",
            "rust:function:e2e_crate.Widget.impl[Display].fmt",
        ),
        // sub module -> its function + its struct (Task 6)
        (
            "rust:module:e2e_crate.sub",
            "rust:function:e2e_crate.sub.helper",
        ),
        (
            "rust:module:e2e_crate.sub",
            "rust:struct:e2e_crate.sub.Gauge",
        ),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b.to_owned()))
    .collect();
    want_contains.sort();

    assert_eq!(
        contains, want_contains,
        "stored rust-scoped contains edge set mismatch.\n  got:  {contains:#?}\n  want: {want_contains:#?}"
    );

    // ── Task 6 (Phase 2): the `derives` + `references` edge SET ───────────────
    //
    // Writer-proving exact-SET assertion for the two Phase-2 edge kinds —
    // `(kind, from_id, to_id, confidence)` tuples, never mere presence: the edge
    // PK is `(kind, from_id, to_id)`, so a silent `ON CONFLICT` merge (the
    // historical failure mode) or a wrongly-minted extra edge both flunk set
    // equality. Expected rows, by fixture site:
    //   - `#[derive(Debug, Bumpable)] struct Widget` → ONE `derives` edge to the
    //     in-project trait `Bumpable` (Resolved); external `Debug` drops (D1).
    //   - `make() -> Widget { Widget { n: 0 } }` → ONE `references` edge
    //     make → Widget (return type + struct literal dedup to the PK pair).
    //   - `sub::Gauge { w: crate::Widget }` field type → Gauge → Widget.
    //   - `sub::helper(w: &crate::Widget)` param type → helper → Widget.
    //   - `sub::helper` body `crate::MAX` → helper → MAX.
    // Everything else in the fixture resolves External (i32, str, std::fmt::*,
    // locals) or is out of envelope (use/impl headers/macros) — no edge.
    let mut phase2_stmt = conn
        .prepare(
            "SELECT kind, from_id, to_id, confidence FROM edges \
             WHERE kind IN ('derives', 'references') \
             ORDER BY kind, from_id, to_id",
        )
        .expect("prepare derives/references edge query");
    let mut phase2: Vec<(String, String, String, String)> = phase2_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .expect("query derives/references edges")
        .map(|r| r.expect("derives/references row"))
        .collect();
    phase2.sort();

    let mut want_phase2: Vec<(String, String, String, String)> = vec![
        (
            "derives",
            "rust:struct:e2e_crate.Widget",
            "rust:trait:e2e_crate.Bumpable",
            "resolved",
        ),
        (
            "references",
            "rust:function:e2e_crate.make",
            "rust:struct:e2e_crate.Widget",
            "resolved",
        ),
        (
            "references",
            "rust:function:e2e_crate.sub.helper",
            "rust:const:e2e_crate.MAX",
            "resolved",
        ),
        (
            "references",
            "rust:function:e2e_crate.sub.helper",
            "rust:struct:e2e_crate.Widget",
            "resolved",
        ),
        (
            "references",
            "rust:struct:e2e_crate.sub.Gauge",
            "rust:struct:e2e_crate.Widget",
            "resolved",
        ),
    ]
    .into_iter()
    .map(|(k, f, t, c)| (k.to_owned(), f.to_owned(), t.to_owned(), c.to_owned()))
    .collect();
    want_phase2.sort();

    assert_eq!(
        phase2, want_phase2,
        "stored derives+references edge set mismatch (exact SET, incl. confidence).\n  \
         got:  {phase2:#?}\n  want: {want_phase2:#?}"
    );
}

/// clarion-6ec7317628 — dual-declared module trees must keep `parent_id` and
/// `contains` in agreement (ADR-026 dual encoding).
///
/// Mirrors tokio's `src/process/` shape: `src/lib.rs` mounts the directory
/// module under a different name via `#[path = "sub/mod.rs"] mod imp;` AND
/// declares an inline `mod sub { pub(crate) use super::imp::*; }` facade.
/// The plugin derives module paths from file paths (no `#[path]` resolution),
/// so BOTH files mint the same `rust:module:dualmod.sub` id:
///   - `src/sub/mod.rs` → the file-level module entity, and
///   - `src/lib.rs`'s inline `mod sub` → an identically-named nested module.
///
/// Both are `file_scope`, so the host re-parents each emission to ITS OWN
/// file and emits a `file -> module` contains edge per file — two contains
/// parents for one entity, with `entities.parent_id` last-write-won. The
/// writer's ADR-026 dual-encoding check (`LMWV-INFRA-PARENT-CONTAINS-MISMATCH`)
/// rejects that and fails the run — `loomweave analyze` over tokio dies on
/// `tokio.process.unix`.
///
/// The fix is host-side wiring (first-claim-wins per module id, seeded from
/// skipped files on incremental runs); entity ids never change. This test is
/// writer-proven and walks three phases:
///   1. full analyze → run completes, exactly ONE `file -> module` contains
///      edge, `parent_id` agrees;
///   2. touch the NON-owner file only → incremental analyze completes and the
///      surviving claim still agrees (the seeded-claim path);
///   3. touch the owner file only → incremental analyze completes and the
///      re-emitted claim still agrees (the re-claim path).
#[test]
fn analyze_e2e_dual_declared_module_keeps_parent_and_contains_in_agreement() {
    const MODULE_ID: &str = "rust:module:dualmod.sub";
    let plugin_dir = setup_plugin_dir();
    let driver = loomweave_driver_path();

    let project = TempDir::new().expect("create project tempdir");
    let root = project.path();
    fs::create_dir_all(root.join("src/sub")).expect("create src/sub");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"dualmod\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        root.join("src/lib.rs"),
        "#[path = \"sub/mod.rs\"]\n\
         #[cfg(unix)]\n\
         mod imp;\n\
         \n\
         #[cfg(unix)]\n\
         pub(crate) mod sub {\n\
             pub(crate) use super::imp::*;\n\
         }\n",
    )
    .expect("write lib.rs");
    fs::write(root.join("src/sub/mod.rs"), "pub(crate) fn helper() {}\n").expect("write mod.rs");

    let install = std::process::Command::new(&driver)
        .args(["install", "--path"])
        .arg(root)
        .status()
        .expect("spawn loomweave install");
    assert!(install.success(), "loomweave install must succeed");

    let synthetic_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");
    let run_analyze = |phase: &str| {
        let _ = std::process::Command::new(&driver)
            .args(["analyze"])
            .arg(root)
            .env("PATH", &synthetic_path)
            .status()
            .unwrap_or_else(|e| panic!("spawn loomweave analyze ({phase}): {e}"));
    };

    let db_path = root.join(".weft/loomweave/loomweave.db");
    // Writer-proven phase assertion: latest run completed, exactly ONE
    // `file -> module` contains edge to the dual-declared module, and the
    // module's `parent_id` equals that edge's `from_id` (ADR-026 agreement).
    // Returns the owning core file id.
    let assert_consistent = |phase: &str| -> String {
        let conn = Connection::open(&db_path).expect("open index db");
        let (run_status, failure_detail): (String, String) = conn
            .query_row(
                "SELECT status, COALESCE(stats, '') FROM runs ORDER BY rowid DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query latest run");
        assert_eq!(
            run_status, "completed",
            "[{phase}] run must complete; dual-declared module must not trip \
             LMWV-INFRA-PARENT-CONTAINS-MISMATCH. stats: {failure_detail}"
        );
        let contains_parents: Vec<String> = conn
            .prepare(
                "SELECT from_id FROM edges WHERE kind = 'contains' AND to_id = ?1 ORDER BY from_id",
            )
            .expect("prepare contains query")
            .query_map([MODULE_ID], |row| row.get::<_, String>(0))
            .expect("query contains parents")
            .map(|r| r.expect("contains row"))
            .collect();
        assert_eq!(
            contains_parents.len(),
            1,
            "[{phase}] exactly one `file -> module` contains edge may exist for \
             the dual-declared module; got {contains_parents:#?}"
        );
        let parent_id: Option<String> = conn
            .query_row(
                "SELECT parent_id FROM entities WHERE id = ?1",
                [MODULE_ID],
                |row| row.get(0),
            )
            .expect("query module parent_id");
        assert_eq!(
            parent_id.as_deref(),
            Some(contains_parents[0].as_str()),
            "[{phase}] module parent_id must agree with its single contains edge"
        );
        contains_parents[0].clone()
    };

    // ── Phase 1: full analyze ────────────────────────────────────────────────
    run_analyze("full");
    let owner_file_id = assert_consistent("full");

    // Map the owning core file id back to the on-disk path; the OTHER emitter
    // is the non-owner. (`core:file:<relpath>`.)
    let owner_rel = owner_file_id
        .strip_prefix("core:file:")
        .unwrap_or_else(|| panic!("unexpected owner file id shape: {owner_file_id}"));
    let (owner_path, non_owner_path) = if owner_rel == "src/lib.rs" {
        (root.join("src/lib.rs"), root.join("src/sub/mod.rs"))
    } else {
        assert_eq!(
            owner_rel, "src/sub/mod.rs",
            "owner must be one of the two emitters"
        );
        (root.join("src/sub/mod.rs"), root.join("src/lib.rs"))
    };
    let append = |path: &Path, line: &str| {
        let mut src = fs::read_to_string(path).expect("read source");
        src.push_str(line);
        fs::write(path, src).expect("append source");
    };

    // ── Phase 2: change ONLY the non-owner (owner skipped → seeded claim) ────
    append(&non_owner_path, "// incremental touch: non-owner\n");
    run_analyze("incremental non-owner");
    let owner_after_phase2 = assert_consistent("incremental non-owner");
    assert_eq!(
        owner_after_phase2, owner_file_id,
        "[incremental non-owner] the skipped owner's claim must survive"
    );

    // ── Phase 3: change ONLY the owner (non-owner skipped → re-claim) ────────
    append(&owner_path, "// incremental touch: owner\n");
    run_analyze("incremental owner");
    assert_consistent("incremental owner");
}

/// Task 11 (Part B2) — END-TO-END exercise of the host seen-entity-set gate.
///
/// The gate (`drop_unready_plugin_edges`) was previously only UNIT-tested. This
/// drives the full `install` -> `analyze` host pipeline over a project crafted so
/// the PLUGIN resolves a *Resolved* anchored edge whose target the HOST never
/// stored, and asserts the run still COMPLETES (no dangling-FK hard-fail) with a
/// non-zero `plugin_edges_dropped_unseen_total`.
///
/// The divergence source is D2, confirmed feasible empirically (the host walk
/// honours `.gitignore` even in a non-git tempdir — `collect_source_files` sets
/// `require_git(false)`; the plugin's `build_symbol_table` walk uses plain
/// `std::fs::read_dir` and is NOT gitignore-aware):
///   - `.gitignore` excludes `src/hidden.rs`,
///   - `src/hidden.rs` declares `pub trait Hidden {}` — IN a crate's `src/` (so
///     the plugin's scope guard accepts it) but gitignored (so the host walk
///     skips it; the host never stores the trait),
///   - `src/widget.rs` carries `impl crate::hidden::Hidden for Widget {}` — the
///     plugin resolves the trait against its gitignore-UNAWARE symbol table
///     (Resolved), emitting an anchored `implements` edge to a trait id the host
///     never stored.
///
/// The host's seen-set gate drops-and-counts that edge: the run is `completed`,
/// no `implements` edge to the hidden trait is stored, and
/// `plugin_edges_dropped_unseen_total` is at least one.
#[test]
fn analyze_e2e_seen_set_gate_drops_resolved_edge_to_gitignored_target() {
    let plugin_dir = setup_plugin_dir();
    let driver = loomweave_driver_path();

    // Build the project inline (NOT the shared fixture) so the gitignored hidden
    // trait + its in-src impl do not perturb the full-ontology snapshot above.
    let project = TempDir::new().expect("create project tempdir");
    let root = project.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"gate_crate\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    // The host walk honours this; the plugin's symbol-table walk does not. The
    // gitignored module file holds the trait the host never stores. (A single
    // module FILE, not a dir: `src/hidden.rs` is module `hidden`, so the trait's
    // qualname is `gate_crate.hidden.Hidden` — what the impl path below resolves
    // to. A `src/hidden/secret.rs` would instead be module `hidden.secret`.)
    fs::write(root.join(".gitignore"), "src/hidden.rs\n").expect("write .gitignore");
    // In-`src/` (plugin-visible) but gitignored (host-invisible): the trait the
    // host never stores.
    fs::write(
        root.join("src/hidden.rs"),
        "pub trait Hidden { fn h(&self); }\n",
    )
    .expect("write hidden.rs");
    fs::write(
        root.join("src/lib.rs"),
        "pub mod hidden;\npub mod widget;\n",
    )
    .expect("write lib.rs");
    // The in-`src/` impl whose trait resolves against the gitignore-unaware
    // plugin symbol table (`crate::hidden::Hidden` -> `gate_crate.hidden.Hidden`)
    // but whose target the host never stored.
    fs::write(
        root.join("src/widget.rs"),
        "pub struct Widget;\n\
         impl crate::hidden::Hidden for Widget { fn h(&self) {} }\n",
    )
    .expect("write widget.rs");

    let install = std::process::Command::new(&driver)
        .args(["install", "--path"])
        .arg(root)
        .status()
        .expect("spawn loomweave install");
    assert!(install.success(), "loomweave install must succeed");

    let synthetic_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");
    let _ = std::process::Command::new(&driver)
        .args(["analyze"])
        .arg(root)
        .env("PATH", &synthetic_path)
        .status()
        .expect("spawn loomweave analyze");

    let conn = Connection::open(root.join(".weft/loomweave/loomweave.db")).expect("open index db");

    // The run COMPLETED — the dangling Resolved-but-unstored edge did NOT FK-fail
    // the run; the gate dropped it instead.
    let run_status: String = conn
        .query_row("SELECT COALESCE(MAX(status), '') FROM runs", [], |row| {
            row.get(0)
        })
        .expect("query runs status");
    assert_eq!(
        run_status, "completed",
        "run must complete: the gate drops the Resolved edge to the gitignored \
         (host-unstored) trait instead of HardFailing on a dangling FK; got {run_status:?}"
    );

    // The host never stored the gitignored trait entity.
    let hidden_trait_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entities WHERE id = 'rust:trait:gate_crate.hidden.Hidden'",
            [],
            |row| row.get(0),
        )
        .expect("query hidden trait");
    assert_eq!(
        hidden_trait_count, 0,
        "the gitignored trait must NOT be stored (host walk skips src/hidden/)"
    );

    // No `implements` edge to the gitignored trait survived.
    let dangling_impl_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edges WHERE kind = 'implements' \
             AND to_id = 'rust:trait:gate_crate.hidden.Hidden'",
            [],
            |row| row.get(0),
        )
        .expect("query dangling implements edge");
    assert_eq!(
        dangling_impl_count, 0,
        "no implements edge to the unstored gitignored trait may survive"
    );

    // The gate dropped-and-counted at least the one Resolved-but-unstored edge.
    let stats: String = conn
        .query_row(
            "SELECT COALESCE(stats, '') FROM runs WHERE status = 'completed' \
             ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("query run stats");
    let stats: serde_json::Value = serde_json::from_str(&stats).expect("runs.stats is valid JSON");
    let dropped = stats
        .get("plugin_edges_dropped_unseen_total")
        .and_then(serde_json::Value::as_u64)
        .expect("stats carries plugin_edges_dropped_unseen_total");
    assert!(
        dropped >= 1,
        "the seen-set gate must drop-and-count the Resolved edge to the \
         gitignored trait (plugin_edges_dropped_unseen_total >= 1); got {dropped}. \
         stats: {stats:#}"
    );
}
