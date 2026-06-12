//! Canonical on-disk layout for Loomweave's per-project store.
//!
//! All of Loomweave's machine-written runtime state for a project lives under
//! `<project_root>/.weft/loomweave/` — the Weft config/store consolidation
//! convention (`.weft/<member>/`, a subtree owned exclusively by that member,
//! which never reads or writes a sibling's subtree). This module is the single
//! source of truth for that location: every consumer routes through it, so the
//! path can never drift across the workspace.
//!
//! This **supersedes the legacy `.loomweave/` directory** (ADR-005, amended by
//! ADR-046). The move is a *clean break* — there is no fallback read of the old
//! location.
//!
//! ## Operator override (`weft.toml`)
//!
//! The operator-authored `<project_root>/weft.toml` may relocate the store via a
//! member-private `[loomweave].store_dir` key (the canonical store-relocation key
//! across the federation). `weft.toml` is **read-only** to Loomweave — install,
//! doctor, and the CLI never write it (Gate `weft-eb3dee402f`: never add a writer
//! to a shared multi-section file). Loomweave reads its own `[loomweave]` table
//! in full, plus the allowlisted cross-read `url` key from a sibling's table
//! (see [`sibling_url`], C-9 §2.1); every other key in a sibling's section is
//! ignored, so the file stays forward-compatible as siblings add their own keys.
//!
//! Resolution is fail-soft (C-9c, normative): a missing OR malformed `weft.toml`
//! — parse error, wrong type, absent table/key, blank value — is treated as
//! absent, and the built-in default applies. It is never a hard failure.
//!
//! ### Override location constraint (`store_dir`)
//!
//! The source-walk, secret-scan, and pyright skip-lists exclude the whole
//! `.weft/` dotdir, so a store kept at the default (or any `store_dir` *inside*
//! `.weft/`) is never walked, scanned, or type-checked as project source. A
//! `store_dir` override is therefore **required to stay within `.weft/`, or else
//! be placed entirely outside the analyzed project root.** An override pointing
//! at a path *under the analyzed tree but outside `.weft/`* is a misconfiguration:
//! `loomweave.db` and its WAL would be walked and secret-scanned as if they were
//! source (clarion-6dd4b8bb85, ADR-046 Consequences). Auto-excluding an arbitrary
//! override location was considered and rejected as not worth the coupling — the
//! recommended override stays within `.weft/`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The shared Weft dotdir under a project root (`.weft/`). Each federation member
/// owns the `<member>/` subtree beneath it; a member never writes another
/// member's subtree, and deleting a sibling's subtree must not break it.
pub const WEFT_DIR: &str = ".weft";

/// Loomweave's member subdirectory name under [`WEFT_DIR`].
pub const MEMBER: &str = "loomweave";

/// The operator-authored federation config file, at the project root.
pub const WEFT_TOML: &str = "weft.toml";

/// `<project_root>/.weft/loomweave/` — Loomweave's exclusively-owned store dir.
///
/// Holds the committed analysis state (`loomweave.db`, `config.json`,
/// `.gitignore`, per-run metadata) and the git-ignored runtime sidecars
/// (`embeddings.db`, `ephemeral.port`, `instance_id`, `*.lock`, WAL files).
///
/// Honors a `[loomweave].store_dir` override in `weft.toml` when present (a
/// relative override resolves against `project_root`; an absolute one is used
/// verbatim). A missing or malformed `weft.toml` falls back to the built-in
/// default — see the module docs for the fail-soft contract.
#[must_use]
pub fn store_dir(project_root: &Path) -> PathBuf {
    match store_dir_override(project_root) {
        Some(dir) if dir.is_absolute() => dir,
        Some(dir) => project_root.join(dir),
        None => project_root.join(WEFT_DIR).join(MEMBER),
    }
}

/// `<project_root>/.weft/loomweave/loomweave.db` — the structural-graph store.
#[must_use]
pub fn db_path(project_root: &Path) -> PathBuf {
    store_dir(project_root).join("loomweave.db")
}

/// `<project_root>/.weft/loomweave/diagnostics/llm-traffic.jsonl` — the
/// metadata-only LLM lookup diagnostics log (rotated to `.1` at the size cap).
///
/// Lives under the member store dir like every other Loomweave-owned runtime
/// artifact (C-9: a member writes only its own `.weft/<member>/` subtree) —
/// the original `.loomweave/diagnostics/` literal predated the Weft store
/// cutover (weft-ac59e8e730).
#[must_use]
pub fn llm_traffic_log_path(project_root: &Path) -> PathBuf {
    store_dir(project_root)
        .join("diagnostics")
        .join("llm-traffic.jsonl")
}

/// Read the member-private `[loomweave].store_dir` override from `weft.toml`, if
/// any. Returns `None` (fail-soft, never an error) when the file is absent or
/// malformed, the `[loomweave]` table or `store_dir` key is absent, or the value
/// is blank.
fn store_dir_override(project_root: &Path) -> Option<PathBuf> {
    let store_dir = parse_weft_toml(project_root)?.loomweave?.store_dir?;
    let trimmed = store_dir.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Read an operator-declared sibling federation endpoint URL from `weft.toml`'s
/// cross-read schema (C-9 shared key layout, blessed; proposal §2.1/§2.2).
///
/// `member` is the sibling's canonical name — `"filigree"`, `"wardline"`,
/// `"legis"`, or Loomweave's own `"loomweave"` (a member reads its own full
/// `[loomweave]` table, and only the allowlisted `url` from a sibling's table).
/// The single cross-read key in v1 is `url`; everything else under a sibling's
/// table is private to that sibling and ignored here.
///
/// Returns `None` (fail-soft, never an error, never a write — Gate
/// `weft-eb3dee402f` / C-4) when `weft.toml` is absent or malformed, the
/// `[<member>]` table or its `url` is absent, or the value is blank. This is the
/// `weft.toml` rung of the sibling-endpoint precedence ladder; callers layer a
/// runtime flag/env above it and on-disk `ephemeral.port` discovery below it.
#[must_use]
pub fn sibling_url(project_root: &Path, member: &str) -> Option<String> {
    let parsed = parse_weft_toml(project_root)?;
    let section = match member {
        MEMBER => parsed.loomweave.map(|s| s.url),
        "filigree" => parsed.filigree.map(|s| s.url),
        "wardline" => parsed.wardline.map(|s| s.url),
        "legis" => parsed.legis.map(|s| s.url),
        _ => None,
    };
    let url = section??;
    let trimmed = url.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Read and parse `weft.toml` from the project root, fail-soft. Returns `None`
/// when the file is absent or unparseable. Parses only the tables/keys Loomweave
/// reads; unknown top-level tables and unknown keys are tolerated (no
/// `deny_unknown_fields`), so the file stays forward-compatible as siblings add
/// keys.
fn parse_weft_toml(project_root: &Path) -> Option<WeftToml> {
    let raw = std::fs::read_to_string(project_root.join(WEFT_TOML)).ok()?;
    match toml::from_str(&raw) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::debug!(error = %err, "weft.toml is malformed; treating as absent");
            None
        }
    }
}

/// The subset of `weft.toml` Loomweave reads: its own member-private table plus
/// the allowlisted cross-read `url` from each sibling's table. No
/// `deny_unknown_fields` — sibling tables and forward-compatible keys are
/// deliberately tolerated.
#[derive(Debug, Deserialize)]
struct WeftToml {
    loomweave: Option<LoomweaveSection>,
    filigree: Option<SiblingSection>,
    wardline: Option<SiblingSection>,
    legis: Option<SiblingSection>,
}

#[derive(Debug, Deserialize)]
struct LoomweaveSection {
    store_dir: Option<String>,
    /// Loomweave's own operator-declared federation endpoint (cross-read by
    /// siblings; also the `weft.toml` rung when Loomweave resolves its own URL).
    url: Option<String>,
}

/// A sibling member's table, of which Loomweave reads only the cross-read `url`.
#[derive(Debug, Deserialize)]
struct SiblingSection {
    url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_weft_loomweave_when_no_weft_toml() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join(".weft/loomweave"));
        assert_eq!(
            db_path(dir.path()),
            dir.path().join(".weft/loomweave/loomweave.db")
        );
    }

    #[test]
    fn llm_traffic_log_lives_under_the_member_store_dir() {
        // weft-ac59e8e730 (C-9): the diagnostics log is a Loomweave-owned
        // runtime artifact and must live under .weft/loomweave/, never a
        // legacy .loomweave/ root — and it must follow a store_dir override.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            llm_traffic_log_path(dir.path()),
            dir.path()
                .join(".weft/loomweave/diagnostics/llm-traffic.jsonl")
        );

        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[loomweave]\nstore_dir = \"custom/store\"\n",
        )
        .unwrap();
        assert_eq!(
            llm_traffic_log_path(dir.path()),
            dir.path()
                .join("custom/store/diagnostics/llm-traffic.jsonl")
        );
    }

    #[test]
    fn relative_override_resolves_against_project_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[loomweave]\nstore_dir = \"custom/store\"\n",
        )
        .unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join("custom/store"));
    }

    #[test]
    fn absolute_override_is_used_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[loomweave]\nstore_dir = \"/var/lib/loomweave\"\n",
        )
        .unwrap();
        assert_eq!(store_dir(dir.path()), Path::new("/var/lib/loomweave"));
    }

    #[test]
    fn sibling_tables_are_ignored() {
        // A sibling's section (and unknown keys in ours) must not make our read
        // reject the file — forward-compatible by design.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[filigree]\nbase_url = \"http://127.0.0.1:8766\"\n\n\
             [loomweave]\nstore_dir = \"s\"\nfuture_key = 42\n",
        )
        .unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join("s"));
    }

    #[test]
    fn malformed_toml_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(WEFT_TOML), "this is not = = toml [[[").unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join(".weft/loomweave"));
    }

    #[test]
    fn wrong_type_store_dir_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(WEFT_TOML), "[loomweave]\nstore_dir = 123\n").unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join(".weft/loomweave"));
    }

    #[test]
    fn absent_table_or_blank_value_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        // No [loomweave] table at all.
        std::fs::write(dir.path().join(WEFT_TOML), "[filigree]\nx = 1\n").unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join(".weft/loomweave"));
        // Present table, blank value.
        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[loomweave]\nstore_dir = \"   \"\n",
        )
        .unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join(".weft/loomweave"));
    }

    // ---- sibling_url (C-9 cross-read schema) --------------------------------

    #[test]
    fn sibling_url_reads_allowlisted_url_per_member() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[filigree]\nurl = \"http://127.0.0.1:8749\"\n\n\
             [loomweave]\nstore_dir = \"s\"\nurl = \"http://127.0.0.1:9111\"\n\n\
             [wardline]\nurl = \"http://127.0.0.1:7000\"\n",
        )
        .unwrap();
        assert_eq!(
            sibling_url(dir.path(), "filigree").as_deref(),
            Some("http://127.0.0.1:8749")
        );
        assert_eq!(
            sibling_url(dir.path(), "wardline").as_deref(),
            Some("http://127.0.0.1:7000")
        );
        // A member reads its OWN [loomweave].url too.
        assert_eq!(
            sibling_url(dir.path(), MEMBER).as_deref(),
            Some("http://127.0.0.1:9111")
        );
        // A sibling without a url, an unknown member, and legis (absent) → None.
        assert_eq!(sibling_url(dir.path(), "legis"), None);
        assert_eq!(sibling_url(dir.path(), "unknown"), None);
    }

    #[test]
    fn sibling_url_is_fail_soft() {
        let dir = tempfile::tempdir().unwrap();
        // Absent weft.toml.
        assert_eq!(sibling_url(dir.path(), "filigree"), None);
        // Malformed.
        std::fs::write(dir.path().join(WEFT_TOML), "not = = toml [[[").unwrap();
        assert_eq!(sibling_url(dir.path(), "filigree"), None);
        // Wrong type.
        std::fs::write(dir.path().join(WEFT_TOML), "[filigree]\nurl = 123\n").unwrap();
        assert_eq!(sibling_url(dir.path(), "filigree"), None);
        // Blank value.
        std::fs::write(dir.path().join(WEFT_TOML), "[filigree]\nurl = \"  \"\n").unwrap();
        assert_eq!(sibling_url(dir.path(), "filigree"), None);
        // Table present, url absent.
        std::fs::write(dir.path().join(WEFT_TOML), "[filigree]\nother = 1\n").unwrap();
        assert_eq!(sibling_url(dir.path(), "filigree"), None);
    }

    #[test]
    fn sibling_url_does_not_disturb_store_dir_reading() {
        // The extended schema still reads store_dir correctly alongside urls.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(WEFT_TOML),
            "[loomweave]\nstore_dir = \"custom/store\"\nurl = \"http://x\"\n",
        )
        .unwrap();
        assert_eq!(store_dir(dir.path()), dir.path().join("custom/store"));
    }
}
