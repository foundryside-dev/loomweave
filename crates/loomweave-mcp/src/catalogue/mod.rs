//! WS5 — the stateless MCP catalogue completion (Wave 4).
//!
//! These tools complete the consult-mode surface the shipped 19-tool core
//! started: read-side inspection (`guidance_for`, `findings_for`,
//! `wardline_for`), faceted search (`find_by_tag`, `find_by_kind`,
//! `find_by_wardline`), and the exploration-elimination shortcuts. Every tool
//! here obeys the WS5 invariants:
//!
//! - **Stateless.** No cursor/session/server-held state; each call is
//!   self-contained and takes explicit ids/scopes.
//! - **Bounded** (NFR-PERF-03). List tools paginate with a pinned default/max
//!   `limit` plus `offset`, and always report `total` + `truncated`. No silent
//!   caps.
//! - **SEI-carrying** (ADR-038). Every entity-returning row goes through
//!   [`crate::entity_json`], which injects the `sei` locator-independent
//!   identity (null until Wave 1's `sei_bindings` exist).
//! - **Honest empty, never fabricated.** Where a categorisation signal the tool
//!   needs is not emitted by any active plugin, the tool returns an honest empty
//!   result and surfaces the missing signal — it never invents an answer.
//!
//! Implementations attach to [`crate::ServerState`] via inherent `impl` blocks
//! in the submodules; `lib.rs` wires them into `list_tools()` and the
//! `tools/call` dispatch.

mod faceted;
mod inspection;
mod semantic;
mod shortcuts;

use std::collections::HashSet;

use serde_json::{Value, json};

use loomweave_storage::{contained_entity_ids, entity_ids_in_namespace};

use crate::ParamError;

/// Pagination window for a list-returning catalogue tool. Parsed from the
/// `limit`/`offset` arguments against a per-tool pinned default and maximum so
/// no tool can return an unbounded set (NFR-PERF-03).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Page {
    pub(crate) limit: usize,
    pub(crate) offset: usize,
}

impl Page {
    /// Parse `limit` (clamped to `[1, max]`, defaulting to `default`) and
    /// `offset` (defaulting to 0) from the tool arguments.
    pub(crate) fn parse(
        arguments: &serde_json::Map<String, Value>,
        default: usize,
        max: usize,
    ) -> std::result::Result<Self, ParamError> {
        let limit = crate::optional_usize(arguments, "limit")?
            .unwrap_or(default)
            .clamp(1, max);
        let offset = crate::optional_usize(arguments, "offset")?.unwrap_or(0);
        Ok(Self { limit, offset })
    }
}

/// Apply a parsed [`Page`] to an already-materialised, in-scope row set,
/// returning the page slice plus the bounded-response metadata
/// (`total`/`offset`/`limit`/`truncated`). `total` is the full count *before*
/// paging; `truncated` is true whenever rows beyond this page exist, so an
/// agent never reads a partial page as the complete set.
pub(crate) fn paginate<T: Clone>(rows: &[T], page: Page) -> (Vec<T>, Value) {
    let total = rows.len();
    let slice: Vec<T> = rows
        .iter()
        .skip(page.offset)
        .take(page.limit)
        .cloned()
        .collect();
    let returned = slice.len();
    let truncated = page.offset.saturating_add(returned) < total;
    let meta = json!({
        "total": total,
        "offset": page.offset,
        "limit": page.limit,
        "returned": returned,
        "truncated": truncated,
    });
    (slice, meta)
}

/// Filter materialised `candidates` by `scope`, paginate, and render
/// SEI-bearing entity rows (via [`crate::entity_json`]) with bounded-response
/// metadata (`page.total`/`returned`/`truncated`, plus `scope_truncated` and
/// `scan_truncated`). Consumes the candidate vec (no clone). Shared by the
/// faceted tools and the churn shortcuts.
pub(crate) fn finalize_entity_page(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    candidates: Vec<loomweave_storage::EntityRow>,
    scope: &ScopeFilter,
    page: Page,
    scan_truncated: bool,
) -> Value {
    let in_scope: Vec<loomweave_storage::EntityRow> = candidates
        .into_iter()
        .filter(|e| scope.contains(&e.id, e.source_file_path.as_deref(), project_root))
        .collect();
    let total = in_scope.len();
    let returned: Vec<loomweave_storage::EntityRow> = in_scope
        .into_iter()
        .skip(page.offset)
        .take(page.limit)
        .collect();
    let returned_count = returned.len();
    let truncated = page.offset.saturating_add(returned_count) < total;
    let entities: Vec<Value> = returned
        .iter()
        .map(|e| crate::entity_json(conn, e))
        .collect();
    json!({
        "entities": entities,
        "page": {
            "total": total,
            "offset": page.offset,
            "limit": page.limit,
            "returned": returned_count,
            "truncated": truncated,
        },
        "scope_truncated": scope.scope_truncated(),
        "scan_truncated": scan_truncated,
    })
}

/// An honest-empty signal note. WS5 shortcuts read *existing* signals
/// (categorisation tags, git churn); where the active plugins emit no such
/// signal the tool returns empty and attaches this block so an agent reads the
/// empty result as "the signal is absent", never as "there is nothing here".
pub(crate) fn missing_signal(signal: &str, reason: &str) -> Value {
    json!({
        "available": false,
        "signal": signal,
        "reason": reason,
    })
}

/// Glob-match `path` against a `**`/`*`/`?` `pattern`. Re-exported from
/// `loomweave-storage` so the read (`scope` / guidance `match_rules`) and write
/// (CLI guidance `--for-entity`) surfaces share one matcher — see
/// `loomweave_storage::glob`.
pub(crate) use loomweave_storage::glob_match;

/// Bound on entity ids materialised when resolving an entity-descendant scope.
const SCOPE_DESCENDANT_CAP: usize = 50_000;

/// A parsed `scope` argument. `scope?` accepts an entity id (its descendants),
/// a bare dotted qualname (`"specimen.dead_code"` — resolved to its entity),
/// **or** a path glob (`"src/auth/**"`); omitted → the whole project.
///
/// Disambiguation: a value that looks like a three-segment entity id
/// (`plugin:kind:qualname`) with no `/` or `*` is an entity scope; a value with
/// a path sigil (`/` or `*`) is a path glob; anything else is a bare qualname,
/// resolved against entity qualnames at [`RawScope::resolve`] time and only
/// falling back to a path glob if nothing matches. This stops a package/module
/// name like `"specimen"` from silently matching no file path (it resolves to
/// the entity and its descendants instead).
#[derive(Debug, Clone)]
pub(crate) enum RawScope {
    Project,
    Entity(String),
    /// A bare dotted qualname (`"specimen.dead_code"`): neither a full entity id
    /// nor a path glob. Resolved to its entity id(s) at `resolve` time, with a
    /// path-glob fallback when it matches no qualname.
    Bare(String),
    PathGlob(String),
}

impl RawScope {
    /// Parse the optional `scope` argument.
    pub(crate) fn parse(
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Self, ParamError> {
        match arguments.get("scope") {
            None | Some(Value::Null) => Ok(Self::Project),
            Some(Value::String(raw)) if raw.is_empty() => Err(ParamError::new(
                "scope must be a non-empty string when present",
            )),
            Some(Value::String(raw)) => Ok(Self::classify(raw)),
            Some(_) => Err(ParamError::new("scope must be a string or null")),
        }
    }

    fn classify(raw: &str) -> Self {
        let has_path_sigil = raw.contains('/') || raw.contains('*');
        let looks_like_entity_id = !has_path_sigil
            && raw.split(':').count() >= 3
            && raw.split(':').take(2).all(|seg| !seg.is_empty());
        if looks_like_entity_id {
            Self::Entity(raw.to_owned())
        } else if has_path_sigil {
            Self::PathGlob(raw.to_owned())
        } else {
            // No path sigil and not a full entity id: a bare dotted qualname
            // (`specimen`, `specimen.dead_code`). Resolve it as a qualname at
            // `resolve()` time so a package/module name scopes correctly,
            // instead of being treated as a path glob that silently matches
            // nothing.
            Self::Bare(raw.to_owned())
        }
    }

    /// Resolve this scope against storage into a membership test. Entity scopes
    /// materialise the anchor plus its descendants (bounded by
    /// [`SCOPE_DESCENDANT_CAP`]; `scope_truncated` is reported when the cap is
    /// hit). The anchor entity must exist (else `EntityNotFound`-style `Err`).
    pub(crate) fn resolve(
        &self,
        conn: &rusqlite::Connection,
    ) -> loomweave_storage::Result<ScopeFilter> {
        match self {
            RawScope::Project => Ok(ScopeFilter::Project),
            RawScope::PathGlob(pattern) => Ok(ScopeFilter::Path {
                pattern: pattern.clone(),
            }),
            RawScope::Entity(id) => {
                let contained = contained_entity_ids(conn, id, SCOPE_DESCENDANT_CAP)?;
                let mut ids: HashSet<String> = contained.entity_ids.into_iter().collect();
                ids.insert(id.clone());
                Ok(ScopeFilter::Ids {
                    ids,
                    truncated: contained.truncated,
                })
            }
            RawScope::Bare(raw) => {
                // A bare dotted scope is a NAMESPACE: select every entity whose
                // qualname is `raw` or a descendant `raw.*`. This covers a package
                // name (`specimen` → every module + their functions/classes, incl.
                // sibling submodules) as well as a single module — broader and more
                // correct than resolve-exact + `contains`-edges, which reached only
                // one module's own members and returned nothing for a package name
                // (lacuna-522ab56124: `scope="specimen"` → 0 on coupling /
                // circular-import). A namespace that matches no entity falls back
                // to a path glob so a filename-shaped token (`utils.py`) still
                // behaves as before.
                let (ids, truncated) =
                    entity_ids_in_namespace(conn, raw, SCOPE_DESCENDANT_CAP)?;
                if ids.is_empty() {
                    return Ok(ScopeFilter::Path {
                        pattern: raw.clone(),
                    });
                }
                Ok(ScopeFilter::Ids {
                    ids: ids.into_iter().collect(),
                    truncated,
                })
            }
        }
    }
}

/// A resolved scope membership test over entity rows.
#[derive(Debug)]
pub(crate) enum ScopeFilter {
    /// Whole project — every entity is in scope.
    Project,
    /// Only entities whose id is in this set (an entity scope: the anchor plus
    /// its descendants). `truncated` is true when the descendant cap was hit.
    Ids {
        ids: HashSet<String>,
        truncated: bool,
    },
    /// Only entities whose source path matches this glob (relative to the
    /// project root, falling back to the absolute path).
    Path { pattern: String },
}

impl ScopeFilter {
    /// Whether an entity (by id and optional source path) is in scope.
    pub(crate) fn contains(
        &self,
        id: &str,
        source_file_path: Option<&str>,
        project_root: &std::path::Path,
    ) -> bool {
        match self {
            ScopeFilter::Project => true,
            ScopeFilter::Ids { ids, .. } => ids.contains(id),
            ScopeFilter::Path { pattern } => source_file_path.is_some_and(|path| {
                let rel = std::path::Path::new(path)
                    .strip_prefix(project_root)
                    .ok()
                    .and_then(|rel| rel.to_str())
                    .unwrap_or(path);
                glob_match(pattern, rel)
            }),
        }
    }

    /// Whether descendant resolution truncated (entity scope only).
    pub(crate) fn scope_truncated(&self) -> bool {
        matches!(
            self,
            ScopeFilter::Ids {
                truncated: true,
                ..
            }
        )
    }

    /// Materialise the set of in-scope entity ids for graph tools that work on
    /// edge endpoints (which have ids, not rows). `None` means "whole project"
    /// (no filter). For a path scope this scans entity source paths up to
    /// [`SCOPE_DESCENDANT_CAP`]; the bool is `truncated` (cap hit).
    pub(crate) fn in_scope_ids(
        &self,
        conn: &rusqlite::Connection,
        project_root: &std::path::Path,
    ) -> loomweave_storage::Result<(Option<HashSet<String>>, bool)> {
        match self {
            ScopeFilter::Project => Ok((None, false)),
            ScopeFilter::Ids { ids, truncated } => Ok((Some(ids.clone()), *truncated)),
            ScopeFilter::Path { pattern } => {
                let limit =
                    i64::try_from(SCOPE_DESCENDANT_CAP.saturating_add(1)).unwrap_or(i64::MAX);
                let mut stmt = conn.prepare(
                    "SELECT id, source_file_path FROM entities \
                     WHERE source_file_path IS NOT NULL ORDER BY id LIMIT ?1",
                )?;
                let mut rows = stmt.query(rusqlite::params![limit])?;
                let mut set = HashSet::new();
                let mut scanned = 0usize;
                let mut truncated = false;
                while let Some(row) = rows.next()? {
                    if scanned >= SCOPE_DESCENDANT_CAP {
                        truncated = true;
                        break;
                    }
                    scanned += 1;
                    let id: String = row.get(0)?;
                    let path: String = row.get(1)?;
                    let rel = std::path::Path::new(&path)
                        .strip_prefix(project_root)
                        .ok()
                        .and_then(|rel| rel.to_str())
                        .unwrap_or(&path);
                    if glob_match(pattern, rel) {
                        set.insert(id);
                    }
                }
                Ok((Some(set), truncated))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_double_star_across_segments() {
        assert!(glob_match("src/auth/**", "src/auth/tokens/refresh.py"));
        assert!(glob_match("src/auth/**", "src/auth/mod.py"));
        assert!(glob_match("src/**", "src/auth/tokens/refresh.py"));
        assert!(glob_match("**/refresh.py", "src/auth/refresh.py"));
    }

    #[test]
    fn glob_single_star_stays_within_segment() {
        assert!(glob_match("src/*.py", "src/main.py"));
        assert!(!glob_match("src/*.py", "src/auth/main.py"));
        assert!(glob_match("src/auth/*.py", "src/auth/tokens.py"));
    }

    #[test]
    fn glob_rejects_non_matches() {
        assert!(!glob_match("src/auth/**", "src/billing/tokens.py"));
        assert!(!glob_match("src/auth/tokens.py", "src/auth/sessions.py"));
    }

    #[test]
    fn glob_question_matches_single_char() {
        assert!(glob_match("src/v?.py", "src/v1.py"));
        assert!(!glob_match("src/v?.py", "src/v10.py"));
    }

    #[test]
    fn raw_scope_classifies_entity_ids_vs_path_globs() {
        assert!(matches!(
            RawScope::classify("python:function:auth.tokens.refresh"),
            RawScope::Entity(_)
        ));
        assert!(matches!(
            RawScope::classify("core:subsystem:abc123"),
            RawScope::Entity(_)
        ));
        assert!(matches!(
            RawScope::classify("src/auth/**"),
            RawScope::PathGlob(_)
        ));
        assert!(matches!(
            RawScope::classify("src/auth/tokens.py"),
            RawScope::PathGlob(_)
        ));
        // A bare dotted qualname (no path sigil, fewer than three colon
        // segments) is neither an entity id nor a path glob — it is a qualname
        // to be resolved, not silently matched as a path.
        assert!(matches!(
            RawScope::classify("specimen"),
            RawScope::Bare(_)
        ));
        assert!(matches!(
            RawScope::classify("specimen.dead_code"),
            RawScope::Bare(_)
        ));
    }

    /// A migrated in-memory store seeded with one Python module entity and a
    /// child function reachable via a `contains` edge.
    fn seeded_conn() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        loomweave_storage::schema::apply_migrations(&mut conn).expect("apply migrations");
        let insert_entity = |id: &str| {
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, source_file_path, created_at, updated_at \
                 ) VALUES (?1, 'python', ?2, ?1, ?1, '{}', 'deadbeef', \
                    'specimen/dead_code.py', '2026-06-14T00:00:00.000Z', \
                    '2026-06-14T00:00:00.000Z')",
                rusqlite::params![id, id.split(':').nth(1).unwrap_or("module")],
            )
            .expect("insert entity");
        };
        insert_entity("python:module:specimen.dead_code");
        insert_entity("python:function:specimen.dead_code.orphaned_helper");
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence) \
             VALUES ('contains', 'python:module:specimen.dead_code', \
                     'python:function:specimen.dead_code.orphaned_helper', 'resolved')",
            [],
        )
        .expect("insert contains edge");
        conn
    }

    #[test]
    fn bare_qualname_scope_resolves_to_entity_and_descendants() {
        let conn = seeded_conn();
        let filter = RawScope::classify("specimen.dead_code")
            .resolve(&conn)
            .expect("resolve bare qualname");
        match filter {
            ScopeFilter::Ids { ids, .. } => {
                assert!(ids.contains("python:module:specimen.dead_code"));
                assert!(
                    ids.contains("python:function:specimen.dead_code.orphaned_helper"),
                    "descendant via contains edge must be in scope, got {ids:?}"
                );
            }
            other => panic!("expected Ids scope, got {other:?}"),
        }
    }

    #[test]
    fn bare_package_scope_includes_sibling_submodules() {
        // lacuna-522ab56124 regression: a package name must select its sibling
        // submodules (and their members), not just the package `__init__`'s own
        // contents. `specimen.hub` is NOT a `contains`-child of the `specimen`
        // module, so the old resolve-exact + contains-edges path returned only
        // `python:module:specimen` and missed `specimen.hub.*` entirely (→ 0 on
        // coupling / circular-import, which operate on the submodule entities).
        let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        loomweave_storage::schema::apply_migrations(&mut conn).expect("apply migrations");
        for id in [
            "python:module:specimen",
            "python:module:specimen.hub",
            "python:function:specimen.hub.dispatch",
            "python:module:specimentary", // prefix-but-not-namespace: must NOT match
        ] {
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, source_file_path, created_at, updated_at \
                 ) VALUES (?1, 'python', ?2, ?1, ?1, '{}', 'deadbeef', NULL, \
                    '2026-06-15T00:00:00.000Z', '2026-06-15T00:00:00.000Z')",
                rusqlite::params![id, id.split(':').nth(1).unwrap_or("module")],
            )
            .expect("insert entity");
        }
        let filter = RawScope::classify("specimen")
            .resolve(&conn)
            .expect("resolve package scope");
        match filter {
            ScopeFilter::Ids { ids, .. } => {
                assert!(ids.contains("python:module:specimen"), "{ids:?}");
                assert!(
                    ids.contains("python:module:specimen.hub"),
                    "sibling submodule must be in a package scope, got {ids:?}"
                );
                assert!(
                    ids.contains("python:function:specimen.hub.dispatch"),
                    "submodule member must be in a package scope, got {ids:?}"
                );
                assert!(
                    !ids.contains("python:module:specimentary"),
                    "a prefix that is not a namespace boundary must NOT match: {ids:?}"
                );
            }
            other => panic!("expected Ids scope, got {other:?}"),
        }
    }

    #[test]
    fn bare_qualname_miss_falls_back_to_path_glob() {
        let conn = seeded_conn();
        // A token that matches no qualname falls back to a path glob rather than
        // erroring — preserving the prior behaviour for filename-shaped tokens.
        let filter = RawScope::classify("totally.bogus.name")
            .resolve(&conn)
            .expect("resolve unmatched bare token");
        assert!(matches!(filter, ScopeFilter::Path { .. }));
    }

    #[test]
    fn paginate_reports_total_and_truncation() {
        let rows: Vec<i32> = (0..10).collect();
        let (slice, meta) = paginate(
            &rows,
            Page {
                limit: 3,
                offset: 0,
            },
        );
        assert_eq!(slice, vec![0, 1, 2]);
        assert_eq!(meta["total"], 10);
        assert_eq!(meta["truncated"], true);
        assert_eq!(meta["returned"], 3);

        let (slice, meta) = paginate(
            &rows,
            Page {
                limit: 5,
                offset: 8,
            },
        );
        assert_eq!(slice, vec![8, 9]);
        assert_eq!(meta["truncated"], false);
        assert_eq!(meta["returned"], 2);
    }
}
