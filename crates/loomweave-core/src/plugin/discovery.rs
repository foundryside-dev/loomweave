//! Plugin discovery via `$PATH` scanning and the running binary's own
//! directory (ADR-021 §L9).
//!
//! Directories are scanned in order: every `$PATH` entry first, then the
//! directory of the running `loomweave` binary (`std::env::current_exe()`'s
//! parent). The exe-dir level makes a PyPI/venv install work — the plugin's
//! console script is co-located in the same `bin/` as `loomweave` but is not on
//! the user's `$PATH`.
//!
//! # Matching rule
//!
//! A file is a Loomweave plugin candidate if its name matches
//! `loomweave-plugin-<suffix>` where `<suffix>` is at least one character
//! consisting solely of `[A-Za-z0-9_-]`.  Names such as `loomweave-plugin-`
//! (empty suffix) or `loomweave-plugin` (no second hyphen) are rejected.
//!
//! Additionally the file must exist, be a regular file, and — on Unix — have
//! at least one executable bit set (`mode & 0o111 != 0`).
//!
//! # Manifest lookup order
//!
//! For an executable at `<dir>/loomweave-plugin-<suffix>`:
//!
//! 1. **Neighbor first**: `<dir>/plugin.toml`.
//! 2. **Install-prefix fallback** (only when `<dir>` has basename `bin`):
//!    `<dir>/../share/loomweave/plugins/<suffix>/plugin.toml`.
//! 3. **Symlink-resolved install-prefix fallback** (only when `<dir>` has
//!    basename `bin` and the executable is a symlink, e.g. pipx layout):
//!    canonicalise the executable, then try
//!    `<canonical-dir>/../share/loomweave/plugins/<suffix>/plugin.toml`.
//!    This catches `pipx install` (which puts a symlink in `~/.local/bin/`
//!    pointing into `~/.local/share/pipx/venvs/<pkg>/bin/`, with the
//!    manifest under that venv's `share/`).
//! 4. None found → [`DiscoveryError::ManifestNotFound`].
//!
//! **Limitation**: when multiple `loomweave-plugin-*` binaries share the same
//! directory (e.g. `/usr/local/bin`), they all resolve to the *same*
//! neighbor `plugin.toml`.  This is a known constraint of the neighbor
//! convention; real installs should use the install-prefix layout so each
//! plugin has its own `share/loomweave/plugins/<suffix>/plugin.toml`.
//!
//! # Deduplication
//!
//! Duplicate `$PATH` directories are skipped.  If the same binary name
//! appears in multiple directories the first occurrence wins (matching
//! POSIX shell / `which` semantics).  Because `$PATH` is scanned before the
//! exe directory, a PATH-installed plugin shadows a same-named sibling
//! co-located next to the binary.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;

use thiserror::Error;

use crate::plugin::{Manifest, ManifestError, parse_manifest};

// ── Public types ──────────────────────────────────────────────────────────────

/// A plugin discovered via a `loomweave-plugin-*` executable on `$PATH`.
#[derive(Debug)]
pub struct DiscoveredPlugin {
    /// Path to the plugin executable **as found during discovery** (on
    /// `$PATH`, or co-located in the running binary's directory).
    ///
    /// Intentionally NOT canonicalised. The neighbour-manifest lookup
    /// joins `plugin.toml` with this path's parent directory;
    /// canonicalising here would follow symlinks (e.g.
    /// `~/bin/loomweave-plugin-python` → `~/.local/pipx/venvs/*/bin/...`)
    /// and the manifest lookup would then miss the neighbour that lives
    /// next to the symlink.
    ///
    /// Deduplication uses a separate canonicalised key
    /// (`seen_dirs` inside `scan_dir`), so the raw-path retained
    /// here does not defeat shadowing.
    ///
    /// If you need the real binary location for an operator message (e.g.
    /// "this plugin's binary is actually at …"), canonicalise at the point
    /// of use; discovery keeps the raw form so downstream consumers can
    /// make the decision.
    pub executable: PathBuf,
    /// Parsed manifest from the plugin's `plugin.toml`.
    pub manifest: Manifest,
    /// Location from which the manifest was loaded (for error messages).
    pub manifest_path: PathBuf,
}

/// Errors produced during plugin discovery.
///
/// Each variant corresponds to a single `loomweave-plugin-*` binary; a
/// failure for one plugin does **not** suppress results for others.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// A `loomweave-plugin-*` binary was found on `$PATH` but no `plugin.toml`
    /// was found at either the neighbor location or the install-prefix
    /// location.
    #[error(
        "no plugin.toml found for {executable} \
         (searched neighbor dir and install-prefix share/)"
    )]
    ManifestNotFound { executable: PathBuf },

    /// The manifest file was found but parse/validation failed.
    #[error("plugin.toml at {path} failed to parse: {source}")]
    ManifestInvalid {
        path: PathBuf,
        #[source]
        source: ManifestError,
    },

    /// An I/O error occurred while reading the manifest file.
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The manifest file exceeded [`MAX_MANIFEST_BYTES`]. Real `plugin.toml`
    /// files are under 2 KiB; anything over 64 KiB is pathological and is
    /// refused before it reaches the TOML parser.
    #[error("plugin.toml at {path} exceeded max size {MAX_MANIFEST_BYTES} bytes")]
    ManifestTooLarge { path: PathBuf },

    /// A `$PATH` directory is world-writable. Any user with write
    /// access could drop a `loomweave-plugin-*` binary into it. Refused
    /// to preserve the ADR-021 "semi-trusted plugin" model — operator
    /// must deliberately install plugins.
    #[error(
        "plugin directory {path} is world-writable and refused for security; \
         install plugins in a 0o755 directory (~/.local/bin, /usr/local/bin)"
    )]
    WorldWritableDir { path: PathBuf },
}

/// Maximum accepted `plugin.toml` size. Real manifests are well under 2 KiB;
/// 64 KiB is a trust-boundary cap, not a style constraint. Discovery refuses
/// anything larger before attempting a TOML parse.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

// ── Public API ────────────────────────────────────────────────────────────────

/// Discover plugins on the user's `$PATH`.
///
/// Reads `$PATH` from the process environment and delegates to
/// [`discover_on_path`].  Returns one `Result` per `loomweave-plugin-*`
/// binary found.
#[cfg(unix)]
pub fn discover() -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    let path_val = std::env::var_os("PATH").unwrap_or_default();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    discover_on_path_and_exe_dir(&path_val, exe_dir.as_deref())
}

#[cfg(not(unix))]
pub fn discover() -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    vec![]
}

/// Discover plugins on the given explicit `PATH` value (useful in tests).
///
/// Parses `path_env` using [`std::env::split_paths`], then scans each
/// directory for `loomweave-plugin-*` executables.  Returns one `Result` per
/// candidate found; a broken plugin does not suppress its siblings.
///
/// **Note**: if two `loomweave-plugin-*` binaries sharing a directory both
/// try to use the neighbor `plugin.toml`, they will resolve to the *same*
/// file.  This is expected behaviour given the neighbor convention; see the
/// module-level docs for the recommended install-prefix layout.
///
/// Primarily useful for testing; production callers should use [`discover`].
#[cfg(unix)]
pub fn discover_on_path(path_env: &OsStr) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    discover_on_path_and_exe_dir(path_env, None)
}

/// Like [`discover_on_path`], but additionally scans `exe_dir` (the directory of
/// the running `loomweave` binary) **after** the `$PATH` entries. `$PATH` entries
/// are scanned first, so a plugin found on `$PATH` shadows a same-named sibling
/// next to the binary (first-match-wins, consistent with PATH shadowing).
///
/// This is the discovery source that makes a PyPI/venv install work: the plugin
/// console script is co-located in the same `bin/` as `loomweave` but is not on
/// the user's `$PATH`. See ADR-021.
#[cfg(unix)]
pub fn discover_on_path_and_exe_dir(
    path_env: &OsStr,
    exe_dir: Option<&std::path::Path>,
) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    let mut results = Vec::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    let exe_dirs = exe_dir.map(std::path::Path::to_path_buf).into_iter();
    for dir in std::env::split_paths(path_env).chain(exe_dirs) {
        scan_dir(&dir, &mut seen_dirs, &mut seen_names, &mut results);
    }

    results
}

/// Scan a single directory for `loomweave-plugin-*` executables, appending results.
/// Shared by every discovery source; honours dir/name de-duplication and the
/// world-writable refusal (ADR-021).
#[cfg(unix)]
fn scan_dir(
    dir: &std::path::Path,
    seen_dirs: &mut HashSet<PathBuf>,
    seen_names: &mut HashSet<String>,
    results: &mut Vec<Result<DiscoveredPlugin, DiscoveryError>>,
) {
    // Skip empty entries (POSIX: empty means cwd — we don't support that).
    if dir.as_os_str().is_empty() {
        return;
    }

    // Deduplicate directories.
    let canonical_dir = match dir.canonicalize() {
        Ok(c) => c,
        // If the dir doesn't exist or can't be canonicalised, still use the
        // raw path for dedup so we don't skip a later entry that resolves
        // differently.
        Err(_) => dir.to_path_buf(),
    };
    if !seen_dirs.insert(canonical_dir.clone()) {
        return;
    }

    // Refuse to load plugins from world-writable directories. On a
    // multi-user machine, any user with write access to a $PATH dir
    // becomes a plugin installer — a threat model the hybrid-
    // authority framing (ADR-021) rules out. Production installs
    // should use `~/.local/bin` (0o755) or `/usr/local/bin` (0o755);
    // only pathologically misconfigured dirs fail this check.
    if is_world_writable(dir) {
        results.push(Err(DiscoveryError::WorldWritableDir {
            path: dir.to_path_buf(),
        }));
        return;
    }

    // Read directory entries; skip silently on I/O error (non-existent
    // dirs are common in $PATH).
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry_result in entries {
        let Ok(entry) = entry_result else {
            continue;
        };

        // non-UTF-8 names can't match our prefix.
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };

        // ── Name filter ───────────────────────────────────────────────────
        let suffix = match extract_plugin_suffix(&file_name) {
            Some(s) => s.to_owned(),
            None => continue,
        };

        // ── Shadowing: first match wins ───────────────────────────────────
        // Safe to key on String: non-UTF-8 names were filtered above.
        if !seen_names.insert(file_name.clone()) {
            continue;
        }

        // `exec_path` is the raw PATH-relative path (not canonicalised).
        // Do not canonicalise here — the neighbour-manifest convention
        // at `load_plugin` / `find_manifest` looks up `plugin.toml` next
        // to this path, and a symlink install pattern (e.g. `~/bin/` full
        // of symlinks into `~/.local/pipx/venvs/*/bin/`) expects the
        // manifest to live next to the symlink, not next to the resolved
        // binary in the venv. See the `executable` field doc-comment on
        // `DiscoveredPlugin` for the full consistency story.
        let exec_path = dir.join(&file_name);

        // ── Exec-bit check ────────────────────────────────────────────────
        if !is_executable(&exec_path) {
            continue;
        }

        // ── Manifest lookup ───────────────────────────────────────────────
        results.push(load_plugin(exec_path, &suffix));
    }
}

#[cfg(not(unix))]
pub fn discover_on_path(_path_env: &OsStr) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    vec![]
}

#[cfg(not(unix))]
pub fn discover_on_path_and_exe_dir(
    _path_env: &OsStr,
    _exe_dir: Option<&std::path::Path>,
) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    vec![]
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Extract the `<suffix>` from a `loomweave-plugin-<suffix>` name, or `None`.
///
/// Suffix must be at least one character and consist only of `[A-Za-z0-9_-]`.
fn extract_plugin_suffix(name: &str) -> Option<&str> {
    let suffix = name.strip_prefix("loomweave-plugin-")?;
    if suffix.is_empty() {
        return None;
    }
    if suffix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some(suffix)
    } else {
        None
    }
}

/// Return `true` if `path` is a regular file with at least one exec bit set.
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

/// Return `true` if `path` has world-write permission set. Returns
/// `false` on metadata errors (the caller treats unreachable
/// directories as "skip silently", not "world-writable").
#[cfg(unix)]
fn is_world_writable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o002 != 0)
}

#[cfg(not(unix))]
fn is_world_writable(_path: &std::path::Path) -> bool {
    false
}

/// Load the manifest for a plugin at `exec_path` with binary-name suffix `suffix`.
fn load_plugin(exec_path: PathBuf, suffix: &str) -> Result<DiscoveredPlugin, DiscoveryError> {
    let manifest_path = find_manifest(&exec_path, suffix)?;

    // Cap the read size BEFORE the TOML parser sees the bytes. A plugin.toml
    // is under 2 KiB in practice; the 64 KiB cap accepts everything legitimate
    // while rejecting pathological payloads that would otherwise force the
    // TOML parser to build an unbounded `toml::Value` tree.
    let file = std::fs::File::open(&manifest_path).map_err(|e| DiscoveryError::Io {
        path: manifest_path.clone(),
        source: e,
    })?;
    // Read one byte past the cap so we can distinguish "at cap" from "over cap".
    let mut bytes = Vec::with_capacity(4096);
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| DiscoveryError::Io {
            path: manifest_path.clone(),
            source: e,
        })?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(DiscoveryError::ManifestTooLarge {
            path: manifest_path,
        });
    }

    let manifest = parse_manifest(&bytes).map_err(|e| DiscoveryError::ManifestInvalid {
        path: manifest_path.clone(),
        source: e,
    })?;

    Ok(DiscoveredPlugin {
        executable: exec_path,
        manifest,
        manifest_path,
    })
}

/// Probe whether `path` is a regular file, distinguishing EACCES from `NotFound`.
///
/// Returns:
/// - `Ok(Some(path))` — exists and is a regular file.
/// - `Ok(None)` — does not exist, or exists but is not a regular file (e.g. a
///   directory or a symlink that resolves to a directory).
/// - `Err(DiscoveryError::Io { … })` — any other I/O error, including EACCES.
///   This surfaces operator misconfigurations that `Path::is_file()` would
///   silently hide (it returns `false` on permission-denied).
fn probe_manifest(path: &std::path::Path) -> Result<Option<PathBuf>, DiscoveryError> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(Some(path.to_owned())),
        Ok(_) => Ok(None), // exists but not a regular file (e.g. directory or symlink-to-dir)
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DiscoveryError::Io {
            path: path.to_owned(),
            source: e,
        }),
    }
}

/// Resolve the `plugin.toml` path for a given executable, or return
/// [`DiscoveryError::ManifestNotFound`].
fn find_manifest(exec_path: &std::path::Path, suffix: &str) -> Result<PathBuf, DiscoveryError> {
    // 1. Neighbor: <exec_dir>/plugin.toml
    if let Some(parent) = exec_path.parent() {
        let neighbor = parent.join("plugin.toml");
        if let Some(found) = probe_manifest(&neighbor)? {
            return Ok(found);
        }

        // 2. Install-prefix fallback: only when parent dir basename is "bin".
        let parent_name = parent.file_name().and_then(|n| n.to_str());
        if parent_name == Some("bin") {
            if let Some(grandparent) = parent.parent()
                && let Some(found) = probe_install_prefix_manifest(grandparent, suffix)?
            {
                return Ok(found);
            }

            // 3. Symlink-resolved install-prefix fallback for pipx-style
            //    layouts: ~/.local/bin/loomweave-plugin-<x> is a symlink into
            //    ~/.local/share/pipx/venvs/<pkg>/bin/loomweave-plugin-<x>, and
            //    the manifest lives under that venv's share/. Canonicalise
            //    the executable and re-try the install-prefix layout from
            //    the resolved location.
            if let Ok(canonical) = exec_path.canonicalize()
                && canonical != exec_path
                && let Some(canon_parent) = canonical.parent()
                && canon_parent.file_name().and_then(|n| n.to_str()) == Some("bin")
                && let Some(canon_grandparent) = canon_parent.parent()
                && let Some(found) = probe_install_prefix_manifest(canon_grandparent, suffix)?
            {
                return Ok(found);
            }
        }
    }

    Err(DiscoveryError::ManifestNotFound {
        executable: exec_path.to_owned(),
    })
}

fn probe_install_prefix_manifest(
    prefix: &std::path::Path,
    suffix: &str,
) -> Result<Option<PathBuf>, DiscoveryError> {
    let share_path = prefix
        .join("share")
        .join("loomweave")
        .join("plugins")
        .join(suffix)
        .join("plugin.toml");
    probe_manifest(&share_path)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    // ── Fixture ───────────────────────────────────────────────────────────────

    fn minimal_manifest_toml(plugin_id: &str) -> String {
        format!(
            r#"[plugin]
name = "loomweave-plugin-{plugin_id}"
plugin_id = "{plugin_id}"
version = "0.1.0"
protocol_version = "1.0"
executable = "loomweave-plugin-{plugin_id}"
language = "{plugin_id}"
extensions = ["mt"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["function"]
edge_kinds = ["calls"]
rule_id_prefix = "LMWV-MT-"
ontology_version = "0.1.0"
"#
        )
    }

    /// Write a file and make it executable.
    fn make_executable(path: &std::path::Path) {
        fs::write(path, b"#!/bin/sh\n").unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    /// Write a file without exec bit (mode 0o644).
    fn make_plain_file(path: &std::path::Path, content: &[u8]) {
        fs::write(path, content).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(path, perms).unwrap();
    }

    /// Build an `OsString` representing a `$PATH`-style list from one or more dirs.
    fn path_os(dirs: &[&std::path::Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    // ── T1: neighbor manifest found ───────────────────────────────────────────

    #[test]
    fn t1_neighbor_manifest_found() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        make_executable(&bin.join("loomweave-plugin-mocktest"));
        fs::write(bin.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();

        let results = discover_on_path(&path_os(&[&bin]));
        assert_eq!(results.len(), 1, "expected exactly one result");

        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(plugin.manifest.plugin.plugin_id, "mocktest");
        assert_eq!(plugin.executable, bin.join("loomweave-plugin-mocktest"));
        assert_eq!(plugin.manifest_path, bin.join("plugin.toml"));
    }

    // ── T2: install-prefix fallback ───────────────────────────────────────────

    #[test]
    fn t2_install_prefix_fallback() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        make_executable(&bin.join("loomweave-plugin-mocktest"));
        // No neighbor plugin.toml — only the share/ location.
        let share = tmp
            .path()
            .join("share")
            .join("loomweave")
            .join("plugins")
            .join("mocktest");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();

        let results = discover_on_path(&path_os(&[&bin]));
        assert_eq!(results.len(), 1);

        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(plugin.manifest.plugin.plugin_id, "mocktest");
        assert_eq!(
            plugin.manifest_path,
            tmp.path()
                .join("share/loomweave/plugins/mocktest/plugin.toml")
        );
    }

    // ── T2b: current_exe() sibling level (install-prefix, NOT on $PATH) ────────

    #[test]
    fn t2b_exe_dir_install_prefix_found_when_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        make_executable(&bin.join("loomweave-plugin-mocktest"));
        let share = tmp.path().join("share/loomweave/plugins/mocktest");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();

        // $PATH is EMPTY — the plugin is only reachable via the exe dir.
        let results = discover_on_path_and_exe_dir(std::ffi::OsStr::new(""), Some(bin.as_path()));
        assert_eq!(results.len(), 1, "exe-dir plugin should be discovered");

        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(plugin.manifest.plugin.plugin_id, "mocktest");
        assert_eq!(
            plugin.manifest_path,
            tmp.path()
                .join("share/loomweave/plugins/mocktest/plugin.toml")
        );
    }

    #[test]
    fn t2c_path_entry_shadows_same_named_exe_dir_sibling() {
        // A plugin on $PATH wins over a same-named sibling next to the binary.
        let tmp = TempDir::new().unwrap();
        let path_bin = tmp.path().join("pathbin");
        let exe_bin = tmp.path().join("exebin");
        fs::create_dir_all(&path_bin).unwrap();
        fs::create_dir_all(&exe_bin).unwrap();

        make_executable(&path_bin.join("loomweave-plugin-mocktest"));
        fs::write(
            path_bin.join("plugin.toml"),
            minimal_manifest_toml("mocktest"),
        )
        .unwrap();
        make_executable(&exe_bin.join("loomweave-plugin-mocktest"));
        fs::write(
            exe_bin.join("plugin.toml"),
            minimal_manifest_toml("mocktest"),
        )
        .unwrap();

        let results = discover_on_path_and_exe_dir(&path_os(&[&path_bin]), Some(exe_bin.as_path()));
        assert_eq!(results.len(), 1, "duplicate name must be de-duplicated");
        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(
            plugin.executable,
            path_bin.join("loomweave-plugin-mocktest"),
            "$PATH entry must shadow the exe-dir sibling"
        );
    }

    #[test]
    fn t2d_exe_dir_equal_to_path_dir_is_deduped() {
        // The exe dir resolving to a dir already on $PATH must not be scanned
        // twice (seen_dirs gate), so the plugin is reported exactly once.
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        make_executable(&bin.join("loomweave-plugin-mocktest"));
        let share = tmp.path().join("share/loomweave/plugins/mocktest");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();

        let results = discover_on_path_and_exe_dir(&path_os(&[&bin]), Some(bin.as_path()));
        assert_eq!(
            results.len(),
            1,
            "exe dir == a $PATH dir must be deduped, not double-scanned"
        );
        assert!(results.into_iter().next().unwrap().is_ok());
    }

    // ── T3: no manifest anywhere → ManifestNotFound ───────────────────────────

    #[test]
    fn t3_no_manifest_returns_manifest_not_found() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        make_executable(&bin.join("loomweave-plugin-orphan"));

        let results = discover_on_path(&path_os(&[&bin]));
        assert_eq!(results.len(), 1);

        let err = results.into_iter().next().unwrap().unwrap_err();
        assert!(
            matches!(err, DiscoveryError::ManifestNotFound { .. }),
            "expected ManifestNotFound, got: {err:?}"
        );
    }

    // ── T4: malformed manifest → ManifestInvalid ─────────────────────────────

    #[test]
    fn t4_malformed_manifest_returns_manifest_invalid() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        make_executable(&bin.join("loomweave-plugin-broken"));
        fs::write(bin.join("plugin.toml"), b"this is not valid toml ][[[").unwrap();

        let results = discover_on_path(&path_os(&[&bin]));
        assert_eq!(results.len(), 1);

        let err = results.into_iter().next().unwrap().unwrap_err();
        assert!(
            matches!(err, DiscoveryError::ManifestInvalid { .. }),
            "expected ManifestInvalid, got: {err:?}"
        );
    }

    // ── T5: non-matching names skipped ────────────────────────────────────────

    #[test]
    fn t5_non_matching_names_skipped() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        // Should NOT match:
        make_executable(&bin.join("not-loomweave-plugin"));
        make_executable(&bin.join("loomweave-plugin-")); // empty suffix
        make_executable(&bin.join("loomweave-plugin")); // no second hyphen

        // Should match:
        make_executable(&bin.join("loomweave-plugin-valid"));
        fs::write(bin.join("plugin.toml"), minimal_manifest_toml("valid")).unwrap();

        let results = discover_on_path(&path_os(&[&bin]));
        assert_eq!(results.len(), 1, "only one name should match");

        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(plugin.manifest.plugin.plugin_id, "valid");
    }

    // ── T6: non-executable file skipped ───────────────────────────────────────

    #[test]
    fn t6_non_executable_file_skipped() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        // File exists but has no exec bit.
        make_plain_file(&bin.join("loomweave-plugin-noexec"), b"#!/bin/sh\n");
        fs::write(bin.join("plugin.toml"), minimal_manifest_toml("noexec")).unwrap();

        let results = discover_on_path(&path_os(&[&bin]));
        assert_eq!(results.len(), 0, "non-executable should be skipped");
    }

    // ── T7: multiple $PATH entries, shadowing ─────────────────────────────────

    #[test]
    fn t7_path_shadowing_first_wins() {
        let tmp = TempDir::new().unwrap();
        let dir_a = tmp.path().join("a").join("bin");
        let dir_b = tmp.path().join("b").join("bin");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(&dir_b).unwrap();

        // Both dirs have the same binary name with valid manifests.
        make_executable(&dir_a.join("loomweave-plugin-dup"));
        fs::write(dir_a.join("plugin.toml"), minimal_manifest_toml("dup")).unwrap();

        make_executable(&dir_b.join("loomweave-plugin-dup"));
        fs::write(dir_b.join("plugin.toml"), minimal_manifest_toml("dup")).unwrap();

        let results = discover_on_path(&path_os(&[dir_a.as_path(), dir_b.as_path()]));
        assert_eq!(
            results.len(),
            1,
            "duplicate name should produce only one result"
        );

        let plugin = results.into_iter().next().unwrap().unwrap();
        // Executable must come from dir_a, not dir_b.
        assert_eq!(plugin.executable, dir_a.join("loomweave-plugin-dup"));
    }

    // ── T8: world-writable directory refused ──────────────────────────────────

    /// A `$PATH` directory with world-write permission is refused with
    /// [`DiscoveryError::WorldWritableDir`] and its contents are not
    /// scanned. Protects the ADR-021 "semi-trusted plugin" model from
    /// the multi-user-machine drop-in-evil-binary threat.
    #[test]
    fn t8_world_writable_dir_is_refused() {
        let dir = TempDir::new().unwrap();
        make_executable(&dir.path().join("loomweave-plugin-evil"));
        fs::write(
            dir.path().join("plugin.toml"),
            minimal_manifest_toml("evil"),
        )
        .unwrap();

        // Make the dir world-writable.
        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o777);
        fs::set_permissions(dir.path(), perms).unwrap();

        let results = discover_on_path(&path_os(&[dir.path()]));
        assert_eq!(
            results.len(),
            1,
            "world-writable dir must produce one error"
        );
        let err = results.into_iter().next().unwrap().unwrap_err();
        match err {
            DiscoveryError::WorldWritableDir { path } => {
                assert_eq!(path, dir.path(), "error must name the offending dir");
            }
            other => panic!("expected WorldWritableDir; got {other:?}"),
        }
    }
}
