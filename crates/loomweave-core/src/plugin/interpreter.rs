//! Project-interpreter discovery for language-server plugins
//! (clarion-5cf9643de9).
//!
//! `pyright-langserver` type-checks against whatever `python` is first on its
//! `PATH` unless the client pins `python.pythonPath`. An `analyze` launched
//! from an agent hook carries no project venv on `PATH`, so every
//! `tests/` -> `src/` call target came back empty while the coverage claim said
//! `complete`, and the incremental skip pinned the hole. The host now runs
//! this discovery before the incremental partition, exports the winner to the
//! plugin as [`PYTHON_INTERPRETER_ENV`], and keys
//! `plugin_index_meta.resolver_environment` on
//! [`ProjectInterpreter::fingerprint`] so a changed interpreter forces a full
//! re-dispatch of the plugin's files.
//!
//! The order is a CROSS-LANGUAGE CONTRACT with
//! `plugins/python/src/loomweave_plugin_python/interpreter.py`. Change both or
//! neither.
//!
//! Rung 2 (`<project_root>/.venv/bin/python`) applies only when that path is
//! **not repository-tracked**: [`crate::hardened_git::tracked_state`] must
//! answer `untracked`, `not_a_git_work_tree`, or `git_unavailable` (`tracked`
//! and the fail-closed `unknown` both skip the rung, logged once per
//! process). A missing `git` binary is the operator's environment, not
//! repository content, so `git_unavailable` is permissive and does NOT skip
//! the rung. pyright executes `python.pythonPath`, so a committed
//! `.venv/bin/python` — or a committed symlink at `.venv` to committed
//! content — would otherwise be code execution as the operator on the first
//! `analyze` of an untrusted corpus. See ADR-063 and the ADR-058 amendment
//! (2026-09-02).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::manifest::Manifest;

/// Env var carrying the host's (or the operator's) interpreter choice to the
/// plugin.
///
/// - Basis: the host's discovery is authoritative under `analyze`; the plugin
///   trusts this var first so the two agree by construction.
/// - Override surface: this var IS the override surface.
/// - Retune trigger: none — a path, not a tunable.
/// - Coupling: `loomweave_plugin_python.interpreter.INTERPRETER_OVERRIDE_ENV`
///   carries the same literal.
pub const PYTHON_INTERPRETER_ENV: &str = "LOOMWEAVE_PYTHON_INTERPRETER";

/// Where the interpreter came from, in contract order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpreterSource {
    /// [`PYTHON_INTERPRETER_ENV`] named an executable file.
    Override,
    /// `<project_root>/.venv/bin/python` — the project's own virtualenv.
    DotVenv,
    /// `$VIRTUAL_ENV/bin/python` — an activated virtualenv.
    VirtualEnv,
    /// `$CONDA_PREFIX/bin/python` — an activated conda environment.
    Conda,
    /// First `python` / `python3` on `PATH` — a guess, not project-owned.
    Path,
    /// Nothing found; `pyright` falls back to its own discovery.
    None,
}

/// The interpreter `pyright` will be pointed at, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInterpreter {
    /// Absolute, lexically normalised, NEVER symlink-resolved path to the
    /// interpreter; `None` when discovery found nothing.
    pub path: Option<PathBuf>,
    /// Which rung of the contract order produced [`ProjectInterpreter::path`].
    pub source: InterpreterSource,
}

impl ProjectInterpreter {
    /// Project-owned (override / `.venv` / `VIRTUAL_ENV` / `CONDA_PREFIX`).
    #[must_use]
    pub fn pinned(&self) -> bool {
        !matches!(
            self.source,
            InterpreterSource::Path | InterpreterSource::None
        )
    }

    /// Stable string for `plugin_index_meta.resolver_environment`.
    ///
    /// An unpinned choice is tagged so it can never compare equal to a pinned
    /// path with the same bytes: acquiring a project venv at a location that
    /// happened to be first on `PATH` still moves the marker.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        match (&self.path, self.pinned()) {
            (Some(path), true) => path.display().to_string(),
            (Some(path), false) => format!("unpinned:{}", path.display()),
            (None, _) => "unpinned:none".to_owned(),
        }
    }
}

/// Executable *for this process*, decided the way Python's `os.access` decides
/// it — `access(2)` with `X_OK`, real uid/gid, ACLs and `noexec` mounts
/// included.
///
/// Raw mode bits (`mode & 0o111`) are NOT equivalent and would break the
/// cross-language contract: a `0o100`-mode file owned by another user, or any
/// executable under a `noexec` mount, passes a mode-bit test and fails
/// `access(2)`. The host would then export a path the plugin's own `usable()`
/// rejects, and the plugin would silently fall through to its next rung — the
/// exact host/plugin disagreement this module exists to prevent.
fn is_executable(candidate: &Path) -> bool {
    nix::unistd::access(candidate, nix::unistd::AccessFlags::X_OK).is_ok()
}

/// Drop trailing `/` characters, the way `PurePath` does at construction on
/// the Python side — WITHOUT touching anything else about the path.
///
/// Python's `Path('/x/python/')` is already `/x/python` before `is_file()`
/// ever runs, so the plugin accepts an override written with a trailing
/// slash. Rust keeps the separator, and both `stat(2)` and `access(2)` then
/// fail with `ENOTDIR` on a regular file — so the host would reject an
/// override the plugin accepts, exporting nothing while the plugin pins the
/// very path the operator asked for. Converging on ACCEPTANCE (rather than
/// making Python reject) keeps the operator-visible behaviour unchanged.
///
/// The root itself is never stripped: `/` stays `/` (and `//` collapses to
/// `/`), so a candidate can never become the empty path.
fn strip_trailing_separators(candidate: &Path) -> &Path {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = candidate.as_os_str().as_bytes();
    let mut end = bytes.len();
    while end > 1 && bytes[end - 1] == b'/' {
        end -= 1;
    }
    Path::new(std::ffi::OsStr::from_bytes(&bytes[..end]))
}

fn usable(candidate: &Path) -> Option<PathBuf> {
    // One stripped value feeds all three of `metadata`, `access(2)` and
    // `absolute` below: `access(2)` fails ENOTDIR on a trailing separator
    // exactly as `metadata` does, so stripping for only one of them would
    // still reject a `…/python/` override.
    let candidate = strip_trailing_separators(candidate);
    // `metadata` follows symlinks, so a venv's `bin/python` is judged by the
    // base interpreter it points at — which is what `access(2)` does too, and
    // what Python's `Path.is_file()` does. Only the RETURNED path stays
    // unresolved.
    if !std::fs::metadata(candidate).is_ok_and(|meta| meta.is_file()) || !is_executable(candidate) {
        return None;
    }
    // NOT canonicalize(): a venv's bin/python symlinks to the base
    // interpreter; pyright must be handed the venv path (Global Constraints).
    // `std::path::absolute` keeps `..` on Unix, so collapse it lexically here
    // to match Python's `os.path.abspath` (normpath) byte-for-byte.
    Some(normalize_lexically(&std::path::absolute(candidate).ok()?))
}

/// Lexical normalisation identical to Python's `os.path.normpath` on an
/// absolute path: drop `.`, pop a component for `..` (never past root),
/// never touch the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `PATH` lookup, hand-rolled rather than delegating to the `which` crate.
///
/// The cross-language contract needs behaviour byte-identical to Python's
/// `shutil.which(name, path=...)`: exactly the directories in the caller-supplied
/// `PATH`, in order, no process-env fallback, no cwd rung, and the same
/// executability predicate (`access(2)` via [`usable`]). The `which` crate
/// applies its own resolution policy — including falling back to the ambient
/// environment — so using it would reintroduce the launcher dependence this
/// module exists to remove.
fn which(name: &str, path_var: Option<&OsString>) -> Option<PathBuf> {
    // An absent PATH means "no PATH" — never fall back to the process env. The
    // caller filters out an EMPTY value before this point: `split_paths("")`
    // yields one EMPTY entry, so `"".join("python")` would be the bare relative
    // path `python`, stat'd against the analyze process's CWD. That is both a
    // divergence from `shutil.which(path="")` (which returns None) and a
    // cwd-dependent binary pickup.
    let path_var = path_var?;
    std::env::split_paths(path_var).find_map(|dir| usable(&dir.join(name)))
}

/// Resolve the project's interpreter in the contract order (module docs).
/// `env` abstracts `std::env::var_os` so tests can inject an environment.
///
/// `project_root` MUST be canonicalised by the caller. Discovery joins
/// `.venv/bin/python` onto the root as given and normalises only lexically, so
/// a symlinked root yields a symlinked interpreter path and a different
/// [`ProjectInterpreter::fingerprint`]. `analyze` (which records the
/// fingerprint) and `PluginHost::spawn_unhandshaken` (which exports the
/// interpreter) both canonicalise first; dropping it at either site would skew
/// the marker against the exported interpreter and re-dispatch every run. See
/// `the_root_canonicalisation_at_both_call_sites_is_load_bearing`.
#[must_use]
pub fn discover_project_interpreter(
    project_root: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> ProjectInterpreter {
    if let Some(raw) = env(PYTHON_INTERPRETER_ENV).filter(|value| !value.is_empty()) {
        if let Some(path) = usable(Path::new(&raw)) {
            return ProjectInterpreter {
                path: Some(path),
                source: InterpreterSource::Override,
            };
        }
        tracing::warn!(
            override_path = %raw.to_string_lossy(),
            "{PYTHON_INTERPRETER_ENV} is not an executable file; ignoring the override"
        );
    }
    if let Some(path) = usable(&project_root.join(".venv/bin/python")) {
        let state = crate::hardened_git::tracked_state(project_root, Path::new(".venv/bin/python"));
        if state.treat_as_tracked() {
            warn_tracked_dotvenv_once(project_root, &state);
        } else {
            return ProjectInterpreter {
                path: Some(path),
                source: InterpreterSource::DotVenv,
            };
        }
    }
    for (var, source) in [
        ("VIRTUAL_ENV", InterpreterSource::VirtualEnv),
        ("CONDA_PREFIX", InterpreterSource::Conda),
    ] {
        if let Some(prefix) = env(var).filter(|value| !value.is_empty())
            && let Some(path) = usable(&Path::new(&prefix).join("bin/python"))
        {
            return ProjectInterpreter {
                path: Some(path),
                source,
            };
        }
    }
    // Empty counts as unset, exactly as on the override / VIRTUAL_ENV /
    // CONDA_PREFIX rungs above (see `which`).
    let path_var = env("PATH").filter(|value| !value.is_empty());
    for name in ["python", "python3"] {
        if let Some(path) = which(name, path_var.as_ref()) {
            return ProjectInterpreter {
                path: Some(path),
                source: InterpreterSource::Path,
            };
        }
    }
    ProjectInterpreter {
        path: None,
        source: InterpreterSource::None,
    }
}

/// Rung 2 is skipped when `.venv/bin/python` is repository content (ADR-063;
/// ADR-058 amendment 2026-09-02): pyright executes `python.pythonPath`, so a
/// committed executable there is code execution as the operator. Logged once
/// per process so the operator sees why resolution degraded (ADR-057 style).
fn warn_tracked_dotvenv_once(project_root: &Path, state: &crate::hardened_git::TrackedState) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            project_root = %project_root.display(),
            tracked_state = state.label(),
            "skipped .venv/bin/python (rung 2): it is tracked by the repository (or its tracked \
             state could not be determined); an operator venv is untracked. Resolution continues \
             with VIRTUAL_ENV/CONDA_PREFIX/PATH."
        );
    });
}

/// The resolver-environment fingerprint a plugin's index depends on: `Some`
/// only for manifests declaring `[capabilities.runtime.pyright]`.
///
/// `project_root` must be canonicalised — see
/// [`discover_project_interpreter`].
#[must_use]
pub fn resolver_environment_for(manifest: &Manifest, project_root: &Path) -> Option<String> {
    manifest.capabilities.runtime.pyright.as_ref()?;
    Some(discover_project_interpreter(project_root, &|key| std::env::var_os(key)).fingerprint())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    fn make_python(path: &Path) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        std::path::absolute(path).unwrap()
    }

    fn env<'a>(map: &'a HashMap<&'a str, String>) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |key| map.get(key).map(OsString::from)
    }

    #[test]
    fn the_env_var_literal_is_the_cross_language_contract() {
        // The plugin side carries this same literal in
        // `loomweave_plugin_python.interpreter.INTERPRETER_OVERRIDE_ENV`. A
        // rename on either side breaks the pass-through SILENTLY — the plugin
        // simply stops seeing the host's choice and falls back to its own
        // discovery, which usually lands on the same interpreter, so nothing
        // fails loudly. Pinning the literal makes a rename a deliberate act
        // with a test to update on both sides.
        assert_eq!(PYTHON_INTERPRETER_ENV, "LOOMWEAVE_PYTHON_INTERPRETER");
    }

    #[test]
    fn the_root_canonicalisation_at_both_call_sites_is_load_bearing() {
        // `analyze` computes the fingerprint from its canonicalised
        // `project_root`; `PluginHost::spawn_unhandshaken` re-canonicalises the
        // root it is handed before running the SAME discovery to decide what to
        // export. They agree only because BOTH canonicalise and canonicalise is
        // idempotent.
        //
        // Discovery itself is deliberately NOT root-invariant: it joins
        // `.venv/bin/python` onto the root as given and lexically normalises,
        // so a symlinked root yields a symlinked interpreter path. That is
        // correct for a venv (see the symlink test below) but it means dropping
        // the canonicalisation at either call site would silently skew the
        // recorded marker against the exported interpreter — an index that
        // re-dispatches every run. This test pins the skew so that removal
        // fails loudly here rather than quietly in production.
        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("real");
        let venv = make_python(&real_root.join(".venv/bin/python"));
        let link_root = dir.path().join("link");
        std::os::unix::fs::symlink(&real_root, &link_root).unwrap();
        let canonical_root = link_root.canonicalize().unwrap();

        assert_eq!(
            discover_project_interpreter(&canonical_root, &env(&HashMap::new())).path,
            Some(venv),
            "a canonical root finds the project .venv at its real path"
        );
        // Idempotence — the property the two call sites actually rely on.
        assert_eq!(
            discover_project_interpreter(&canonical_root, &env(&HashMap::new())).fingerprint(),
            discover_project_interpreter(
                &canonical_root.canonicalize().unwrap(),
                &env(&HashMap::new())
            )
            .fingerprint(),
            "canonicalising twice must not move the fingerprint"
        );
        // And the skew a dropped canonicalisation would introduce.
        assert_ne!(
            discover_project_interpreter(&link_root, &env(&HashMap::new())).fingerprint(),
            discover_project_interpreter(&canonical_root, &env(&HashMap::new())).fingerprint(),
            "an UNcanonicalised root yields a different fingerprint — which is why both \
             `analyze` and `spawn_unhandshaken` must canonicalise before discovering"
        );
    }

    /// Sets the process CWD for the duration of a test and restores it on drop
    /// (including on panic, so a failing assertion cannot leak a bad CWD into
    /// another test in the same binary).
    ///
    /// CWD is process-global. This is safe here because it is the ONLY
    /// CWD-mutating test in the crate and no other `loomweave-core` test reads
    /// the CWD (`rg 'current_dir'` finds only `Command::current_dir` in
    /// `hardened_git`, which sets the CHILD's directory explicitly). The
    /// `MUTEX` serialises it against any future sibling regardless.
    struct CwdGuard {
        original: PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CwdGuard {
        fn set(to: &Path) -> Self {
            static MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = MUTEX
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(to).unwrap();
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.original).unwrap();
        }
    }

    #[test]
    fn empty_env_values_count_as_unset() {
        // CONTRACT: an empty env value is unset, on every rung. The Python side
        // gets this for free (`if override:`, `if prefix and ...`, and
        // `shutil.which(name, path="")` -> None, verified against CPython
        // 3.12). Rust does NOT: `std::env::split_paths("")` yields one EMPTY
        // entry, so `"".join("python")` is the bare relative path `python`,
        // stat'd against whatever CWD the analyze process happened to have.
        // That is both a divergence from `shutil.which` and a CWD-dependent
        // binary pickup — the exact launcher dependence this module exists to
        // remove.
        //
        // The test therefore RUNS FROM a directory containing an executable
        // `python`. Without that, every assertion below passes even with the
        // filter deleted (nothing named `python` sits in the default test CWD),
        // which is precisely the vacuous test this comment exists to prevent.
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("cwd");
        let decoy = make_python(&cwd.join("python"));
        let _guard = CwdGuard::set(&cwd);

        // The hazard, pinned directly: unfiltered, an empty PATH resolves to
        // the CWD's `python`.
        assert_eq!(
            which("python", Some(&OsString::from(""))),
            Some(decoy),
            "an empty PATH degrades to a CWD-relative stat — this is what the \
             caller's `.filter(|v| !v.is_empty())` exists to prevent"
        );

        // ...and with the filter, discovery ignores it entirely.
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let all_empty = HashMap::from([
            (PYTHON_INTERPRETER_ENV, String::new()),
            ("VIRTUAL_ENV", String::new()),
            ("CONDA_PREFIX", String::new()),
            ("PATH", String::new()),
        ]);
        assert_eq!(
            discover_project_interpreter(&root, &env(&all_empty)),
            ProjectInterpreter {
                path: None,
                source: InterpreterSource::None
            },
            "empty values on every rung must discover nothing — NOT the CWD's python"
        );

        // An empty override falls through to `.venv` without taking the
        // warning path (that branch is for an operator who set a BAD path, not
        // for an unset variable), and an empty PATH cannot outrank it.
        let dotvenv = make_python(&root.join(".venv/bin/python"));
        assert_eq!(
            discover_project_interpreter(&root, &env(&all_empty)),
            ProjectInterpreter {
                path: Some(dotvenv),
                source: InterpreterSource::DotVenv
            },
            "an empty override falls through to .venv"
        );

        // Control: a NON-empty PATH naming a directory with no interpreter
        // reaches the same `None`, the legitimate way.
        let empty_dir = dir.path().join("empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let real_but_empty = HashMap::from([("PATH", empty_dir.display().to_string())]);
        assert_eq!(
            discover_project_interpreter(&dir.path().join("no-venv"), &env(&real_but_empty)).source,
            InterpreterSource::None
        );
    }

    #[test]
    fn dotvenv_wins_over_virtual_env_and_path() {
        let dir = tempfile::tempdir().unwrap();
        let dotvenv = make_python(&dir.path().join(".venv/bin/python"));
        let other = make_python(&dir.path().join("elsewhere/bin/python"));
        let map = HashMap::from([
            (
                "VIRTUAL_ENV",
                dir.path().join("elsewhere").display().to_string(),
            ),
            ("PATH", other.parent().unwrap().display().to_string()),
        ]);
        let found = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(
            found,
            ProjectInterpreter {
                path: Some(dotvenv.clone()),
                source: InterpreterSource::DotVenv
            }
        );
        assert!(found.pinned());
        assert_eq!(found.fingerprint(), dotvenv.display().to_string());
    }

    #[test]
    fn override_wins_and_an_unusable_override_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        let dotvenv = make_python(&dir.path().join(".venv/bin/python"));
        let custom = make_python(&dir.path().join("custom/python"));
        let map = HashMap::from([(PYTHON_INTERPRETER_ENV, custom.display().to_string())]);
        assert_eq!(
            discover_project_interpreter(dir.path(), &env(&map)).source,
            InterpreterSource::Override
        );
        let map = HashMap::from([(
            PYTHON_INTERPRETER_ENV,
            dir.path().join("nope").display().to_string(),
        )]);
        let found = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(found.source, InterpreterSource::DotVenv);
        assert_eq!(found.path, Some(dotvenv));
    }

    #[test]
    fn virtual_env_then_conda_then_path_then_none() {
        let dir = tempfile::tempdir().unwrap();
        let venv = make_python(&dir.path().join("venv/bin/python"));
        let conda = make_python(&dir.path().join("conda/bin/python"));
        let on_path = make_python(&dir.path().join("bin/python3"));
        let path_dir = on_path.parent().unwrap().display().to_string();
        let map = HashMap::from([
            ("VIRTUAL_ENV", dir.path().join("venv").display().to_string()),
            (
                "CONDA_PREFIX",
                dir.path().join("conda").display().to_string(),
            ),
            ("PATH", path_dir.clone()),
        ]);
        assert_eq!(
            discover_project_interpreter(dir.path(), &env(&map)).path,
            Some(venv)
        );
        let map = HashMap::from([
            (
                "CONDA_PREFIX",
                dir.path().join("conda").display().to_string(),
            ),
            ("PATH", path_dir.clone()),
        ]);
        assert_eq!(
            discover_project_interpreter(dir.path(), &env(&map)).path,
            Some(conda)
        );
        let map = HashMap::from([("PATH", path_dir)]);
        let unpinned = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(unpinned.source, InterpreterSource::Path);
        assert!(!unpinned.pinned());
        assert_eq!(
            unpinned.fingerprint(),
            format!("unpinned:{}", on_path.display())
        );
        let map = HashMap::from([("PATH", dir.path().join("empty").display().to_string())]);
        let none = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(
            none,
            ProjectInterpreter {
                path: None,
                source: InterpreterSource::None
            }
        );
        assert_eq!(none.fingerprint(), "unpinned:none");
    }

    #[test]
    fn override_path_is_lexically_normalised_but_symlinks_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let real = make_python(&dir.path().join("real/bin/python"));
        fs::create_dir_all(dir.path().join("real/sub")).unwrap();
        let dotted = dir.path().join("real/sub/../bin/python");
        let map = HashMap::from([(PYTHON_INTERPRETER_ENV, dotted.display().to_string())]);
        assert_eq!(
            discover_project_interpreter(dir.path(), &env(&map)).path,
            Some(real.clone())
        );
        let link = dir.path().join(".venv/bin/python");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let found = discover_project_interpreter(dir.path(), &env(&HashMap::new()));
        assert_eq!(found.path, Some(link), "the symlink path, not its target");
        assert_eq!(found.source, InterpreterSource::DotVenv);
    }

    #[test]
    fn an_override_with_a_trailing_separator_is_accepted_like_the_plugin_accepts_it() {
        // CROSS-LANGUAGE CONTRACT. Python's `Path('/x/python/')` drops the
        // separator at construction, so `is_file()` succeeds and the plugin
        // pins the override. Rust's `metadata`/`access(2)` on the raw
        // `…/python/` fail with ENOTDIR, so without the strip the HOST would
        // fall through to `.venv` (or export nothing) while the PLUGIN pinned
        // the operator's path — the two disagreeing on the same environment,
        // which is the failure mode this module exists to prevent. The
        // returned path must also be the stripped, normalised one, byte-equal
        // to what Python's `os.path.normpath` yields. Pinned by the Python
        // test `test_override_with_a_trailing_separator_matches_the_rust_host`.
        let dir = tempfile::tempdir().unwrap();
        let real = make_python(&dir.path().join("real/bin/python"));
        let with_slash = format!("{}/", real.display());
        let map = HashMap::from([(PYTHON_INTERPRETER_ENV, with_slash)]);
        assert_eq!(
            discover_project_interpreter(dir.path(), &env(&map)),
            ProjectInterpreter {
                path: Some(real),
                source: InterpreterSource::Override
            }
        );
        // The root is never stripped away — `/` must stay a path, not become
        // the empty one (which `metadata` would answer for the CWD).
        assert_eq!(strip_trailing_separators(Path::new("/")), Path::new("/"));
        assert_eq!(strip_trailing_separators(Path::new("//")), Path::new("/"));
        assert_eq!(
            strip_trailing_separators(Path::new("/x/python///")),
            Path::new("/x/python")
        );
    }

    #[test]
    fn path_lookup_prefers_python_over_python3_and_skips_non_executables() {
        let dir = tempfile::tempdir().unwrap();
        let py = make_python(&dir.path().join("bin/python"));
        make_python(&dir.path().join("bin/python3"));
        let map = HashMap::from([("PATH", py.parent().unwrap().display().to_string())]);
        assert_eq!(
            discover_project_interpreter(dir.path(), &env(&map)).path,
            Some(py.clone())
        );
        fs::set_permissions(&py, fs::Permissions::from_mode(0o644)).unwrap();
        let found = discover_project_interpreter(dir.path(), &env(&map));
        assert_eq!(found.path.unwrap().file_name().unwrap(), "python3");
    }

    /// `GIT_CONFIG_GLOBAL=/dev/null` + `GIT_CONFIG_NOSYSTEM=1` on top of the
    /// author/committer identity, so a developer's own global git config
    /// cannot alter these fixtures (review finding on Task 3's builders,
    /// applied here too).
    fn git(root: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn a_repository_tracked_dotvenv_is_skipped_and_the_ladder_continues() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        make_python(&root.join(".venv/bin/python"));
        git(&root, &["add", "-f", ".venv/bin/python"]);
        git(&root, &["commit", "-q", "-m", "hostile"]);
        let venv = make_python(&root.join("operator-venv/bin/python"));
        let vars = HashMap::from([(
            "VIRTUAL_ENV",
            root.join("operator-venv").display().to_string(),
        )]);
        let chosen = discover_project_interpreter(&root, &env(&vars));
        assert_eq!(chosen.source, InterpreterSource::VirtualEnv);
        assert_eq!(chosen.path.as_deref(), Some(venv.as_path()));
    }

    #[test]
    fn an_untracked_dotvenv_in_a_repository_is_still_chosen() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        let venv = make_python(&root.join(".venv/bin/python"));
        let chosen = discover_project_interpreter(&root, &env(&HashMap::new()));
        assert_eq!(chosen.source, InterpreterSource::DotVenv);
        assert_eq!(chosen.path.as_deref(), Some(venv.as_path()));
    }

    #[test]
    fn a_tracked_symlink_dotvenv_is_skipped() {
        if !git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        make_python(&root.join("payload/bin/python"));
        std::os::unix::fs::symlink(root.join("payload"), root.join(".venv")).unwrap();
        git(&root, &["add", "-f", ".venv"]);
        git(&root, &["commit", "-q", "-m", "hostile"]);
        let chosen = discover_project_interpreter(&root, &env(&HashMap::new()));
        assert_ne!(chosen.source, InterpreterSource::DotVenv);
    }

    #[test]
    fn outside_a_repository_the_dotvenv_rung_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let venv = make_python(&root.join(".venv/bin/python"));
        let chosen = discover_project_interpreter(&root, &env(&HashMap::new()));
        assert_eq!(chosen.source, InterpreterSource::DotVenv);
        assert_eq!(chosen.path.as_deref(), Some(venv.as_path()));
    }
}
