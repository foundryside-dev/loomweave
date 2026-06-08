//! Crate-root discovery: map each `.rs` file to its crate name by reading
//! `Cargo.toml [package].name` as TEXT (never `cargo metadata`). ADR-049 §1.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Crate roots discovered under a project root: a map from each crate's source
/// root directory to its (underscored) crate name, longest-prefix matched.
pub struct CrateRoots {
    /// Sorted by path so longest-prefix lookup is deterministic.
    roots: Vec<(PathBuf, String)>,
}

impl CrateRoots {
    /// The crate name owning `file`, by longest directory-prefix match.
    #[must_use]
    pub fn crate_name_for(&self, file: &Path) -> Option<String> {
        self.roots
            .iter()
            .filter(|(dir, _)| file.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_os_str().len())
            .map(|(_, name)| name.clone())
    }

    /// The crate root directory owning `file` (the dir holding `Cargo.toml` /
    /// `src/`), by the same longest directory-prefix match as
    /// [`Self::crate_name_for`]. Join `src` onto this to get the crate's source
    /// root for [`crate::module_path::module_path_for`].
    #[must_use]
    pub fn crate_dir_for(&self, file: &Path) -> Option<PathBuf> {
        self.roots
            .iter()
            .filter(|(dir, _)| file.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_os_str().len())
            .map(|(dir, _)| dir.clone())
    }
}

/// Underscore a crate name the way Rust does (`a-b` → `a_b`).
fn normalise(name: &str) -> String {
    name.replace('-', "_")
}

/// Walk `project_root` and discover every crate's source-root directory and
/// its (underscored) crate name. Reads each `Cargo.toml [package].name` as
/// text; falls back to the directory name when a manifest lacks a package name.
#[must_use]
pub fn discover_crate_roots(project_root: &Path) -> CrateRoots {
    let mut roots: BTreeMap<PathBuf, String> = BTreeMap::new();
    visit(project_root, &mut roots);
    CrateRoots {
        roots: roots.into_iter().collect(),
    }
}

fn visit(dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cargo = dir.join("Cargo.toml");
    if cargo.is_file()
        && let Ok(text) = std::fs::read_to_string(&cargo)
        && let Ok(value) = text.parse::<toml::Value>()
        && let Some(name) = value
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
    {
        out.insert(dir.to_path_buf(), normalise(name));
    } else if (dir.join("src/lib.rs").is_file() || dir.join("src/main.rs").is_file())
        && let Some(base) = dir.file_name().and_then(|n| n.to_str())
    {
        out.entry(dir.to_path_buf())
            .or_insert_with(|| normalise(base));
    }
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !is_ignored(&path) {
            visit(&path, out);
        }
    }
}

/// Skip vendored / build / store directories the host also skips.
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
    fn maps_each_crate_dir_to_its_package_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // crate A
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"loomweave_core\"\n",
        )
        .unwrap();
        fs::write(root.join("crates/a/src/lib.rs"), "").unwrap();
        // crate B (hyphenated name normalises to underscores)
        fs::create_dir_all(root.join("crates/b/src")).unwrap();
        fs::write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname = \"loomweave-cli\"\n",
        )
        .unwrap();
        fs::write(root.join("crates/b/src/main.rs"), "").unwrap();

        let roots = discover_crate_roots(root);
        assert_eq!(
            roots.crate_name_for(&root.join("crates/a/src/lib.rs")),
            Some("loomweave_core".to_owned())
        );
        assert_eq!(
            roots.crate_name_for(&root.join("crates/b/src/main.rs")),
            Some("loomweave_cli".to_owned())
        );
    }

    #[test]
    fn falls_back_to_dir_holding_lib_or_main_when_no_package_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap(); // no Cargo.toml [package]
        let roots = discover_crate_roots(root);
        // directory name underscored
        let name = roots.crate_name_for(&root.join("src/lib.rs"));
        assert!(name.is_some());
    }
}
