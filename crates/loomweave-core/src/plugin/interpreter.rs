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

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

/// Non-unix targets have no mode bits to consult; the plugin host itself is
/// Linux/macOS-only, so this arm exists only to keep the crate compiling.
#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

fn usable(candidate: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(candidate).ok()?;
    if !meta.is_file() || !is_executable(&meta) {
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

fn which(name: &str, path_var: Option<&OsString>) -> Option<PathBuf> {
    // An absent PATH means "no PATH" — never fall back to the process env.
    let path_var = path_var?;
    std::env::split_paths(path_var).find_map(|dir| usable(&dir.join(name)))
}

/// Resolve the project's interpreter in the contract order (module docs).
/// `env` abstracts `std::env::var_os` so tests can inject an environment.
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
        return ProjectInterpreter {
            path: Some(path),
            source: InterpreterSource::DotVenv,
        };
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
    let path_var = env("PATH");
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

/// The resolver-environment fingerprint a plugin's index depends on: `Some`
/// only for manifests declaring `[capabilities.runtime.pyright]`.
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
}
