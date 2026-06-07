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
//! to a shared multi-section file). Loomweave reads **only its own
//! `[loomweave]` table**; every other top-level table (a sibling's section) is
//! ignored, so the file stays forward-compatible as siblings add their own keys.
//!
//! Resolution is fail-soft (C-9c, normative): a missing OR malformed `weft.toml`
//! — parse error, wrong type, absent table/key, blank value — is treated as
//! absent, and the built-in default applies. It is never a hard failure.

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

/// Read the member-private `[loomweave].store_dir` override from `weft.toml`, if
/// any. Returns `None` (fail-soft, never an error) when the file is absent or
/// malformed, the `[loomweave]` table or `store_dir` key is absent, or the value
/// is blank.
fn store_dir_override(project_root: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(project_root.join(WEFT_TOML)).ok()?;
    // Parse only our own `[loomweave]` table; unknown top-level tables (a
    // sibling's section) are ignored by serde's default, so a future `[filigree]`
    // never makes this parse reject the file.
    let parsed: WeftToml = match toml::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "weft.toml is malformed; falling back to the default store dir"
            );
            return None;
        }
    };
    let store_dir = parsed.loomweave?.store_dir?;
    let trimmed = store_dir.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// The subset of `weft.toml` Loomweave reads: only its own member-private table.
/// No `deny_unknown_fields` — sibling tables and forward-compatible keys are
/// deliberately tolerated.
#[derive(Debug, Deserialize)]
struct WeftToml {
    loomweave: Option<LoomweaveSection>,
}

#[derive(Debug, Deserialize)]
struct LoomweaveSection {
    store_dir: Option<String>,
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
}
