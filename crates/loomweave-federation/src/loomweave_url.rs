//! Resolve the live Loomweave read-API base URL (ADR-044).
//!
//! The reference reader of the `.weft/loomweave/ephemeral.port` file contract and
//! the twin of [`crate::filigree_url`]. Resolution walks the precedence
//! ladder (highest wins), reporting which rung produced the URL:
//!   1. `WEFT_LOOMWEAVE_URL` env (a per-process operator override), verbatim;
//!   2. the published `.weft/loomweave/ephemeral.port`;
//!   3. the consumer's configured URL; else
//!   4. nothing (`None`).
//!
//! There is deliberately no `weft.toml [loomweave].url` rung — retired for
//! the same security reason the `[filigree].url` rung was
//! (clarion-c1b3bea8af): repository content may be untrusted, and a repo
//! file must never steer where a consumer that attaches credentials sends
//! them. Operator overrides use the process environment
//! (`WEFT_LOOMWEAVE_URL`) or the consumer's private configuration.
//!
//! This supersedes the earlier ADR-044 division of labour (where the explicit
//! flag/env rung was each consumer's own job and this function read only the
//! port file): the env rung is now resolved here, with the env getter
//! injected so the rung stays testable. A runtime flag (e.g. Wardline's
//! `--loomweave-url`) still sits above all of these and is applied by the
//! consumer before calling. Fail-soft throughout: a blank/absent/corrupt value
//! at any rung falls through to the next (federation simply degrades).

use std::path::Path;

use crate::loomweave_port::{published_port_path, read_published_port_at};

/// The runtime environment override `WEFT_LOOMWEAVE_URL` (C-9 §2.2 rung-2
/// `WEFT_<X>_URL`) — a per-process operator declaration above every durable source.
pub const SOURCE_ENV: &str = "env:WEFT_LOOMWEAVE_URL";
/// The live published port file `.weft/loomweave/ephemeral.port`.
pub const SOURCE_EPHEMERAL_PORT: &str = ".weft/loomweave/ephemeral.port";
/// A statically configured URL from a consumer's own (private, non-repo)
/// config. Note: Wardline does *not* use this rung for Loomweave — its
/// `resolve_loomweave_url` reads no config-file URL key.
pub const SOURCE_CONFIG: &str = "config";
/// Neither a published file nor a configured URL — federation is absent.
pub const SOURCE_NONE: &str = "none";

/// Where a resolved Loomweave read-API URL came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoomweaveUrlResolution {
    /// The URL a consumer should call, or `None` when nothing resolves.
    pub resolved_url: Option<String>,
    /// One of the `SOURCE_*` labels.
    pub source: &'static str,
}

/// Resolve the Loomweave read-API URL along the C-9 §2.2 precedence ladder.
///
/// Highest wins:
/// 1. `WEFT_LOOMWEAVE_URL` env (`getenv`) → `source = "env:WEFT_LOOMWEAVE_URL"`,
///    verbatim.
/// 2. The live published `.weft/loomweave/ephemeral.port` → `http://127.0.0.1:<port>`.
/// 3. `configured_url` (the consumer's static fallback) → `source = "config"`.
/// 4. Nothing → `None`, `source = "none"`.
///
/// There is deliberately no `weft.toml [loomweave].url` rung — see the
/// module docs (retired like the `[filigree].url` rung, clarion-c1b3bea8af).
///
/// `getenv` is injected for testability; production passes
/// `|name| std::env::var(name).ok()`. Every rung is fail-soft: a blank/absent
/// value falls through to the next.
///
/// The port rung re-derives `.weft/loomweave/ephemeral.port` from
/// `project_root` via `published_port_path` — correct for a standalone
/// checkout or the main worktree, but NOT for a linked `git worktree`, whose
/// `serve` publishes to its isolated `StorePaths::port` instead
/// (`http_read.rs`'s `spawn`). A caller that has already resolved a
/// `WorktreeContext` must call [`resolve_loomweave_url_at`] with
/// `store_paths.port` instead of this function — see
/// `loomweave-mcp/src/tools/status.rs`'s `loomweave_read_api_json` and
/// `loomweave-cli/src/doctor.rs`.
#[must_use]
pub fn resolve_loomweave_url(
    configured_url: Option<&str>,
    project_root: &Path,
    getenv: impl Fn(&str) -> Option<String>,
) -> LoomweaveUrlResolution {
    resolve_loomweave_url_at(configured_url, &published_port_path(project_root), getenv)
}

/// [`resolve_loomweave_url`], but the port rung reads the *given* port-file
/// path directly instead of re-deriving `.weft/loomweave/ephemeral.port`
/// from a project root. (With the `weft.toml` rung retired,
/// clarion-c1b3bea8af, no rung reads a project root at all.)
#[must_use]
pub fn resolve_loomweave_url_at(
    configured_url: Option<&str>,
    port_path: &Path,
    getenv: impl Fn(&str) -> Option<String>,
) -> LoomweaveUrlResolution {
    // Rung 1: WEFT_LOOMWEAVE_URL env, verbatim.
    if let Some(url) = getenv("WEFT_LOOMWEAVE_URL").filter(|u| !u.trim().is_empty()) {
        return LoomweaveUrlResolution {
            resolved_url: Some(url.trim().to_owned()),
            source: SOURCE_ENV,
        };
    }
    // Deliberately NO weft.toml [loomweave].url rung here
    // (clarion-c1b3bea8af, owner-ratified 2026-07-29): repository content
    // may be untrusted, and a repo file must never steer where a consumer
    // that attaches credentials sends them — the same reason the
    // [filigree].url rung was removed. Operator overrides use the process
    // environment (WEFT_LOOMWEAVE_URL) or the consumer's private config.
    //
    // Rung 2: live published port.
    if let Some(port) = read_published_port_at(port_path) {
        return LoomweaveUrlResolution {
            resolved_url: Some(format!("http://127.0.0.1:{port}")),
            source: SOURCE_EPHEMERAL_PORT,
        };
    }
    // Rung 3/4: configured fallback, else nothing.
    match configured_url {
        Some(url) if !url.trim().is_empty() => LoomweaveUrlResolution {
            resolved_url: Some(url.to_owned()),
            source: SOURCE_CONFIG,
        },
        _ => LoomweaveUrlResolution {
            resolved_url: None,
            source: SOURCE_NONE,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loomweave_port::publish_port;

    #[test]
    fn published_port_beats_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |_| None);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9412"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }

    /// worktree-index Task 7 fix-loop finding 2: `resolve_loomweave_url_at`'s
    /// whole reason to exist is that rung 3 must check an EXPLICIT port
    /// path, not one re-derived from `project_root` — the exact bug that
    /// made `loomweave_read_api_json` report `null` for a linked worktree's
    /// live HTTP API (`serve` publishes to `StorePaths::port`, an isolated
    /// path with no fixed relationship to `published_port_path(project_root)`).
    /// This proves the decoupling directly: publish to an arbitrary path
    /// that shares no prefix with `project_root`, and confirm it still wins
    /// over the configured fallback.
    #[test]
    fn resolve_loomweave_url_at_reads_the_given_port_path_not_project_root() {
        let project_dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let isolated_port_path = elsewhere.path().join("isolated").join("ephemeral.port");
        crate::loomweave_port::publish_port_at(&isolated_port_path, 9777).unwrap();

        // Sanity: project_root's own (default) port path is untouched.
        assert!(!published_port_path(project_dir.path()).exists());

        let res =
            resolve_loomweave_url_at(Some("http://127.0.0.1:9111"), &isolated_port_path, |_| None);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9777"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }

    /// `resolve_loomweave_url` (the unrouted convenience wrapper, still used
    /// by main/standalone call sites) must be byte-identical to
    /// `resolve_loomweave_url_at` given `published_port_path(project_root)`
    /// — i.e. wrapping introduced no behavior change for existing callers.
    #[test]
    fn resolve_loomweave_url_wrapper_matches_resolve_loomweave_url_at_default_port_path() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9413).unwrap();
        let via_wrapper =
            resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |_| None);
        let via_at = resolve_loomweave_url_at(
            Some("http://127.0.0.1:9111"),
            &published_port_path(dir.path()),
            |_| None,
        );
        assert_eq!(via_wrapper, via_at);
    }

    #[test]
    fn falls_back_to_configured_url_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |_| None);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9111"));
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn corrupt_file_folds_to_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        let store = loomweave_core::store::store_dir(dir.path());
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("ephemeral.port"), "not-a-port").unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |_| None);
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn nothing_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(None, dir.path(), |_| None);
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_NONE);
    }

    #[test]
    fn blank_config_with_no_file_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(Some("   "), dir.path(), |_| None);
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_NONE);
    }

    fn write_weft_loomweave_url(root: &Path, url: &str) {
        std::fs::write(
            root.join("weft.toml"),
            format!("[loomweave]\nurl = \"{url}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn env_url_wins_verbatim_over_published_port_and_weft_toml() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).unwrap();
        write_weft_loomweave_url(dir.path(), "http://weft-host:1234");
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |name| {
            (name == "WEFT_LOOMWEAVE_URL").then(|| "http://env-host:9000".to_owned())
        });
        assert_eq!(res.resolved_url.as_deref(), Some("http://env-host:9000"));
        assert_eq!(res.source, SOURCE_ENV);
    }

    /// clarion-c1b3bea8af (owner-ratified 2026-07-29): the weft.toml
    /// [loomweave].url rung is retired — repository content must never
    /// steer where a credentialed consumer sends its credentials, the same
    /// posture as the [filigree].url removal. A weft.toml URL is IGNORED
    /// and resolution proceeds down the remaining ladder.
    #[test]
    fn weft_toml_url_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).unwrap();
        write_weft_loomweave_url(dir.path(), "http://attacker-or-stale-host:9111");
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |_| None);
        assert_eq!(
            res.resolved_url.as_deref(),
            Some("http://127.0.0.1:9412"),
            "the live published port must win; weft.toml must not be consulted"
        );
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }

    #[test]
    fn blank_env_falls_through_to_published_port() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).unwrap();
        let res = resolve_loomweave_url(None, dir.path(), |_| Some("  ".to_owned()));
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9412"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }
}
