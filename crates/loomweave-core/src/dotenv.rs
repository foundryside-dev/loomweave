//! The repository `.env` as a credential sidecar — never as environment.
//!
//! `loomweave` used to load `<cwd>/.env` into the process environment
//! (`dotenvy::dotenv()`) for every operator-facing command. Everything in a
//! file the analyzed repository controls thereby became visible to (a) every
//! child the command spawns — the `loomweave analyze` started by `serve`'s
//! `analyze_start`, the Filigree / Warpline MCP launchers, `git` — and (b)
//! every launcher override this binary reads from the environment
//! (`LOOMWEAVE_FILIGREE_MCP_COMMAND`, `LOOMWEAVE_WARPLINE_MCP_COMMAND`,
//! `LOOMWEAVE_CODEX_CONFIG`), plus whatever third-party code consults
//! (`LD_PRELOAD`, `HTTPS_PROXY`, `PYTHONPATH`). Because `dotenvy` only sets
//! variables that are *unset*, exactly the normally-unset ones were
//! attacker-fillable: a committed `.env` chose the program `serve` executed.
//!
//! The one legitimate use of a repository `.env` is to supply the values of
//! variables Loomweave's **own configuration names**: provider API keys
//! (`api_key_env`), Filigree tokens (`token_env` / `identity_token_env`), and
//! `RUST_LOG`. So the file is now parsed into a private map and consulted
//! **only** by [`var`], which those lookups call. The process environment is
//! never modified: children inherit nothing from `.env`, and `std::env::var`
//! reads — every launcher override — cannot observe it. The real environment
//! always wins over the sidecar, preserving `dotenvy`'s never-clobber
//! precedence for operators who export a value explicitly. (ADR-062)

use std::collections::HashMap;
use std::sync::OnceLock;

static SIDECAR: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Parse the `.env` that `dotenvy::dotenv()` would have loaded (the current
/// directory, then its ancestors) into the sidecar. Idempotent: the first call
/// wins. A missing or unparseable file yields an empty sidecar; malformed lines
/// are skipped. Returns the number of entries the sidecar now holds.
pub fn load_sidecar() -> usize {
    SIDECAR
        .get_or_init(|| {
            dotenvy::dotenv_iter()
                .map(|iter| iter.filter_map(Result::ok).collect())
                .unwrap_or_default()
        })
        .len()
}

/// The value of `name`: the real process environment first, then the sidecar
/// (only if [`load_sidecar`] ran). This is the *only* reader of `.env`; use it
/// for config-named credentials and never for launcher or path overrides.
#[must_use]
pub fn var(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        return Some(value);
    }
    SIDECAR.get().and_then(|map| map.get(name).cloned())
}

/// Whether [`load_sidecar`] has run in this process (diagnostics only).
#[must_use]
pub fn sidecar_loaded() -> bool {
    SIDECAR.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::{load_sidecar, sidecar_loaded, var};

    /// Sets the process CWD for the test and restores it on drop (including on
    /// panic). The sidecar is a process-global `OnceLock`, so this module's
    /// tests are written as ONE test: under `cargo test` they would share the
    /// binary; under nextest each test is its own process either way.
    struct CwdGuard(std::path::PathBuf);
    impl CwdGuard {
        fn enter(dir: &std::path::Path) -> Self {
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self(prev)
        }
    }
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn sidecar_is_visible_to_var_but_never_to_the_process_environment() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "LOOMWEAVE_TEST_SIDECAR_ONLY=from-dotenv\n\
             # a launcher override a hostile repo would love to set\n\
             LOOMWEAVE_FILIGREE_MCP_COMMAND=/tmp/evil\n\
             LD_PRELOAD=/tmp/evil.so\n\
             not a valid line\n",
        )
        .unwrap();
        let _cwd = CwdGuard::enter(dir.path());

        assert!(!sidecar_loaded());
        assert_eq!(
            var("LOOMWEAVE_TEST_SIDECAR_ONLY"),
            None,
            "nothing is read from .env before load_sidecar()"
        );

        let loaded = load_sidecar();
        assert_eq!(
            loaded, 3,
            "three well-formed entries, one malformed line skipped"
        );
        assert!(sidecar_loaded());

        // Config-named lookups see the sidecar…
        assert_eq!(
            var("LOOMWEAVE_TEST_SIDECAR_ONLY").as_deref(),
            Some("from-dotenv")
        );
        // …but the process environment — what children inherit and what every
        // `std::env::var` launcher-override read observes — is untouched.
        for name in [
            "LOOMWEAVE_TEST_SIDECAR_ONLY",
            "LOOMWEAVE_FILIGREE_MCP_COMMAND",
            "LD_PRELOAD",
        ] {
            assert!(
                std::env::var_os(name).is_none(),
                "{name} must never enter the process environment"
            );
        }
        let child = std::process::Command::new("sh")
            .args([
                "-c",
                "printf '%s' \"${LOOMWEAVE_TEST_SIDECAR_ONLY:-unset}\"",
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&child.stdout), "unset");

        // The real environment wins over the sidecar (never-clobber precedence).
        // PATH is always set, so it is a safe, deterministic probe.
        assert_eq!(var("PATH"), std::env::var("PATH").ok());

        // Idempotent: a second load neither re-reads nor grows the sidecar.
        std::fs::write(dir.path().join(".env"), "LOOMWEAVE_TEST_LATE=1\n").unwrap();
        assert_eq!(load_sidecar(), 3);
        assert_eq!(var("LOOMWEAVE_TEST_LATE"), None);
    }
}
