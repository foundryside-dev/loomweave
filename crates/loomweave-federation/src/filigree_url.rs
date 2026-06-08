//! Resolve the live Filigree API base URL.
//!
//! Mirrors Filigree's ethereal endpoint-discovery convention: the dashboard
//! publishes its live port to a per-project `ephemeral.port` file (a plain
//! integer, written atomically, present only while the dashboard runs) and
//! serves the read API on that port. The port is chosen deterministically but
//! unpredictably (`8400 + sha256(path) % 1000` with fallback), so it must be
//! *read*, never computed.
//!
//! **Location (Weft store consolidation, ADR-046):** Filigree publishes its
//! runtime state under the shared `.weft/<member>/` dotdir, so the port file
//! lives at `<project>/.weft/filigree/ephemeral.port` — the single location this
//! resolver reads. There is **no** fallback to the pre-consolidation
//! `.filigree/` path: after the coordinated cutover every sibling is at `.weft/`
//! by construction, so a port file found only on the legacy path means a
//! mis-sequenced cutover, and resolving it would silently bind to a stale dir
//! (the lacuna-401 failure mode). Instead the resolver folds to the configured
//! URL (`source = "config"`), and the wire-facing `source` label reports that —
//! a loud, visible signal rather than a quiet stale resolve.
//! This mirrors the Filigree sources:
//!   - `filigree/src/filigree/ephemeral.py::{write,read}_port_file`
//!   - `filigree/src/filigree/scanner_callback.py::resolve_scanner_api_url_with_source`
//!
//! Federation discipline (`docs/suite/weft.md` §5): this is enrich-only
//! connection discovery. Loomweave stays solo-useful — when no live port file is
//! present (or Filigree is disabled) Loomweave falls back to its *own* configured
//! `base_url`, never to a Filigree-internal default (copying Filigree's
//! `DEFAULT_PORT` would be a silent cross-product coupling). Reading the port
//! file is fail-soft: any missing/corrupt/out-of-range content degrades to the
//! configured URL.
//!
//! Scope: ethereal mode only. Filigree's `server` mode resolves through a
//! home-directory global (`~/.config/filigree/server.json`); that path is not
//! exercised here and is left as a known gap (clarion-318f1254eb tracks the
//! issues_for-side resolution diagnostics that build on this resolver).

use std::path::Path;

use serde::Serialize;

use crate::config::FiligreeConfig;

/// Wire-facing `source` labels for a resolved Filigree URL. Reported verbatim
/// by `project_status` (and, per clarion-318f1254eb, `issues_for`) so an agent
/// can tell *where* the URL came from without shelling out to probe ports.
pub const SOURCE_DISABLED: &str = "disabled";
/// The runtime environment override `WEFT_FILIGREE_URL` (the C-9 §2.2 rung-2
/// `WEFT_<X>_URL` spelling) — a per-process operator declaration that outranks
/// every durable/on-disk source.
pub const SOURCE_ENV: &str = "env:WEFT_FILIGREE_URL";
/// The operator-declared durable endpoint `weft.toml [filigree].url` (C-9 §2.2
/// rung-3). Outranks on-disk port discovery: it is the operator's explicit
/// "Filigree is here" (e.g. a remote host with no local `ephemeral.port`).
pub const SOURCE_WEFT_TOML: &str = "weft.toml";
/// The live ethereal port published by Filigree's running dashboard at the
/// consolidated `.weft/filigree/` location — the only location read (ADR-046).
pub const SOURCE_EPHEMERAL_PORT: &str = ".weft/filigree/ephemeral.port";
/// Loomweave's own configured `integrations.filigree.base_url`.
pub const SOURCE_CONFIG: &str = "config";

/// The outcome of resolving where Loomweave should reach Filigree's read API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FiligreeUrlResolution {
    /// Whether the Filigree integration is enabled in config at all.
    pub enabled: bool,
    /// The statically configured base URL (`integrations.filigree.base_url`).
    pub configured_url: String,
    /// The URL Loomweave will actually call. `None` only when disabled.
    pub resolved_url: Option<String>,
    /// Which input produced [`Self::resolved_url`]; one of the `SOURCE_*` labels.
    pub source: &'static str,
}

/// Resolve the Filigree read-API base URL along the C-9 §2.2 precedence ladder.
///
/// Highest wins, after the enabled short-circuit:
/// 1. `WEFT_FILIGREE_URL` env (`getenv`) → `source = "env:WEFT_FILIGREE_URL"`,
///    used verbatim — a per-process operator override.
/// 2. `weft.toml [filigree].url` → `source = "weft.toml"`, used verbatim — the
///    operator's durable declaration (e.g. a remote Filigree with no local
///    `ephemeral.port`). Outranks on-disk discovery by design (§2.2).
/// 3. A valid `<project_root>/.weft/filigree/ephemeral.port` → the configured
///    URL with its port overridden by the live port,
///    `source = ".weft/filigree/ephemeral.port"`.
/// 4. Otherwise → the configured URL unchanged, `source = "config"`. A port file
///    present only at the pre-consolidation `.filigree/` path is **not** read;
///    it folds here, so a mis-sequenced cutover is visible (not a stale
///    resolve).
///
/// - Disabled → no resolved URL, `source = "disabled"` (the env/weft.toml rungs
///   do not revive a disabled integration).
///
/// `getenv` is injected (rather than reading `std::env` directly) so the rung is
/// testable without mutating process env; production passes
/// `|name| std::env::var(name).ok()`. Both the env and `weft.toml` rungs are
/// fail-soft: a blank/absent value falls through to the next rung.
#[must_use]
pub fn resolve_filigree_url(
    config: &FiligreeConfig,
    project_root: &Path,
    getenv: impl Fn(&str) -> Option<String>,
) -> FiligreeUrlResolution {
    let configured_url = config.base_url.clone();
    if !config.enabled {
        return FiligreeUrlResolution {
            enabled: false,
            configured_url,
            resolved_url: None,
            source: SOURCE_DISABLED,
        };
    }
    // Rung 1: WEFT_FILIGREE_URL env, used verbatim.
    if let Some(url) = getenv("WEFT_FILIGREE_URL").filter(|u| !u.trim().is_empty()) {
        return FiligreeUrlResolution {
            enabled: true,
            configured_url,
            resolved_url: Some(url.trim().to_owned()),
            source: SOURCE_ENV,
        };
    }
    // Rung 2: weft.toml [filigree].url, used verbatim (outranks on-disk port).
    if let Some(url) = loomweave_core::store::sibling_url(project_root, "filigree") {
        return FiligreeUrlResolution {
            enabled: true,
            configured_url,
            resolved_url: Some(url),
            source: SOURCE_WEFT_TOML,
        };
    }
    // Rung 3: live ethereal port overrides the configured URL's port.
    match read_ephemeral_port(project_root) {
        Some((port, source)) => {
            let resolved = override_port(&configured_url, port);
            FiligreeUrlResolution {
                enabled: true,
                configured_url,
                resolved_url: Some(resolved),
                source,
            }
        }
        // Rung 4: configured base_url unchanged.
        None => FiligreeUrlResolution {
            enabled: true,
            resolved_url: Some(configured_url.clone()),
            configured_url,
            source: SOURCE_CONFIG,
        },
    }
}

/// Filigree's live published ephemeral port at the consolidated
/// `.weft/filigree/` location. `None` when it does not resolve (fail-soft). Use
/// this instead of reading the port file directly so the canonical-location
/// policy stays in one place.
#[must_use]
pub fn read_filigree_ephemeral_port(project_root: &Path) -> Option<u16> {
    read_ephemeral_port(project_root).map(|(port, _source)| port)
}

/// Read Filigree's published ephemeral port from the consolidated
/// `.weft/filigree/ephemeral.port` location (ADR-046). Returns the port and the
/// `SOURCE_EPHEMERAL_PORT` label.
///
/// Mirrors Filigree's `read_port_file`: a plain trimmed integer. Any
/// missing/corrupt/out-of-range/zero content folds to `None` (fail-soft). The
/// pre-consolidation `.filigree/` path is deliberately not consulted — see the
/// module docs.
fn read_ephemeral_port(project_root: &Path) -> Option<(u16, &'static str)> {
    let path = project_root
        .join(".weft")
        .join("filigree")
        .join("ephemeral.port");
    let raw = std::fs::read_to_string(&path).ok()?;
    let port = raw.trim().parse::<u16>().ok().filter(|port| *port != 0)?;
    Some((port, SOURCE_EPHEMERAL_PORT))
}

/// Replace the port in a `scheme://host[:port][/path]` URL, preserving the
/// scheme, host, and any trailing path. Returns the input unchanged when it
/// has no recognizable `scheme://` authority. IPv6 literal hosts are out of
/// scope — Filigree binds `127.0.0.1`.
fn override_port(base_url: &str, port: u16) -> String {
    let Some((scheme, rest)) = base_url.split_once("://") else {
        return base_url.to_owned();
    };
    let (authority, path) = match rest.find('/') {
        Some(slash) => (&rest[..slash], &rest[slash..]),
        None => (rest, ""),
    };
    // Strip an existing `:port` suffix, but only when it is genuinely a numeric
    // port (so a bare `host` with no port is preserved intact).
    let host = match authority.rsplit_once(':') {
        Some((host, maybe_port))
            if !maybe_port.is_empty() && maybe_port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            host
        }
        _ => authority,
    };
    format!("{scheme}://{host}:{port}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config() -> FiligreeConfig {
        FiligreeConfig {
            enabled: true,
            ..FiligreeConfig::default()
        }
    }

    fn write_weft_port_file(root: &Path, contents: &str) {
        let dir = root.join(".weft").join("filigree");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ephemeral.port"), contents).unwrap();
    }

    fn write_legacy_port_file(root: &Path, contents: &str) {
        let dir = root.join(".filigree");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ephemeral.port"), contents).unwrap();
    }

    #[test]
    fn disabled_integration_resolves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = FiligreeConfig::default(); // enabled: false
        let res = resolve_filigree_url(&config, dir.path(), |_| None);
        assert!(!res.enabled);
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_DISABLED);
        assert_eq!(res.configured_url, "http://127.0.0.1:8766");
    }

    #[test]
    fn live_ephemeral_port_overrides_the_stale_configured_port() {
        // The dogfood bug: configured 8766 is dead; the live dashboard is on
        // 8542 per the consolidated .weft/filigree/ephemeral.port.
        let dir = tempfile::tempdir().unwrap();
        write_weft_port_file(dir.path(), "8542\n");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| None);
        assert!(res.enabled);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8542"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
        // The configured URL is still reported verbatim alongside the resolved one.
        assert_eq!(res.configured_url, "http://127.0.0.1:8766");
    }

    #[test]
    fn legacy_filigree_port_is_not_resolved_after_clean_break() {
        // ADR-046 clean break: a sibling still on the pre-consolidation
        // `.filigree/` path is NOT read. The live legacy port is ignored and the
        // resolver folds to the configured URL, so `source == "config"` surfaces
        // the mis-sequenced cutover loudly instead of silently binding the stale
        // dir (the lacuna-401 wrong-but-quiet-resolve failure mode).
        let dir = tempfile::tempdir().unwrap();
        write_legacy_port_file(dir.path(), "8542\n");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| None);
        assert_eq!(res.source, SOURCE_CONFIG);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8766"));
    }

    #[test]
    fn falls_back_to_configured_url_when_no_port_file() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| None);
        assert!(res.enabled);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8766"));
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn corrupt_port_file_folds_to_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        write_weft_port_file(dir.path(), "not-a-port");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| None);
        assert_eq!(res.source, SOURCE_CONFIG);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8766"));
    }

    #[test]
    fn zero_port_is_rejected_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        write_weft_port_file(dir.path(), "0");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| None);
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn override_port_preserves_scheme_host_and_path() {
        assert_eq!(
            override_port("http://127.0.0.1:8766", 8542),
            "http://127.0.0.1:8542"
        );
        assert_eq!(
            override_port("http://localhost", 8542),
            "http://localhost:8542"
        );
        assert_eq!(
            override_port("https://example.test:1/api", 8542),
            "https://example.test:8542/api"
        );
    }

    #[test]
    fn override_port_returns_input_without_scheme() {
        assert_eq!(override_port("127.0.0.1:8766", 8542), "127.0.0.1:8766");
    }

    fn write_weft_url(root: &Path, member: &str, url: &str) {
        std::fs::write(
            root.join("weft.toml"),
            format!("[{member}]\nurl = \"{url}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn env_url_wins_verbatim_over_everything() {
        let dir = tempfile::tempdir().unwrap();
        // A live port AND a weft.toml url are present; the env override still wins.
        write_weft_port_file(dir.path(), "8542\n");
        write_weft_url(dir.path(), "filigree", "http://weft-host:1234");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |name| {
            (name == "WEFT_FILIGREE_URL").then(|| "http://env-host:9000".to_owned())
        });
        assert_eq!(res.resolved_url.as_deref(), Some("http://env-host:9000"));
        assert_eq!(res.source, SOURCE_ENV);
    }

    #[test]
    fn weft_toml_url_wins_verbatim_over_live_port() {
        // The operator's durable declaration (e.g. a remote Filigree) outranks
        // the on-disk live port (§2.2 rung-3 above rung-4).
        let dir = tempfile::tempdir().unwrap();
        write_weft_port_file(dir.path(), "8542\n");
        write_weft_url(dir.path(), "filigree", "http://remote-host:8749");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| None);
        assert_eq!(res.resolved_url.as_deref(), Some("http://remote-host:8749"));
        assert_eq!(res.source, SOURCE_WEFT_TOML);
    }

    #[test]
    fn blank_env_falls_through_to_lower_rungs() {
        let dir = tempfile::tempdir().unwrap();
        write_weft_port_file(dir.path(), "8542\n");
        let res = resolve_filigree_url(&enabled_config(), dir.path(), |_| Some("   ".to_owned()));
        // Blank env is skipped; the live port resolves.
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8542"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }

    #[test]
    fn disabled_is_not_revived_by_env_or_weft_toml() {
        let dir = tempfile::tempdir().unwrap();
        write_weft_url(dir.path(), "filigree", "http://remote-host:8749");
        let res = resolve_filigree_url(&FiligreeConfig::default(), dir.path(), |_| {
            Some("http://env-host:9000".to_owned())
        });
        assert!(!res.enabled);
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_DISABLED);
    }
}
