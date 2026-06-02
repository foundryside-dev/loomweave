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

use clarion_core::EdgeConfidence;
use clarion_storage::entity_by_id;

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, RawScope};
use crate::{entity_json, flatten_storage_envelope_result, optional_confidence, success_envelope};

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
