use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ignore::{DirEntry, WalkBuilder};

use super::canonical_or_original;

const SKIP_DIRS: &[&str] = &[
    ".clarion",
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".venv",
    "__pycache__",
    "node_modules",
];

pub(crate) fn collect_scan_files(root: &Path, source_files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: BTreeSet<PathBuf> = source_files
        .iter()
        .map(|path| canonical_or_original(path))
        .collect();
    for path in collect_secret_scan_sidecars(root) {
        out.insert(canonical_or_original(&path));
    }
    out.into_iter().collect()
}

fn collect_secret_scan_sidecars(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .require_git(false)
        .filter_entry(|entry| !is_skipped_dir(entry));

    for entry in builder.build().filter_map(std::result::Result::ok) {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.into_path();
        if file_type.is_file() && is_secret_scan_sidecar(&path) {
            out.push(path);
        }
    }
    out
}

fn is_secret_scan_sidecar(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str());
    file_name.is_some_and(|name| name == ".env" || name.starts_with(".env."))
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("env"))
}

fn is_skipped_dir(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .is_some_and(|file_type| file_type.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SKIP_DIRS.contains(&name))
}
