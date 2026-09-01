//! Exclusive, unpredictable staging for atomic file and directory replacement.
//!
//! Every "write a sibling temp, then rename over the destination" site in the
//! CLI used to derive the staging name from the process id
//! (`<name>.tmp-<pid>`). That name is guessable, and the staging path lives in
//! a directory the analyzed repository controls (`.claude/`, the project root,
//! `.weft/loomweave/`, a backup output directory). A repository that commits a
//! symlink at the guessed name turns the staging write into a write-through to
//! wherever the link points (`fs::write` and SQLite both follow symlinks), and
//! a planted regular file can pre-empt the rename. `O_CREAT|O_EXCL` on a
//! random name closes both: the staging file is created by this process or the
//! call fails, and nothing pre-planted is ever opened or renamed over the
//! destination.
//!
//! The helpers keep the staging entry in the destination's own directory so
//! the final rename stays a same-filesystem atomic swap, and they request
//! `0o666` at creation so the kernel applies the caller's umask — the same
//! resulting mode `fs::write` would have produced — instead of `tempfile`'s
//! owner-only default, which would silently make an installed `CLAUDE.md` or
//! `settings.json` unreadable to other users of a shared checkout.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use tempfile::{Builder, NamedTempFile, TempDir};

/// Create an exclusive staging file in `dir` whose name starts with `prefix`.
///
/// # Errors
///
/// Returns an error if `dir` cannot be written.
pub(crate) fn staging_file_in(dir: &Path, prefix: &str) -> Result<NamedTempFile> {
    let mut builder = Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    builder
        .tempfile_in(dir)
        .with_context(|| format!("create staging file in {}", dir.display()))
}

/// Create an exclusive, empty staging directory in `dir` whose name starts
/// with `prefix`.
///
/// # Errors
///
/// Returns an error if `dir` cannot be written.
pub(crate) fn staging_dir_in(dir: &Path, prefix: &str) -> Result<TempDir> {
    Builder::new()
        .prefix(prefix)
        .tempdir_in(dir)
        .with_context(|| format!("create staging directory in {}", dir.display()))
}

/// Atomically replace `dest` with `bytes`: stage exclusively in `dest`'s
/// directory, then rename over it. `prefix` names the staging file so an
/// interrupted run leaves a recognisable sibling.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, or the staging
/// write or the final rename fails. A failed call never leaves a staging
/// sibling behind (`NamedTempFile` unlinks on drop).
pub(crate) fn replace_file(dest: &Path, prefix: &str, bytes: &[u8]) -> Result<()> {
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let mut staging = staging_file_in(dir, prefix)?;
    staging
        .write_all(bytes)
        .with_context(|| format!("write staging {}", staging.path().display()))?;
    staging
        .persist(dest)
        .map_err(|err| err.error)
        .with_context(|| format!("rename staging file -> {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{replace_file, staging_dir_in, staging_file_in};

    #[test]
    fn replace_file_writes_a_regular_file_and_leaves_no_staging_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.txt");
        replace_file(&dest, ".out.txt.tmp-", b"first\n").unwrap();
        replace_file(&dest, ".out.txt.tmp-", b"second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "second\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".out.txt.tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging sibling leaked: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replace_file_never_follows_a_planted_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.txt");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep\n").unwrap();
        // The name a PID-derived scheme would have used, and the destination
        // itself, both pre-planted as links to the victim.
        let planted = dir
            .path()
            .join(format!(".out.txt.tmp-{}", std::process::id()));
        symlink(&victim, &planted).unwrap();
        symlink(&victim, &dest).unwrap();

        replace_file(&dest, ".out.txt.tmp-", b"payload\n").unwrap();

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep\n");
        assert!(
            !std::fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the destination must be replaced by a regular file, not written through"
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "payload\n");
        assert!(
            std::fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_file_honours_umask_like_fs_write() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control");
        std::fs::write(&control, b"x").unwrap();
        let staged = staging_file_in(dir.path(), ".probe-").unwrap();
        let expect = std::fs::metadata(&control).unwrap().permissions().mode() & 0o777;
        let got = staged.as_file().metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(got, expect, "staging mode must match what fs::write yields");
    }

    #[test]
    fn staging_dir_is_created_exclusively_in_the_requested_parent() {
        let dir = tempfile::tempdir().unwrap();
        let staged = staging_dir_in(dir.path(), ".pack.tmp-").unwrap();
        assert_eq!(staged.path().parent(), Some(dir.path()));
        assert!(staged.path().is_dir());
        assert!(
            staged
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".pack.tmp-")
        );
    }
}
