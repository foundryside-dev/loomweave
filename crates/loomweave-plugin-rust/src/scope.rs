//! Shared crate-scope discipline (formerly inline in `symbol_table.rs`).
//! A file is *emittable* only when it is part of the library/binary crate the
//! ADR-049 qualname scheme names: under the crate's `src/` tree and not a
//! redundant `main.rs` shadowing a sibling `lib.rs`. `tests/`, `benches/`,
//! `examples/`, and `build.rs` are SEPARATE compilation units — folding them in
//! would mint colliding `rust:module:<crate>` locators (each one's bare-crate
//! fallback), a data-loss / `FailRun` at the storage writer.
use std::path::{Path, PathBuf};

use crate::crate_roots::CrateRoots;
use crate::module_path::logical_module_path;
use crate::mounts::ModMounts;

/// `(crate_name, dotted_module_path)` for an emittable file, else `None`.
///
/// Mirrors the guard sequence formerly inline in
/// [`build_symbol_table`](crate::symbol_table::build_symbol_table): resolve the
/// owning crate name and source root, require the file to live under that
/// `src/` tree, skip a redundant `main.rs` shadowing a sibling `lib.rs`, then
/// derive the dotted module path — `#[path]`-mount-aware since ADR-049
/// Amendment 8 (`mounts` overlays the filesystem route; an empty overlay is
/// byte-identical to the pre-amendment behaviour).
#[must_use]
pub fn emittable_scope(
    roots: &CrateRoots,
    file: &Path,
    mounts: &ModMounts,
) -> Option<(String, String)> {
    let (crate_name, src_root) = crate_src_scope(roots, file)?;
    let module_path = logical_module_path(&crate_name, &src_root, file, mounts);
    Some((crate_name, module_path))
}

/// The crate-scope gate alone — `(crate_name, src_root)` when `file` belongs
/// to the library/binary crate the ADR-049 qualname scheme names, else `None`
/// (`tests/`, `benches/`, `examples/`, `build.rs`, or a redundant `main.rs`
/// shadowing a sibling `lib.rs`). Split out of [`emittable_scope`] so
/// `#[path]`-mount discovery (which runs BEFORE any module path can be
/// derived) can gate declaring files and mount targets on exactly the scope
/// rule the emission path enforces.
#[must_use]
pub fn crate_src_scope(roots: &CrateRoots, file: &Path) -> Option<(String, PathBuf)> {
    let crate_name = roots.crate_name_for(file)?;
    let src_root = roots.crate_dir_for(file)?.join("src");
    if !file.starts_with(&src_root) {
        return None; // tests/ benches/ examples/ build.rs
    }
    if file == src_root.join("main.rs") && src_root.join("lib.rs").is_file() {
        return None; // redundant binary root; lib.rs is canonical
    }
    Some((crate_name, src_root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_src_and_redundant_main_are_not_emittable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("c/src")).unwrap();
        std::fs::create_dir_all(root.join("c/tests")).unwrap();
        std::fs::write(root.join("c/Cargo.toml"), "[package]\nname=\"c_crate\"\n").unwrap();
        std::fs::write(root.join("c/src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(root.join("c/src/main.rs"), "fn main() {}\n").unwrap(); // redundant (lib exists)
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
            emittable_scope(&roots, &root.join("c/src/main.rs"), &mounts).is_none(),
            "redundant main"
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
}
