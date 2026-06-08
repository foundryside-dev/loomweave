//! File-level module-path resolution (Task 2). ADR-049 §1: dotted, crate-rooted.
use std::path::Path;

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
