//! Embedded `clarion-workflow` skill pack and its on-disk installer.
//!
//! The pack is compiled into the binary with `include_str!` (matching the
//! `include_str!` migration-embedding convention in
//! `clarion-storage/src/schema.rs`). Each entry is `(relative_path, contents)`;
//! growing the pack with a `references/` directory is a data change here, not a
//! logic change. The fingerprint over the pack bytes drives drift-aware
//! re-copy (Phase 3 hook resync + `--skills` idempotency).

/// `(relative_path, contents)` for every file in the bundled skill pack.
pub const SKILL_PACK: &[(&str, &str)] = &[(
    "SKILL.md",
    include_str!("../assets/skills/clarion-workflow/SKILL.md"),
)];

/// The on-disk subdirectory name the pack installs into.
pub const PACK_DIR_NAME: &str = "clarion-workflow";

/// Deterministic blake3 hex digest over the pack's `(rel_path, contents)`
/// entries. Order-stable because `SKILL_PACK` is a fixed slice. Used as the
/// drift sentinel: a `.fingerprint` file written next to the installed pack
/// records the version on disk.
#[must_use]
pub fn pack_fingerprint() -> String {
    let mut hasher = blake3::Hasher::new();
    for (rel, contents) in SKILL_PACK {
        hasher.update(rel.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(contents.as_bytes());
        hasher.update(&[0u8]);
    }
    hasher.finalize().to_hex().to_string()
}

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// The two skill roots Clarion installs into, relative to the project root.
/// `.claude/skills/` is read by Claude Code; `.agents/skills/` is the
/// tool-agnostic convention so non-Claude agent harnesses find it too.
const SKILL_ROOTS: &[&str] = &[".claude/skills", ".agents/skills"];

const FINGERPRINT_FILE: &str = ".fingerprint";

/// Outcome of an [`install_skill_pack`] call.
#[derive(Debug, Clone, Copy)]
pub struct SkillInstallReport {
    /// True if the pack bytes were (re)written this call; false if every
    /// destination already matched the embedded fingerprint.
    pub copied: bool,
}

/// Install (or re-sync on drift) the embedded skill pack into both skill roots
/// under `project_root`, idempotently.
///
/// For each root, the pack lands at `<root>/clarion-workflow/`. A
/// `.fingerprint` file recording [`pack_fingerprint`] is written alongside the
/// pack files. If every root's `.fingerprint` already equals the embedded
/// fingerprint, the call is a no-op (`copied: false`).
///
/// Writes are atomic: each pack is staged into a sibling temp directory in the
/// same skill root (so `rename` stays on one filesystem), then `rename`d over
/// the destination.
///
/// # Errors
///
/// Returns an error if any directory create, temp write, or rename fails.
pub fn install_skill_pack(project_root: &Path) -> Result<SkillInstallReport> {
    let fingerprint = pack_fingerprint();
    let mut copied = false;
    for root_rel in SKILL_ROOTS {
        let root = project_root.join(root_rel);
        let dest = root.join(PACK_DIR_NAME);
        if installed_fingerprint(&dest).as_deref() == Some(fingerprint.as_str()) {
            continue;
        }
        stage_and_swap(&root, &dest, &fingerprint)
            .with_context(|| format!("install skill pack into {}", dest.display()))?;
        copied = true;
    }
    Ok(SkillInstallReport { copied })
}

fn installed_fingerprint(dest: &Path) -> Option<String> {
    fs::read_to_string(dest.join(FINGERPRINT_FILE))
        .ok()
        .map(|s| s.trim().to_owned())
}

fn stage_and_swap(root: &Path, dest: &Path, fingerprint: &str) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("mkdir {}", root.display()))?;
    // Stage in a sibling temp dir so the final rename is same-filesystem.
    let staging = root.join(format!(".clarion-workflow.tmp-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("clear stale staging {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("mkdir {}", staging.display()))?;
    for (rel, contents) in SKILL_PACK {
        let target = staging.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        fs::write(&target, contents).with_context(|| format!("write {}", target.display()))?;
    }
    fs::write(staging.join(FINGERPRINT_FILE), fingerprint)
        .with_context(|| format!("write fingerprint in {}", staging.display()))?;
    // Remove any prior install, then move the staged pack into place.
    if dest.exists() {
        fs::remove_dir_all(dest).with_context(|| format!("remove old {}", dest.display()))?;
    }
    fs::rename(&staging, dest)
        .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SKILL_PACK, pack_fingerprint};

    #[test]
    fn pack_contains_skill_md_with_frontmatter() {
        let (rel, contents) = SKILL_PACK
            .iter()
            .find(|(rel, _)| *rel == "SKILL.md")
            .expect("SKILL.md present in pack");
        assert_eq!(*rel, "SKILL.md");
        assert!(
            contents.contains("name: clarion-workflow"),
            "SKILL.md is missing its frontmatter name"
        );
    }

    #[test]
    fn fingerprint_is_stable_and_64_hex_chars() {
        let fp1 = pack_fingerprint();
        let fp2 = pack_fingerprint();
        assert_eq!(fp1, fp2, "fingerprint must be deterministic");
        assert_eq!(fp1.len(), 64, "blake3 hex digest is 64 chars");
        assert!(fp1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn install_writes_pack_into_both_skill_roots() {
        use super::install_skill_pack;
        let dir = tempfile::tempdir().unwrap();
        let report = install_skill_pack(dir.path()).expect("install ok");
        assert!(report.copied, "first install should copy");

        for root in [".claude/skills", ".agents/skills"] {
            let skill = dir
                .path()
                .join(root)
                .join("clarion-workflow")
                .join("SKILL.md");
            assert!(skill.exists(), "missing {}", skill.display());
            let body = std::fs::read_to_string(&skill).unwrap();
            assert!(body.contains("name: clarion-workflow"));
        }
    }

    #[test]
    fn install_is_idempotent_when_fingerprint_matches() {
        use super::install_skill_pack;
        let dir = tempfile::tempdir().unwrap();
        let first = install_skill_pack(dir.path()).unwrap();
        assert!(first.copied);
        let second = install_skill_pack(dir.path()).unwrap();
        assert!(!second.copied, "second install should be a no-op on match");
    }

    #[test]
    fn install_recopies_when_installed_pack_drifted() {
        use super::install_skill_pack;
        let dir = tempfile::tempdir().unwrap();
        install_skill_pack(dir.path()).unwrap();
        // Corrupt one installed copy + its fingerprint to simulate drift.
        let skill = dir
            .path()
            .join(".claude/skills/clarion-workflow/SKILL.md");
        std::fs::write(&skill, "STALE").unwrap();
        let fp = dir
            .path()
            .join(".claude/skills/clarion-workflow/.fingerprint");
        std::fs::write(&fp, "deadbeef").unwrap();

        let report = install_skill_pack(dir.path()).unwrap();
        assert!(report.copied, "drift should trigger re-copy");
        let body = std::fs::read_to_string(&skill).unwrap();
        assert!(body.contains("name: clarion-workflow"), "drift not repaired");
    }
}
