//! Path-jail enforcement for the Clarion plugin host.
//!
//! Implements ADR-021 §2a: every file path that a plugin names — whether in a
//! request parameter or in a returned entity — must lie *inside* the project
//! root. The jail function resolves both paths via
//! [`std::fs::canonicalize`] (which follows symlinks per UQ-WP2-03) and
//! asserts a `starts_with` relationship.
//!
//! # Policy
//!
//! When `jail` returns `JailError::EscapedRoot`, the *caller* decides the
//! response. ADR-021 §2a specifies "drop-entity, not kill-plugin" on a first
//! offence; the [`PathEscapeBreaker`](super::limits::PathEscapeBreaker) in
//! `limits.rs` accumulates escape events and trips to "kill-plugin" after more
//! than 10 escapes in 60 seconds. Task 6 (the plugin supervisor) wires these
//! two pieces together.
//!
//! # Wire boundary
//!
//! JSON-RPC frames carry paths as UTF-8 strings. Use [`jail_to_string`] at
//! the wire boundary; it calls [`jail`] and then converts the canonical
//! [`PathBuf`] to `String`, returning [`JailError::NonUtf8Path`] if the
//! canonicalized path is not valid UTF-8.

use std::path::{Path, PathBuf};

use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by [`jail`] and [`jail_to_string`].
#[derive(Debug, Error)]
pub enum JailError {
    /// The candidate path resolves outside the root.
    ///
    /// ADR-021 §2a — the supervisor must record this escape against the plugin's
    /// [`PathEscapeBreaker`](super::limits::PathEscapeBreaker) tally before
    /// deciding whether to drop the entity or kill the plugin.
    #[error("path escape: {offending:?} resolves outside the jail root")]
    EscapedRoot { offending: PathBuf },

    /// [`std::fs::canonicalize`] failed — the candidate or root does not exist,
    /// or a permission error occurred.
    #[error("jail canonicalize error: {0}")]
    Io(#[from] std::io::Error),

    /// The canonicalized path is not valid UTF-8 (returned only by
    /// [`jail_to_string`], never by [`jail`]).
    #[error("path is not valid UTF-8: {offending:?}")]
    NonUtf8Path { offending: PathBuf },
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Assert that `candidate` is inside `root` after symlink resolution.
///
/// Both paths are canonicalized via [`std::fs::canonicalize`] before
/// comparison (UQ-WP2-03: symlinks must be followed so that a symlink inside
/// the root pointing outside is caught, not tolerated).
///
/// # Returns
///
/// - `Ok(canonical_candidate)` — the resolved, confirmed-safe path.
/// - `Err(JailError::EscapedRoot)` — `candidate` resolves outside `root`.
/// - `Err(JailError::Io)` — either path does not exist or cannot be resolved.
///
/// The returned path is a membership proof at canonicalization time, not a
/// durable file handle. Code that later opens or serves the path must account
/// for filesystem races between this check and the later open. Use an
/// `openat`-style strategy anchored to a pinned root descriptor, or re-run the
/// jail check after opening and compare against the canonical path accepted
/// here.
pub fn jail(root: &Path, candidate: &Path) -> Result<PathBuf, JailError> {
    let canonical_root = std::fs::canonicalize(root)?;
    let canonical_candidate = std::fs::canonicalize(candidate)?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(JailError::EscapedRoot {
            offending: canonical_candidate,
        });
    }

    Ok(canonical_candidate)
}

/// Open a candidate file safely, mitigating TOCTOU symlink swap hazards by
/// verifying that the opened file's metadata matches the jail-checked canonical path.
pub fn safe_open(root: &Path, candidate: &Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::File::open(candidate)?;
    let canonical_root = std::fs::canonicalize(root)?;
    let canonical_candidate = std::fs::canonicalize(candidate)?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Path escape: {} resolves outside jail root",
                canonical_candidate.display()
            ),
        ));
    }

    // TOCTOU mitigation: verify the open file handle matches the canonical path
    let meta_file = file.metadata()?;
    let meta_canonical = std::fs::metadata(&canonical_candidate)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta_file.dev() != meta_canonical.dev() || meta_file.ino() != meta_canonical.ino() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "TOCTOU validation failure: device or inode mismatch",
            ));
        }
    }

    #[cfg(not(unix))]
    {
        // Best-effort fallback for non-Unix targets
        if meta_file.len() != meta_canonical.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "TOCTOU validation failure: file length mismatch",
            ));
        }
    }

    Ok(file)
}

/// Assert that `candidate` is inside `root` and return the canonical path as
/// a UTF-8 `String`.
///
/// Calls [`jail`] then converts via `PathBuf::into_os_string().into_string()`.
/// Returns [`JailError::NonUtf8Path`] if the canonical path contains non-UTF-8
/// bytes (platform-specific; possible on Linux where filenames are arbitrary
/// byte sequences).
///
/// This is the form Task 6 uses at the JSON-RPC wire boundary.
pub fn jail_to_string(root: &Path, candidate: &Path) -> Result<String, JailError> {
    let canonical = jail(root, candidate)?;
    canonical
        .into_os_string()
        .into_string()
        .map_err(|os_str| JailError::NonUtf8Path {
            offending: PathBuf::from(os_str),
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    // ── Helper: build a real file inside a TempDir ────────────────────────────

    fn make_file(dir: &TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, b"").expect("create test file");
        path
    }

    // ── jail_01: path inside root is admitted ─────────────────────────────────

    /// A path that genuinely resides inside the root is admitted and the
    /// canonical path is returned.
    #[test]
    fn jail_01_inside_root_is_admitted() {
        let root = TempDir::new().expect("tmpdir");
        let candidate = make_file(&root, "src.py");

        let result = jail(root.path(), &candidate).expect("must succeed");
        assert!(
            result.starts_with(root.path().canonicalize().unwrap()),
            "canonical path must start with canonical root"
        );
    }

    // ── jail_02: `..`-based escape is rejected with EscapedRoot ──────────────

    /// A path constructed with `..` that resolves *above* the root must be
    /// rejected. `std::fs::canonicalize` resolves the `..` before the
    /// `starts_with` check, so lexical parent segments cannot bypass the jail.
    #[test]
    fn jail_02_dotdot_escape_returns_escaped_root() {
        let root = TempDir::new().expect("tmpdir");
        // Create a subdir inside root so the `..`-path is resolv-able.
        let subdir = root.path().join("sub");
        std::fs::create_dir(&subdir).expect("mkdir sub");
        // Create a sibling TempDir outside the root. Both live under the same
        // OS temp directory (e.g. /tmp), so we can reach `outside_file` by
        // going `subdir/../../<outside_dir_name>/secret.py`.
        let outside_root = TempDir::new().expect("outside tmpdir");
        // Create the file so canonicalize can resolve it; we navigate to it by
        // dir-name + filename rather than storing the PathBuf return value.
        make_file(&outside_root, "secret.py");

        // `subdir` is `<root>/sub`. One `..` reaches `<root>`; a second `..`
        // reaches `<root>`'s parent (the OS temp dir). From there we use only
        // the dir-name of `outside_root` + the hardcoded filename so the path
        // stays within the temp directory tree.
        let outside_dir_name = outside_root
            .path()
            .file_name()
            .expect("outside TempDir must have a file name");
        let escape = subdir
            .join("../..")
            .join(outside_dir_name)
            .join("secret.py");

        assert!(
            escape.exists(),
            "escape path must exist — both TempDirs should live under the same parent"
        );
        let err = jail(root.path(), &escape).expect_err("must reject escape");
        assert!(
            matches!(err, JailError::EscapedRoot { .. }),
            "expected EscapedRoot, got: {err:?}"
        );
    }

    // ── jail_03: symlink inside root pointing outside is rejected ─────────────

    /// A symlink that physically lives inside the root but *targets* a path
    /// outside the root must be rejected. This is the UQ-WP2-03 resolution:
    /// `canonicalize` follows symlinks, so the resolved path escapes.
    #[cfg(unix)]
    #[test]
    fn jail_03_symlink_inside_root_pointing_outside_is_rejected() {
        let root = TempDir::new().expect("root tmpdir");
        let outside = TempDir::new().expect("outside tmpdir");
        let outside_file = make_file(&outside, "outside.py");

        // Create a symlink inside the root whose target is the outside file.
        let link_path = root.path().join("link.py");
        std::os::unix::fs::symlink(&outside_file, &link_path).expect("create symlink");

        let err = jail(root.path(), &link_path).expect_err("symlink escape must be rejected");
        assert!(
            matches!(err, JailError::EscapedRoot { .. }),
            "expected EscapedRoot for symlink escape, got: {err:?}"
        );
    }

    // ── jail_04: non-existent candidate is rejected with Io ──────────────────

    /// A path that does not exist on the filesystem cannot be canonicalized;
    /// `jail` returns `JailError::Io`.
    #[test]
    fn jail_04_nonexistent_candidate_returns_io_error() {
        let root = TempDir::new().expect("tmpdir");
        let missing = root.path().join("does_not_exist.py");

        let err = jail(root.path(), &missing).expect_err("nonexistent path must error");
        assert!(
            matches!(err, JailError::Io(_)),
            "expected JailError::Io for nonexistent path, got: {err:?}"
        );
    }

    // ── jail_05: non-UTF-8 path is rejected by jail_to_string ────────────────

    /// On Unix, filenames are arbitrary byte sequences. A file whose name is
    /// not valid UTF-8 passes `jail` (returns `Ok(PathBuf)`) but fails
    /// `jail_to_string` with `JailError::NonUtf8Path`.
    #[cfg(unix)]
    #[test]
    fn jail_05_non_utf8_path_rejected_by_jail_to_string() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let root = TempDir::new().expect("tmpdir");

        // Construct a filename byte sequence that is not valid UTF-8.
        // 0xFF 0xFE are invalid UTF-8 start bytes.
        let bad_bytes: &[u8] = &[0xff, 0xfe, b'.', b'p', b'y'];
        let bad_name = OsStr::from_bytes(bad_bytes);
        let bad_path = root.path().join(bad_name);

        // Write an actual file with that name so canonicalize can resolve it.
        std::fs::write(&bad_path, b"").expect("create non-UTF-8 file");

        // `jail` itself should succeed — it returns a PathBuf.
        let canonical =
            jail(root.path(), &bad_path).expect("jail must succeed for valid (non-UTF-8) path");
        // The canonical path must be non-UTF-8 for this test to be meaningful.
        assert!(
            canonical.to_str().is_none(),
            "canonical path should be non-UTF-8; if this fails, the OS normalised the name"
        );

        // `jail_to_string` must fail with NonUtf8Path.
        let err = jail_to_string(root.path(), &bad_path)
            .expect_err("jail_to_string must fail for non-UTF-8 path");
        assert!(
            matches!(err, JailError::NonUtf8Path { .. }),
            "expected NonUtf8Path, got: {err:?}"
        );
    }
}
