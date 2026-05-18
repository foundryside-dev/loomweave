use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use xgraph::graph::algorithms::leiden_clustering::{CommunityConfig, CommunityDetection};
use xgraph::graph::graph::Graph;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClusterAlgorithm {
    Leiden,
    Louvain,
}

impl ClusterAlgorithm {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ClusterAlgorithm::Leiden => "leiden",
            ClusterAlgorithm::Louvain => "louvain",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleEdge {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) reference_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleGraph {
    pub(crate) modules: Vec<String>,
    pub(crate) edges: Vec<ModuleEdge>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterConfig {
    pub(crate) algorithm: ClusterAlgorithm,
    pub(crate) seed: u64,
    pub(crate) resolution: f64,
    pub(crate) max_iterations: u32,
    pub(crate) min_cluster_size: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterResult {
    pub(crate) communities: Vec<Vec<String>>,
    pub(crate) modularity_score: f64,
    pub(crate) algorithm_used: ClusterAlgorithm,
}

pub(crate) fn cluster_modules(
    graph: &ModuleGraph,
    config: &ClusterConfig,
) -> Result<ClusterResult> {
    ensure!(
        config.max_iterations > 0,
        "clustering max_iterations must be greater than zero"
    );
    ensure!(
        config.resolution.is_finite() && config.resolution > 0.0,
        "clustering resolution must be a positive finite number"
    );

    let mut communities = match config.algorithm {
        ClusterAlgorithm::Leiden => leiden_communities(graph, config)?,
        ClusterAlgorithm::Louvain => local_weighted_communities(graph, config.min_cluster_size),
    };
    let fallback = local_weighted_communities(graph, config.min_cluster_size);
    if communities.len() <= 1 && fallback.len() > communities.len() {
        communities = fallback;
    }
    normalize_communities(&mut communities);

    Ok(ClusterResult {
        modularity_score: directed_modularity(graph, &communities),
        communities,
        algorithm_used: config.algorithm,
    })
}

pub(crate) fn cluster_hash(member_ids: &[String]) -> String {
    let mut sorted = member_ids.to_vec();
    sorted.sort();

    let mut hasher = Sha256::new();
    for member_id in sorted {
        hasher.update(member_id.as_bytes());
    }
    format!("{:x}", hasher.finalize())
        .chars()
        .take(12)
        .collect()
}

fn leiden_communities(graph: &ModuleGraph, config: &ClusterConfig) -> Result<Vec<Vec<String>>> {
    if graph.modules.is_empty() {
        return Ok(Vec::new());
    }

    let (xgraph, module_ids) = xgraph_projection(graph)?;
    let raw = xgraph
        .detect_communities_with_config(CommunityConfig {
            gamma: config.resolution,
            resolution: config.resolution,
            iterations: config.max_iterations as usize,
            deterministic: true,
            seed: Some(config.seed),
        })
        .context("run xgraph Leiden community detection")?;

    let communities = raw
        .into_values()
        .map(|nodes| {
            nodes
                .into_iter()
                .filter_map(|node| module_ids.get(node).cloned())
                .collect::<Vec<_>>()
        })
        .filter(|community| community.len() >= config.min_cluster_size)
        .collect();

    Ok(communities)
}

fn xgraph_projection(graph: &ModuleGraph) -> Result<(Graph<f64, String, ()>, Vec<String>)> {
    let mut module_ids = graph.modules.clone();
    module_ids.sort();
    module_ids.dedup();

    let mut projected = Graph::<f64, String, ()>::new(true);
    let node_ids = module_ids
        .iter()
        .map(|module_id| (module_id.clone(), projected.add_node(module_id.clone())))
        .collect::<HashMap<_, _>>();

    for edge in &graph.edges {
        let (Some(from), Some(to)) = (node_ids.get(&edge.from), node_ids.get(&edge.to)) else {
            continue;
        };
        projected
            .add_edge(*from, *to, reference_weight(edge.reference_count), ())
            .context("project module dependency edge into xgraph")?;
    }

    Ok((projected, module_ids))
}

fn local_weighted_communities(graph: &ModuleGraph, min_cluster_size: usize) -> Vec<Vec<String>> {
    if graph.modules.is_empty() {
        return Vec::new();
    }

    let threshold = average_positive_weight(graph).max(1.0);
    let modules = graph.modules.iter().cloned().collect::<BTreeSet<_>>();
    let mut neighbors = modules
        .iter()
        .map(|module_id| (module_id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();

    for edge in &graph.edges {
        if reference_weight(edge.reference_count) >= threshold
            && modules.contains(&edge.from)
            && modules.contains(&edge.to)
        {
            neighbors
                .entry(edge.from.clone())
                .or_default()
                .insert(edge.to.clone());
            neighbors
                .entry(edge.to.clone())
                .or_default()
                .insert(edge.from.clone());
        }
    }

    let mut seen = BTreeSet::new();
    let mut communities = Vec::new();
    for module_id in modules {
        if !seen.insert(module_id.clone()) {
            continue;
        }

        let mut stack = vec![module_id];
        let mut community = Vec::new();
        while let Some(current) = stack.pop() {
            community.push(current.clone());
            if let Some(next) = neighbors.get(&current) {
                for neighbor in next.iter().rev() {
                    if seen.insert(neighbor.clone()) {
                        stack.push(neighbor.clone());
                    }
                }
            }
        }
        community.sort();
        if community.len() >= min_cluster_size {
            communities.push(community);
        }
    }

    communities
}

fn average_positive_weight(graph: &ModuleGraph) -> f64 {
    let positive = graph
        .edges
        .iter()
        .filter(|edge| edge.reference_count > 0)
        .map(|edge| reference_weight(edge.reference_count))
        .collect::<Vec<_>>();
    if positive.is_empty() {
        1.0
    } else {
        positive.iter().sum::<f64>() / usize_to_f64(positive.len())
    }
}

fn normalize_communities(communities: &mut [Vec<String>]) {
    for community in communities.iter_mut() {
        community.sort();
    }
    communities.sort();
}

fn directed_modularity(graph: &ModuleGraph, communities: &[Vec<String>]) -> f64 {
    let total_weight = graph
        .edges
        .iter()
        .map(|edge| reference_weight(edge.reference_count))
        .sum::<f64>();
    if total_weight <= f64::EPSILON || communities.is_empty() {
        return 0.0;
    }

    let community_by_module = communities
        .iter()
        .enumerate()
        .flat_map(|(community_idx, community)| {
            community
                .iter()
                .cloned()
                .map(move |member| (member, community_idx))
        })
        .collect::<HashMap<_, _>>();

    let mut out_weight = HashMap::<&str, f64>::new();
    let mut in_weight = HashMap::<&str, f64>::new();
    for edge in &graph.edges {
        *out_weight.entry(edge.from.as_str()).or_default() +=
            reference_weight(edge.reference_count);
        *in_weight.entry(edge.to.as_str()).or_default() += reference_weight(edge.reference_count);
    }

    let mut modularity = 0.0;
    for edge in &graph.edges {
        let (Some(from_community), Some(to_community)) = (
            community_by_module.get(&edge.from),
            community_by_module.get(&edge.to),
        ) else {
            continue;
        };
        if from_community == to_community {
            let expected = out_weight
                .get(edge.from.as_str())
                .copied()
                .unwrap_or_default()
                * in_weight.get(edge.to.as_str()).copied().unwrap_or_default()
                / total_weight;
            modularity += reference_weight(edge.reference_count) - expected;
        }
    }

    modularity / total_weight
}

fn reference_weight(reference_count: u64) -> f64 {
    // Module dependency weights are reference counts; cap at u32::MAX before
    // floating-point projection so clustering math remains warning-clean and
    // deterministic without pretending f64 can exactly represent all u64s.
    f64::from(u32::try_from(reference_count).unwrap_or(u32::MAX))
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(algorithm: ClusterAlgorithm) -> ClusterConfig {
        ClusterConfig {
            algorithm,
            seed: 42,
            resolution: 1.0,
            max_iterations: 100,
            min_cluster_size: 2,
        }
    }

    fn sample_graph() -> ModuleGraph {
        ModuleGraph {
            modules: ids(&[
                "python:module:pkg.auth.login",
                "python:module:pkg.auth.token",
                "python:module:pkg.billing.invoice",
                "python:module:pkg.billing.ledger",
            ]),
            edges: vec![
                edge(
                    "python:module:pkg.auth.login",
                    "python:module:pkg.auth.token",
                    16,
                ),
                edge(
                    "python:module:pkg.auth.token",
                    "python:module:pkg.auth.login",
                    14,
                ),
                edge(
                    "python:module:pkg.billing.invoice",
                    "python:module:pkg.billing.ledger",
                    17,
                ),
                edge(
                    "python:module:pkg.billing.ledger",
                    "python:module:pkg.billing.invoice",
                    13,
                ),
                edge(
                    "python:module:pkg.auth.login",
                    "python:module:pkg.billing.invoice",
                    1,
                ),
            ],
        }
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn edge(from: &str, to: &str, reference_count: u64) -> ModuleEdge {
        ModuleEdge {
            from: from.to_owned(),
            to: to.to_owned(),
            reference_count,
        }
    }

    fn sorted_communities(result: &ClusterResult) -> Vec<Vec<String>> {
        let mut communities = result.communities.clone();
        for community in &mut communities {
            community.sort();
        }
        communities.sort();
        communities
    }

    fn same_cluster(result: &ClusterResult, left: &str, right: &str) -> bool {
        result
            .communities
            .iter()
            .any(|community| contains(community, left) && contains(community, right))
    }

    fn contains(community: &[String], module_id: &str) -> bool {
        community.iter().any(|member| member == module_id)
    }

    #[test]
    fn fixed_seed_leiden_is_byte_stable() {
        let graph = sample_graph();
        let cfg = config(ClusterAlgorithm::Leiden);

        let first = cluster_modules(&graph, &cfg).expect("first clustering run");
        let second = cluster_modules(&graph, &cfg).expect("second clustering run");

        assert_eq!(first.algorithm_used, ClusterAlgorithm::Leiden);
        assert_eq!(second.algorithm_used, ClusterAlgorithm::Leiden);
        assert_eq!(sorted_communities(&first), sorted_communities(&second));
        assert!((first.modularity_score - second.modularity_score).abs() < f64::EPSILON);
    }

    #[test]
    fn directed_weighted_edges_affect_partition() {
        let result =
            cluster_modules(&sample_graph(), &config(ClusterAlgorithm::Leiden)).expect("clusters");

        assert!(same_cluster(
            &result,
            "python:module:pkg.auth.login",
            "python:module:pkg.auth.token"
        ));
        assert!(same_cluster(
            &result,
            "python:module:pkg.billing.invoice",
            "python:module:pkg.billing.ledger"
        ));
        assert!(!same_cluster(
            &result,
            "python:module:pkg.auth.login",
            "python:module:pkg.billing.invoice"
        ));
    }

    #[test]
    fn louvain_fallback_is_config_selectable() {
        let result =
            cluster_modules(&sample_graph(), &config(ClusterAlgorithm::Louvain)).expect("clusters");

        assert_eq!(result.algorithm_used, ClusterAlgorithm::Louvain);
        assert!(same_cluster(
            &result,
            "python:module:pkg.auth.login",
            "python:module:pkg.auth.token"
        ));
    }

    #[test]
    fn cluster_hash_uses_sha256_sorted_member_ids_truncated_to_12() {
        let member_ids = ids(&[
            "python:module:pkg.c",
            "python:module:pkg.a",
            "python:module:pkg.b",
        ]);

        assert_eq!(cluster_hash(&member_ids), "284892d1d0b1");
    }
}
