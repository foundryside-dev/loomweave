//! WS5 exploration-elimination shortcuts.
//!
//! Two families live here:
//!
//! - **Real graph queries** over the already-built edge graph — `find_circular_imports`
//!   and `find_coupling_hotspots`. No analyze-time precompute (ADR-030): each is a
//!   cheap read over `edges`. Edge-derived, so results declare a confidence tier
//!   (ADR-028), default `>= resolved`.
//! - **Honest-empty categorisation/churn shortcuts** (Task 4) — added alongside,
//!   each reading an existing signal (categorisation tag / git churn) and returning
//!   an honest empty result with a missing-signal note where the signal is absent.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use clarion_core::{EdgeConfidence, McpErrorCode};
use clarion_storage::{call_edges_targeting, entities_by_churn, entity_by_id};

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, RawScope, finalize_entity_page, missing_signal};
use crate::{
    entity_json, flatten_storage_envelope_result, optional_confidence, required_str,
    success_envelope, tool_error_envelope,
};

/// Scan bound on edges materialised for a graph query.
const EDGE_SCAN_CAP: usize = 500_000;
const HOTSPOTS_PAGE_DEFAULT: usize = 20;
const HOTSPOTS_PAGE_MAX: usize = 200;
const CYCLES_PAGE_DEFAULT: usize = 50;
const CYCLES_PAGE_MAX: usize = 200;

/// The confidence strings included at or below a requested tier (the tier is a
/// ceiling: `resolved` → resolved only; `inferred` → all).
fn allowed_confidences(max: EdgeConfidence) -> &'static [&'static str] {
    match max {
        EdgeConfidence::Resolved => &["resolved"],
        EdgeConfidence::Ambiguous => &["resolved", "ambiguous"],
        EdgeConfidence::Inferred => &["resolved", "ambiguous", "inferred"],
    }
}

fn confidence_in_clause(max: EdgeConfidence) -> String {
    allowed_confidences(max)
        .iter()
        .map(|c| format!("'{c}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl ServerState {
    /// `find_circular_imports(scope?, confidence?, limit?)` — import cycles in the
    /// module import graph (`imports` edges). Each cycle is a strongly-connected
    /// component of size > 1 (or a self-import). Edge-derived: default confidence
    /// `resolved`; the tier is echoed. Scope restricts to cycles whose members are
    /// all in scope. Bounded; each member carries its `sei`.
    pub(crate) async fn tool_find_circular_imports(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let scope = RawScope::parse(arguments)?;
        let confidence = optional_confidence(arguments)?;
        let page = Page::parse(arguments, CYCLES_PAGE_DEFAULT, CYCLES_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (in_scope, scope_truncated) = filter.in_scope_ids(conn, &project_root)?;

                // Build the import adjacency, restricted to in-scope endpoints.
                let in_clause = confidence_in_clause(confidence);
                let sql = format!(
                    "SELECT from_id, to_id FROM edges \
                     WHERE kind = 'imports' AND confidence IN ({in_clause}) LIMIT ?1"
                );
                let cap = i64::try_from(EDGE_SCAN_CAP.saturating_add(1)).unwrap_or(i64::MAX);
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(rusqlite::params![cap])?;
                let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
                let mut edge_count = 0usize;
                let mut scan_truncated = false;
                while let Some(row) = rows.next()? {
                    if edge_count >= EDGE_SCAN_CAP {
                        scan_truncated = true;
                        break;
                    }
                    edge_count += 1;
                    let from: String = row.get(0)?;
                    let to: String = row.get(1)?;
                    let keep = in_scope
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&from) && ids.contains(&to));
                    if keep {
                        adjacency.entry(from).or_default().push(to);
                    }
                }

                let cycles = strongly_connected_cycles(&adjacency);
                let total = cycles.len();
                let returned: Vec<Vec<String>> = cycles
                    .into_iter()
                    .skip(page.offset)
                    .take(page.limit)
                    .collect();
                let returned_count = returned.len();
                let truncated = page.offset.saturating_add(returned_count) < total;

                let cycles_json: Vec<Value> = returned
                    .iter()
                    .map(|members| {
                        let entities: Vec<Value> = members
                            .iter()
                            .map(|id| match entity_by_id(conn, id) {
                                Ok(Some(entity)) => entity_json(conn, &entity),
                                _ => json!({ "id": id, "sei": Value::Null }),
                            })
                            .collect();
                        json!({ "length": members.len(), "members": entities })
                    })
                    .collect();

                Ok(success_envelope(json!({
                    "cycles": cycles_json,
                    "confidence": confidence.as_str(),
                    "page": {
                        "total": total,
                        "offset": page.offset,
                        "limit": page.limit,
                        "returned": returned_count,
                        "truncated": truncated,
                    },
                    "scope_truncated": scope_truncated,
                    "scan_truncated": scan_truncated,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `find_coupling_hotspots(limit?, scope?, confidence?)` — entities ranked by
    /// coupling (distinct fan-in + fan-out over all edges) within scope.
    /// Edge-derived: default confidence `resolved`; the tier is echoed. Bounded;
    /// each entity carries its `sei`.
    pub(crate) async fn tool_find_coupling_hotspots(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let scope = RawScope::parse(arguments)?;
        let confidence = optional_confidence(arguments)?;
        let page = Page::parse(arguments, HOTSPOTS_PAGE_DEFAULT, HOTSPOTS_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (in_scope, scope_truncated) = filter.in_scope_ids(conn, &project_root)?;
                let in_clause = confidence_in_clause(confidence);

                let mut coupling: HashMap<String, (i64, i64)> = HashMap::new();
                // out-degree (distinct callees / targets)
                let out_sql = format!(
                    "SELECT from_id, COUNT(DISTINCT to_id) FROM edges \
                     WHERE confidence IN ({in_clause}) GROUP BY from_id"
                );
                let mut stmt = conn.prepare(&out_sql)?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    coupling.entry(id).or_default().1 = row.get(1)?;
                }
                // in-degree (distinct callers / sources)
                let in_sql = format!(
                    "SELECT to_id, COUNT(DISTINCT from_id) FROM edges \
                     WHERE confidence IN ({in_clause}) GROUP BY to_id"
                );
                let mut stmt = conn.prepare(&in_sql)?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    coupling.entry(id).or_default().0 = row.get(1)?;
                }

                let mut ranked: Vec<(String, i64, i64)> = coupling
                    .into_iter()
                    .filter(|(id, _)| in_scope.as_ref().is_none_or(|ids| ids.contains(id)))
                    .map(|(id, (fan_in, fan_out))| (id, fan_in, fan_out))
                    .collect();
                // Rank by total coupling desc, ties by id for determinism.
                ranked.sort_by(|a, b| {
                    (b.1 + b.2)
                        .cmp(&(a.1 + a.2))
                        .then_with(|| a.0.cmp(&b.0))
                });

                let total = ranked.len();
                let returned: Vec<(String, i64, i64)> = ranked
                    .into_iter()
                    .skip(page.offset)
                    .take(page.limit)
                    .collect();
                let returned_count = returned.len();
                let truncated = page.offset.saturating_add(returned_count) < total;

                let hotspots: Vec<Value> = returned
                    .iter()
                    .map(|(id, fan_in, fan_out)| {
                        let entity = match entity_by_id(conn, id) {
                            Ok(Some(entity)) => entity_json(conn, &entity),
                            _ => json!({ "id": id, "sei": Value::Null }),
                        };
                        json!({
                            "entity": entity,
                            "fan_in": fan_in,
                            "fan_out": fan_out,
                            "coupling": fan_in + fan_out,
                        })
                    })
                    .collect();

                Ok(success_envelope(json!({
                    "hotspots": hotspots,
                    "confidence": confidence.as_str(),
                    "page": {
                        "total": total,
                        "offset": page.offset,
                        "limit": page.limit,
                        "returned": returned_count,
                        "truncated": truncated,
                    },
                    "scope_truncated": scope_truncated,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }
}

const SHORTCUT_PAGE_DEFAULT: usize = 50;
const SHORTCUT_PAGE_MAX: usize = 200;
const CHURN_SCAN_CAP: usize = 50_000;

impl ServerState {
    /// `find_entry_points(scope?)` — entities tagged as entry points. Reads the
    /// `entry-point` categorisation tag; honest-empty when no plugin emits it.
    pub(crate) async fn tool_find_entry_points(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        self.categorisation_shortcut(
            arguments,
            "entry-point",
            "no entity is tagged as an entry point; entry-point categorisation is not emitted by \
             the active plugins (honest-empty, not a guaranteed absence of entry points)",
        )
        .await
    }

    /// `find_http_routes(scope?)` — entities tagged as HTTP routes (honest-empty
    /// when the `http-route` tag is not emitted).
    pub(crate) async fn tool_find_http_routes(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        self.categorisation_shortcut(
            arguments,
            "http-route",
            "no entity is tagged as an HTTP route; route categorisation is not emitted by the \
             active plugins",
        )
        .await
    }

    /// `find_data_models(scope?)` — entities tagged as data models (honest-empty
    /// when the `data-model` tag is not emitted).
    pub(crate) async fn tool_find_data_models(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        self.categorisation_shortcut(
            arguments,
            "data-model",
            "no entity is tagged as a data model; data-model categorisation is not emitted by the \
             active plugins",
        )
        .await
    }

    /// `find_tests(scope?)` — entities tagged as tests (honest-empty when the
    /// `test` tag is not emitted).
    pub(crate) async fn tool_find_tests(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        self.categorisation_shortcut(
            arguments,
            "test",
            "no entity is tagged as a test; test categorisation is not emitted by the active \
             plugins",
        )
        .await
    }

    /// `find_deprecations(scope?)` — entities tagged deprecated (honest-empty
    /// when the `deprecated` tag is not emitted).
    pub(crate) async fn tool_find_deprecations(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        self.categorisation_shortcut(
            arguments,
            "deprecated",
            "no entity is tagged as deprecated; deprecation categorisation is not emitted by the \
             active plugins",
        )
        .await
    }

    /// `find_todos(scope?)` — entities tagged with a TODO marker (honest-empty
    /// when the `todo` tag is not emitted).
    pub(crate) async fn tool_find_todos(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        self.categorisation_shortcut(
            arguments,
            "todo",
            "no entity is tagged with a TODO/FIXME marker; TODO extraction is not emitted by the \
             active plugins",
        )
        .await
    }

    /// Shared body for the categorisation shortcuts: parse scope/page, then run
    /// the canonical tag through [`ServerState::tag_facet`].
    async fn categorisation_shortcut(
        &self,
        arguments: &serde_json::Map<String, Value>,
        tag: &'static str,
        missing_reason: &'static str,
    ) -> std::result::Result<Value, ParamError> {
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, SHORTCUT_PAGE_DEFAULT, SHORTCUT_PAGE_MAX)?;
        Ok(self
            .tag_facet(tag.to_owned(), missing_reason, scope, page)
            .await)
    }

    /// `what_tests_this(id)` — the test entities that exercise an entity: its
    /// callers that carry the `test` categorisation tag. Honest-empty when test
    /// categorisation is not emitted (so an empty result is never read as "this
    /// is untested"). Stateless, bounded, SEI-carrying.
    pub(crate) async fn tool_what_tests_this(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let page = Page::parse(arguments, SHORTCUT_PAGE_DEFAULT, SHORTCUT_PAGE_MAX)?;
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };

                // Callers tagged `test`. Resolved-tier callers only.
                let callers = call_edges_targeting(conn, &entity.id, EdgeConfidence::Resolved)?;
                let caller_ids: HashSet<String> =
                    callers.into_iter().map(|edge| edge.from_id).collect();

                let mut test_callers: Vec<Value> = Vec::new();
                for caller_id in &caller_ids {
                    let is_test: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM entity_tags WHERE entity_id = ?1 AND tag = 'test')",
                        rusqlite::params![caller_id],
                        |row| row.get(0),
                    )?;
                    if is_test
                        && let Some(caller) = entity_by_id(conn, caller_id)?
                    {
                        test_callers.push(entity_json(conn, &caller));
                    }
                }
                test_callers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

                let total = test_callers.len();
                let returned: Vec<Value> = test_callers
                    .into_iter()
                    .skip(page.offset)
                    .take(page.limit)
                    .collect();
                let returned_count = returned.len();
                let truncated = page.offset.saturating_add(returned_count) < total;

                let mut response = json!({
                    "entity": entity_json(conn, &entity),
                    "tests": returned,
                    "page": {
                        "total": total,
                        "offset": page.offset,
                        "limit": page.limit,
                        "returned": returned_count,
                        "truncated": truncated,
                    },
                });
                if total == 0
                    && let Some(object) = response.as_object_mut()
                {
                    object.insert(
                        "signal".to_owned(),
                        missing_signal(
                            "entity_tags",
                            "no test-tagged caller found; test categorisation is not emitted by \
                             the active plugins, so this is not a guarantee the entity is untested",
                        ),
                    );
                }
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `high_churn(limit?, scope?)` — entities ranked by `git_churn_count`
    /// descending. The analyze pipeline does not populate churn in v1.0, so this
    /// is honest-empty in practice (the missing signal is surfaced); the query is
    /// real, so it lights up if churn is ever populated. Bounded, SEI-carrying.
    pub(crate) async fn tool_high_churn(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, SHORTCUT_PAGE_DEFAULT, SHORTCUT_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (rows, scan_truncated) = entities_by_churn(conn, CHURN_SCAN_CAP)?;
                // Keep churn alongside; finalize over the entity rows, then graft
                // the churn count onto each returned entity.
                let churn_by_id: std::collections::HashMap<String, i64> =
                    rows.iter().map(|(e, c)| (e.id.clone(), *c)).collect();
                let entities: Vec<_> = rows.into_iter().map(|(e, _)| e).collect();
                let mut response =
                    finalize_entity_page(conn, &project_root, entities, &filter, page, scan_truncated);
                if let Some(list) = response["entities"].as_array() {
                    let grafted: Vec<Value> = list
                        .iter()
                        .map(|entity| {
                            let mut entity = entity.clone();
                            if let Some(object) = entity.as_object_mut()
                                && let Some(id) = object.get("id").and_then(Value::as_str)
                                && let Some(churn) = churn_by_id.get(id)
                            {
                                object.insert("git_churn_count".to_owned(), json!(churn));
                            }
                            entity
                        })
                        .collect();
                    if let Some(object) = response.as_object_mut() {
                        object.insert("entities".to_owned(), Value::Array(grafted));
                    }
                }
                if response["page"]["total"] == json!(0)
                    && let Some(object) = response.as_object_mut()
                {
                    object.insert(
                        "signal".to_owned(),
                        missing_signal(
                            "git_churn_count",
                            "no entity carries git churn; the analyze pipeline does not populate \
                             git_churn_count in v1.0",
                        ),
                    );
                }
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `recently_changed(since?, scope?)` — entities changed since a timestamp.
    /// Clarion does not index a per-entity git change timestamp in v1.0, so this
    /// is an honest no-op: it returns an empty set with a missing-signal note
    /// pointing at `index_diff` for repo-level freshness. The args are accepted
    /// for forward-compatibility. Never fabricates a change set.
    // Honest no-op: no storage read, but kept `async` for the uniform tool
    // dispatch interface (every `tool_*` is awaited in `handle_tool_call`).
    #[allow(clippy::unused_async)]
    pub(crate) async fn tool_recently_changed(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        // Validate args so a malformed call still errors honestly.
        let _ = RawScope::parse(arguments)?;
        let since = match arguments.get("since") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => return Err(ParamError::new("since must be an ISO-8601 string or null")),
        };
        Ok(success_envelope(json!({
            "entities": [],
            "since": since,
            "page": { "total": 0, "offset": 0, "limit": 0, "returned": 0, "truncated": false },
            "signal": missing_signal(
                "git_change_time",
                "Clarion does not index a per-entity git change timestamp in v1.0; use index_diff \
                 for repo-level freshness (HEAD vs last analyze)"
            ),
        })))
    }
}

/// Tarjan strongly-connected components over the adjacency map; returns the
/// components that form a cycle (size > 1, or a single node with a self-edge).
/// Each component's members are sorted for deterministic output, and the
/// components themselves are sorted by first member.
fn strongly_connected_cycles(adjacency: &HashMap<String, Vec<String>>) -> Vec<Vec<String>> {
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut low: HashMap<&str, usize> = HashMap::new();
    let mut on_stack: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&str> = Vec::new();
    let mut next_index = 0usize;
    let mut components: Vec<Vec<String>> = Vec::new();

    // Iterative Tarjan to avoid deep recursion on large graphs.
    let nodes: Vec<&str> = adjacency.keys().map(String::as_str).collect();
    for &start in &nodes {
        if index_of.contains_key(start) {
            continue;
        }
        // Work stack of (node, next-neighbour-index).
        let mut work: Vec<(&str, usize)> = vec![(start, 0)];
        index_of.insert(start, next_index);
        low.insert(start, next_index);
        next_index += 1;
        stack.push(start);
        on_stack.insert(start);

        while let Some(&(node, child_idx)) = work.last() {
            let empty: &[String] = &[];
            let neighbours = adjacency.get(node).map_or(empty, Vec::as_slice);
            if child_idx < neighbours.len() {
                work.last_mut().unwrap().1 += 1;
                let next = neighbours[child_idx].as_str();
                if !index_of.contains_key(next) {
                    index_of.insert(next, next_index);
                    low.insert(next, next_index);
                    next_index += 1;
                    stack.push(next);
                    on_stack.insert(next);
                    work.push((next, 0));
                } else if on_stack.contains(next) {
                    let next_index_val = index_of[next];
                    let entry = low.get_mut(node).unwrap();
                    *entry = (*entry).min(next_index_val);
                }
            } else {
                // Done with node; propagate low-link to parent and maybe close an SCC.
                if low[node] == index_of[node] {
                    let mut component: Vec<String> = Vec::new();
                    while let Some(top) = stack.pop() {
                        on_stack.remove(top);
                        component.push(top.to_owned());
                        if top == node {
                            break;
                        }
                    }
                    let has_self_edge = adjacency
                        .get(node)
                        .is_some_and(|tos| tos.iter().any(|to| to == node));
                    if component.len() > 1 || has_self_edge {
                        component.sort();
                        components.push(component);
                    }
                }
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    let node_low = low[node];
                    let entry = low.get_mut(parent).unwrap();
                    *entry = (*entry).min(node_low);
                }
            }
        }
    }
    components.sort_by(|a, b| a.first().cmp(&b.first()));
    components
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &str)]) -> HashMap<String, Vec<String>> {
        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
        for (from, to) in edges {
            adjacency
                .entry((*from).to_owned())
                .or_default()
                .push((*to).to_owned());
        }
        adjacency
    }

    #[test]
    fn detects_a_two_node_cycle() {
        let g = graph(&[("a", "b"), ("b", "a"), ("b", "c")]);
        let cycles = strongly_connected_cycles(&g);
        assert_eq!(cycles, vec![vec!["a".to_owned(), "b".to_owned()]]);
    }

    #[test]
    fn no_cycle_in_a_dag() {
        let g = graph(&[("a", "b"), ("b", "c"), ("a", "c")]);
        assert!(strongly_connected_cycles(&g).is_empty());
    }

    #[test]
    fn detects_a_self_import() {
        let g = graph(&[("a", "a")]);
        assert_eq!(cycles_len(&g), 1);
    }

    #[test]
    fn detects_a_three_node_cycle() {
        let g = graph(&[("a", "b"), ("b", "c"), ("c", "a")]);
        let cycles = strongly_connected_cycles(&g);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 3);
    }

    fn cycles_len(g: &HashMap<String, Vec<String>>) -> usize {
        strongly_connected_cycles(g).len()
    }

    #[test]
    fn confidence_in_clause_is_a_ceiling() {
        assert_eq!(confidence_in_clause(EdgeConfidence::Resolved), "'resolved'");
        assert_eq!(
            confidence_in_clause(EdgeConfidence::Ambiguous),
            "'resolved', 'ambiguous'"
        );
        assert_eq!(
            confidence_in_clause(EdgeConfidence::Inferred),
            "'resolved', 'ambiguous', 'inferred'"
        );
    }
}
