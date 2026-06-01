//! Stable Entity Identity (SEI) — minting, the deterministic fail-closed
//! matcher, binding/lineage state, and resolution (Wave 1 / WS1).
//!
//! Implements ADR-038 and the Loom SEI conformance standard §3 (matcher),
//! §4 (resolution surface). Identity is a durable opaque surrogate keyed in
//! `sei_bindings` (migration 0005), decoupled from the cumulative `entities`
//! table. The qualname id (`{plugin}:{kind}:{qualname}`) is demoted to a
//! mutable **locator**; the SEI is the sole cross-tool binding key.
//!
//! ## Determinism boundary (ADR-038)
//! SEI allocation is **stateful**: the matcher carries-or-mints by reading the
//! persisted `sei_bindings`. Clarion's byte-identical-run guarantee covers
//! entity/edge/finding *state* — it does **NOT** extend to identity *values*:
//! two from-scratch runs with different `run_id`s mint different SEIs for a
//! brand-new entity. That is correct, because in a real re-index the prior
//! binding is *carried*, never re-minted. What IS deterministic is the
//! carry/mint *decision* given the same `sei_bindings` + source.
//!
//! ## Fail-closed
//! When the matcher cannot PROVE sameness it mints a new SEI and marks the old
//! binding `orphaned`; it never silently re-points an identity (and therefore
//! never silently carries a governance attestation) across an unproven match.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::error::StorageError;

/// The reserved locator namespace (ADR-038 §4 / SEI spec REQ-F-02). No plugin
/// locator may occupy it; an SEI always starts with it. This is what lets
/// `resolve(locator)` fail-closed-reject an SEI-shaped input — a colon-count
/// check is insufficient, since an SEI carries the same two colons a locator
/// does.
pub const SEI_PREFIX: &str = "clarion:eid:";

/// Number of hex chars retained from the blake3 digest (128 bits of identity).
const SEI_HEX_LEN: usize = 32;

/// Mint a fresh SEI for `locator` in run `mint_run_id` (REQ-C-02 / ADR-038 §1).
///
/// `clarion:eid:<lowercase-hex(blake3(utf8(locator) ++ 0x00 ++ utf8(mint_run_id)))[:32]>`.
///
/// The `0x00` separator makes the byte concatenation unambiguous (no
/// `locator`/`run_id` pair can alias another). Keyed on `mint_run_id` — never
/// on `first_seen_commit`, which the pipeline never populates and would
/// degenerate the token to the collision-prone `blake3(locator)`. Because the
/// matcher only *mints* (never *carries*) on a reused locator, and minting
/// happens in a later run with a different `mint_run_id`, the token is
/// collision-free under locator reuse.
#[must_use]
pub fn mint_sei(locator: &str, mint_run_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(locator.as_bytes());
    hasher.update(&[0x00]);
    hasher.update(mint_run_id.as_bytes());
    let hex = hasher.finalize().to_hex();
    format!("{SEI_PREFIX}{}", &hex[..SEI_HEX_LEN])
}

/// True if `s` is in the reserved SEI namespace (i.e. is an SEI, not a
/// locator). The fail-closed guard for `resolve(locator)` (REQ-F-02).
#[must_use]
pub fn is_reserved_sei(s: &str) -> bool {
    s.starts_with(SEI_PREFIX)
}

/// Lifecycle status of a binding (ADR-038 §3). `alive` is the one resolvable
/// state; `orphaned` is a vanished/unproven-match binding kept for audit;
/// `superseded` is reserved for future split/merge lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingStatus {
    Alive,
    Orphaned,
    Superseded,
}

impl BindingStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BindingStatus::Alive => "alive",
            BindingStatus::Orphaned => "orphaned",
            BindingStatus::Superseded => "superseded",
        }
    }
}

/// An identity event recorded in the append-only `sei_lineage` log
/// (REQ-L-01 — INSERT only, no UPDATE path in v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageEvent {
    /// A new SEI was minted for a brand-new entity.
    Born,
    /// A carried SEI's locator changed via a git-detected rename.
    LocatorChanged,
    /// A carried SEI moved to a new module/locator (identical body + signature).
    Moved,
    /// A binding was orphaned (its locator vanished with no confident match).
    Orphaned,
    /// Reserved for future split/merge lineage.
    Superseded,
}

impl LineageEvent {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LineageEvent::Born => "born",
            LineageEvent::LocatorChanged => "locator_changed",
            LineageEvent::Moved => "moved",
            LineageEvent::Orphaned => "orphaned",
            LineageEvent::Superseded => "superseded",
        }
    }
}

/// A row of the durable identity store (`sei_bindings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeiBinding {
    pub sei: String,
    pub current_locator: Option<String>,
    pub body_hash: Option<String>,
    pub signature: Option<String>,
    pub status: BindingStatus,
}

/// One current-run entity as the matcher sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntityDescriptor {
    /// The entity id string (`{plugin}:{kind}:{qualname}`) — the locator.
    pub locator: String,
    /// `entities.content_hash` (spans the full entity source incl. the `def`
    /// line). `None` when the entity has no hashable body.
    pub body_hash: Option<String>,
    /// `entities.signature` (plugin-declared versioned JSON), `None` where the
    /// plugin declares no signature for the kind (modules, etc.).
    pub signature: Option<String>,
}

/// A git-detected rename, expressed in **locator** terms (SEI spec §6:
/// `{old_locator, new_locator, …}`). The matcher consumes this typed signal,
/// never "Clarion's git code" (REQ-C-05) — so `legis` can supply a concrete
/// `GitRenameSource` later with no change to the matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRename {
    pub old_locator: String,
    pub new_locator: String,
}

/// The typed git-rename seam (REQ-C-05). v1's concrete implementation
/// (`ShellGitRenameSource`) lives in `clarion-cli`, where the qualname/path
/// derivation needed to translate file renames into locator renames already
/// exists; `legis` becomes the first external supplier post-v1 with no model
/// change.
pub trait GitRenameSource {
    /// Locator-level renames detected since `base_commit`. Implementations are
    /// best-effort: an empty result simply means the move case (identical
    /// body + signature) carries the load without git.
    fn renames_since(&self, base_commit: &str) -> Vec<GitRename>;
}

/// The matcher's per-entity verdict (ADR-038 §3). Orphaning is computed
/// SEPARATELY (see [`orphaned_bindings`]) by diffing the alive set against the
/// current run — it is a property of vanished bindings, not of a new entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeiDecision {
    /// Identity preserved. `event` is `None` for the trivial locator-unchanged
    /// case (the content axis carries any body change), or
    /// `Some(LocatorChanged|Moved)` for a rename/move carry.
    Carry {
        sei: String,
        event: Option<LineageEvent>,
    },
    /// A fresh SEI was minted (brand-new entity, or fail-closed on ambiguity).
    Mint { sei: String },
}

impl SeiDecision {
    /// The SEI this decision binds, whether carried or minted.
    #[must_use]
    pub fn sei(&self) -> &str {
        match self {
            SeiDecision::Carry { sei, .. } | SeiDecision::Mint { sei } => sei,
        }
    }
}

// ---------------------------------------------------------------------------
// Binding-state reads (the matcher's "what is currently bound" view).
// ---------------------------------------------------------------------------

fn binding_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SeiBinding> {
    Ok(SeiBinding {
        sei: row.get::<_, String>(0)?,
        current_locator: row.get::<_, Option<String>>(1)?,
        body_hash: row.get::<_, Option<String>>(2)?,
        signature: row.get::<_, Option<String>>(3)?,
        // status is validated below in the caller via BindingStatus::from_db.
        status: BindingStatus::Alive,
    })
}

/// The single alive binding currently bound to `locator`, if any.
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn alive_binding_for_locator(conn: &Connection, locator: &str) -> Result<Option<SeiBinding>> {
    conn.query_row(
        "SELECT sei, current_locator, body_hash, signature FROM sei_bindings \
         WHERE current_locator = ?1 AND status = 'alive'",
        params![locator],
        binding_from_row,
    )
    .optional()
    .map(|opt| {
        opt.map(|mut b| {
            b.status = BindingStatus::Alive;
            b
        })
    })
    .map_err(StorageError::from)
}

/// All alive bindings keyed by `current_locator` — the matcher's snapshot of
/// "what is currently bound" at the start of a re-index.
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn alive_bindings_snapshot(conn: &Connection) -> Result<HashMap<String, SeiBinding>> {
    let mut stmt = conn.prepare(
        "SELECT sei, current_locator, body_hash, signature FROM sei_bindings \
         WHERE status = 'alive' AND current_locator IS NOT NULL",
    )?;
    let rows = stmt.query_map([], binding_from_row)?;
    let mut out = HashMap::new();
    for row in rows {
        let mut b = row.map_err(StorageError::from)?;
        b.status = BindingStatus::Alive;
        if let Some(loc) = b.current_locator.clone() {
            out.insert(loc, b);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The deterministic, fail-closed matcher (ADR-038 §3 / SEI spec §3).
// ---------------------------------------------------------------------------

/// Decide the SEI for one current-run entity (SEI spec §3). Pure and
/// deterministic given its inputs.
///
/// * `alive` — prior alive bindings keyed by their `current_locator`.
/// * `current_locators` — every locator present in the current run; used to
///   tell which alive bindings *vanished* (candidates for rename/move). Pass
///   the full run's locator set so a still-present entity's SEI is never
///   stolen. Wave 2's incremental skip extends this set with skipped-unchanged
///   locators so their entities are not falsely treated as vanished.
/// * `git_renames` — locator-level rename signals (REQ-C-05).
/// * `mint_run_id` — the current run's id, used as the token's mint key.
///
/// Per-entity logic:
/// 1. locator still present in `alive` → carry, no event (a changed body is the
///    *content* axis, not an identity event).
/// 2. locator new this run, but (a) a `GitRename` maps a *vanished* alive
///    binding to it with an unchanged `body_hash` → carry `locator_changed`;
///    or (b) **exactly one** vanished alive binding has an identical
///    `body_hash` AND identical (non-null) `signature` → carry `moved`.
/// 3. otherwise → fail-closed mint (`born`).
///
/// Cross-entity contention (two new entities matching one vanished binding) is
/// resolved by the caller (the run driver dedups carries of the same SEI,
/// keeping the first deterministic match and re-minting the rest).
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always use the default hasher
pub fn rebind_or_mint(
    new_entity: &NewEntityDescriptor,
    alive: &HashMap<String, SeiBinding>,
    current_locators: &HashSet<String>,
    git_renames: &[GitRename],
    mint_run_id: &str,
) -> SeiDecision {
    // Case 1: locator unchanged — trivial carry.
    if let Some(binding) = alive.get(&new_entity.locator) {
        return SeiDecision::Carry {
            sei: binding.sei.clone(),
            event: None,
        };
    }

    // Case 2a: git-detected rename of a vanished binding with unchanged body.
    if let Some(rename) = git_renames
        .iter()
        .find(|r| r.new_locator == new_entity.locator)
        && let Some(binding) = alive.get(&rename.old_locator)
        && !current_locators.contains(&rename.old_locator)
        && binding.body_hash == new_entity.body_hash
    {
        return SeiDecision::Carry {
            sei: binding.sei.clone(),
            event: Some(LineageEvent::LocatorChanged),
        };
    }

    // Case 2b: move — identical body + identical (non-null) signature at a new
    // locator, matching EXACTLY ONE vanished binding. Null signature or body
    // cannot match (fail-closed): a `null` simply means the move case abstains.
    if new_entity.body_hash.is_some() && new_entity.signature.is_some() {
        let mut candidates = alive.values().filter(|b| {
            b.current_locator
                .as_deref()
                .is_some_and(|loc| !current_locators.contains(loc))
                && b.body_hash == new_entity.body_hash
                && b.signature.is_some()
                && b.signature == new_entity.signature
        });
        // Exactly one candidate is an unambiguous move; multiple identical-body
        // candidates are ambiguous and fall through to a fail-closed mint.
        if let Some(first) = candidates.next()
            && candidates.next().is_none()
        {
            return SeiDecision::Carry {
                sei: first.sei.clone(),
                event: Some(LineageEvent::Moved),
            };
        }
    }

    // Case 3: no confident match → fail-closed mint.
    SeiDecision::Mint {
        sei: mint_sei(&new_entity.locator, mint_run_id),
    }
}

/// SEIs of alive bindings that must flip to `orphaned`: their `current_locator`
/// is absent from the current run AND was not rematched (carried) by a
/// rename/move. A still-present or rematched binding is never orphaned.
///
/// `rematched` holds the *old* (vanished) locators whose SEI was carried to a
/// new locator this run.
#[must_use]
#[allow(clippy::implicit_hasher)] // callers always use the default hasher
pub fn orphaned_bindings(
    alive: &HashMap<String, SeiBinding>,
    current_locators: &HashSet<String>,
    rematched: &HashSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = alive
        .iter()
        .filter(|(locator, _)| {
            !current_locators.contains(locator.as_str()) && !rematched.contains(locator.as_str())
        })
        .map(|(_, binding)| binding.sei.clone())
        .collect();
    // Deterministic order (the map iteration order is not stable).
    out.sort_unstable();
    out
}

// ---------------------------------------------------------------------------
// Resolution surface reads (ADR-038 §4 / SEI spec §4). Identity is read from
// `sei_bindings`; `entities` is joined only for `content_hash`.
// ---------------------------------------------------------------------------

/// A resolved alive identity record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeiRecord {
    pub sei: String,
    pub current_locator: Option<String>,
    pub content_hash: Option<String>,
}

/// Result of resolving an SEI: either an alive record, or `alive: false` with
/// the lineage trail (orphaned/superseded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeiLookupResult {
    Alive(SeiRecord),
    NotAlive { lineage: Vec<SeiLineageRow> },
}

/// A row of the append-only lineage log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeiLineageRow {
    pub event: String,
    pub old_locator: Option<String>,
    pub new_locator: Option<String>,
    pub run_id: String,
    pub recorded_at: String,
}

/// Resolve a locator to its alive SEI (`resolve(locator)`), joining `entities`
/// for the current `content_hash`. Returns `None` when the locator resolves to
/// nothing alive.
///
/// Callers MUST reject reserved (`clarion:eid:`-prefixed) inputs *before*
/// calling this (REQ-F-02); this function does not re-check (it is a pure
/// lookup), so a mis-routed SEI would simply miss.
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn resolve_locator(conn: &Connection, locator: &str) -> Result<Option<SeiRecord>> {
    conn.query_row(
        "SELECT b.sei, b.current_locator, e.content_hash \
         FROM sei_bindings b \
         LEFT JOIN entities e ON e.id = b.current_locator \
         WHERE b.current_locator = ?1 AND b.status = 'alive'",
        params![locator],
        |row| {
            Ok(SeiRecord {
                sei: row.get::<_, String>(0)?,
                current_locator: row.get::<_, Option<String>>(1)?,
                content_hash: row.get::<_, Option<String>>(2)?,
            })
        },
    )
    .optional()
    .map_err(StorageError::from)
}

/// Resolve an SEI (`resolve_sei(sei)`): an alive record, or `alive: false`
/// plus lineage when orphaned/superseded/unknown.
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if a query fails.
pub fn resolve_sei(conn: &Connection, sei: &str) -> Result<SeiLookupResult> {
    let alive: Option<SeiRecord> = conn
        .query_row(
            "SELECT b.sei, b.current_locator, e.content_hash \
             FROM sei_bindings b \
             LEFT JOIN entities e ON e.id = b.current_locator \
             WHERE b.sei = ?1 AND b.status = 'alive'",
            params![sei],
            |row| {
                Ok(SeiRecord {
                    sei: row.get::<_, String>(0)?,
                    current_locator: row.get::<_, Option<String>>(1)?,
                    content_hash: row.get::<_, Option<String>>(2)?,
                })
            },
        )
        .optional()
        .map_err(StorageError::from)?;

    match alive {
        Some(record) => Ok(SeiLookupResult::Alive(record)),
        None => Ok(SeiLookupResult::NotAlive {
            lineage: sei_lineage(conn, sei)?,
        }),
    }
}

/// The ordered lineage event list for an SEI (`lineage(sei)`).
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn sei_lineage(conn: &Connection, sei: &str) -> Result<Vec<SeiLineageRow>> {
    let mut stmt = conn.prepare(
        "SELECT event, old_locator, new_locator, run_id, recorded_at \
         FROM sei_lineage WHERE sei = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![sei], |row| {
        Ok(SeiLineageRow {
            event: row.get::<_, String>(0)?,
            old_locator: row.get::<_, Option<String>>(1)?,
            new_locator: row.get::<_, Option<String>>(2)?,
            run_id: row.get::<_, String>(3)?,
            recorded_at: row.get::<_, String>(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(StorageError::from)?);
    }
    Ok(out)
}

/// Resolve a single entity's alive SEI by its locator (the MCP/HTTP read-time
/// join). `None` on a pre-SEI DB or an orphaned/unbound locator — graceful
/// degrade.
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn sei_for_locator(conn: &Connection, locator: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT sei FROM sei_bindings WHERE current_locator = ?1 AND status = 'alive'",
        params![locator],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(StorageError::from)
}

/// Whether this DB has any alive SEI bindings (i.e. SEI has been populated).
/// Used by `project_status` / `orientation_pack` to report SEI availability.
///
/// # Errors
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn has_any_alive_binding(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sei_bindings WHERE status = 'alive')",
        [],
        |row| row.get(0),
    )?;
    Ok(n != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_migrations;

    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        conn
    }

    fn binding(sei: &str, locator: &str, body: &str, sig: Option<&str>) -> SeiBinding {
        SeiBinding {
            sei: sei.to_owned(),
            current_locator: Some(locator.to_owned()),
            body_hash: Some(body.to_owned()),
            signature: sig.map(str::to_owned),
            status: BindingStatus::Alive,
        }
    }

    fn entity(locator: &str, body: Option<&str>, sig: Option<&str>) -> NewEntityDescriptor {
        NewEntityDescriptor {
            locator: locator.to_owned(),
            body_hash: body.map(str::to_owned),
            signature: sig.map(str::to_owned),
        }
    }

    fn locset(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    // ---- mint_sei (REQ-C-02) ----

    #[test]
    fn mint_is_deterministic_for_same_locator_and_run() {
        let a = mint_sei("python:function:m.f", "run-1");
        let b = mint_sei("python:function:m.f", "run-1");
        assert_eq!(a, b);
    }

    #[test]
    fn mint_differs_across_runs_for_same_locator() {
        // The collision-on-reuse guard: a reused locator minted in a later run
        // gets a different token.
        let a = mint_sei("python:function:m.f", "run-1");
        let b = mint_sei("python:function:m.f", "run-2");
        assert_ne!(a, b);
    }

    #[test]
    fn mint_differs_across_locators_within_a_run() {
        let a = mint_sei("python:function:m.f", "run-1");
        let b = mint_sei("python:function:m.g", "run-1");
        assert_ne!(a, b);
    }

    #[test]
    fn mint_carries_reserved_prefix_and_fixed_length() {
        let s = mint_sei("python:function:m.f", "run-1");
        assert!(s.starts_with(SEI_PREFIX), "missing reserved prefix: {s}");
        assert!(is_reserved_sei(&s));
        assert_eq!(s.len(), SEI_PREFIX.len() + SEI_HEX_LEN);
        assert!(
            s[SEI_PREFIX.len()..].chars().all(|c| c.is_ascii_hexdigit()),
            "non-hex token body: {s}"
        );
    }

    #[test]
    fn mint_separator_prevents_concatenation_aliasing() {
        // Without the 0x00 separator, ("ab","c") and ("a","bc") would alias.
        assert_ne!(mint_sei("ab", "c"), mint_sei("a", "bc"));
    }

    // ---- matcher: rebind_or_mint (SEI spec §3 / §8) ----

    #[test]
    fn matcher_carries_unchanged_locator_with_no_event() {
        let alive = HashMap::from([(
            "python:function:m.f".to_owned(),
            binding("clarion:eid:0001", "python:function:m.f", "h1", Some("s1")),
        )]);
        let cur = locset(&["python:function:m.f"]);
        let decision = rebind_or_mint(
            &entity("python:function:m.f", Some("h1"), Some("s1")),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        assert_eq!(
            decision,
            SeiDecision::Carry {
                sei: "clarion:eid:0001".to_owned(),
                event: None,
            }
        );
    }

    #[test]
    fn matcher_carries_unchanged_locator_even_when_body_changed() {
        // A changed body on the SAME locator is the content axis, not identity.
        let alive = HashMap::from([(
            "python:function:m.f".to_owned(),
            binding("clarion:eid:0001", "python:function:m.f", "h1", Some("s1")),
        )]);
        let cur = locset(&["python:function:m.f"]);
        let decision = rebind_or_mint(
            &entity("python:function:m.f", Some("h2-changed"), Some("s1")),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        assert_eq!(
            decision,
            SeiDecision::Carry {
                sei: "clarion:eid:0001".to_owned(),
                event: None,
            }
        );
    }

    #[test]
    fn matcher_carries_git_rename_with_identical_body_as_locator_changed() {
        // File/module rename: old locator vanished, git maps old->new, body
        // identical → carry locator_changed.
        let alive = HashMap::from([(
            "python:function:auth.login".to_owned(),
            binding(
                "clarion:eid:0001",
                "python:function:auth.login",
                "h1",
                Some("s1"),
            ),
        )]);
        let cur = locset(&["python:function:authn.login"]); // old vanished
        let renames = vec![GitRename {
            old_locator: "python:function:auth.login".to_owned(),
            new_locator: "python:function:authn.login".to_owned(),
        }];
        let decision = rebind_or_mint(
            &entity("python:function:authn.login", Some("h1"), Some("s1")),
            &alive,
            &cur,
            &renames,
            "run-2",
        );
        assert_eq!(
            decision,
            SeiDecision::Carry {
                sei: "clarion:eid:0001".to_owned(),
                event: Some(LineageEvent::LocatorChanged),
            }
        );
    }

    #[test]
    fn matcher_does_not_carry_git_rename_when_body_changed() {
        // Rename WITH a body edit → fail closed (the git signal alone is not
        // proof; the body must be byte-identical).
        let alive = HashMap::from([(
            "python:function:auth.login".to_owned(),
            binding(
                "clarion:eid:0001",
                "python:function:auth.login",
                "h1",
                Some("s1"),
            ),
        )]);
        let cur = locset(&["python:function:authn.login"]);
        let renames = vec![GitRename {
            old_locator: "python:function:auth.login".to_owned(),
            new_locator: "python:function:authn.login".to_owned(),
        }];
        let decision = rebind_or_mint(
            &entity("python:function:authn.login", Some("h2-edited"), Some("s1")),
            &alive,
            &cur,
            &renames,
            "run-2",
        );
        assert!(matches!(decision, SeiDecision::Mint { .. }));
    }

    #[test]
    fn matcher_carries_move_on_identical_body_and_signature() {
        // Cross-module move, no git signal: identical body + signature at a new
        // locator, exactly one vanished candidate → carry moved.
        let alive = HashMap::from([(
            "a.mod.f".to_owned(),
            binding("clarion:eid:0001", "a.mod.f", "h1", Some("s1")),
        )]);
        let cur = locset(&["b.mod.f"]); // a.mod.f vanished
        let decision = rebind_or_mint(
            &entity("b.mod.f", Some("h1"), Some("s1")),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        assert_eq!(
            decision,
            SeiDecision::Carry {
                sei: "clarion:eid:0001".to_owned(),
                event: Some(LineageEvent::Moved),
            }
        );
    }

    #[test]
    fn matcher_fails_closed_when_move_is_ambiguous() {
        // Two vanished bindings share the body+sig → cannot prove which → mint.
        let alive = HashMap::from([
            (
                "a.f".to_owned(),
                binding("clarion:eid:0001", "a.f", "h1", Some("s1")),
            ),
            (
                "a.g".to_owned(),
                binding("clarion:eid:0002", "a.g", "h1", Some("s1")),
            ),
        ]);
        let cur = locset(&["b.new"]); // both a.f and a.g vanished
        let decision = rebind_or_mint(
            &entity("b.new", Some("h1"), Some("s1")),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        assert!(matches!(decision, SeiDecision::Mint { .. }));
    }

    #[test]
    fn matcher_does_not_steal_sei_from_a_still_present_entity() {
        // A still-alive binding with an identical body must NOT be carried to a
        // new locator (it has not vanished).
        let alive = HashMap::from([(
            "a.f".to_owned(),
            binding("clarion:eid:0001", "a.f", "h1", Some("s1")),
        )]);
        let cur = locset(&["a.f", "b.copy"]); // a.f STILL present
        let decision = rebind_or_mint(
            &entity("b.copy", Some("h1"), Some("s1")),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        assert!(matches!(decision, SeiDecision::Mint { .. }));
    }

    #[test]
    fn matcher_does_not_move_match_on_null_signature() {
        // A null signature cannot satisfy the move predicate (fail-closed).
        let alive = HashMap::from([(
            "a.f".to_owned(),
            binding("clarion:eid:0001", "a.f", "h1", None),
        )]);
        let cur = locset(&["b.f"]);
        let decision = rebind_or_mint(
            &entity("b.f", Some("h1"), None),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        assert!(matches!(decision, SeiDecision::Mint { .. }));
    }

    #[test]
    fn matcher_mints_brand_new_locator() {
        let alive = HashMap::new();
        let cur = locset(&["python:function:m.fresh"]);
        let decision = rebind_or_mint(
            &entity("python:function:m.fresh", Some("h1"), Some("s1")),
            &alive,
            &cur,
            &[],
            "run-2",
        );
        match decision {
            SeiDecision::Mint { sei } => {
                assert_eq!(sei, mint_sei("python:function:m.fresh", "run-2"));
            }
            SeiDecision::Carry { .. } => panic!("expected Mint, got Carry"),
        }
    }

    // ---- orphan detection ----

    #[test]
    fn orphans_vanished_unmatched_bindings_only() {
        let alive = HashMap::from([
            (
                "still.here".to_owned(),
                binding("clarion:eid:0001", "still.here", "h1", Some("s1")),
            ),
            (
                "renamed.old".to_owned(),
                binding("clarion:eid:0002", "renamed.old", "h2", Some("s2")),
            ),
            (
                "deleted.gone".to_owned(),
                binding("clarion:eid:0003", "deleted.gone", "h3", Some("s3")),
            ),
        ]);
        let current = locset(&["still.here", "renamed.new"]);
        let rematched = locset(&["renamed.old"]); // its SEI was carried
        let orphans = orphaned_bindings(&alive, &current, &rematched);
        assert_eq!(orphans, vec!["clarion:eid:0003".to_owned()]);
    }

    #[test]
    fn orphans_empty_when_nothing_vanished() {
        let alive = HashMap::from([(
            "still.here".to_owned(),
            binding("clarion:eid:0001", "still.here", "h1", Some("s1")),
        )]);
        let current = locset(&["still.here"]);
        let orphans = orphaned_bindings(&alive, &current, &HashSet::new());
        assert!(orphans.is_empty());
    }

    // ---- DB binding-state reads ----

    fn insert_binding(conn: &Connection, b: &SeiBinding) {
        conn.execute(
            "INSERT INTO sei_bindings \
             (sei, current_locator, body_hash, signature, status, born_run_id, updated_run_id, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'r0', 'r0', 't0')",
            params![
                b.sei,
                b.current_locator,
                b.body_hash,
                b.signature,
                b.status.as_str()
            ],
        )
        .unwrap();
    }

    #[test]
    fn alive_snapshot_excludes_orphaned_and_keys_by_locator() {
        let conn = migrated_conn();
        insert_binding(
            &conn,
            &binding("clarion:eid:0001", "a.f", "h1", Some("s1")),
        );
        insert_binding(
            &conn,
            &SeiBinding {
                status: BindingStatus::Orphaned,
                ..binding("clarion:eid:0002", "a.g", "h2", Some("s2"))
            },
        );
        let snap = alive_bindings_snapshot(&conn).unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap["a.f"].sei, "clarion:eid:0001");
        assert_eq!(
            alive_binding_for_locator(&conn, "a.f").unwrap().unwrap().sei,
            "clarion:eid:0001"
        );
        assert!(alive_binding_for_locator(&conn, "a.g").unwrap().is_none());
    }

    #[test]
    fn has_any_alive_binding_reflects_state() {
        let conn = migrated_conn();
        assert!(!has_any_alive_binding(&conn).unwrap());
        insert_binding(
            &conn,
            &binding("clarion:eid:0001", "a.f", "h1", Some("s1")),
        );
        assert!(has_any_alive_binding(&conn).unwrap());
    }
}
