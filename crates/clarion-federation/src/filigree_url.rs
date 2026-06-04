//! Resolve the live Filigree API base URL.
//!
//! Mirrors Filigree's ethereal endpoint-discovery convention: the dashboard
//! publishes its live port to `<project>/.filigree/ephemeral.port` (a plain
//! integer, written atomically, present only while the dashboard runs) and
//! serves the read API on that port. The port is chosen deterministically but
//! unpredictably (`8400 + sha256(path) % 1000` with fallback), so it must be
//! *read*, never computed. This mirrors the Filigree sources:
//!   - `filigree/src/filigree/ephemeral.py::{write,read}_port_file`
//!   - `filigree/src/filigree/scanner_callback.py::resolve_scanner_api_url_with_source`
//!
//! Federation discipline (`docs/suite/loom.md` §5): this is enrich-only
//! connection discovery. Clarion stays solo-useful — when no live port file is
//! present (or Filigree is disabled) Clarion falls back to its *own* configured
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
/// The live ethereal port published by Filigree's running dashboard.
pub const SOURCE_EPHEMERAL_PORT: &str = ".filigree/ephemeral.port";
/// Clarion's own configured `integrations.filigree.base_url`.
pub const SOURCE_CONFIG: &str = "config";

/// The outcome of resolving where Clarion should reach Filigree's read API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FiligreeUrlResolution {
    /// Whether the Filigree integration is enabled in config at all.
    pub enabled: bool,
    /// The statically configured base URL (`integrations.filigree.base_url`).
    pub configured_url: String,
    /// The URL Clarion will actually call. `None` only when disabled.
    pub resolved_url: Option<String>,
    /// Which input produced [`Self::resolved_url`]; one of the `SOURCE_*` labels.
    pub source: &'static str,
}

/// Resolve the Filigree read-API base URL, preferring the live ethereal port.
///
/// - Disabled → no resolved URL, `source = "disabled"`.
/// - A valid `<project_root>/.filigree/ephemeral.port` → the configured URL
///   with its port overridden by the live port, `source = ".filigree/ephemeral.port"`.
/// - Otherwise → the configured URL unchanged, `source = "config"`.
#[must_use]
pub fn resolve_filigree_url(config: &FiligreeConfig, project_root: &Path) -> FiligreeUrlResolution {
    let configured_url = config.base_url.clone();
    if !config.enabled {
        return FiligreeUrlResolution {
            enabled: false,
            configured_url,
            resolved_url: None,
            source: SOURCE_DISABLED,
        };
    }
    match read_ephemeral_port(project_root) {
        Some(port) => {
            let resolved = override_port(&configured_url, port);
            FiligreeUrlResolution {
                enabled: true,
                configured_url,
                resolved_url: Some(resolved),
                source: SOURCE_EPHEMERAL_PORT,
            }
        }
        None => FiligreeUrlResolution {
            enabled: true,
            resolved_url: Some(configured_url.clone()),
            configured_url,
            source: SOURCE_CONFIG,
        },
    }
}

/// Read `<project_root>/.filigree/ephemeral.port` as a TCP port.
///
/// Mirrors Filigree's `read_port_file`: a plain trimmed integer. Any
/// missing/corrupt/out-of-range/zero content folds to `None` (fail-soft).
fn read_ephemeral_port(project_root: &Path) -> Option<u16> {
    let path = project_root.join(".filigree").join("ephemeral.port");
    let raw = std::fs::read_to_string(&path).ok()?;
    raw.trim().parse::<u16>().ok().filter(|port| *port != 0)
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

    fn write_port_file(root: &Path, contents: &str) {
        let dir = root.join(".filigree");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ephemeral.port"), contents).unwrap();
    }

    #[test]
    fn disabled_integration_resolves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let config = FiligreeConfig::default(); // enabled: false
        let res = resolve_filigree_url(&config, dir.path());
        assert!(!res.enabled);
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_DISABLED);
        assert_eq!(res.configured_url, "http://127.0.0.1:8766");
    }

    #[test]
    fn live_ephemeral_port_overrides_the_stale_configured_port() {
        // The dogfood bug: configured 8766 is dead; the live dashboard is on
        // 8542 per .filigree/ephemeral.port.
        let dir = tempfile::tempdir().unwrap();
        write_port_file(dir.path(), "8542\n");
        let res = resolve_filigree_url(&enabled_config(), dir.path());
        assert!(res.enabled);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8542"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
        // The configured URL is still reported verbatim alongside the resolved one.
        assert_eq!(res.configured_url, "http://127.0.0.1:8766");
    }

    #[test]
    fn falls_back_to_configured_url_when_no_port_file() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_filigree_url(&enabled_config(), dir.path());
        assert!(res.enabled);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8766"));
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn corrupt_port_file_folds_to_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        write_port_file(dir.path(), "not-a-port");
        let res = resolve_filigree_url(&enabled_config(), dir.path());
        assert_eq!(res.source, SOURCE_CONFIG);
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:8766"));
    }

    #[test]
    fn zero_port_is_rejected_as_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        write_port_file(dir.path(), "0");
        let res = resolve_filigree_url(&enabled_config(), dir.path());
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
}
