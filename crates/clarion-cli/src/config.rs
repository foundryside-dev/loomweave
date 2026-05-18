use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::clustering::ClusterAlgorithm;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AnalyzeConfig {
    pub(crate) analysis: AnalysisConfig,
}

impl AnalyzeConfig {
    pub(crate) fn load(project_root: &Path, explicit_path: Option<&Path>) -> Result<Self> {
        if let Some(path) = explicit_path {
            return Self::from_path(path)
                .with_context(|| format!("load analyze config {}", path.display()));
        }

        let default_path = project_root.join("clarion.yaml");
        if default_path.exists() {
            Self::from_path(&default_path)
                .with_context(|| format!("load analyze config {}", default_path.display()))
        } else {
            Ok(Self::default())
        }
    }

    pub(crate) fn to_json_string(&self) -> Result<String> {
        serde_json::to_string(self).context("serialize resolved analyze config")
    }

    fn from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read analyze config {}", path.display()))?;
        Self::from_yaml_str(&raw)
    }

    fn from_yaml_str(raw: &str) -> Result<Self> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        let config: Self = serde_norway::from_str(raw)
            .map_err(|err| anyhow::anyhow!("invalid analyze config: {err}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let clustering = &self.analysis.clustering;
        ensure!(
            clustering.resolution.is_finite() && clustering.resolution > 0.0,
            "invalid analyze config: analysis.clustering.resolution must be a positive finite number"
        );
        ensure!(
            clustering.max_iterations > 0,
            "invalid analyze config: analysis.clustering.max_iterations must be greater than zero"
        );
        ensure!(
            clustering.min_cluster_size > 0,
            "invalid analyze config: analysis.clustering.min_cluster_size must be greater than zero"
        );
        if clustering.edge_types.is_empty() {
            bail!("invalid analyze config: analysis.clustering.edge_types must not be empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AnalysisConfig {
    pub(crate) clustering: ClusteringConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ClusteringConfig {
    pub(crate) enabled: bool,
    pub(crate) algorithm: ClusterAlgorithm,
    pub(crate) seed: u64,
    pub(crate) resolution: f64,
    pub(crate) max_iterations: u32,
    pub(crate) min_cluster_size: usize,
    pub(crate) edge_types: Vec<ClusteringEdgeType>,
    pub(crate) weight_by: ClusteringWeightBy,
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            algorithm: ClusterAlgorithm::Leiden,
            seed: 42,
            resolution: 1.0,
            max_iterations: 100,
            min_cluster_size: 3,
            edge_types: vec![ClusteringEdgeType::Imports, ClusteringEdgeType::Calls],
            weight_by: ClusteringWeightBy::ReferenceCount,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClusteringEdgeType {
    Imports,
    Calls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClusteringWeightBy {
    ReferenceCount,
}
