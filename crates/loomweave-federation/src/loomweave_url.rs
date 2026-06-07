//! Resolve the live Loomweave read-API base URL (ADR-044).
//!
//! The reference reader of the `.weft/loomweave/ephemeral.port` file contract and
//! the twin of [`crate::filigree_url`]. Precedence (consumer-side): the
//! published live port wins over a configured URL, which wins over nothing.
//! (ADR-044's higher "explicit flag/env" precedence level is realized by each
//! consumer's own CLI/env handling — e.g. Wardline's `--loomweave-url` — not by
//! this library function.) Fail-soft throughout: a missing/corrupt file folds
//! to the configured URL; absent both, `None` (federation simply degrades).

use std::path::Path;

use crate::loomweave_port::read_published_port;

/// The live published port file `.weft/loomweave/ephemeral.port`.
pub const SOURCE_EPHEMERAL_PORT: &str = ".weft/loomweave/ephemeral.port";
/// A statically configured URL (e.g. `wardline.yaml: loomweave.url`).
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

/// Resolve the read-API URL, preferring the live published port over the
/// configured URL. `configured_url` is the consumer's static fallback (pass
/// `None` if it has none).
#[must_use]
pub fn resolve_loomweave_url(
    configured_url: Option<&str>,
    project_root: &Path,
) -> LoomweaveUrlResolution {
    if let Some(port) = read_published_port(project_root) {
        return LoomweaveUrlResolution {
            resolved_url: Some(format!("http://127.0.0.1:{port}")),
            source: SOURCE_EPHEMERAL_PORT,
        };
    }
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
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path());
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9412"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }

    #[test]
    fn falls_back_to_configured_url_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path());
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9111"));
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn corrupt_file_folds_to_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        let store = loomweave_core::store::store_dir(dir.path());
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("ephemeral.port"), "not-a-port").unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path());
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn nothing_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(None, dir.path());
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_NONE);
    }

    #[test]
    fn blank_config_with_no_file_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(Some("   "), dir.path());
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_NONE);
    }
}
