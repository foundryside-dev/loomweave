use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use loomweave_analysis::ClusterAlgorithm;
use loomweave_federation::config::{McpConfig, ProviderSelection, select_provider_with_env};
use serde::{Deserialize, Serialize};

// NOTE: Do not use `\` line-continuation in this string — Rust strips both the
// newline AND all leading whitespace on the continuation line, producing flat
// (and therefore broken) YAML. Use raw newlines + explicit indentation.
//
// This is the single source of truth for the default `loomweave.yaml`: both
// `loomweave install` (writes it on init) and `loomweave config example` (prints
// it) use this exact text, so they can never drift. A round-trip test
// (`stub_parses_under_deny_unknown_fields`) asserts it parses cleanly under the
// config structs' `deny_unknown_fields` — guarding against stub↔struct drift.
pub(crate) const LOOMWEAVE_YAML_STUB: &str = "# loomweave.yaml — user-edited config.
# Do not delete this file: loomweave serve reads MCP, LLM, and integration
# settings from here when present. Validate it any time with `loomweave config check`.
version: 1
# --- LLM summaries (entity_summary_get) --------------------------------------
# OFF by default. To enable LIVE summaries:
#   1. set both enabled: true AND allow_live_provider: true below; then
#   2. either keep provider: openrouter and export the key named by
#      openrouter.api_key_env (default OPENROUTER_API_KEY), OR switch provider to
#      claude_cli / codex_cli to drive a locally-authenticated coding-agent CLI
#      (no API key stored in this file).
# `loomweave config check` prints the resulting effective state and any warnings.
llm_policy:
  enabled: false
  provider: openrouter
  allow_live_provider: false
  openrouter:
    endpoint_url: https://openrouter.ai/api/v1
    api_key_env: OPENROUTER_API_KEY
    attribution:
      referer: https://github.com/foundryside-dev/loomweave
      title: Loomweave
  codex_cli:
    executable: codex
    model: null
    profile: null
    sandbox: read-only
    timeout_seconds: 300
  claude_cli:
    executable: claude
    model: null
    permission_mode: plan
    tools: []
    timeout_seconds: 300
    max_turns: 2
    no_session_persistence: true
    exclude_dynamic_system_prompt_sections: true
  model_id: anthropic/claude-sonnet-4.6
  session_token_ceiling: 1000000
  max_inferred_edges_per_caller: 8
  cache_max_age_days: 180
integrations:
  filigree:
    enabled: false
    base_url: http://127.0.0.1:8766
    actor: loomweave-mcp
    token_env: WEFT_FEDERATION_TOKEN
    timeout_seconds: 5
serve:
  mcp:
    enable_write_tools: false
  http:
    enabled: false
    # The read-API port is auto-selected per project (deterministic, with an
    # ephemeral fallback) and published to .loomweave/ephemeral.port while
    # serving. Set `bind:` explicitly only to pin a fixed port (ADR-044).
";

/// Dispatch `loomweave config <subcommand>`.
pub(crate) fn run(command: crate::cli::ConfigCommand) -> Result<()> {
    match command {
        crate::cli::ConfigCommand::Example { provider } => run_example(provider.as_deref()),
        crate::cli::ConfigCommand::Check { path, config } => run_check(&path, config.as_deref()),
    }
}

/// Print the annotated default `loomweave.yaml`, optionally pre-selecting the
/// active LLM provider block.
fn run_example(provider: Option<&str>) -> Result<()> {
    let yaml = match provider {
        None | Some("openrouter") => LOOMWEAVE_YAML_STUB.to_owned(),
        Some(p @ ("codex_cli" | "claude_cli")) => {
            // The stub already carries every provider sub-block, so selecting a
            // provider is just swapping the active `provider:` line.
            LOOMWEAVE_YAML_STUB.replacen("  provider: openrouter", &format!("  provider: {p}"), 1)
        }
        Some(other) => bail!(
            "unknown --provider {other:?}; expected one of: openrouter, codex_cli, claude_cli"
        ),
    };
    print!("{yaml}");
    Ok(())
}

/// Parse + validate `loomweave.yaml` and print the effective LLM provider state.
/// A parse/validate failure bubbles as an error (non-zero exit); a
/// provider-selection error (e.g. live provider with a missing API key) is a
/// real misconfiguration and also exits non-zero, after printing the diagnosis.
fn run_check(path: &Path, explicit_config: Option<&Path>) -> Result<()> {
    let default_path = path.join("loomweave.yaml");
    let config_path = explicit_config.unwrap_or(&default_path);
    let (config, source) = if config_path.exists() {
        let config = McpConfig::from_path(config_path)
            .with_context(|| format!("parse {}", config_path.display()))?;
        (config, config_path.display().to_string())
    } else {
        (
            McpConfig::default(),
            "(absent — built-in defaults in effect)".to_owned(),
        )
    };

    let selection = select_provider_with_env(&config, |name| std::env::var(name).ok());

    println!("loomweave.yaml:        {source}");
    println!("LLM enabled:           {}", config.llm.enabled);
    println!("Provider (configured): {}", config.llm.provider.as_str());
    println!("allow_live_provider:   {}", config.llm.allow_live_provider);
    println!(
        "Effective model:       {}",
        config.llm.effective_model_label()
    );
    match &selection {
        Ok(sel) => {
            let live = matches!(
                sel,
                ProviderSelection::OpenRouter { .. }
                    | ProviderSelection::CodexCli
                    | ProviderSelection::ClaudeCli
            );
            println!(
                "Live:                  {}",
                if live {
                    "yes — entity_summary_get will dispatch to the provider"
                } else {
                    "no — entity_summary_get is cache-only"
                }
            );
        }
        Err(err) => println!("Live:                  error — {err}"),
    }

    let warnings = config.llm_warnings();
    if warnings.is_empty() {
        println!("\nNo warnings.");
    } else {
        println!("\nWarnings:");
        for warning in &warnings {
            println!("  - {warning}");
        }
    }

    if selection.is_err() {
        std::process::exit(1);
    }
    Ok(())
}

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

        let default_path = project_root.join("loomweave.yaml");
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
        ensure!(
            clustering.weak_modularity_threshold.is_finite()
                && clustering.weak_modularity_threshold >= 0.0,
            "invalid analyze config: analysis.clustering.weak_modularity_threshold must be a non-negative finite number"
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
    pub(crate) weak_modularity_threshold: f64,
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
            weak_modularity_threshold: 0.3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClusteringEdgeType {
    Imports,
    Calls,
}

impl ClusteringEdgeType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ClusteringEdgeType::Imports => "imports",
            ClusteringEdgeType::Calls => "calls",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClusteringWeightBy {
    ReferenceCount,
}

impl ClusteringWeightBy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ClusteringWeightBy::ReferenceCount => "reference_count",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LOOMWEAVE_YAML_STUB;
    use loomweave_federation::config::McpConfig;

    #[test]
    fn stub_parses_under_deny_unknown_fields() {
        // The default loomweave.yaml `install` writes (and `config example`
        // prints) must parse cleanly through the config structs, which now use
        // deny_unknown_fields. This guards against the stub drifting from the
        // structs — a drift would otherwise ship a config the binary rejects.
        let config = McpConfig::from_yaml_str(LOOMWEAVE_YAML_STUB)
            .expect("install stub must parse under deny_unknown_fields");
        assert_eq!(config.version, 1);
        assert!(
            !config.llm.enabled,
            "stub ships with LLM disabled by default"
        );
        assert!(!config.serve.mcp.enable_write_tools);
    }

    #[test]
    fn stub_also_parses_via_analyze_config() {
        // install/analyze read the same file through AnalyzeConfig (clustering
        // only); confirm the stub round-trips there too.
        super::AnalyzeConfig::from_yaml_str(LOOMWEAVE_YAML_STUB)
            .expect("install stub must parse as analyze config");
    }
}
