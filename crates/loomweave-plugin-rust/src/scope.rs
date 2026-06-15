//! Shared crate-scope discipline (formerly inline in `symbol_table.rs`).
//! A file is *emittable* only when it is part of the library/binary crate the
//! ADR-049 qualname scheme names: under the crate's `src/` tree. `tests/`,
//! `benches/`, `examples/`, and `build.rs` are SEPARATE compilation units —
//! folding them in would mint colliding `rust:module:<crate>` locators (each
//! one's bare-crate fallback), a data-loss / `FailRun` at the storage writer.
//!
//! Cargo BINARY TARGETS that share the library's `src/` tree are NOT dropped —
//! they carry application-only code (entrypoints, CLI glue) that belongs in the
//! graph — but they MUST NOT land in the library namespace, where they would
//! collide. Two shapes need a distinct binary-target module root:
//!
//! * `src/main.rs` alongside a sibling `src/lib.rs` — a lib+bin crate. The
//!   bare-crate path `<crate>` is the LIBRARY's (`lib.rs` is canonical), so
//!   `main.rs`'s items route to the bin-target root `<crate>@bin(<crate>)`.
//! * `src/bin/<name>.rs` (or `src/bin/<name>/main.rs`) — Cargo's automatic
//!   binary targets. These are separate compilation units that merely live
//!   under `src/`; routing them by filesystem to `<crate>.bin.<name>` mixes
//!   them into the library namespace and collides with a real `mod bin`. They
//!   route to `<crate>@bin(<name>)` instead.
//!
//! The `@bin(<name>)` root segment can never collide with a real module: module
//! names are bare Rust identifiers (a `mod bin` is `<crate>.bin`, dot-joined),
//! and `@…(…)` is the same reserved-suffix grammar ADR-049 §3 uses for
//! `@cfg(…)`. A PURE binary crate (only `src/main.rs`, no `lib.rs`) keeps the
//! bare `<crate>` root: `main.rs` IS its canonical crate root, nothing collides.
use std::path::{Path, PathBuf};

use crate::crate_roots::CrateRoots;
use crate::module_path::{logical_module_path, module_path_for};
use crate::mounts::ModMounts;

/// `(crate_name, dotted_module_path)` for an emittable file, else `None`.
///
/// Mirrors the guard sequence formerly inline in
/// [`build_symbol_table`](crate::symbol_table::build_symbol_table): resolve the
/// owning crate name and source root, require the file to live under that
/// `src/` tree, then derive the dotted module path — binary-target-aware (a
/// Cargo bin target routes to its own `<crate>@bin(<name>)` root, see the
/// module docs) and `#[path]`-mount-aware since ADR-049 Amendment 8 (`mounts`
/// overlays the filesystem route; an empty overlay is byte-identical to the
/// pre-amendment behaviour).
#[must_use]
pub fn emittable_scope(
    roots: &CrateRoots,
    file: &Path,
    mounts: &ModMounts,
) -> Option<(String, String)> {
    let (crate_name, src_root) = crate_src_scope(roots, file)?;
    let module_path = scoped_module_path(&crate_name, &src_root, file, mounts);
    Some((crate_name, module_path))
}

/// The crate-scope gate alone — `(crate_name, src_root)` when `file` belongs to
/// the library/binary crate the ADR-049 qualname scheme names, else `None`
/// (`tests/`, `benches/`, `examples/`, `build.rs`, or any path outside a known
/// crate's `src/` tree). Split out of [`emittable_scope`] so `#[path]`-mount
/// discovery (which runs BEFORE any module path can be derived) can gate
/// declaring files and mount targets on exactly the src-membership rule the
/// emission path enforces.
///
/// Binary targets (`src/main.rs`, `src/bin/*.rs`) ARE in scope — the
/// distinct-namespace routing that keeps them from colliding with the library
/// happens in the module-path derivation ([`scoped_module_path`]), not here, so
/// that a `#[path]` mount declared inside a binary target is still discovered.
#[must_use]
pub fn crate_src_scope(roots: &CrateRoots, file: &Path) -> Option<(String, PathBuf)> {
    let crate_name = roots.crate_name_for(file)?;
    let src_root = roots.crate_dir_for(file)?.join("src");
    if !file.starts_with(&src_root) {
        return None; // tests/ benches/ examples/ build.rs
    }
    Some((crate_name, src_root))
}

/// The dotted module path of an in-scope `file`: binary targets route to their
/// own `<crate>@bin(<name>)` root (see the module docs); every other file
/// routes through the `#[path]`-mount-aware library derivation. Shared between
/// [`emittable_scope`] and [`mounts::ModMounts::fs_logical`](crate::mounts) so
/// the binary-target rewrite is identical on both the emission path and the
/// mount-fallback path.
#[must_use]
pub fn scoped_module_path(
    crate_name: &str,
    src_root: &Path,
    file: &Path,
    mounts: &ModMounts,
) -> String {
    if let Some(bin_path) = binary_target_module_path(crate_name, src_root, file) {
        return bin_path;
    }
    logical_module_path(crate_name, src_root, file, mounts)
}

/// [`scoped_module_path`] without the `#[path]` overlay — the pure-filesystem
/// route, binary-target-aware. Used by mount discovery's filesystem-default arm
/// ([`mounts::MountResolver::fs_logical`](crate::mounts)), where the overlay is
/// still being built and does not apply; a bin target short-circuits to its
/// `<crate>@bin(<name>)` root, every other file routes through
/// [`module_path_for`].
#[must_use]
pub fn fs_module_path(crate_name: &str, src_root: &Path, file: &Path) -> String {
    if let Some(bin_path) = binary_target_module_path(crate_name, src_root, file) {
        return bin_path;
    }
    module_path_for(crate_name, src_root, file)
}

/// The `<crate>@bin(<name>)`-rooted dotted module path when `file` is a Cargo
/// binary target that must NOT share the library namespace, else `None`:
///
/// * `src/main.rs` AND a sibling `src/lib.rs` exists → the lib+bin shape.
///   `main.rs` is the bin root; the bare target name is the crate name.
/// * `src/bin/<name>.rs` → bin target `<name>`, the file IS the root.
/// * `src/bin/<name>/…` → bin target `<name>`, deeper files are submodules
///   under the bin root (`main.rs`/`mod.rs` stems collapse, as in the lib
///   derivation).
///
/// A pure binary crate's lone `src/main.rs` (no sibling `lib.rs`) is NOT a
/// binary target here — it is the crate's canonical root and keeps the bare
/// `<crate>` path — so this returns `None` for it.
fn binary_target_module_path(crate_name: &str, src_root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(src_root).ok()?;
    let comps: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    // src/main.rs shadowing a sibling src/lib.rs: the bin target is the crate.
    if comps == ["main.rs"] && src_root.join("lib.rs").is_file() {
        return Some(format!("{crate_name}@bin({crate_name})"));
    }

    // src/bin/<name>.rs — a single-file Cargo binary target. The file IS its
    // own root; it carries no submodule remainder, so it maps straight to the
    // bin-target root.
    if comps.len() == 2 && comps[0] == "bin" {
        let target = Path::new(&comps[1])
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        return Some(format!("{crate_name}@bin({target})"));
    }

    // src/bin/<name>/… — a multi-file Cargo binary target. The dir <name> is the
    // bin root; deeper files are its submodules. Reuse the library dotted-path
    // derivation, rooted at `<crate>@bin(<name>)` and the target's own dir, so a
    // `<root_dir>`-relative route collapses `main.rs`/`mod.rs`/`lib.rs` stems
    // exactly as the lib namespace does.
    if comps.len() >= 3 && comps[0] == "bin" {
        let bin_root = format!("{crate_name}@bin({target})", target = comps[1]);
        let root_dir = src_root.join("bin").join(&comps[1]);
        return Some(module_path_for(&bin_root, &root_dir, file));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_src_and_separate_units_are_not_emittable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("c/src")).unwrap();
        std::fs::create_dir_all(root.join("c/tests")).unwrap();
        std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        std::fs::write(root.join("c/src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(root.join("c/tests/it.rs"), "fn h() {}\n").unwrap();
        std::fs::write(root.join("c/build.rs"), "fn main() {}\n").unwrap();
        let roots = crate::crate_roots::discover_crate_roots(root);
        let mounts = ModMounts::empty();

        assert!(emittable_scope(&roots, &root.join("c/src/lib.rs"), &mounts).is_some());
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/lib.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate".to_owned())
        );
        assert!(
            emittable_scope(&roots, &root.join("c/tests/it.rs"), &mounts).is_none(),
            "integration test"
        );
        assert!(
            emittable_scope(&roots, &root.join("c/build.rs"), &mounts).is_none(),
            "build script"
        );
    }

    #[test]
    fn lib_plus_bin_main_routes_to_a_distinct_bin_root() {
        // FINDING A: a normal lib+bin crate. `src/main.rs` used to be dropped
        // (returned `None`, "redundant"), silently losing the binary
        // entrypoint's code from the graph. It must now be emittable, under a
        // bin-target root distinct from the library's bare-crate path.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("c/src")).unwrap();
        std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        std::fs::write(root.join("c/src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(root.join("c/src/main.rs"), "fn main() {}\n").unwrap();
        let roots = crate::crate_roots::discover_crate_roots(root);
        let mounts = ModMounts::empty();

        // lib.rs keeps the bare-crate path.
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/lib.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate".to_owned())
        );
        // main.rs is NO LONGER dropped — it routes to its own bin-target root.
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/main.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate@bin(c_crate)".to_owned()),
            "lib+bin main.rs must be emittable under a distinct bin root, not dropped"
        );
    }

    #[test]
    fn pure_binary_crate_keeps_the_bare_crate_root() {
        // A crate with only `src/main.rs` (no lib): `main.rs` IS the canonical
        // crate root and keeps the bare `<crate>` path — it is not a separate
        // binary TARGET sharing a library's namespace, so no `@bin(...)`.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("c/src")).unwrap();
        std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        std::fs::write(root.join("c/src/main.rs"), "fn main() {}\n").unwrap();
        let roots = crate::crate_roots::discover_crate_roots(root);
        let mounts = ModMounts::empty();

        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/main.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate".to_owned())
        );
    }

    #[test]
    fn src_bin_targets_get_their_own_root_not_the_library_bin_module() {
        // FINDING B: Cargo automatic binary targets under `src/bin/`. A
        // `src/bin/tool.rs` used to route by filesystem to `<crate>.bin.tool`,
        // mixing it into the library namespace and colliding with a real
        // `mod bin`. It must route to `<crate>@bin(tool)` instead, and a
        // multi-file bin target (`src/bin/<name>/...`) routes its submodules
        // under that bin root.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("c/src/bin/multi")).unwrap();
        std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        std::fs::write(
            root.join("c/src/lib.rs"),
            "pub mod bin { pub fn real() {} }\n",
        )
        .unwrap();
        std::fs::write(root.join("c/src/bin/tool.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("c/src/bin/multi/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("c/src/bin/multi/helper.rs"), "fn h() {}\n").unwrap();
        let roots = crate::crate_roots::discover_crate_roots(root);
        let mounts = ModMounts::empty();

        // single-file bin target
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/bin/tool.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate@bin(tool)".to_owned()),
            "src/bin/tool.rs must route to its own bin-target root"
        );
        // multi-file bin target: root file...
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/bin/multi/main.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate@bin(multi)".to_owned()),
            "src/bin/<name>/main.rs is the bin-target root"
        );
        // ...and a submodule under it.
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/bin/multi/helper.rs"), &mounts).unwrap(),
            ("c_crate".to_owned(), "c_crate@bin(multi).helper".to_owned()),
            "src/bin/<name>/helper.rs is a submodule of the bin target"
        );
        // the REAL `mod bin` in the library keeps the dotted library path —
        // distinct from every `@bin(...)` target above, so no collision.
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/lib.rs"), &mounts)
                .unwrap()
                .1,
            "c_crate",
        );
    }
}
