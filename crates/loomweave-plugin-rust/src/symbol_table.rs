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
        // Crate-scope discipline (src/-only gate, binary-target namespace
        // routing, module-path derivation) is shared with `analyze_one_file` via
        // `scope::emittable_scope`: a file outside the library/binary crate the
        // ADR-049 qualname scheme names contributes nothing rather than minting a
        // colliding `rust:module:<crate>` locator, and a Cargo binary target
        // (main.rs alongside lib.rs, or src/bin/*.rs) routes to its own
        // `<crate>@bin(<name>)` root rather than the library namespace.
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

/// Directories the HOST's source walk skips unconditionally
/// (`loomweave-cli`'s `analyze::SKIP_DIRS`). Mirrored here so this plugin's
/// init walk sees the same directory set the host dispatches from. Kept in
/// lockstep with that list by hand — the two crates do not share a constant.
const SKIP_DIRS: &[&str] = &[
    ".weft",
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".venv",
    "__pycache__",
    "node_modules",
];

/// Collect every `.rs` file under `root` that the HOST would dispatch — i.e.
/// honouring the SAME ignore policy the host's source walk applies before it
/// decides which files are stored (`.gitignore` / `.ignore` / global gitignore /
/// git-exclude, plus the `SKIP_DIRS` set), via the same `ignore` crate.
///
/// FINDING C (resolver/host source-set divergence): the init walk feeds the
/// symbol table the resolver consults. If it walked the raw filesystem while the
/// host walked an ignore-filtered set, a gitignored/generated `.rs` file could
/// define a symbol a tracked file imports — the resolver would mark the
/// reference *resolved* against that ignored symbol, then the host would DROP
/// the edge (its target file was never stored), leaving neither a real edge nor
/// an unresolved-call-site record. Mirroring the host's policy here keeps the
/// resolver's view a subset of what the host actually stores, so a resolved
/// reference always has a stored target. The plugin protocol carries only
/// `project_root` (no dispatched file list — see `InitializeParams`), so this
/// MIRRORS the host policy rather than consuming the dispatched set; the two
/// `SKIP_DIRS` lists are kept in lockstep by hand.
///
/// Shared with [`discover_mounts`](crate::mounts::discover_mounts) so mount
/// discovery sees exactly the file set the table build does (same ignore
/// policy, same symlink rule).
pub(crate) fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        // Do NOT follow directory symlinks. The host's path-jail covers
        // `analyze_file` paths but not this init walk; a symlinked dir is either
        // an out-of-tree escape (reads files outside the project) or a cycle
        // (re-collects in-tree files under an aliased path, double-minting ids).
        // The `ignore` crate's walker enforces this without per-entry file_type
        // probing.
        .follow_links(false)
        // Match the host's `collect_source_files` filter set byte-for-byte:
        // hidden files ARE walked (`hidden(false)` = do not skip them), but
        // `.gitignore` / `.ignore` / global gitignore / git-exclude DO apply,
        // and `require_git(false)` makes the gitignore rules apply even outside a
        // git checkout (the testbed / vendored-tree case).
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|entry| !is_skipped_dir(entry));

    for result in builder.build() {
        // A per-entry error (unreadable dirent, ignored-path error) is skipped:
        // the same silent-skip semantics the rest of the init walk already uses,
        // and the host's own walk logs+counts these too. A file we cannot see
        // contributes nothing rather than aborting the table build.
        let Ok(entry) = result else { continue };
        let path = entry.path();
        if entry.file_type().is_some_and(|t| t.is_file())
            && path.extension().and_then(|e| e.to_str()) == Some("rs")
        {
            out.push(path.to_path_buf());
        }
    }
    out
}

/// Skip the host's `SKIP_DIRS` directories (mirrors
/// `loomweave-cli::analyze::is_skipped_dir`).
fn is_skipped_dir(entry: &ignore::DirEntry) -> bool {
    entry.file_type().is_some_and(|t| t.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SKIP_DIRS.contains(&name))
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
        // roots sharing a source dir; routing both to the bare crate module path
        // would collide. ADR-049 makes `lib.rs` canonical for `rust:module:<crate>`;
        // the binary entrypoint is NOT dropped (Finding A) — it routes to its own
        // `<crate>@bin(<crate>)` root, so the bin's code stays in the graph while
        // the crate module is emitted exactly once.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        fs::write(root.join("c/src/lib.rs"), "pub fn lib_fn() {}\n").unwrap();
        fs::write(
            root.join("c/src/main.rs"),
            "fn run_app() {}\nfn main() {}\n",
        )
        .unwrap();

        let table = build_symbol_table(root);
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
        // lib.rs is canonical for the bare crate module + lib items.
        assert!(table.contains_id("rust:module:c_crate"));
        assert!(table.contains_id("rust:function:c_crate.lib_fn"));
        // main.rs's application code is preserved under the distinct bin root.
        assert!(table.contains_id("rust:module:c_crate@bin(c_crate)"));
        assert!(table.contains_id("rust:function:c_crate@bin(c_crate).run_app"));
        assert!(table.contains_id("rust:function:c_crate@bin(c_crate).main"));
    }

    #[test]
    fn src_bin_targets_stay_out_of_the_library_namespace() {
        // Finding B: Cargo automatic binary targets under `src/bin/`. A
        // `src/bin/tool.rs` must NOT land at `c_crate.bin.tool` (the library
        // namespace, colliding with a real `mod bin`) — it routes to its own
        // `c_crate@bin(tool)` root, distinct from the library's `c_crate.bin`.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src/bin")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        // a REAL `mod bin` in the library — would collide with a naive route.
        fs::write(
            root.join("c/src/lib.rs"),
            "pub mod bin {\n    pub fn real() {}\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("c/src/bin/tool.rs"),
            "fn helper() {}\nfn main() {}\n",
        )
        .unwrap();

        let table = build_symbol_table(root);
        assert_eq!(
            table.duplicate_ids(),
            Vec::<String>::new(),
            "the src/bin target must not collide with the library `mod bin`"
        );
        // The library's real `mod bin` keeps the dotted library path.
        assert!(table.contains_id("rust:module:c_crate.bin"));
        assert!(table.contains_id("rust:function:c_crate.bin.real"));
        // The bin TARGET routes to its own `@bin(...)` root, NOT the library.
        assert!(table.contains_id("rust:module:c_crate@bin(tool)"));
        assert!(table.contains_id("rust:function:c_crate@bin(tool).helper"));
        assert!(
            !table.contains_id("rust:function:c_crate.bin.tool.helper"),
            "bin target must not be folded into the library `bin` module"
        );
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

    #[test]
    fn gitignored_rs_files_are_excluded_from_the_symbol_table() {
        // FINDING C: the init walk must honour the same ignore policy the host's
        // source walk applies before it decides which files are stored. A
        // gitignored / generated `.rs` file is NEVER dispatched by the host, so
        // its symbols must NOT enter the resolver's table — otherwise a tracked
        // file's reference to such a symbol resolves against a target the host
        // dropped, losing both the edge and its unresolved-call-site record.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("c/src")).unwrap();
        fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        fs::write(root.join("c/src/lib.rs"), "pub fn tracked() {}\n").unwrap();
        // A generated file the host's `.gitignore`-honouring walk would skip.
        fs::write(
            root.join("c/src/generated.rs"),
            "pub fn from_generated() {}\n",
        )
        .unwrap();
        // .gitignore lives at the crate dir (the `ignore` crate reads it with
        // `require_git(false)`, so no git checkout is needed).
        fs::write(root.join("c/.gitignore"), "/src/generated.rs\n").unwrap();

        let table = build_symbol_table(root);
        // The tracked file's symbol IS present.
        assert!(table.contains_id("rust:function:c_crate.tracked"));
        // The gitignored file's symbol is NOT — the resolver's view is a subset
        // of what the host stores, so a resolved reference always has a target.
        assert!(
            !table.contains_id("rust:function:c_crate.from_generated"),
            "gitignored .rs symbol must not enter the resolver table (Finding C)"
        );
        assert!(table.iter_ids().all(|id| !id.contains("from_generated")));
    }
}
