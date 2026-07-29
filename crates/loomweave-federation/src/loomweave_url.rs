//! Resolve the live Loomweave read-API base URL (ADR-044).
//!
//! The reference reader of the `.weft/loomweave/ephemeral.port` file contract and
//! the twin of [`crate::filigree_url`]. Resolution walks the C-9 §2.2
//! precedence ladder (highest wins), reporting which rung produced the URL:
//!   1. `WEFT_LOOMWEAVE_URL` env (a per-process operator override), verbatim;
//!   2. `weft.toml [loomweave].url` (the operator's durable declaration),
//!      verbatim — deliberately above on-disk discovery, since a remote
//!      Loomweave has no local `ephemeral.port`;
//!   3. the published `.weft/loomweave/ephemeral.port`;
//!   4. the consumer's configured URL; else
//!   5. nothing (`None`).
//!
//! This supersedes the earlier ADR-044 division of labour (where the explicit
//! flag/env rung was each consumer's own job and this function read only the
//! port file): the env + `weft.toml` rungs are now resolved here, with the env
//! getter injected so the rung stays testable. A runtime flag (e.g. Wardline's
//! `--loomweave-url`) still sits above all of these and is applied by the
//! consumer before calling. Fail-soft throughout: a blank/absent/corrupt value
//! at any rung falls through to the next (federation simply degrades).

use std::path::Path;

use crate::loomweave_port::{published_port_path, read_published_port_at};

/// The runtime environment override `WEFT_LOOMWEAVE_URL` (C-9 §2.2 rung-2
/// `WEFT_<X>_URL`) — a per-process operator declaration above every durable source.
pub const SOURCE_ENV: &str = "env:WEFT_LOOMWEAVE_URL";
/// The operator-declared durable endpoint `weft.toml [loomweave].url` (C-9 §2.2
/// rung-3). Outranks on-disk port discovery: the operator's explicit
/// "Loomweave is here" (e.g. a remote host with no local `ephemeral.port`).
pub const SOURCE_WEFT_TOML: &str = "weft.toml";
/// The live published port file `.weft/loomweave/ephemeral.port`.
pub const SOURCE_EPHEMERAL_PORT: &str = ".weft/loomweave/ephemeral.port";
/// A statically configured URL from a consumer's own config (distinct from the
/// `weft.toml` rung above). Note: Wardline does *not* use this rung for
/// Loomweave — its `resolve_loomweave_url` reads no config-file URL key.
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
/// 2. `weft.toml [loomweave].url` → `source = "weft.toml"`, verbatim — the
///    operator's durable declaration; outranks on-disk discovery (§2.2).
/// 3. The live published `.weft/loomweave/ephemeral.port` → `http://127.0.0.1:<port>`.
/// 4. `configured_url` (the consumer's static fallback) → `source = "config"`.
/// 5. Nothing → `None`, `source = "none"`.
///
/// `getenv` is injected for testability; production passes
/// `|name| std::env::var(name).ok()`. Every rung is fail-soft: a blank/absent
/// value falls through to the next.
///
/// Rung 3 re-derives `.weft/loomweave/ephemeral.port` from `project_root` via
/// `published_port_path` — correct for a standalone checkout or the main
/// worktree, but NOT for a linked `git worktree`, whose `serve` publishes to
/// its isolated `StorePaths::port` instead (`http_read.rs`'s `spawn`). A
/// caller that has already resolved a `WorktreeContext` must call
/// [`resolve_loomweave_url_at`] with `store_paths.port` instead of this
/// function — see `loomweave-mcp/src/tools/status.rs`'s
/// `loomweave_read_api_json` and `loomweave-cli/src/doctor.rs`.
#[must_use]
pub fn resolve_loomweave_url(
    configured_url: Option<&str>,
    project_root: &Path,
    getenv: impl Fn(&str) -> Option<String>,
) -> LoomweaveUrlResolution {
    resolve_loomweave_url_at(
        configured_url,
        project_root,
        &published_port_path(project_root),
        getenv,
    )
}

/// [`resolve_loomweave_url`], but rung 3 reads the *given* port-file path
/// directly instead of re-deriving `.weft/loomweave/ephemeral.port` from
/// `project_root`. Rung 2 (`weft.toml [loomweave].url`) still reads from
/// `project_root` unconditionally — `weft.toml` is a checked-in repo config
/// file every worktree checkout of a repository shares byte-for-byte, not a
/// per-store artifact, so re-deriving it from `project_root` is correct even
/// for a linked worktree.
#[must_use]
pub fn resolve_loomweave_url_at(
    configured_url: Option<&str>,
    project_root: &Path,
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
    // Rung 2: weft.toml [loomweave].url, verbatim (outranks on-disk port).
    //
    // SECURITY INVARIANT (clarion-c1b3bea8af): `weft.toml` is repository
    // content and may be untrusted. The analogous `[filigree].url` rung was
    // REMOVED on the 1.5.0 line because Filigree clients attach
    // operator-owned bearer tokens to the resolved endpoint — a repo file
    // must never steer where credentials are sent. This rung survives only
    // because every current consumer is unauthenticated (doctor's
    // `http.config` display and `project_status`'s read-API block attach
    // nothing). Any future consumer that attaches credentials to a resolved
    // Loomweave URL MUST either refuse `SOURCE_WEFT_TOML`-sourced results
    // (the `source` field exists to make that check one comparison) or this
    // rung must be retired the way the Filigree one was.
    if let Some(url) =
        loomweave_core::store::sibling_url(project_root, loomweave_core::store::MEMBER)
    {
        return LoomweaveUrlResolution {
            resolved_url: Some(url),
            source: SOURCE_WEFT_TOML,
        };
    }
    // Rung 3: live published port.
    if let Some(port) = read_published_port_at(port_path) {
        return LoomweaveUrlResolution {
            resolved_url: Some(format!("http://127.0.0.1:{port}")),
            source: SOURCE_EPHEMERAL_PORT,
        };
    }
    // Rung 4/5: configured fallback, else nothing.
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

        let res = resolve_loomweave_url_at(
            Some("http://127.0.0.1:9111"),
            project_dir.path(),
            &isolated_port_path,
            |_| None,
        );
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
            dir.path(),
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

    #[test]
    fn weft_toml_url_wins_verbatim_over_published_port() {
        // Operator's durable [loomweave].url outranks the live local port (§2.2).
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).unwrap();
        write_weft_loomweave_url(dir.path(), "http://remote-host:9111");
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path(), |_| None);
        assert_eq!(res.resolved_url.as_deref(), Some("http://remote-host:9111"));
        assert_eq!(res.source, SOURCE_WEFT_TOML);
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
