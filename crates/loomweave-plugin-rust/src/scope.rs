//! Shared crate-scope discipline (formerly inline in `symbol_table.rs`).
//! A file is *emittable* only when it is part of the library/binary crate the
//! ADR-049 qualname scheme names: under the crate's `src/` tree and not a
//! redundant `main.rs` shadowing a sibling `lib.rs`. `tests/`, `benches/`,
//! `examples/`, and `build.rs` are SEPARATE compilation units — folding them in
//! would mint colliding `rust:module:<crate>` locators (each one's bare-crate
//! fallback), a data-loss / `FailRun` at the storage writer.
use std::path::Path;

use crate::crate_roots::CrateRoots;
use crate::module_path::module_path_for;

/// `(crate_name, dotted_module_path)` for an emittable file, else `None`.
///
/// Mirrors the guard sequence formerly inline in
/// [`build_symbol_table`](crate::symbol_table::build_symbol_table): resolve the
/// owning crate name and source root, require the file to live under that
/// `src/` tree, skip a redundant `main.rs` shadowing a sibling `lib.rs`, then
/// derive the dotted module path.
#[must_use]
pub fn emittable_scope(roots: &CrateRoots, file: &Path) -> Option<(String, String)> {
    let crate_name = roots.crate_name_for(file)?;
    let src_root = roots.crate_dir_for(file)?.join("src");
    if !file.starts_with(&src_root) {
        return None; // tests/ benches/ examples/ build.rs
    }
    if file == src_root.join("main.rs") && src_root.join("lib.rs").is_file() {
        return None; // redundant binary root; lib.rs is canonical
    }
    let module_path = module_path_for(&crate_name, &src_root, file);
    Some((crate_name, module_path))
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

        assert!(emittable_scope(&roots, &root.join("c/src/lib.rs")).is_some());
        assert_eq!(
            emittable_scope(&roots, &root.join("c/src/lib.rs")).unwrap(),
            ("c_crate".to_owned(), "c_crate".to_owned())
        );
        assert!(
            emittable_scope(&roots, &root.join("c/src/main.rs")).is_none(),
            "redundant main"
        );
        assert!(
            emittable_scope(&roots, &root.join("c/tests/it.rs")).is_none(),
            "integration test"
        );
        assert!(
            emittable_scope(&roots, &root.join("c/build.rs")).is_none(),
            "build script"
        );
    }
}
