//! Embedded `clarion-workflow` skill pack and its on-disk installer.
//!
//! The pack is compiled into the binary with `include_str!` (matching the
//! `include_str!` migration-embedding convention in
//! `clarion-storage/src/schema.rs`). Each entry is `(relative_path, contents)`;
//! growing the pack with a `references/` directory is a data change here, not a
//! logic change. The fingerprint over the pack bytes drives drift-aware
//! re-copy (Phase 3 hook resync + `--skills` idempotency).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// `(relative_path, contents)` for every file in the bundled skill pack.
pub const SKILL_PACK: &[(&str, &str)] = &[(
    "SKILL.md",
    include_str!("../assets/skills/clarion-workflow/SKILL.md"),
)];

/// The on-disk subdirectory name the pack installs into.
pub const PACK_DIR_NAME: &str = "clarion-workflow";

/// Deterministic blake3 hex digest over the pack's `(rel_path, contents)`
/// entries. Order-stable because `SKILL_PACK` is a fixed slice. This is the
/// drift sentinel: [`installed_fingerprint`] recomputes the same digest over
/// the files actually on disk and re-copies when they differ. A `.fingerprint`
/// file is also written next to the installed pack as a human-readable
/// provenance marker, but it is informational only — it is not read back as
/// the drift signal (a stale sidecar cannot mask content corruption).
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
/// pack files as a provenance marker. If the digest recomputed from the files
/// on disk in every root (see [`installed_fingerprint`]) already equals the
/// embedded fingerprint, the call is a no-op (`copied: false`); any content
/// drift or missing file triggers a re-copy.
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

/// Recompute the fingerprint from the pack files actually on disk under
/// `dest`, so drift detection catches *content* corruption (a hand-edited or
/// truncated `SKILL.md`), not merely a stale `.fingerprint` sidecar. Returns
/// `None` if any pack file is missing or unreadable, which forces a rewrite.
fn installed_fingerprint(dest: &Path) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    for (rel, _) in SKILL_PACK {
        let contents = fs::read(dest.join(rel)).ok()?;
        hasher.update(rel.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(&contents);
        hasher.update(&[0u8]);
    }
    Some(hasher.finalize().to_hex().to_string())
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

    // Cleanup guard: if writing the staged files fails, remove the staging dir
    // before bubbling the error so we don't leak a `.clarion-workflow.tmp-*`
    // sibling. Matches the partial-state-cleanup precedent on the `.clarion/`
    // path in install.rs. The original error is preserved.
    if let Err(err) = write_staged_pack(&staging, fingerprint) {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }

    // Crash-safe swap: move the existing pack aside, rename the staged pack
    // into place, then drop the backup. On failure, restore the backup so the
    // project is never left without an installed skill.
    //
    // The only remaining failure window is between the two renames (back up,
    // then move the staged pack in). If the second rename fails we restore the
    // backup, so the previously-installed pack is always recoverable; we never
    // delete `dest` ahead of a rename that might not happen.
    let had_existing = dest.exists();
    let backup = root.join(format!(".clarion-workflow.bak-{}", std::process::id()));
    if had_existing {
        if backup.exists() {
            fs::remove_dir_all(&backup)
                .with_context(|| format!("clear stale backup {}", backup.display()))?;
        }
        fs::rename(dest, &backup).with_context(|| {
            format!(
                "back up existing {} -> {}",
                dest.display(),
                backup.display()
            )
        })?;
    }
    match fs::rename(&staging, dest) {
        Ok(()) => {
            if had_existing {
                let _ = fs::remove_dir_all(&backup);
            }
            Ok(())
        }
        Err(err) => {
            // Restore the previous pack so orientation is never left broken.
            if had_existing {
                let _ = fs::rename(&backup, dest);
            }
            let _ = fs::remove_dir_all(&staging);
            Err(anyhow::Error::new(err))
                .with_context(|| format!("swap staged pack into {}", dest.display()))
        }
    }
}

/// Write every pack file plus the `.fingerprint` sidecar into `staging`. Does
/// not touch `dest`; the crash-safe swap is performed by the caller.
fn write_staged_pack(staging: &Path, fingerprint: &str) -> Result<()> {
    for (rel, contents) in SKILL_PACK {
        let target = staging.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        fs::write(&target, contents).with_context(|| format!("write {}", target.display()))?;
    }
    fs::write(staging.join(FINGERPRINT_FILE), fingerprint)
        .with_context(|| format!("write fingerprint in {}", staging.display()))?;
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
    fn install_leaves_no_backup_dir_after_successful_recopy() {
        use super::install_skill_pack;
        let dir = tempfile::tempdir().unwrap();
        install_skill_pack(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(".claude/skills/clarion-workflow/SKILL.md"),
            "STALE",
        )
        .unwrap();
        install_skill_pack(dir.path()).unwrap(); // triggers the swap
        let root = dir.path().join(".claude/skills");
        for entry in std::fs::read_dir(&root).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(
                !name.to_string_lossy().contains(".clarion-workflow.bak-"),
                "leftover backup dir: {name:?}"
            );
        }
    }

    #[test]
    fn install_recopies_when_installed_pack_drifted() {
        use super::install_skill_pack;
        let dir = tempfile::tempdir().unwrap();
        install_skill_pack(dir.path()).unwrap();
        // Corrupt one installed copy + its fingerprint to simulate drift.
        let skill = dir.path().join(".claude/skills/clarion-workflow/SKILL.md");
        std::fs::write(&skill, "STALE").unwrap();
        let fp = dir
            .path()
            .join(".claude/skills/clarion-workflow/.fingerprint");
        std::fs::write(&fp, "deadbeef").unwrap();

        let report = install_skill_pack(dir.path()).unwrap();
        assert!(report.copied, "drift should trigger re-copy");
        let body = std::fs::read_to_string(&skill).unwrap();
        assert!(
            body.contains("name: clarion-workflow"),
            "drift not repaired"
        );
    }

    /// True when the filesystem actually enforces directory write permissions
    /// for this process. Returns `false` when running as root (DAC checks are
    /// bypassed), so permission-based failure injection below can be skipped
    /// rather than misreported as a pass. (clarion-86f4614c0b)
    #[cfg(unix)]
    fn perms_enforced() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = tempfile::tempdir().unwrap();
        let ro = probe.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        // If we can still create a file inside a read-only dir, perms are not
        // enforced for us (root) and the injection would not actually fail.
        std::fs::write(ro.join("probe"), b"x").is_err()
    }

    /// A failed re-install must be crash-safe: it surfaces the error, leaks no
    /// `.clarion-workflow.tmp-*` / `.bak-*` sibling, and never destroys the
    /// already-installed pack (the guarantee the stage/backup/restore cleanup
    /// code exists to protect). The failure is the only portable injection
    /// point — `stage_and_swap` clears any pre-seeded staging dir on entry — so
    /// we make the skill root read-only and force a drift re-copy into it.
    /// (clarion-86f4614c0b)
    #[cfg(unix)]
    #[test]
    fn failed_reinstall_is_crash_safe_and_leaks_no_temp() {
        use super::install_skill_pack;
        use std::os::unix::fs::PermissionsExt;

        if !perms_enforced() {
            eprintln!("skipping: directory permissions not enforced (running as root?)");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        install_skill_pack(dir.path()).expect("first install ok");
        let root = dir.path().join(".claude/skills");
        let skill_md = root.join("clarion-workflow/SKILL.md");

        // Force a drift so the next install attempts a swap, then make the skill
        // root read-only so staging the new pack into it fails with EACCES.
        // Drift is detected from pack *content* (installed_fingerprint rehashes
        // SKILL.md), so corrupt SKILL.md itself; the clarion-workflow child dir
        // stays writable, so this write succeeds before we lock the root.
        std::fs::write(&skill_md, "STALE — drifted content").unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = install_skill_pack(dir.path());

        let leaked: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                name.starts_with(".clarion-workflow.tmp-")
                    || name.starts_with(".clarion-workflow.bak-")
            })
            .collect();

        // Restore perms so tempdir cleanup succeeds.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_err(),
            "re-install into a read-only skill root must fail, not silently succeed"
        );
        assert!(
            leaked.is_empty(),
            "a failed re-install must leak no staging/backup sibling, found: {leaked:?}"
        );
        assert!(
            skill_md.exists(),
            "a failed re-install must not destroy the already-installed pack"
        );
    }
}
