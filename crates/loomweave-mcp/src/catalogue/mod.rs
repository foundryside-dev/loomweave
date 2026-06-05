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

use loomweave_storage::contained_entity_ids;

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

/// A parsed `scope` argument. `scope?` accepts an entity id (its descendants)
/// **or** a path glob (`"src/auth/**"`); omitted → the whole project.
///
/// Disambiguation: a value that looks like a three-segment entity id
/// (`plugin:kind:qualname`) with no `/` or `*` is an entity scope; anything else
/// is a path glob.
#[derive(Debug, Clone)]
pub(crate) enum RawScope {
    Project,
    Entity(String),
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
        let looks_like_entity_id = !raw.contains('/')
            && !raw.contains('*')
            && raw.split(':').count() >= 3
            && raw.split(':').take(2).all(|seg| !seg.is_empty());
        if looks_like_entity_id {
            Self::Entity(raw.to_owned())
        } else {
            Self::PathGlob(raw.to_owned())
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
        }
    }
}

/// A resolved scope membership test over entity rows.
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
