//! File-level module-path resolution (Task 2). ADR-049 §1: dotted, crate-rooted.
//!
//! Two entry points since Amendment 8: [`module_path_for`] is the pure
//! filesystem route (UNCHANGED — the corpus `module_route` rows pin it), and
//! [`logical_module_path`] is the `#[path]`-mount-aware route every emission
//! path uses: mount overlay first, filesystem default second.
use std::path::Path;

use crate::mounts::ModMounts;

/// The `#[path]`-mount-aware dotted module path of `file` (ADR-049
/// Amendment 8): exact-file mount lookup, then the longest mounted-subtree
/// prefix (remaining components rewritten, a trailing `mod` stem collapsed),
/// else byte-identical delegation to [`module_path_for`]. A project with no
/// mounts (`ModMounts::empty()`) routes exactly as before Amendment 8.
#[must_use]
pub fn logical_module_path(
    crate_name: &str,
    src_root: &Path,
    file: &Path,
    mounts: &ModMounts,
) -> String {
    mounts
        .logical_path_for(file)
        .unwrap_or_else(|| module_path_for(crate_name, src_root, file))
}

/// Dotted module path from `crate_name` to the module defined by `file`,
/// where `src_root` is the crate's source root (the dir holding lib.rs/main.rs).
#[must_use]
pub fn module_path_for(crate_name: &str, src_root: &Path, file: &Path) -> String {
    let Ok(rel) = file.strip_prefix(src_root) else {
        return crate_name.to_owned();
    };
    let mut segs: Vec<String> = Vec::new();
    let comps: Vec<_> = rel.components().collect();
    for (i, comp) in comps.iter().enumerate() {
        let part = comp.as_os_str().to_string_lossy();
        let is_last = i == comps.len() - 1;
        if is_last {
            let stem = Path::new(part.as_ref())
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            // lib.rs / main.rs / mod.rs contribute no segment
            if stem != "lib" && stem != "main" && stem != "mod" {
                segs.push(stem);
            }
        } else {
            segs.push(part.into_owned());
        }
    }
    std::iter::once(crate_name.to_owned())
        .chain(segs)
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn crate_root_files_map_to_the_crate_name() {
        assert_eq!(
            module_path_for(
                "loomweave_core",
                Path::new("/p/crates/c/src"),
                Path::new("/p/crates/c/src/lib.rs")
            ),
            "loomweave_core"
        );
        assert_eq!(
            module_path_for(
                "loomweave_core",
                Path::new("/p/crates/c/src"),
                Path::new("/p/crates/c/src/main.rs")
            ),
            "loomweave_core"
        );
    }

    #[test]
    fn overlay_miss_is_byte_identical_to_module_path_for() {
        // With no mount covering a file, `logical_module_path` MUST delegate
        // byte-for-byte to the unchanged filesystem route — the corpus
        // `module_route` rows keep passing through the new entry point.
        let mounts = crate::mounts::ModMounts::empty();
        for (krate, src_root, file) in [
            (
                "loomweave_core",
                "/p/crates/c/src",
                "/p/crates/c/src/lib.rs",
            ),
            (
                "loomweave_core",
                "/p/crates/c/src",
                "/p/crates/c/src/main.rs",
            ),
            ("k", "/p/src", "/p/src/config.rs"),
            ("k", "/p/src", "/p/src/plugin/host.rs"),
            ("k", "/p/src", "/p/src/plugin/mod.rs"),
            ("demo", "/p/src", "/p/src/renamed.rs"),
        ] {
            assert_eq!(
                logical_module_path(krate, Path::new(src_root), Path::new(file), &mounts),
                module_path_for(krate, Path::new(src_root), Path::new(file)),
                "overlay miss must delegate unchanged for {file}",
            );
        }
    }

    #[test]
    fn overlay_hit_covers_exact_prefix_and_trailing_mod_collapse() {
        // Built through the real discovery (the overlay's fields are private
        // by design): one mod.rs mount exercises the exact hit, a child-file
        // prefix rewrite, and the trailing-`mod` collapse in a rewritten
        // remainder.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"k\"\n").unwrap();
        std::fs::create_dir_all(root.join("src/eng/sub")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "#[path = \"eng/mod.rs\"]\nmod engine;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/eng/mod.rs"),
            "pub mod worker;\npub mod sub;\n",
        )
        .unwrap();
        std::fs::write(root.join("src/eng/worker.rs"), "pub fn w() {}\n").unwrap();
        std::fs::write(root.join("src/eng/sub/mod.rs"), "pub fn d() {}\n").unwrap();
        let roots = crate::crate_roots::discover_crate_roots(root);
        let mounts = crate::mounts::discover_mounts(root, &roots);
        let src = root.join("src");
        assert_eq!(
            logical_module_path("k", &src, &root.join("src/eng/mod.rs"), &mounts),
            "k.engine",
            "exact-file overlay hit"
        );
        assert_eq!(
            logical_module_path("k", &src, &root.join("src/eng/worker.rs"), &mounts),
            "k.engine.worker",
            "dir-prefix rewrite"
        );
        assert_eq!(
            logical_module_path("k", &src, &root.join("src/eng/sub/mod.rs"), &mounts),
            "k.engine.sub",
            "trailing `mod` stem collapses in the rewritten remainder"
        );
    }

    #[test]
    fn nested_files_and_mod_rs_dot_join() {
        assert_eq!(
            module_path_for("k", Path::new("/p/src"), Path::new("/p/src/config.rs")),
            "k.config"
        );
        assert_eq!(
            module_path_for("k", Path::new("/p/src"), Path::new("/p/src/plugin/host.rs")),
            "k.plugin.host"
        );
        assert_eq!(
            module_path_for("k", Path::new("/p/src"), Path::new("/p/src/plugin/mod.rs")),
            "k.plugin"
        );
    }
}
