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
}
