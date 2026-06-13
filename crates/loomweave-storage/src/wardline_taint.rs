//! Wardline taint-fact store (SP9, ADR-036). Dedicated per-entity table;
//! `wardline_json` is opaque (stored/returned verbatim). Resolution is the
//! exact tier: Wardline pre-composes its dotted qualname to byte-match
//! Loomweave's `canonical_qualified_name`, so resolution is a direct existence
//! lookup of `{plugin}:function:<qualname>`. The candidate plugins are the
//! plugins that actually have function entities (queried per batch), so a Rust
//! qualname resolves to `rust:function:<qualname>` exactly as a Python one
//! resolves to `python:function:<qualname>` (clarion-69db8b2739; ADR-036
//! Amendment 2026-06-11). A batch may carry an optional **plugin hint**
//! ([`resolve_wardline_qualnames_for_plugin`], clarion-b1a158f7f5): the hint is
//! a CONSTRAINT that restricts resolution to that one plugin's namespace —
//! never a preference order — so a qualname owned only by another plugin
//! resolves `None` under the hint. Heuristic tier is Flow B B.2.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};

use crate::query::existing_entity_ids;
use crate::{Result, StorageError};

/// Resolution of a Wardline qualname against Loomweave's entity catalog.
///
/// Exact tier only at 1.1. The Heuristic tier is Flow B B.2
/// (clarion-ca2d26ffbe), which extends THIS enum (e.g. a
/// `Heuristic { entity_id, alternatives }` variant) and must not reimplement
/// resolution. Keeping this a sum type means an illegal combination — a
/// confidence without an id, or alternatives on an exact hit — is
/// unrepresentable rather than merely undocumented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Byte-exact match: the pre-composed qualname maps to exactly one entity.
    Exact { entity_id: String },
    /// The same pre-composed qualname exists under MORE THAN ONE plugin (e.g.
    /// `python:function:<q>` AND `rust:function:<q>` both exist). The candidate
    /// ids are deterministically sorted. This is the variant reserved by the
    /// enum's doc comment for a multi-candidate outcome; minting candidates per
    /// plugin (clarion-69db8b2739) is the first place it can arise.
    Ambiguous { entity_ids: Vec<String> },
    /// No entity matched.
    None,
}

impl Resolution {
    /// Borrow the resolved entity id, if any.
    ///
    /// Returns `None` for `Ambiguous`: with more than one candidate there is no
    /// single id to return, and picking one arbitrarily would violate ADR-036's
    /// exact-only-write contract. The federation surfaces
    /// (`/api/wardline/resolve` + the taint-fact write/read paths) drive off
    /// this accessor, so an ambiguous hit degrades to "unresolved" there — it is
    /// never written as a taint fact and never collapsed onto an arbitrary
    /// plugin, and the single-id `ResolveResponse` wire shape Wardline consumes
    /// is preserved.
    #[must_use]
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Resolution::Exact { entity_id } => Some(entity_id),
            Resolution::Ambiguous { .. } | Resolution::None => Option::None,
        }
    }

    /// Consume into the resolved entity id, if any.
    ///
    /// Returns `None` for `Ambiguous` for the same reason as [`Self::entity_id`]:
    /// no single id to hand back, and the federation accessors must degrade an
    /// ambiguous match to "unresolved" rather than pick a plugin arbitrarily
    /// (ADR-036 exact-only-write).
    #[must_use]
    pub fn into_entity_id(self) -> Option<String> {
        match self {
            Resolution::Exact { entity_id } => Some(entity_id),
            Resolution::Ambiguous { .. } | Resolution::None => Option::None,
        }
    }
}

/// Plugins that own at least one `function` entity, sorted. The candidate id
/// for a pre-composed qualname is `{plugin}:function:<qualname>` for each such
/// plugin — taint facts are function/method-scoped (request §3) and methods are
/// `function`-kind in every plugin's ontology (ADR-022/ADR-049, fixture-
/// confirmed). The qualname is NOT parsed to guess its plugin: qualnames are
/// opaque (ADR-003/ADR-049), so we enumerate the plugins that actually carry
/// functions and probe each. One scan per batch call; resolution still goes
/// through the PK `IN`-probe (`existing_entity_ids`), so the probe set is
/// `qualnames x plugins`.
fn function_plugins(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT plugin_id FROM entities WHERE kind = 'function' ORDER BY plugin_id",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut plugins = Vec::new();
    for row in rows {
        plugins.push(row.map_err(StorageError::from)?);
    }
    Ok(plugins)
}

/// Resolve one pre-composed Wardline qualname to a Loomweave entity id (exact
/// tier). Returns `Exact` with the id when exactly one entity exists,
/// `Ambiguous` when it exists under more than one plugin, else `None`.
pub fn resolve_wardline_qualname(conn: &Connection, qualname: &str) -> Result<Resolution> {
    let resolved = resolve_wardline_qualnames(conn, std::slice::from_ref(&qualname.to_owned()))?;
    Ok(resolved
        .into_iter()
        .next()
        .map_or(Resolution::None, |(_, r)| r))
}

/// Batch resolve. Returns `(qualname, Resolution)` pairs in input order.
///
/// For each input qualname we mint one candidate id per plugin that has
/// function entities (`{plugin}:function:<qualname>`), then PK-probe them all
/// in one chunked `IN` lookup. Per qualname: 0 existing candidates → `None`;
/// exactly 1 → `Exact`; more than 1 → `Ambiguous` (candidate ids sorted).
///
/// Delegates to [`resolve_wardline_qualnames_for_plugin`] with no plugin hint;
/// this entry point keeps every pre-hint caller byte-for-byte unchanged.
pub fn resolve_wardline_qualnames(
    conn: &Connection,
    qualnames: &[String],
) -> Result<Vec<(String, Resolution)>> {
    resolve_wardline_qualnames_for_plugin(conn, qualnames, None)
}

/// Batch resolve with an optional batch-scoped **plugin hint**
/// (clarion-b1a158f7f5; ADR-036 plugin-hint amendment, agreed with Wardline in
/// `wardline/docs/integration/2026-06-11-wardline-resolve-plugin-hint-proposal.md`).
///
/// - `plugin: None` — exactly [`resolve_wardline_qualnames`]'s documented
///   cross-plugin behavior (the candidate plugins are enumerated from the
///   store; multi-plugin hits resolve `Ambiguous`).
/// - `plugin: Some(p)` — resolution is RESTRICTED to that plugin's namespace:
///   the `DISTINCT plugin_id` enumeration is skipped entirely and exactly one
///   candidate, `{p}:function:<qualname>`, is minted per qualname. A unique
///   match resolves `Exact`; no match resolves `None` **even if another plugin
///   owns the qualname** — the hint is a constraint, never a preference order.
///   An unknown plugin id is therefore just a constraint nothing satisfies
///   (whole batch `None`); plugin ids are NOT validated against the store
///   (adjudicated: blank-rejection is the HTTP layer's job, anything non-blank
///   is honored as a constraint). With one candidate per qualname `Ambiguous`
///   is unreachable in this mode; the classification below stays exhaustive
///   anyway so a future multi-candidate hinted tier cannot silently misfile.
pub fn resolve_wardline_qualnames_for_plugin(
    conn: &Connection,
    qualnames: &[String],
    plugin: Option<&str>,
) -> Result<Vec<(String, Resolution)>> {
    if qualnames.is_empty() {
        // Zero-SQL on an empty batch: skip the plugin-enumeration scan too.
        return Ok(Vec::new());
    }
    let plugins = match plugin {
        // Hinted: the named plugin is the ONLY candidate namespace.
        Some(p) => vec![p.to_owned()],
        Option::None => function_plugins(conn)?,
    };
    // One candidate id per (qualname, plugin) pair — the full probe set.
    let mut candidates = Vec::with_capacity(qualnames.len().saturating_mul(plugins.len()));
    for qualname in qualnames {
        for plugin in &plugins {
            candidates.push(format!("{plugin}:function:{qualname}"));
        }
    }
    let found: HashSet<String> = existing_entity_ids(conn, &candidates)?;
    Ok(qualnames
        .iter()
        .map(|qualname| {
            // `plugins` is already sorted, so the surviving ids are too.
            let mut hits: Vec<String> = plugins
                .iter()
                .map(|plugin| format!("{plugin}:function:{qualname}"))
                .filter(|candidate| found.contains(candidate))
                .collect();
            let resolution = match hits.pop() {
                None => Resolution::None,
                // Exactly one hit: nothing left after the pop.
                Some(only) if hits.is_empty() => Resolution::Exact { entity_id: only },
                // More than one: push the popped id back and keep sorted order.
                Some(last) => {
                    hits.push(last);
                    Resolution::Ambiguous { entity_ids: hits }
                }
            };
            (qualname.clone(), resolution)
        })
        .collect())
}

/// Entity kinds whose `qualified_name` segment is NOT the dotted-qualname
/// dialect: `file` is path-keyed and `subsystem` is hash-keyed (ADR-003).
/// Probing them against a pasted qualname would be noise at best and a
/// path/hash side-channel at worst, so qualname resolution excludes them —
/// both from candidate enumeration and as an explicit `kind` constraint.
const NON_QUALNAME_KINDS: [&str; 2] = ["file", "subsystem"];

/// Distinct `(plugin_id, kind)` pairs that own at least one entity in a
/// qualname-dialect kind, sorted. Pair-sorted order yields lexicographically
/// sorted candidate ids (`{plugin}:{kind}:{qualname}` segments compare in the
/// same order).
fn qualname_kind_pairs(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT plugin_id, kind FROM entities \
         WHERE kind NOT IN ('file', 'subsystem') ORDER BY plugin_id, kind",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut pairs = Vec::new();
    for row in rows {
        pairs.push(row.map_err(StorageError::from)?);
    }
    Ok(pairs)
}

/// Batch resolve pre-composed qualnames across ALL qualname-dialect entity
/// kinds (clarion-c2bb394f46) — the MCP `entity_resolve` reverse-map. Returns
/// `(qualname, Resolution)` pairs in input order.
///
/// This is a SEPARATE surface from [`resolve_wardline_qualnames`] /
/// [`resolve_wardline_qualnames_for_plugin`], which stay function-only: those
/// back the federation `/api/wardline/resolve` contract (ADR-036, taint facts
/// are function-scoped) and must not change resolution behavior
/// (clarion-7b0795f9e8 adjudication).
///
/// Candidates are minted as `{plugin}:{kind}:{qualname}` over the distinct
/// `(plugin, kind)` pairs present in the store, excluding the non-qualname
/// dialects (`file`, `subsystem`). An input that is ALREADY a fully-formed
/// entity id under one of those pairs (it begins with `{plugin}:{kind}:`) also
/// resolves VERBATIM — the `locator` dialect — so a caller holding a real
/// Loomweave id (e.g. warpline's `python:function:pkg.mod.fn` from HX1, per the
/// 2026-06-13 warpline interface-lock) resolves directly, not only one holding
/// the bare qualname tail. Because the verbatim probe is gated on the same
/// `pairs`, file/subsystem locators stay excluded and the constraints below
/// carry over unchanged. `kind` and `plugin` are optional hard
/// CONSTRAINTS with the same semantics as the ADR-036 plugin hint: an unknown
/// value is simply a constraint nothing satisfies (resolves `None`), never an
/// error, and values are not validated against the store (blank-rejection is
/// the caller's job). When both are given, exactly one candidate is minted per
/// qualname and enumeration is skipped entirely. An explicit non-qualname
/// `kind` (`file` / `subsystem`) resolves the whole batch `None`.
pub fn resolve_qualnames_all_kinds(
    conn: &Connection,
    qualnames: &[String],
    kind: Option<&str>,
    plugin: Option<&str>,
) -> Result<Vec<(String, Resolution)>> {
    if qualnames.is_empty() {
        // Zero-SQL on an empty batch: skip the pair-enumeration scan too.
        return Ok(Vec::new());
    }
    if kind.is_some_and(|k| NON_QUALNAME_KINDS.contains(&k)) {
        return Ok(qualnames
            .iter()
            .map(|qualname| (qualname.clone(), Resolution::None))
            .collect());
    }
    let pairs: Vec<(String, String)> = match (plugin, kind) {
        // Both constraints: exactly one candidate namespace, no enumeration —
        // mirrors the hinted path of `resolve_wardline_qualnames_for_plugin`.
        (Some(p), Some(k)) => vec![(p.to_owned(), k.to_owned())],
        _ => qualname_kind_pairs(conn)?
            .into_iter()
            .filter(|(p, k)| {
                plugin.is_none_or(|hint| hint == p) && kind.is_none_or(|hint| hint == k)
            })
            .collect(),
    };
    // Per input we probe two dialects: the bare qualname segment minted across
    // every allowed pair (`{plugin}:{kind}:{qualname}`), AND the input verbatim
    // when it is itself an entity id under an allowed pair (the `locator`
    // dialect). The prefix guard reuses `pairs`, so file/subsystem stay excluded
    // and the `kind`/`plugin` constraints carry over.
    let candidate_ids = |qualname: &str| -> Vec<String> {
        let mut ids = Vec::with_capacity(pairs.len() + 1);
        for (plugin, kind) in &pairs {
            let prefix = format!("{plugin}:{kind}:");
            ids.push(format!("{prefix}{qualname}"));
            if qualname.starts_with(&prefix) {
                ids.push(qualname.to_owned());
            }
        }
        ids
    };
    let probe: Vec<String> = qualnames.iter().flat_map(|q| candidate_ids(q)).collect();
    let found: HashSet<String> = existing_entity_ids(conn, &probe)?;
    Ok(qualnames
        .iter()
        .map(|qualname| {
            // Surviving candidate ids, deduped + sorted for a deterministic,
            // lexicographically ordered Ambiguous set.
            let mut hits: Vec<String> = candidate_ids(qualname)
                .into_iter()
                .filter(|candidate| found.contains(candidate))
                .collect();
            hits.sort();
            hits.dedup();
            let resolution = match hits.len() {
                0 => Resolution::None,
                1 => Resolution::Exact {
                    entity_id: hits.remove(0),
                },
                _ => Resolution::Ambiguous { entity_ids: hits },
            };
            (qualname.clone(), resolution)
        })
        .collect())
}

/// A single taint fact to persist. `wardline_json` is opaque to Loomweave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFact {
    pub entity_id: String,
    pub wardline_json: String,
    pub scan_id: Option<String>,
    pub content_hash_at_compute: Option<String>,
    pub updated_at: String,
    /// The fact's Stable Entity Identity at write time (T3.4, migration 0006).
    /// A SECOND, rename-stable lookup key alongside the locator `entity_id`:
    /// the write path resolves the alive `sei_bindings` row for `entity_id`
    /// (or accepts a caller-supplied SEI), so a fact stays retrievable by SEI
    /// after the entity is renamed. `None` on a pre-SEI database / unbound
    /// locator (graceful degrade — the fact is still locator-keyed). Opaque:
    /// stored and matched verbatim, never parsed.
    pub sei: Option<String>,
}

/// Upsert one taint fact (per-entity replace). Idempotent on `entity_id`.
/// Runs on the writer-actor's connection (Task 3) outside any run transaction.
pub fn upsert_taint_fact(conn: &Connection, fact: &TaintFact) -> Result<()> {
    conn.execute(
        "INSERT INTO wardline_taint_facts \
            (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at, sei) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(entity_id) DO UPDATE SET \
            wardline_json = excluded.wardline_json, \
            scan_id = excluded.scan_id, \
            content_hash_at_compute = excluded.content_hash_at_compute, \
            updated_at = excluded.updated_at, \
            sei = excluded.sei",
        params![
            fact.entity_id,
            fact.wardline_json,
            fact.scan_id,
            fact.content_hash_at_compute,
            fact.updated_at,
            fact.sei,
        ],
    )?;
    Ok(())
}

/// A fetched taint fact joined with the entity's containing-file path. The
/// freshness signal (`current_content_hash`) is NOT stored here: the read
/// surface derives it live from `source_file_path` via
/// [`crate::query::current_file_hash`], because the stored
/// `entities.content_hash` is a span-scoped, LF-normalized hash for function
/// entities and would be wrong for the whole-file freshness contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaintFactRow {
    pub entity_id: String,
    pub wardline_json: String,
    /// The containing file's stored path; the read surface derives the live
    /// `current_content_hash` from it (see `query::current_file_hash`). `None`
    /// only if the entity row has no `source_file_path`.
    pub source_file_path: Option<String>,
    /// The SEI stored on the fact at write time (migration 0006). `None` for a
    /// pre-SEI fact. The read-by-SEI surface keys on this; the locator read
    /// surface ignores it.
    pub sei: Option<String>,
}

/// Fetch taint facts for a set of already-resolved entity ids. Returns ONLY
/// the rows that have a stored fact — an id with no fact is simply absent
/// from the result (the sole consumer keys by `entity_id` and treats a miss
/// as "no fact"), so there is no absent-row sentinel to misread as JSON.
/// Resolves in one `IN (...)` query (chunked) rather than N point lookups,
/// matching the batched resolution side. The caller derives the live
/// whole-file freshness hash from `source_file_path`; this function does NOT
/// read the filesystem.
pub fn get_taint_facts(conn: &Connection, entity_ids: &[String]) -> Result<Vec<TaintFactRow>> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(entity_ids.len());
    for chunk in entity_ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT f.entity_id, f.wardline_json, e.source_file_path, f.sei \
               FROM wardline_taint_facts f \
               JOIN entities e ON e.id = f.entity_id \
              WHERE f.entity_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let fetched = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok(TaintFactRow {
                entity_id: row.get::<_, String>(0)?,
                wardline_json: row.get::<_, String>(1)?,
                source_file_path: row.get::<_, Option<String>>(2)?,
                sei: row.get::<_, Option<String>>(3)?,
            })
        })?;
        for row in fetched {
            rows.push(row.map_err(StorageError::from)?);
        }
    }
    Ok(rows)
}

/// Fetch the **most-recent** taint fact per SEI (T3.4, migration 0006).
///
/// The read-by-SEI surface: given a set of opaque SEIs, return each one's
/// latest fact regardless of which locator it was written under — so a fact
/// written before a rename is still retrievable after it. Strictly keyed on
/// the stored `sei` column; a `NULL`-SEI (pre-migration) fact is not reachable
/// here (it is still reachable by locator via [`get_taint_facts`]).
///
/// A single SEI can match more than one row only when a caller writes explicit
/// SEIs that collide (server-populated SEIs cannot — `ux_sei_alive_locator`
/// enforces one alive locator per SEI); ordering by `updated_at DESC, rowid
/// DESC` makes the winner deterministic. The reduce is done in Rust (rows-per-
/// SEI is tiny) rather than a `GROUP BY` over bare columns. Returns at most one
/// row per input SEI; SEIs with no stored fact are simply absent. The caller
/// derives the live whole-file freshness hash from `source_file_path`; this
/// function does NOT read the filesystem.
pub fn get_taint_facts_by_sei(conn: &Connection, seis: &[String]) -> Result<Vec<TaintFactRow>> {
    if seis.is_empty() {
        return Ok(Vec::new());
    }
    // sei -> most-recent row. Rows arrive most-recent-first (ORDER BY below),
    // so `or_insert` keeps the winner.
    let mut chosen: HashMap<String, TaintFactRow> = HashMap::new();
    for chunk in seis.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        // ORDER BY (updated_at, rowid) DESC: the first row seen per SEI is the
        // most-recent write, deterministically (rowid breaks an updated_at tie).
        let sql = format!(
            "SELECT f.entity_id, f.wardline_json, e.source_file_path, f.sei \
               FROM wardline_taint_facts f \
               JOIN entities e ON e.id = f.entity_id \
              WHERE f.sei IN ({placeholders}) \
              ORDER BY f.updated_at DESC, f.rowid DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let fetched = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok(TaintFactRow {
                entity_id: row.get::<_, String>(0)?,
                wardline_json: row.get::<_, String>(1)?,
                source_file_path: row.get::<_, Option<String>>(2)?,
                sei: row.get::<_, Option<String>>(3)?,
            })
        })?;
        for row in fetched {
            let row = row.map_err(StorageError::from)?;
            if let Some(sei) = row.sei.clone() {
                chosen.entry(sei).or_insert(row);
            }
        }
    }
    // Emit at most one row per input SEI, in input order, first occurrence wins.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for sei in seis {
        if seen.insert(sei.as_str())
            && let Some(row) = chosen.remove(sei)
        {
            out.push(row);
        }
    }
    Ok(out)
}

/// Resolve the alive SEI for each of a set of locators in one batched query
/// (chunked `IN`). Returns a `locator -> sei` map containing only locators
/// that have an alive `sei_bindings` row. Used by the taint-fact write path to
/// stamp the SEI on each fact without an N+1 of per-locator point lookups.
///
/// A pre-SEI database / unbound locator is simply absent from the map (the
/// fact is then written with `sei = NULL` — graceful degrade).
pub fn seis_for_locators(
    conn: &Connection,
    locators: &[String],
) -> Result<HashMap<String, String>> {
    if locators.is_empty() {
        return Ok(HashMap::new());
    }
    let mut out = HashMap::new();
    for chunk in locators.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT current_locator, sei FROM sei_bindings \
              WHERE status = 'alive' AND current_locator IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let fetched = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in fetched {
            let (locator, sei) = row.map_err(StorageError::from)?;
            out.insert(locator, sei);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::apply_migrations;

    /// In-memory connection with the REAL schema applied (entities +
    /// `wardline_taint_facts` from migration 0003 + `schema_migrations`). Tests
    /// build from the migration runner so they cannot drift from the DDL.
    ///
    /// Applies `apply_read_pragmas` so `foreign_keys` is ON, matching every
    /// production connection (`pragma::apply_{read,write}_pragmas` both enable
    /// it). Without this the FK cascade is silently inert in-memory and a
    /// cascade test would false-pass. The write-side pragma set is NOT used
    /// here: its `journal_mode = WAL` invariant check rejects an in-memory DB
    /// (which reports `memory`), and WAL is irrelevant to these tests.
    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        crate::pragma::apply_read_pragmas(&conn).unwrap();
        conn
    }

    /// Insert a full, valid `entities` row (mirrors the column list of
    /// `tests/writer_actor.rs::seed_entity_row`). `source_file_path` is the
    /// column the fetch tests vary (the read surface derives the live freshness
    /// hash from it), so it is the sole parameter besides the id.
    fn insert_entity(conn: &Connection, id: &str, source_file_path: Option<&str>) {
        conn.execute(
            "INSERT INTO entities ( \
                id, plugin_id, kind, name, short_name, properties, \
                content_hash, source_file_path, created_at, updated_at \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                "python",
                "function",
                id,
                id.rsplit('.').next().unwrap_or(id),
                "{}",
                "deadbeef",
                source_file_path,
                "2026-05-31T00:00:00.000Z",
                "2026-05-31T00:00:00.000Z",
            ],
        )
        .unwrap();
    }

    fn seed(conn: &Connection, ids: &[&str]) {
        for id in ids {
            insert_entity(conn, id, None);
        }
    }

    /// Insert a `function` entity under an explicit `plugin_id` (the per-plugin
    /// candidate-minting tests need a `rust:function:` row, whose `plugin_id`
    /// column must be `rust` so `function_plugins` enumerates it).
    fn insert_entity_for_plugin(conn: &Connection, plugin: &str, id: &str) {
        conn.execute(
            "INSERT INTO entities ( \
                id, plugin_id, kind, name, short_name, properties, \
                content_hash, source_file_path, created_at, updated_at \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                plugin,
                "function",
                id,
                id.rsplit('.').next().unwrap_or(id),
                "{}",
                "deadbeef",
                Option::<&str>::None,
                "2026-05-31T00:00:00.000Z",
                "2026-05-31T00:00:00.000Z",
            ],
        )
        .unwrap();
    }

    /// Insert an entity under an explicit `plugin_id` AND `kind` (the
    /// all-kinds resolution tests need class/module/struct rows whose
    /// `(plugin_id, kind)` pair `qualname_kind_pairs` enumerates).
    fn insert_entity_for_plugin_kind(conn: &Connection, plugin: &str, kind: &str, id: &str) {
        conn.execute(
            "INSERT INTO entities ( \
                id, plugin_id, kind, name, short_name, properties, \
                content_hash, source_file_path, created_at, updated_at \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                plugin,
                kind,
                id,
                id.rsplit('.').next().unwrap_or(id),
                "{}",
                "deadbeef",
                Option::<&str>::None,
                "2026-05-31T00:00:00.000Z",
                "2026-05-31T00:00:00.000Z",
            ],
        )
        .unwrap();
    }

    fn wardline_qualname_fixture() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../../docs/federation/fixtures/wardline-qualname-normalization.json"
        ))
        .expect("parse wardline qualname fixture")
    }

    #[test]
    fn resolves_fixture_vectors_exact() {
        let conn = migrated_conn();
        let fixture = wardline_qualname_fixture();
        let vectors = fixture["qualified_name_vectors"]
            .as_array()
            .expect("qualified_name_vectors array");

        for vector in vectors
            .iter()
            .filter(|vector| vector["kind"].as_str() == Some("function"))
        {
            insert_entity(
                &conn,
                vector["expected_entity_id"]
                    .as_str()
                    .expect("expected_entity_id string"),
                None,
            );
        }
        for vector in vectors
            .iter()
            .filter(|vector| vector["kind"].as_str() == Some("function"))
        {
            let qualname = vector["expected_qualified_name"]
                .as_str()
                .expect("expected_qualified_name string");
            let expected_entity_id = vector["expected_entity_id"]
                .as_str()
                .expect("expected_entity_id string");
            let r = resolve_wardline_qualname(&conn, qualname).unwrap();
            assert_eq!(
                r,
                Resolution::Exact {
                    entity_id: expected_entity_id.to_owned(),
                },
                "{}",
                vector["description"].as_str().unwrap_or(qualname)
            );
        }
    }

    #[test]
    fn unknown_qualname_resolves_none() {
        let conn = migrated_conn();
        seed(&conn, &["python:function:auth.tokens.TokenManager.verify"]);
        let r = resolve_wardline_qualname(&conn, "auth.tokens.does_not_exist").unwrap();
        assert_eq!(r, Resolution::None);
    }

    #[test]
    fn resolves_rust_function_qualname_exact() {
        // Per-plugin candidate minting (clarion-69db8b2739): a qualname that
        // exists ONLY under the `rust` plugin resolves Exact to its
        // `rust:function:` id — the resolver no longer hardcodes `python:`.
        let conn = migrated_conn();
        insert_entity_for_plugin(&conn, "rust", "rust:function:mcp_fixture.ops.entry");
        let r = resolve_wardline_qualname(&conn, "mcp_fixture.ops.entry").unwrap();
        assert_eq!(
            r,
            Resolution::Exact {
                entity_id: "rust:function:mcp_fixture.ops.entry".to_owned(),
            }
        );
    }

    #[test]
    fn same_qualname_under_two_plugins_resolves_ambiguous_sorted() {
        // The same dotted qualname exists under BOTH plugins. Resolution is
        // Ambiguous, carrying both ids deterministically sorted (`python` <
        // `rust`), and the accessors degrade it to "no single id".
        let conn = migrated_conn();
        insert_entity_for_plugin(&conn, "python", "python:function:dual.target");
        insert_entity_for_plugin(&conn, "rust", "rust:function:dual.target");
        let r = resolve_wardline_qualname(&conn, "dual.target").unwrap();
        assert_eq!(
            r,
            Resolution::Ambiguous {
                entity_ids: vec![
                    "python:function:dual.target".to_owned(),
                    "rust:function:dual.target".to_owned(),
                ],
            }
        );
        assert_eq!(r.entity_id(), None, "ambiguous has no single id");
        assert_eq!(r.into_entity_id(), None, "ambiguous has no single id");
    }

    #[test]
    fn batch_preserves_input_order_and_mixed_results() {
        // One batch carrying all three outcomes — Exact, Ambiguous, None — plus
        // a duplicate: results echo back in input order and the dual qualname's
        // rust row must not cross-contaminate the python-only Exact entries.
        let conn = migrated_conn();
        seed(&conn, &["python:function:a.b.c"]);
        insert_entity_for_plugin(&conn, "python", "python:function:dual.target");
        insert_entity_for_plugin(&conn, "rust", "rust:function:dual.target");
        let qs = vec![
            "a.b.c".to_owned(),
            "dual.target".to_owned(),
            "x.y.z".to_owned(),
            "a.b.c".to_owned(),
        ];
        let out = resolve_wardline_qualnames(&conn, &qs).unwrap();
        assert_eq!(out.len(), 4);
        let echoed: Vec<&str> = out.iter().map(|(q, _)| q.as_str()).collect();
        assert_eq!(
            echoed,
            vec!["a.b.c", "dual.target", "x.y.z", "a.b.c"],
            "input order preserved, duplicates included"
        );
        // Exact stays Exact even though the rust plugin now mints candidates
        // for every qualname (only the python row exists for a.b.c).
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "python:function:a.b.c".to_owned(),
            }
        );
        assert_eq!(
            out[1].1,
            Resolution::Ambiguous {
                entity_ids: vec![
                    "python:function:dual.target".to_owned(),
                    "rust:function:dual.target".to_owned(),
                ],
            }
        );
        assert_eq!(out[2].1, Resolution::None);
        assert_eq!(out[3].1, out[0].1, "duplicate input resolves identically");
    }

    // ── All-kinds qualname resolution (clarion-c2bb394f46) ──
    // `resolve_qualnames_all_kinds`: the MCP reverse-map surface. The
    // federation resolvers above stay function-only; these tests pin the
    // widened kind dimension, the constraint semantics, and the non-qualname
    // kind exclusion.

    #[test]
    fn all_kinds_resolves_class_module_and_struct_qualnames() {
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "class", "python:class:pkg.mod.Cls");
        insert_entity_for_plugin_kind(&conn, "python", "module", "python:module:pkg.mod");
        insert_entity_for_plugin_kind(
            &conn,
            "rust",
            "struct",
            "rust:struct:crate_a.widgets.Widget",
        );
        let qs = vec![
            "pkg.mod.Cls".to_owned(),
            "pkg.mod".to_owned(),
            "crate_a.widgets.Widget".to_owned(),
        ];
        let out = resolve_qualnames_all_kinds(&conn, &qs, None, None).unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "python:class:pkg.mod.Cls".to_owned(),
            }
        );
        assert_eq!(
            out[1].1,
            Resolution::Exact {
                entity_id: "python:module:pkg.mod".to_owned(),
            }
        );
        assert_eq!(
            out[2].1,
            Resolution::Exact {
                entity_id: "rust:struct:crate_a.widgets.Widget".to_owned(),
            }
        );
    }

    #[test]
    fn all_kinds_cross_kind_collision_is_ambiguous_sorted() {
        // The same qualname owned by a class AND a function under one plugin:
        // honest Ambiguous, candidates sorted (class < function).
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:pkg.thing");
        insert_entity_for_plugin_kind(&conn, "python", "class", "python:class:pkg.thing");
        let out =
            resolve_qualnames_all_kinds(&conn, &["pkg.thing".to_owned()], None, None).unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Ambiguous {
                entity_ids: vec![
                    "python:class:pkg.thing".to_owned(),
                    "python:function:pkg.thing".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn all_kinds_kind_constraint_restricts_and_unknown_kind_matches_nothing() {
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:pkg.thing");
        insert_entity_for_plugin_kind(&conn, "python", "class", "python:class:pkg.thing");
        let out =
            resolve_qualnames_all_kinds(&conn, &["pkg.thing".to_owned()], Some("class"), None)
                .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "python:class:pkg.thing".to_owned(),
            },
            "kind constraint collapses the cross-kind collision"
        );
        let out =
            resolve_qualnames_all_kinds(&conn, &["pkg.thing".to_owned()], Some("nosuch"), None)
                .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::None,
            "unknown kind is a constraint nothing satisfies, not an error"
        );
    }

    #[test]
    fn all_kinds_plugin_constraint_restricts_and_unknown_plugin_matches_nothing() {
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:dual.target");
        insert_entity_for_plugin_kind(&conn, "rust", "function", "rust:function:dual.target");
        let out =
            resolve_qualnames_all_kinds(&conn, &["dual.target".to_owned()], None, Some("python"))
                .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "python:function:dual.target".to_owned(),
            },
            "plugin constraint collapses the cross-plugin collision"
        );
        let out =
            resolve_qualnames_all_kinds(&conn, &["dual.target".to_owned()], None, Some("cobol"))
                .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::None,
            "unknown plugin resolves None even though other plugins own the qualname"
        );
    }

    #[test]
    fn all_kinds_both_constraints_mint_single_candidate() {
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:pkg.thing");
        insert_entity_for_plugin_kind(&conn, "python", "class", "python:class:pkg.thing");
        insert_entity_for_plugin_kind(&conn, "rust", "function", "rust:function:pkg.thing");
        let out = resolve_qualnames_all_kinds(
            &conn,
            &["pkg.thing".to_owned()],
            Some("function"),
            Some("rust"),
        )
        .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "rust:function:pkg.thing".to_owned(),
            },
            "kind+plugin pins exactly one candidate"
        );
    }

    #[test]
    fn all_kinds_excludes_file_and_subsystem_dialects() {
        // file/subsystem qualified-name segments are NOT the qualname dialect
        // (path-keyed / hash-keyed): they never resolve — neither by default
        // enumeration nor as an explicit kind constraint.
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "core", "file", "core:file:src.app");
        insert_entity_for_plugin_kind(&conn, "core", "subsystem", "core:subsystem:src.app");
        let out = resolve_qualnames_all_kinds(&conn, &["src.app".to_owned()], None, None).unwrap();
        assert_eq!(
            out[0].1,
            Resolution::None,
            "no probe against file/subsystem"
        );
        let out = resolve_qualnames_all_kinds(&conn, &["src.app".to_owned()], Some("file"), None)
            .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::None,
            "explicit kind=file is denied, not honored"
        );
    }

    #[test]
    fn all_kinds_resolves_full_locator_input_verbatim() {
        // HX1 (warpline interface-lock 2026-06-13): warpline passes a fully-formed
        // Loomweave id (`python:function:pkg.mod.fn`), not the bare qualname.
        // It must resolve to the same entity (→ its SEI), not miss.
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:pkg.mod.fn");
        insert_entity_for_plugin_kind(&conn, "python", "module", "python:module:pkg.mod");
        let qs = vec![
            "python:function:pkg.mod.fn".to_owned(),
            "python:module:pkg.mod".to_owned(),
        ];
        let out = resolve_qualnames_all_kinds(&conn, &qs, None, None).unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "python:function:pkg.mod.fn".to_owned(),
            },
            "full function locator resolves verbatim"
        );
        assert_eq!(
            out[1].1,
            Resolution::Exact {
                entity_id: "python:module:pkg.mod".to_owned(),
            },
            "full module locator (warpline's file: branch) resolves verbatim"
        );
    }

    #[test]
    fn all_kinds_bare_qualname_still_resolves_alongside_locator() {
        // The bare-qualname dialect is unchanged by the verbatim-locator probe.
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:pkg.mod.fn");
        let out =
            resolve_qualnames_all_kinds(&conn, &["pkg.mod.fn".to_owned()], None, None).unwrap();
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "python:function:pkg.mod.fn".to_owned(),
            }
        );
    }

    #[test]
    fn all_kinds_locator_input_honors_constraints_and_excludes_file() {
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "function", "python:function:pkg.mod.fn");
        insert_entity_for_plugin_kind(&conn, "core", "file", "core:file:src/app.py");
        // A verbatim file locator never resolves — the (core, file) pair is not
        // enumerated, so the prefix guard never admits it (no path side-channel).
        let out =
            resolve_qualnames_all_kinds(&conn, &["core:file:src/app.py".to_owned()], None, None)
                .unwrap();
        assert_eq!(out[0].1, Resolution::None, "file locator stays excluded");
        // The plugin constraint still filters a verbatim locator's pair.
        let out = resolve_qualnames_all_kinds(
            &conn,
            &["python:function:pkg.mod.fn".to_owned()],
            None,
            Some("rust"),
        )
        .unwrap();
        assert_eq!(
            out[0].1,
            Resolution::None,
            "plugin=rust excludes the python: locator's pair"
        );
    }

    #[test]
    fn all_kinds_empty_batch_is_empty() {
        let conn = migrated_conn();
        let out = resolve_qualnames_all_kinds(&conn, &[], None, None).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn all_kinds_batch_preserves_input_order() {
        let conn = migrated_conn();
        insert_entity_for_plugin_kind(&conn, "python", "class", "python:class:a.Cls");
        let qs = vec!["missing.x".to_owned(), "a.Cls".to_owned()];
        let out = resolve_qualnames_all_kinds(&conn, &qs, None, None).unwrap();
        let echoed: Vec<&str> = out.iter().map(|(q, _)| q.as_str()).collect();
        assert_eq!(echoed, vec!["missing.x", "a.Cls"]);
        assert_eq!(out[0].1, Resolution::None);
        assert_eq!(
            out[1].1,
            Resolution::Exact {
                entity_id: "python:class:a.Cls".to_owned(),
            }
        );
    }

    // ── ResolveRequest plugin hint (clarion-b1a158f7f5, ADR-036 amendment) ──
    // The Some(plugin) path of `resolve_wardline_qualnames_for_plugin`: the
    // hint is a CONSTRAINT, never a preference order. These mirror the
    // route-level cases in loomweave-cli's http_read/wardline.rs and stand in
    // for the proposal's three conformance rows (hinted-hit / hinted-miss /
    // unhinted-ambiguous) at the resolver level.

    #[test]
    fn hinted_resolve_hit_in_named_plugin() {
        // hinted-hit: a qualname owned only by `rust`, with a `rust` hint,
        // resolves Exact to the rust id.
        let conn = migrated_conn();
        insert_entity_for_plugin(&conn, "rust", "rust:function:mcp_fixture.ops.entry");
        let out = resolve_wardline_qualnames_for_plugin(
            &conn,
            &["mcp_fixture.ops.entry".to_owned()],
            Some("rust"),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].1,
            Resolution::Exact {
                entity_id: "rust:function:mcp_fixture.ops.entry".to_owned(),
            }
        );
    }

    #[test]
    fn hinted_resolve_misses_when_only_other_plugin_owns_qualname() {
        // hinted-miss: the qualname exists ONLY under `python`; a `rust` hint
        // must resolve None — the hint is a constraint, NOT a preference order
        // that falls back to whoever owns the qualname.
        let conn = migrated_conn();
        insert_entity_for_plugin(&conn, "python", "python:function:py.only.fn");
        let out =
            resolve_wardline_qualnames_for_plugin(&conn, &["py.only.fn".to_owned()], Some("rust"))
                .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Resolution::None);
    }

    #[test]
    fn hinted_resolve_disambiguates_dual_plugin_qualname() {
        // The headline case: a qualname under BOTH plugins is Ambiguous
        // unhinted (degrades to unresolved on the wire), but a hint pins it to
        // the named plugin's id — each hint resolving its own plugin's entity.
        let conn = migrated_conn();
        insert_entity_for_plugin(&conn, "python", "python:function:dual.target");
        insert_entity_for_plugin(&conn, "rust", "rust:function:dual.target");
        let qs = vec!["dual.target".to_owned()];

        let unhinted = resolve_wardline_qualnames_for_plugin(&conn, &qs, None).unwrap();
        assert!(
            matches!(unhinted[0].1, Resolution::Ambiguous { .. }),
            "unhinted stays Ambiguous: {:?}",
            unhinted[0].1
        );

        let rust = resolve_wardline_qualnames_for_plugin(&conn, &qs, Some("rust")).unwrap();
        assert_eq!(
            rust[0].1,
            Resolution::Exact {
                entity_id: "rust:function:dual.target".to_owned(),
            }
        );
        let python = resolve_wardline_qualnames_for_plugin(&conn, &qs, Some("python")).unwrap();
        assert_eq!(
            python[0].1,
            Resolution::Exact {
                entity_id: "python:function:dual.target".to_owned(),
            }
        );
    }

    #[test]
    fn hinted_resolve_unknown_plugin_resolves_none() {
        // An unknown (non-blank) plugin is just a constraint nothing satisfies:
        // everything resolves None. No store-coupled validation of plugin ids
        // (adjudicated under clarion-b1a158f7f5; blank rejection is the HTTP
        // layer's job).
        let conn = migrated_conn();
        insert_entity_for_plugin(&conn, "python", "python:function:py.only.fn");
        let out =
            resolve_wardline_qualnames_for_plugin(&conn, &["py.only.fn".to_owned()], Some("java"))
                .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, Resolution::None);
    }

    #[test]
    fn hinted_resolve_none_hint_is_byte_identical_to_unhinted() {
        // `None` delegates to today's cross-plugin behavior exactly — the
        // no-regression pin for every existing caller of
        // `resolve_wardline_qualnames`.
        let conn = migrated_conn();
        seed(&conn, &["python:function:a.b.c"]);
        insert_entity_for_plugin(&conn, "python", "python:function:dual.target");
        insert_entity_for_plugin(&conn, "rust", "rust:function:dual.target");
        let qs = vec![
            "a.b.c".to_owned(),
            "dual.target".to_owned(),
            "x.y.z".to_owned(),
        ];
        assert_eq!(
            resolve_wardline_qualnames_for_plugin(&conn, &qs, None).unwrap(),
            resolve_wardline_qualnames(&conn, &qs).unwrap()
        );
    }

    #[test]
    fn hinted_resolve_empty_batch_returns_empty() {
        let conn = migrated_conn();
        assert!(
            resolve_wardline_qualnames_for_plugin(&conn, &[], Some("rust"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn upsert_then_fetch_roundtrips_verbatim() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", Some("/abs/pkg/mod.py"));
        let blob =
            r#"{"schema_version":"wardline-taint-1","taint":{"actual_return":"EXTERNAL_RAW"}}"#;
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:a.b.c".to_owned(),
                wardline_json: blob.to_owned(),
                scan_id: Some("scan-1".to_owned()),
                content_hash_at_compute: Some("deadbeef".to_owned()),
                updated_at: "2026-05-31T00:00:00.000Z".to_owned(),
                sei: None,
            },
        )
        .unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].wardline_json, blob, "blob stored verbatim");
        // The row carries the entity's stored path; the read surface derives
        // the live freshness hash from it (NOT from entities.content_hash).
        assert_eq!(rows[0].source_file_path.as_deref(), Some("/abs/pkg/mod.py"));
    }

    #[test]
    fn upsert_replaces_per_entity() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", None);
        let mk = |json: &str| TaintFact {
            entity_id: "python:function:a.b.c".to_owned(),
            wardline_json: json.to_owned(),
            scan_id: None,
            content_hash_at_compute: None,
            updated_at: "t".to_owned(),
            sei: None,
        };
        // Multi-key blobs whose contents differ only by key ORDER: a naive
        // overwrite that compared parsed JSON (or kept the first write) would
        // pass with a single-key blob but fail here. The store keeps bytes
        // verbatim, so the second write's exact byte order must win.
        let first = r#"{"schema_version":"wardline-taint-1","taint":{"a":1,"b":2}}"#;
        let second = r#"{"schema_version":"wardline-taint-1","taint":{"b":2,"a":1}}"#;
        upsert_taint_fact(&conn, &mk(first)).unwrap();
        upsert_taint_fact(&conn, &mk(second)).unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].wardline_json, second,
            "overwrite must keep the latest write's exact bytes (key order included)"
        );
    }

    #[test]
    fn fetch_absent_entity_is_omitted() {
        let conn = migrated_conn();
        // An id with no stored fact is simply absent from the result — no
        // sentinel row, so no empty-string masquerading as verbatim JSON.
        let rows = get_taint_facts(&conn, &["python:function:missing".to_owned()]).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn fetch_returns_only_present_rows_for_mixed_input() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:present", None);
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:present".to_owned(),
                wardline_json: r#"{"v":1}"#.to_owned(),
                scan_id: None,
                content_hash_at_compute: None,
                updated_at: "t".to_owned(),
                sei: None,
            },
        )
        .unwrap();
        let rows = get_taint_facts(
            &conn,
            &[
                "python:function:present".to_owned(),
                "python:function:absent".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_id, "python:function:present");
    }

    #[test]
    fn fetch_empty_input_returns_empty() {
        let conn = migrated_conn();
        assert!(get_taint_facts(&conn, &[]).unwrap().is_empty());
    }

    #[test]
    fn deleting_entity_cascades_to_taint_fact() {
        // The FK `wardline_taint_facts.entity_id → entities.id` is declared
        // `ON DELETE CASCADE` (migration 0003). This guards that contract —
        // and is only meaningful because `migrated_conn` enables
        // `foreign_keys` (production parity); with the pragma OFF the row
        // would survive and this test would silently false-pass.
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", None);
        upsert_taint_fact(
            &conn,
            &TaintFact {
                entity_id: "python:function:a.b.c".to_owned(),
                wardline_json: r#"{"v":1}"#.to_owned(),
                scan_id: None,
                content_hash_at_compute: None,
                updated_at: "t".to_owned(),
                sei: None,
            },
        )
        .unwrap();
        assert_eq!(
            get_taint_facts(&conn, &["python:function:a.b.c".to_owned()])
                .unwrap()
                .len(),
            1
        );

        conn.execute(
            "DELETE FROM entities WHERE id = ?1",
            params!["python:function:a.b.c"],
        )
        .unwrap();

        assert!(
            get_taint_facts(&conn, &["python:function:a.b.c".to_owned()])
                .unwrap()
                .is_empty(),
            "deleting the entity must cascade-delete its taint fact"
        );
    }

    /// Insert an `sei_bindings` row directly (tests for the SEI-keyed read /
    /// the batched locator→SEI lookup). `status` is 'alive' or 'orphaned'.
    fn insert_binding(conn: &Connection, sei: &str, locator: &str, status: &str) {
        conn.execute(
            "INSERT INTO sei_bindings \
                (sei, current_locator, body_hash, signature, status, \
                 born_run_id, updated_run_id, updated_at) \
             VALUES (?1, ?2, NULL, NULL, ?3, 'run-0', 'run-0', 't')",
            params![sei, locator, status],
        )
        .unwrap();
    }

    fn mk_fact(entity_id: &str, json: &str, updated_at: &str, sei: Option<&str>) -> TaintFact {
        TaintFact {
            entity_id: entity_id.to_owned(),
            wardline_json: json.to_owned(),
            scan_id: None,
            content_hash_at_compute: None,
            updated_at: updated_at.to_owned(),
            sei: sei.map(str::to_owned),
        }
    }

    #[test]
    fn upsert_persists_sei_and_fetch_returns_it() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", None);
        upsert_taint_fact(
            &conn,
            &mk_fact(
                "python:function:a.b.c",
                r#"{"v":1}"#,
                "t",
                Some("loomweave:eid:abc123"),
            ),
        )
        .unwrap();
        let rows = get_taint_facts(&conn, &["python:function:a.b.c".to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sei.as_deref(), Some("loomweave:eid:abc123"));
    }

    #[test]
    fn by_sei_returns_most_recent_across_two_locators() {
        // T3.4 storage oracle. A fact is written under locator L1, then the
        // entity is renamed to L2 and re-scanned: a second fact is written
        // under L2 carrying the SAME SEI. read-by-SEI must return the L2 fact
        // (most recent), regardless of which locator each was written under.
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:old.name", None);
        insert_entity(&conn, "python:function:new.name", None);
        let sei = "loomweave:eid:stable";
        upsert_taint_fact(
            &conn,
            &mk_fact(
                "python:function:old.name",
                r#"{"gen":1}"#,
                "2026-01-01T00:00:00.000Z",
                Some(sei),
            ),
        )
        .unwrap();
        upsert_taint_fact(
            &conn,
            &mk_fact(
                "python:function:new.name",
                r#"{"gen":2}"#,
                "2026-02-01T00:00:00.000Z",
                Some(sei),
            ),
        )
        .unwrap();
        let rows = get_taint_facts_by_sei(&conn, &[sei.to_owned()]).unwrap();
        assert_eq!(rows.len(), 1, "exactly one row per SEI");
        assert_eq!(rows[0].entity_id, "python:function:new.name");
        assert_eq!(rows[0].wardline_json, r#"{"gen":2}"#);
    }

    #[test]
    fn by_sei_returns_pre_rename_fact_when_only_old_locator_written() {
        // Before the post-rename re-scan, only the fact under the OLD locator
        // exists. read-by-SEI must still return it (it is stranded under the
        // dead locator otherwise — the whole point of T3.4).
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:old.name", None);
        let sei = "loomweave:eid:stable";
        upsert_taint_fact(
            &conn,
            &mk_fact("python:function:old.name", r#"{"gen":1}"#, "t", Some(sei)),
        )
        .unwrap();
        let rows = get_taint_facts_by_sei(&conn, &[sei.to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_id, "python:function:old.name");
    }

    #[test]
    fn by_sei_omits_null_sei_facts_and_unknown_seis() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:untagged", None);
        // A pre-migration fact (sei = NULL) is NOT reachable by SEI.
        upsert_taint_fact(
            &conn,
            &mk_fact("python:function:untagged", r#"{"v":1}"#, "t", None),
        )
        .unwrap();
        assert!(
            get_taint_facts_by_sei(&conn, &["loomweave:eid:nope".to_owned()])
                .unwrap()
                .is_empty(),
            "an unknown SEI matches nothing"
        );
    }

    #[test]
    fn by_sei_empty_input_returns_empty() {
        let conn = migrated_conn();
        assert!(get_taint_facts_by_sei(&conn, &[]).unwrap().is_empty());
    }

    #[test]
    fn by_sei_breaks_equal_updated_at_tie_by_rowid() {
        // Determinism guard: when two locators carry the same SEI with an
        // IDENTICAL updated_at, the later-inserted row (higher rowid) wins, so
        // the result is deterministic rather than arbitrary across runs.
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:first", None);
        insert_entity(&conn, "python:function:second", None);
        let sei = "loomweave:eid:tie";
        let same_ts = "2026-03-01T00:00:00.000Z";
        upsert_taint_fact(
            &conn,
            &mk_fact(
                "python:function:first",
                r#"{"order":1}"#,
                same_ts,
                Some(sei),
            ),
        )
        .unwrap();
        upsert_taint_fact(
            &conn,
            &mk_fact(
                "python:function:second",
                r#"{"order":2}"#,
                same_ts,
                Some(sei),
            ),
        )
        .unwrap();
        let rows = get_taint_facts_by_sei(&conn, &[sei.to_owned()]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].entity_id, "python:function:second",
            "the later-inserted row (higher rowid) wins an updated_at tie"
        );
    }

    #[test]
    fn by_sei_dedups_duplicate_input_seis() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:function:a.b.c", None);
        let sei = "loomweave:eid:dup";
        upsert_taint_fact(
            &conn,
            &mk_fact("python:function:a.b.c", r#"{"v":1}"#, "t", Some(sei)),
        )
        .unwrap();
        let rows = get_taint_facts_by_sei(&conn, &[sei.to_owned(), sei.to_owned()]).unwrap();
        assert_eq!(rows.len(), 1, "a repeated input SEI yields one row");
    }

    #[test]
    fn seis_for_locators_returns_only_alive_bindings() {
        let conn = migrated_conn();
        insert_binding(
            &conn,
            "loomweave:eid:alive",
            "python:function:live",
            "alive",
        );
        insert_binding(
            &conn,
            "loomweave:eid:dead",
            "python:function:gone",
            "orphaned",
        );
        let map = seis_for_locators(
            &conn,
            &[
                "python:function:live".to_owned(),
                "python:function:gone".to_owned(),
                "python:function:never".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(
            map.get("python:function:live").map(String::as_str),
            Some("loomweave:eid:alive")
        );
        assert!(
            !map.contains_key("python:function:gone"),
            "an orphaned binding is not a live SEI"
        );
        assert!(!map.contains_key("python:function:never"));
    }

    #[test]
    fn seis_for_locators_empty_input_returns_empty() {
        let conn = migrated_conn();
        assert!(seis_for_locators(&conn, &[]).unwrap().is_empty());
    }
}
