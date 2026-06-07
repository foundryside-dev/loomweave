use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ignore::{DirEntry, WalkBuilder};

use super::canonical_or_original;

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
    let mut skipped: u64 = 0;
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

    for result in builder.build() {
        match result {
            Ok(entry) => {
                let Some(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.into_path();
                if file_type.is_file() && is_secret_scan_sidecar(&path) {
                    out.push(path);
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "sidecar walk: skipping unreadable entry; secret-scan coverage is incomplete \
                     for this path",
                );
                skipped += 1;
            }
        }
    }

    if skipped > 0 {
        tracing::warn!(
            skipped = skipped,
            root = %root.display(),
            "sidecar walk skipped {skipped} unreadable entr{suffix}; secret-scan gate is \
             incomplete for those paths",
            suffix = if skipped == 1 { "y" } else { "ies" },
        );
    }
    out
}

pub(super) fn is_secret_scan_sidecar(path: &Path) -> bool {
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

#[cfg(test)]
mod tests {
    use super::collect_scan_files;
    use std::path::{Path, PathBuf};

    #[test]
    fn sidecar_walk_collects_dotenv_variants_and_skips_known_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(root.join("src/app.py"), "print('ok')\n");
        write(root.join(".env"), "TOKEN=one\n");
        write(root.join(".env.local"), "TOKEN=two\n");
        write(root.join(".env.production"), "TOKEN=three\n");
        write(root.join("nested/service.env"), "TOKEN=four\n");
        write(root.join("nested/.env"), "TOKEN=five\n");
        write(root.join("nested/not-env.txt"), "TOKEN=six\n");
        write(root.join(".weft/loomweave/.env"), "TOKEN=skip\n");
        write(root.join("node_modules/.env"), "TOKEN=skip\n");

        let files = collect_scan_files(root, &[root.join("src/app.py")]);
        let rel = relative_names(root, files);

        assert!(rel.contains(&".env".to_owned()));
        assert!(rel.contains(&".env.local".to_owned()));
        assert!(rel.contains(&".env.production".to_owned()));
        assert!(rel.contains(&"nested/service.env".to_owned()));
        assert!(rel.contains(&"nested/.env".to_owned()));
        assert!(rel.contains(&"src/app.py".to_owned()));
        assert!(!rel.contains(&"nested/not-env.txt".to_owned()));
        assert!(!rel.contains(&".weft/loomweave/.env".to_owned()));
        assert!(!rel.contains(&"node_modules/.env".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_walk_does_not_follow_directory_symlinks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&root).expect("create root");
        std::fs::create_dir(&outside).expect("create outside");
        write(outside.join(".env"), "TOKEN=outside\n");
        std::os::unix::fs::symlink(&outside, root.join("linked")).expect("symlink");

        let files = collect_scan_files(&root, &[]);
        let rel = relative_names(&root, files);

        assert!(!rel.contains(&"linked/.env".to_owned()));
    }

    fn write(path: PathBuf, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write fixture");
    }

    fn relative_names(root: &Path, files: Vec<PathBuf>) -> Vec<String> {
        let root = root.canonicalize().expect("canonical root");
        files
            .into_iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .expect("scan path under root")
                    .display()
                    .to_string()
            })
            .collect()
    }
}
