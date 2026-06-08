//! Init-time project symbol table (Task 7, spec §2.3).
//!
//! At `initialize`, walk `project_root`, one `syn` parse per `.rs` file, and
//! build a map from every declared entity qualname → its id. Phase 1a does not
//! yet resolve cross-file edges (Phase 1b does), but the table is built and
//! proven now so 1b can resolve against it; building it here also lets the
//! dogfood gate (Task 14) assert global uniqueness.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::crate_roots::discover_crate_roots;
use crate::extract::extract_file;
use crate::module_path::module_path_for;

/// A project-wide map from every declared entity id to its qualified name,
/// plus the ids that were seen more than once during the init-time walk.
pub struct SymbolTable {
    /// entity id -> qualified name (the resolution surface for Phase 1b edges).
    by_id: BTreeMap<String, String>,
    /// ids seen more than once during the walk (must be empty — the gate).
    duplicates: Vec<String>,
}

impl SymbolTable {
    /// Whether an entity with this id was declared anywhere in the project.
    #[must_use]
    pub fn contains_id(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// The ids that collided during the walk (must be empty for a healthy
    /// project — this is what the dogfood gate asserts).
    #[must_use]
    pub fn duplicate_ids(&self) -> Vec<String> {
        self.duplicates.clone()
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
/// project-wide symbol table. Files whose crate root cannot be resolved or
/// whose source fails to parse are skipped (their items contribute nothing;
/// the degraded single-module fallback is Task 9). Collisions are recorded in
/// [`SymbolTable::duplicate_ids`] rather than silently dropped.
#[must_use]
pub fn build_symbol_table(project_root: &Path) -> SymbolTable {
    let roots = discover_crate_roots(project_root);
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    let mut duplicates = Vec::new();
    for file in walk_rs_files(project_root) {
        let Some(crate_name) = roots.crate_name_for(&file) else {
            continue;
        };
        let Some(src_root) = src_root_of(&roots, &file) else {
            continue;
        };
        let module_path = module_path_for(&crate_name, &src_root, &file);
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(entities) = extract_file(&crate_name, &module_path, &file.to_string_lossy(), &src)
        else {
            continue;
        };
        for e in entities {
            let id = e["id"].as_str().unwrap_or_default().to_owned();
            let q = e["qualified_name"].as_str().unwrap_or_default().to_owned();
            if by_id.insert(id.clone(), q).is_some() {
                duplicates.push(id);
            }
        }
    }
    SymbolTable { by_id, duplicates }
}

/// The crate's source root directory (`<crate-dir>/src`) for `file`, or `None`
/// when `file` belongs to no discovered crate.
fn src_root_of(roots: &crate::crate_roots::CrateRoots, file: &Path) -> Option<PathBuf> {
    roots.crate_dir_for(file).map(|dir| dir.join("src"))
}

/// Recursively collect every `.rs` file under `root`, skipping vendored /
/// build / store directories the host also skips.
fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
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
}
