//! Duplicate entity-locator detection (clarion-b19fe90c3e).
//!
//! The writer absorbs a colliding entity id via `ON CONFLICT(id) DO UPDATE` —
//! deliberately: that upsert is load-bearing for incremental re-analysis of
//! unchanged entities. The cost is that a GENUINE collision (two distinct
//! source declarations assembling the same locator) is silent last-write-wins
//! data loss: the stored entity becomes a chimera of both declarations. This
//! guard is the standing alarm for that shape — host-side and plugin-agnostic,
//! so it covers every language plugin without a protocol change.
//!
//! Two detection rules, both scoped to one plugin's emissions within one run
//! (entity ids are plugin-prefixed, so cross-plugin collisions cannot exist):
//!
//! 1. **In-run** — the same id emitted twice by this run, whether from one
//!    file (a plugin bug) or from different files (the classic chimera).
//! 2. **Cross-run** — a re-analyzed file emits an id whose stored owner is a
//!    SKIPPED-unchanged file of this run. The skipped file's row survives the
//!    run untouched, so both declarations claim the id. A genuine move never
//!    matches: the old file either changed (then it was re-dispatched and the
//!    in-run rule governs) or was deleted/excluded (then its entities are
//!    orphan-deleted by the SEI pass and the claim dies this run).
//!
//! Carve-out: `file_scope` (module) dual-claims are a legitimate language
//! shape (clarion-6ec7317628 — an inline `mod sub {}` facade colliding with
//! the path-derived module of `sub/mod.rs`) that the first-claim-wins
//! machinery already reconciles, in-run and across runs. Those suppressed
//! cross-file emissions stay silent here. A `file_scope` id re-emitted by the
//! SAME file is not a dual declaration, though — that is a plugin bug, and it
//! IS flagged.
//!
//! Exactly one finding per colliding id per run; the alarm detects, it never
//! blocks (the run outcome is unchanged and the upsert still applies).

use std::collections::{BTreeMap, HashMap, HashSet};

use loomweave_core::HostFinding;

/// Plugin-neutral rule id (deliberately not `LMWV-RUST-*`/`LMWV-PY-*`; the
/// legacy plugin-prefixed names are a known wart, clarion-a65cb18b02).
/// Severity is ERROR — see `infra_severity` — because the absorbed collision
/// is silent data loss.
pub(crate) const DUPLICATE_LOCATOR_RULE_ID: &str = "LMWV-DUPLICATE-LOCATOR";

/// Per-plugin, per-run duplicate-locator tracker. One `HashMap<id, path>`
/// across the run's entities is fine at the 100k-entity scale `analyze`
/// targets.
pub(crate) struct DuplicateLocatorGuard {
    /// id → first-seen `source_file_path` among this run's accepted emissions.
    first_seen: HashMap<String, String>,
    /// id → owning `source_file_path` for entities anchored in this plugin's
    /// SKIPPED-unchanged files (canonical stored form). Empty on a full run.
    prior_owners: BTreeMap<String, String>,
    /// ids already reported this run — one finding per id, not per occurrence.
    reported: HashSet<String>,
}

impl DuplicateLocatorGuard {
    pub(crate) fn new(prior_owners: BTreeMap<String, String>) -> Self {
        Self {
            first_seen: HashMap::new(),
            prior_owners,
            reported: HashSet::new(),
        }
    }

    /// Record an accepted (non-suppressed) emission of `entity_id` anchored at
    /// `source_file_path`; returns a collision finding when this emission
    /// collides in-run with an earlier one, or cross-run with an entity owned
    /// by a skipped-unchanged file.
    ///
    /// `file_scope` entities skip the cross-run rule: a `file_scope` id owned by
    /// a skipped file is pre-seeded into the dual-claim set, so its re-emission
    /// is suppressed before ever reaching this method — reaching here means
    /// this is the run's first, claiming emission.
    pub(crate) fn record(
        &mut self,
        entity_id: &str,
        source_file_path: &str,
        file_scope: bool,
    ) -> Option<HostFinding> {
        if let Some(first) = self.first_seen.get(entity_id) {
            let shape = if first == source_file_path {
                CollisionShape::SameFile
            } else {
                CollisionShape::CrossFile
            };
            let first = first.clone();
            return self.report(entity_id, &first, source_file_path, shape);
        }
        self.first_seen
            .insert(entity_id.to_owned(), source_file_path.to_owned());
        if !file_scope
            && let Some(owner) = self.prior_owners.get(entity_id)
            // Defensive: a skipped file is never re-dispatched (the partition
            // is disjoint), so owner == emitter can only be a path-form
            // coincidence — never a collision. Fail silent, not noisy.
            && owner != source_file_path
        {
            let owner = owner.clone();
            return self.report(
                entity_id,
                &owner,
                source_file_path,
                CollisionShape::CrossRun,
            );
        }
        None
    }

    /// Record a `file_scope` emission that the dual-claim first-claim-wins
    /// machinery suppressed. Cross-file dual declarations (and re-emissions
    /// against a prior claim held by a skipped file) are the reconciled,
    /// legitimate shape — silent. A SAME-file re-emission is a plugin bug and
    /// returns a finding.
    pub(crate) fn record_suppressed_file_scope(
        &mut self,
        entity_id: &str,
        source_file_path: &str,
    ) -> Option<HostFinding> {
        match self.first_seen.get(entity_id) {
            Some(first) if first == source_file_path => {
                let first = first.clone();
                self.report(
                    entity_id,
                    &first,
                    source_file_path,
                    CollisionShape::SameFile,
                )
            }
            _ => None,
        }
    }

    /// Build the finding, once per id per run.
    fn report(
        &mut self,
        entity_id: &str,
        first_path: &str,
        second_path: &str,
        shape: CollisionShape,
    ) -> Option<HostFinding> {
        if !self.reported.insert(entity_id.to_owned()) {
            return None;
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("entity_id".to_owned(), entity_id.to_owned());
        metadata.insert("first_source_file_path".to_owned(), first_path.to_owned());
        metadata.insert(
            "colliding_source_file_path".to_owned(),
            second_path.to_owned(),
        );
        metadata.insert("shape".to_owned(), shape.as_str().to_owned());
        // Anchor the finding to a real file (the first-seen declaration) so it
        // carries a `source_file_path` and reaches Filigree's scan-results emit.
        // Without this, `host_finding_anchor_id` falls back to the file-less
        // project anchor (`core:project:*`), which the emit skips as
        // `skipped_no_path` — leaving the duplicate-locator lacuna untrackable
        // in Filigree (the residual half of the dogfood's Friction A).
        metadata.insert("anchor_file_path".to_owned(), first_path.to_owned());
        Some(HostFinding {
            subcode: DUPLICATE_LOCATOR_RULE_ID.to_owned(),
            message: format!(
                "duplicate entity locator {entity_id}: declared in {first_path} and {second_path}; \
                 the store keeps one row per id (last write wins), so one declaration silently \
                 shadows the other"
            ),
            metadata,
        })
    }
}

/// Which collision rule fired — carried as `shape` metadata so a consumer can
/// triage a plugin bug (same file) apart from a source-level collision
/// (cross file / cross run) without parsing the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollisionShape {
    SameFile,
    CrossFile,
    CrossRun,
}

impl CollisionShape {
    fn as_str(self) -> &'static str {
        match self {
            Self::SameFile => "in_run_same_file",
            Self::CrossFile => "in_run_cross_file",
            Self::CrossRun => "cross_run_unchanged_file",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owners(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, path)| ((*id).to_owned(), (*path).to_owned()))
            .collect()
    }

    fn shape_of(finding: &HostFinding) -> &str {
        finding.metadata.get("shape").map(String::as_str).unwrap()
    }

    #[test]
    fn unique_ids_stay_silent() {
        let mut guard = DuplicateLocatorGuard::new(BTreeMap::new());
        assert!(guard.record("p:fn:a", "a.rs", false).is_none());
        assert!(guard.record("p:fn:b", "a.rs", false).is_none());
        assert!(guard.record("p:fn:c", "b.rs", false).is_none());
    }

    #[test]
    fn in_run_same_file_collision_is_flagged_once() {
        let mut guard = DuplicateLocatorGuard::new(BTreeMap::new());
        assert!(guard.record("p:fn:a", "a.rs", false).is_none());
        let finding = guard.record("p:fn:a", "a.rs", false).expect("finding");
        assert_eq!(finding.subcode, DUPLICATE_LOCATOR_RULE_ID);
        assert_eq!(shape_of(&finding), "in_run_same_file");
        assert!(finding.message.contains("p:fn:a"));
        // Third occurrence: one finding per id per run.
        assert!(guard.record("p:fn:a", "a.rs", false).is_none());
    }

    #[test]
    fn in_run_cross_file_collision_names_both_paths() {
        let mut guard = DuplicateLocatorGuard::new(BTreeMap::new());
        assert!(guard.record("p:fn:a", "a.rs", false).is_none());
        let finding = guard.record("p:fn:a", "b.rs", false).expect("finding");
        assert_eq!(shape_of(&finding), "in_run_cross_file");
        assert!(finding.message.contains("a.rs") && finding.message.contains("b.rs"));
    }

    #[test]
    fn cross_run_collision_against_skipped_owner_is_flagged() {
        let mut guard = DuplicateLocatorGuard::new(owners(&[("p:fn:a", "old.rs")]));
        let finding = guard.record("p:fn:a", "new.rs", false).expect("finding");
        assert_eq!(shape_of(&finding), "cross_run_unchanged_file");
        assert!(finding.message.contains("old.rs") && finding.message.contains("new.rs"));
        // The id is already reported: a later in-run duplicate stays deduped.
        assert!(guard.record("p:fn:a", "third.rs", false).is_none());
    }

    #[test]
    fn cross_run_same_path_is_defensively_silent() {
        let mut guard = DuplicateLocatorGuard::new(owners(&[("p:fn:a", "a.rs")]));
        assert!(guard.record("p:fn:a", "a.rs", false).is_none());
    }

    #[test]
    fn file_scope_skips_the_cross_run_rule() {
        // A file_scope id owned by a skipped file would normally be suppressed
        // before reaching record(); if it does reach record() (first claim of
        // the run), the dual-claim machinery owns reconciliation — silent.
        let mut guard = DuplicateLocatorGuard::new(owners(&[("p:mod:m", "old.rs")]));
        assert!(guard.record("p:mod:m", "new.rs", true).is_none());
    }

    #[test]
    fn suppressed_file_scope_cross_file_dual_claim_is_silent() {
        let mut guard = DuplicateLocatorGuard::new(BTreeMap::new());
        assert!(guard.record("p:mod:m", "a.rs", true).is_none());
        assert!(
            guard
                .record_suppressed_file_scope("p:mod:m", "b.rs")
                .is_none()
        );
        // Prior-claim flavour: no in-run first emission at all.
        assert!(
            guard
                .record_suppressed_file_scope("p:mod:n", "c.rs")
                .is_none()
        );
    }

    #[test]
    fn suppressed_file_scope_same_file_is_a_plugin_bug_finding() {
        let mut guard = DuplicateLocatorGuard::new(BTreeMap::new());
        assert!(guard.record("p:mod:m", "a.rs", true).is_none());
        let finding = guard
            .record_suppressed_file_scope("p:mod:m", "a.rs")
            .expect("finding");
        assert_eq!(shape_of(&finding), "in_run_same_file");
    }
}
