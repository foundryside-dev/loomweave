//! Init-time project symbol table (Task 7, spec §2.3).
//!
//! At `initialize`, walk `project_root`, one `syn` parse per `.rs` file, and
//! build a map from every declared entity qualname → its id. Phase 1a does not
//! yet resolve cross-file edges (Phase 1b does), but the table is built and
//! proven now so 1b can resolve against it; building it here also lets the
//! dogfood gate (Task 14) assert global uniqueness.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::crate_roots::{CrateRoots, discover_crate_roots};
use crate::extract::extract_file;
use crate::mounts::{ModMounts, discover_mounts};
use crate::scope::emittable_scope;

/// A project-wide map from every declared entity id to its qualified name,
/// plus the ids that were seen more than once during the init-time walk.
pub struct SymbolTable {
    /// entity id -> qualified name (the resolution surface for Phase 1b edges).
    by_id: BTreeMap<String, String>,
    /// qualified name -> the ids that share it (the REVERSE index Phase 1b's
    /// resolver inverts). A `Vec` because one qualname can map to several kinds
    /// (e.g. a `struct S` and a `fn S` share qualname `crate.mod.S` but differ
    /// in kind, which lives in the id, not the qualname). Kept sorted so the
    /// resolver's multi-kind "first by sorted order" tiebreak is deterministic.
    by_qualname: BTreeMap<String, Vec<String>>,
    /// ids seen more than once during the walk (must be empty — the gate).
    duplicates: Vec<String>,
}

/// Empty id slice returned for a qualname absent from the reverse index.
static EMPTY_IDS: &[String] = &[];

impl SymbolTable {
    /// Whether an entity with this id was declared anywhere in the project.
    #[must_use]
    pub fn contains_id(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// The ids that collided during the walk (must be empty for a healthy
    /// project — this is what the dogfood gate asserts).
    ///
    /// Test/oracle surface only: at runtime the HOST detects duplicate
    /// locators plugin-agnostically in its analyze path and surfaces them as
    /// `LMWV-DUPLICATE-LOCATOR` ERROR findings (clarion-b19fe90c3e), so this
    /// accessor is consulted by the dogfood-uniqueness test and the
    /// `qualname_check` example binary, not by the analyze pipeline.
    #[must_use]
    pub fn duplicate_ids(&self) -> Vec<String> {
        self.duplicates.clone()
    }

    /// The ids declared at this qualified name, sorted (empty slice if absent).
    /// One qualname can map to several ids that differ only in kind; the
    /// Phase 1b resolver inverts a `use`/trait path into this slice.
    #[must_use]
    pub fn ids_for_qualname(&self, q: &str) -> &[String] {
        self.by_qualname.get(q).map_or(EMPTY_IDS, Vec::as_slice)
    }

    /// Every entity id in the table, in sorted order (Tasks 7/8 may iterate it).
    pub fn iter_ids(&self) -> impl Iterator<Item = &str> {
        self.by_id.keys().map(String::as_str)
    }

    /// Number of distinct entity ids in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the table holds no entities.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Walk `project_root` (one `syn` parse per `.rs` file) and build the
/// project-wide symbol table. Files whose crate root cannot be resolved,
/// whose source fails to parse, or which are rejected by the pre-parse guards
/// (oversize / depth bomb / prefix bomb — ADR-050; same silent-skip semantics
/// as a parse error: their items contribute nothing, and the visible degraded
/// finding is `analyze_file`'s job) are skipped. Collisions are recorded in
/// [`SymbolTable::duplicate_ids`] rather than silently dropped.
#[must_use]
pub fn build_symbol_table(project_root: &Path) -> SymbolTable {
    let roots = discover_crate_roots(project_root);
    // `#[path]` mounts MUST be discovered before any module path is derived
    // (ADR-049 Amendment 8): the table's qualnames have to be mount-correct or
    // use-resolution desyncs from `analyze_file`'s emissions.
    let mounts = discover_mounts(project_root, &roots);
    build_symbol_table_with(project_root, &roots, &mounts)
}

/// [`build_symbol_table`] with caller-supplied crate roots and `#[path]`
/// mounts. The serve loop uses this so the SAME `ModMounts` instance feeds
/// both the init-time table build and every later `analyze_file` scope
/// derivation — building them separately could let the two routes diverge.
#[must_use]
pub fn build_symbol_table_with(
    project_root: &Path,
    roots: &CrateRoots,
    mounts: &ModMounts,
) -> SymbolTable {
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    let mut by_qualname: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut duplicates = Vec::new();
    for file in walk_rs_files(project_root) {
        // Crate-scope discipline (src/-only, redundant-main skip, module-path
        // derivation) is shared with `analyze_one_file` via `scope::emittable_scope`:
        // a file outside the library/binary crate the ADR-049 qualname scheme
        // names contributes nothing rather than minting a colliding
        // `rust:module:<crate>` locator.
        let Some((crate_name, module_path)) = emittable_scope(roots, &file, mounts) else {
            continue;
        };
        // Pre-parse guards (ADR-050): an oversize file is skipped WITHOUT
        // reading it; a depth/prefix bomb is skipped before it can overflow
        // the parser stack. This init walk runs inside the host's `initialize`
        // handshake — a crash here would fail the whole plugin spawn.
        if crate::parse_guard::check_file_size(&file).is_err() {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        if crate::parse_guard::scan_source(&src).is_err() {
            continue;
        }
        let Ok(entities) = extract_file(&crate_name, &module_path, &file.to_string_lossy(), &src)
        else {
            continue;
        };
        for e in entities {
            let id = e["id"].as_str().unwrap_or_default().to_owned();
            let q = e["qualified_name"].as_str().unwrap_or_default().to_owned();
            // Invert into the reverse index. Re-declared ids (collisions) are NOT
            // double-counted here: a colliding id contributes its qualname once
            // and is recorded in `duplicates` below for the gate.
            if by_id.insert(id.clone(), q.clone()).is_some() {
                duplicates.push(id);
            } else {
                by_qualname.entry(q).or_default().push(id);
            }
        }
    }
    // Determinism: the resolver's multi-kind "first by sorted order" tiebreak
    // relies on each qualname's id vec being sorted.
    for ids in by_qualname.values_mut() {
        ids.sort();
    }
    SymbolTable {
        by_id,
        by_qualname,
        duplicates,
    }
}

/// Recursively collect every `.rs` file under `root`, skipping vendored /
/// build / store directories the host also skips. Shared with
/// [`discover_mounts`] so mount discovery sees exactly the file set the table
/// build does (same symlink rules, same ignored dirs).
pub(crate) fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Do NOT follow symlinked directories. The host's path-jail covers
        // `analyze_file` paths but not this init walk; a symlinked dir is either
        // an out-of-tree escape (reads files outside the project) or a cycle
        // (re-collects in-tree files under an aliased path, double-minting ids).
        // `DirEntry::file_type()` reports the link itself (it does NOT traverse),
        // unlike `Path::is_dir()` which follows the link. Skip the entry when it
        // IS a symlink, AND when its type cannot be determined: on an `Err` we
        // must not fall through to `Path::is_dir()`, which would follow a
        // symlink we failed to classify. Treat can-not-determine as do-not-recurse.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            if !is_ignored(&path) {
                walk(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Skip vendored / build / store directories (mirrors `crate_roots`).
fn is_ignored(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("target" | ".git" | ".weft" | "node_modules")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn builds_a_table_over_a_two_crate_workspace_with_no_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname=\"a_crate\"\n",
        )
        .unwrap();
        fs::write(root.join("crates/a/src/lib.rs"), "pub struct X;\n").unwrap();
        fs::create_dir_all(root.join("crates/b/src")).unwrap();
        fs::write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname=\"b_crate\"\n",
        )
        .unwrap();
        fs::write(root.join("crates/b/src/lib.rs"), "pub struct X;\n").unwrap();

        let table = build_symbol_table(root);
        // same item name in two crates -> two DISTINCT ids, no collision
        assert!(table.contains_id("rust:struct:a_crate.X"));
        assert!(table.contains_id("rust:struct:b_crate.X"));
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
    }

    #[test]
    fn integration_tests_and_benches_are_out_of_scope_and_do_not_collide() {
        // A crate's `tests/` and `benches/` files are separate compilation
        // units; folding them into the library's namespace would mint a
        // second `rust:module:<crate>` locator. The walk must skip them.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src")).unwrap();
        fs::create_dir_all(root.join("c/tests")).unwrap();
        fs::create_dir_all(root.join("c/benches")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        fs::write(root.join("c/src/lib.rs"), "pub fn lib_fn() {}\n").unwrap();
        fs::write(root.join("c/tests/it.rs"), "fn helper() {}\n").unwrap();
        fs::write(root.join("c/benches/b.rs"), "fn bench_helper() {}\n").unwrap();

        let table = build_symbol_table(root);
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
        // the lib's own module/function are present...
        assert!(table.contains_id("rust:module:c_crate"));
        assert!(table.contains_id("rust:function:c_crate.lib_fn"));
        // ...but the test/bench helpers (which would have landed at the bare
        // crate path) are NOT attributed to the library crate.
        assert!(!table.contains_id("rust:function:c_crate.helper"));
        assert!(!table.contains_id("rust:function:c_crate.bench_helper"));
    }

    #[test]
    fn lib_and_main_in_one_crate_do_not_collide_on_the_crate_module() {
        // A crate shipping both `src/lib.rs` and `src/main.rs` has two crate
        // roots sharing a source dir; both would resolve to the bare crate
        // module path. ADR-049 makes `lib.rs` canonical, so `main.rs` is skipped
        // and `rust:module:<crate>` is emitted exactly once.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        fs::write(root.join("c/src/lib.rs"), "pub fn lib_fn() {}\n").unwrap();
        fs::write(root.join("c/src/main.rs"), "fn main() {}\n").unwrap();

        let table = build_symbol_table(root);
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
        assert!(table.contains_id("rust:module:c_crate"));
        assert!(table.contains_id("rust:function:c_crate.lib_fn"));
    }

    #[test]
    fn pure_binary_crate_keeps_its_main_root() {
        // A crate with only `src/main.rs` (no lib) — `main.rs` IS its root and
        // must be kept, not skipped by the redundant-main rule.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        fs::write(root.join("c/src/main.rs"), "fn run_it() {}\nfn main() {}\n").unwrap();

        let table = build_symbol_table(root);
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
        assert!(table.contains_id("rust:module:c_crate"));
        assert!(table.contains_id("rust:function:c_crate.run_it"));
    }

    #[test]
    fn path_mounted_module_splits_from_its_inline_facade() {
        // ADR-049 Amendment 8 minimal repro (clarion-bdb1eccf48), the tokio
        // `src/process` shape: `unix/mod.rs` is mounted as `mod imp;`
        // (cfg-twinned with a windows mount) and an inline facade
        // `mod unix { … }` re-exports it. Pre-amendment, the file walk routed
        // unix/mod.rs by filesystem to `foo.unix` — the same id the inline
        // facade mints — and the duplicate either silently merged or FailRan
        // the writer. Post-amendment the mounted file routes to its logical
        // path `foo.imp@cfg(unix)` (twin mount → @cfg split) and the facade
        // keeps `foo.unix`.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src/unix")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"foo\"\n").unwrap();
        fs::write(
            root.join("c/src/lib.rs"),
            "#[cfg(unix)]\n#[path = \"unix/mod.rs\"]\nmod imp;\n\
             #[cfg(windows)]\n#[path = \"windows/mod.rs\"]\nmod imp;\n\
             #[cfg(unix)]\npub(crate) mod unix {\n    pub(crate) use super::imp::*;\n}\n",
        )
        .unwrap();
        fs::write(root.join("c/src/unix/mod.rs"), "pub(crate) fn spawn() {}\n").unwrap();

        let table = build_symbol_table(root);
        assert_eq!(
            table.duplicate_ids(),
            Vec::<String>::new(),
            "mounted module and its inline facade must not collide"
        );
        assert!(
            table.contains_id("rust:module:foo.imp@cfg(unix)"),
            "mounted file must route to its logical (twin-cfg-split) path"
        );
        assert!(
            table.contains_id("rust:module:foo.unix"),
            "the inline facade keeps the public name"
        );
        assert!(
            table.contains_id("rust:function:foo.imp@cfg(unix).spawn"),
            "items in the mounted file ride the mounted module path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_symbol_table_does_not_follow_symlinked_dirs() {
        // The init walk is load-bearing (Phase 1b resolves against it) and must
        // not follow directory symlinks: a symlinked dir is either an out-of-tree
        // ESCAPE (reads files the host's path-jail never sanctioned) or a CYCLE
        // (re-collects in-tree files under an aliased path, double-minting ids and
        // tripping the duplicates gate; on POSIX the kernel's ELOOP cap stops the
        // recursion before a stack overflow, so the harm is collisions, not a crash).
        use std::os::unix::fs::symlink;
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path();
        fs::create_dir_all(root.join("c/src")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        fs::write(root.join("c/src/lib.rs"), "pub fn f() {}\n").unwrap();

        // ESCAPE: an out-of-tree dir holding a `.rs` file, symlinked *inside* the
        // crate's src so a naive walk would collect it as `c_crate.sub.evil`.
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("evil.rs"), "pub fn evil() {}\n").unwrap();
        symlink(outside.path(), root.join("c/src/sub")).unwrap();

        // CYCLE: root/loop -> root. A followed cycle re-collects c_crate's files
        // under the aliased path and double-mints their ids.
        symlink(root, root.join("loop")).unwrap();

        let table = build_symbol_table(root); // must RETURN (no hang/overflow)

        // CYCLE not followed: no id was minted twice.
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
        // ESCAPE not followed: nothing from the out-of-tree `evil.rs`.
        assert!(table.iter_ids().all(|id| !id.contains("evil")));
        // The real in-project fn IS present (the walk still works).
        assert!(table.contains_id("rust:function:c_crate.f"));
    }
}
