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

/// The secret-scan file set plus its coverage signal.
///
/// `sidecar_walk_skipped` counts `.env`/`*.env` sidecar-walk entries that could
/// not be read (IO / permission / path error). It is **load-bearing for the
/// stale-finding sweep gate**: a skipped sidecar means the secret scan did not
/// examine that path, so the caller must NOT run the unbounded global stale
/// sweep (it would retire a still-open secret finding for an *unexamined* file —
/// "never looked" ≠ "looked, fixed"). The source-tree walk has its own skip
/// counter (`source_walk_skipped_entries`); this is the sidecar-walk twin, which
/// would otherwise be invisible to that gate (clarion / secret-scan coverage).
pub(crate) struct ScanFileWalk {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) sidecar_walk_skipped: u64,
}

pub(crate) fn collect_scan_files(root: &Path, source_files: &[PathBuf]) -> ScanFileWalk {
    let mut out: BTreeSet<PathBuf> = source_files
        .iter()
        .map(|path| canonical_or_original(path))
        .collect();
    let sidecars = collect_secret_scan_sidecars(root);
    for path in sidecars.files {
        out.insert(canonical_or_original(&path));
    }
    ScanFileWalk {
        files: out.into_iter().collect(),
        sidecar_walk_skipped: sidecars.sidecar_walk_skipped,
    }
}

/// Internal result of the sidecar-only walk: the discovered sidecar files plus
/// the count of unreadable entries skipped during the walk.
struct SidecarWalk {
    files: Vec<PathBuf>,
    sidecar_walk_skipped: u64,
}

fn collect_secret_scan_sidecars(root: &Path) -> SidecarWalk {
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
    SidecarWalk {
        files: out,
        sidecar_walk_skipped: skipped,
    }
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

        let walk = collect_scan_files(root, &[root.join("src/app.py")]);
        assert_eq!(walk.sidecar_walk_skipped, 0, "no unreadable entries");
        let rel = relative_names(root, walk.files);

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

        let walk = collect_scan_files(&root, &[]);
        let rel = relative_names(&root, walk.files);

        assert!(!rel.contains(&"linked/.env".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_walk_surfaces_skip_count_on_unreadable_subdir() {
        // An unreadable directory makes the walker yield an Err entry: the walk
        // must SURFACE that as `sidecar_walk_skipped > 0` (not just log it), so
        // the analyze stale-finding gate can suppress the unbounded global sweep
        // for the unexamined paths underneath ("never looked" ≠ "looked, clean").
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        write(root.join(".env"), "TOKEN=readable\n");
        let locked = root.join("locked");
        std::fs::create_dir(&locked).expect("create locked dir");
        write(locked.join(".env"), "TOKEN=unreadable\n");
        // Drop all permissions so the walker cannot descend into `locked/`.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("chmod 000");

        // root bypasses permission bits, so the chmod-0 trick only blocks a
        // non-root reader. Probe directly: if we can still enumerate the dir
        // (running as root, or an exotic FS), the skip cannot trigger — skip the
        // assertion rather than assert a precondition the platform won't honor.
        let still_readable = std::fs::read_dir(&locked).is_ok();

        let walk = collect_scan_files(root, &[]);
        // Restore perms so the tempdir can be cleaned up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .expect("restore perms");

        if still_readable {
            return; // permission bits not enforced for this reader; nothing to assert.
        }
        assert!(
            walk.sidecar_walk_skipped > 0,
            "an unreadable subdir must surface as a non-zero skip count, not be swallowed"
        );
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
