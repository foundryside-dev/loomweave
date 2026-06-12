use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use loomweave_analysis::ClusterAlgorithm;
use loomweave_federation::config::{
    LlmConfigEditResult, LlmConfigPatch, LlmProviderKind, McpConfig, ProviderSelection,
    SemanticConfigEditResult, SemanticConfigPatch, SemanticProviderKind, select_provider_with_env,
    update_llm_config_file, update_semantic_config_file,
};
use rusqlite::{Connection, OptionalExtension};
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
#      claude_sidecar / codex_sidecar to drive a locally-authenticated
#      coding-agent CLI (canonical values: claude_cli / codex_cli)
#      (no API key stored in this file).
# `loomweave config llm set --enable --allow-live --provider codex_sidecar
# --enable-write-tools` updates these fields without hand-editing.
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
# --- Semantic search embeddings (entity_semantic_search_list) ----------------
# OFF by default. To enable local semantic ranking:
#   1. run a local OpenAI-compatible embeddings server on loopback; then
#   2. run:
#      loomweave config semantic set --enable --provider local_openai \
#        --endpoint-url http://127.0.0.1:11434/v1 \
#        --model-id nomic-embed-text --dimensions 768
#   3. rerun `loomweave analyze` to populate .weft/loomweave/embeddings.db.
# Hosted OpenAI-compatible APIs use provider: api, allow_live_provider: true,
# and api_key_env naming the API key env var.
semantic_search:
  enabled: false
  provider: local_openai
  allow_live_provider: false
  endpoint_url: http://127.0.0.1:11434/v1
  model_id: nomic-embed-text
  dimensions: 768
  api_key_env: OPENAI_API_KEY
  timeout_seconds: 60
  session_token_ceiling: 5000000
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
    # ephemeral fallback) and published to .weft/loomweave/ephemeral.port while
    # serving. Set `bind:` explicitly only to pin a fixed port (ADR-044).
";

/// Dispatch `loomweave config <subcommand>`.
pub(crate) fn run(command: crate::cli::ConfigCommand) -> Result<()> {
    match command {
        crate::cli::ConfigCommand::Example { provider } => run_example(provider.as_deref()),
        crate::cli::ConfigCommand::Check { path, config } => run_check(&path, config.as_deref()),
        crate::cli::ConfigCommand::Llm { command } => run_llm(command),
        crate::cli::ConfigCommand::Semantic { command } => run_semantic(command),
    }
}

fn run_llm(command: crate::cli::LlmConfigCommand) -> Result<()> {
    match command {
        crate::cli::LlmConfigCommand::Status { path, config } => {
            run_check(&path, config.as_deref())
        }
        crate::cli::LlmConfigCommand::Set {
            path,
            config,
            enable,
            disable,
            allow_live,
            disallow_live,
            enable_write_tools,
            disable_write_tools,
            provider,
            model_id,
            codex_model,
            claude_model,
            openrouter_api_key_env,
            openrouter_endpoint_url,
        } => {
            let config_path = config.unwrap_or_else(|| path.join("loomweave.yaml"));
            let patch = LlmConfigPatch {
                enabled: bool_patch(enable, disable, "--enable", "--disable")?,
                provider: provider
                    .as_deref()
                    .map(LlmProviderKind::parse)
                    .transpose()?,
                allow_live_provider: bool_patch(
                    allow_live,
                    disallow_live,
                    "--allow-live",
                    "--disallow-live",
                )?,
                enable_write_tools: bool_patch(
                    enable_write_tools,
                    disable_write_tools,
                    "--enable-write-tools",
                    "--disable-write-tools",
                )?,
                model_id,
                codex_model,
                claude_model,
                openrouter_api_key_env,
                openrouter_endpoint_url,
            };
            ensure_patch_non_empty(&patch)?;
            let result = update_llm_config_file(&config_path, &patch)
                .with_context(|| format!("update {}", config_path.display()))?;
            print_llm_edit_result(&result);
            Ok(())
        }
    }
}

fn run_semantic(command: crate::cli::SemanticConfigCommand) -> Result<()> {
    match command {
        crate::cli::SemanticConfigCommand::Status { path, config } => {
            run_semantic_status(&path, config.as_deref())
        }
        crate::cli::SemanticConfigCommand::Set {
            path,
            config,
            enable,
            disable,
            provider,
            allow_live,
            disallow_live,
            model_id,
            dimensions,
            endpoint_url,
            api_key_env,
            timeout_seconds,
            session_token_ceiling,
        } => {
            let config_path = config.unwrap_or_else(|| path.join("loomweave.yaml"));
            let patch = SemanticConfigPatch {
                enabled: bool_patch(enable, disable, "--enable", "--disable")?,
                provider: provider
                    .as_deref()
                    .map(SemanticProviderKind::parse)
                    .transpose()?,
                allow_live_provider: bool_patch(
                    allow_live,
                    disallow_live,
                    "--allow-live",
                    "--disallow-live",
                )?,
                model_id,
                dimensions,
                endpoint_url,
                api_key_env,
                timeout_seconds,
                session_token_ceiling,
            };
            ensure_semantic_patch_non_empty(&patch)?;
            let result = update_semantic_config_file(&config_path, &patch)
                .with_context(|| format!("update {}", config_path.display()))?;
            print_semantic_edit_result(&path, &result);
            Ok(())
        }
    }
}

fn bool_patch(
    enabled: bool,
    disabled: bool,
    enable_flag: &str,
    disable_flag: &str,
) -> Result<Option<bool>> {
    if enabled && disabled {
        bail!("{enable_flag} conflicts with {disable_flag}");
    }
    Ok(match (enabled, disabled) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
        (true, true) => unreachable!("checked above"),
    })
}

fn ensure_patch_non_empty(patch: &LlmConfigPatch) -> Result<()> {
    ensure!(
        patch.enabled.is_some()
            || patch.provider.is_some()
            || patch.allow_live_provider.is_some()
            || patch.enable_write_tools.is_some()
            || patch.model_id.is_some()
            || patch.codex_model.is_some()
            || patch.claude_model.is_some()
            || patch.openrouter_api_key_env.is_some()
            || patch.openrouter_endpoint_url.is_some(),
        "no LLM config changes requested"
    );
    Ok(())
}

fn ensure_semantic_patch_non_empty(patch: &SemanticConfigPatch) -> Result<()> {
    ensure!(
        patch.enabled.is_some()
            || patch.provider.is_some()
            || patch.allow_live_provider.is_some()
            || patch.model_id.is_some()
            || patch.dimensions.is_some()
            || patch.endpoint_url.is_some()
            || patch.api_key_env.is_some()
            || patch.timeout_seconds.is_some()
            || patch.session_token_ceiling.is_some(),
        "no semantic search config changes requested"
    );
    Ok(())
}

fn print_llm_edit_result(result: &LlmConfigEditResult) {
    println!("Updated:               {}", result.path);
    println!("Created:               {}", result.created);
    println!("LLM enabled:           {}", result.config.llm.enabled);
    println!(
        "Provider (configured): {}",
        result.config.llm.provider.as_str()
    );
    println!(
        "allow_live_provider:   {}",
        result.config.llm.allow_live_provider
    );
    println!(
        "Effective model:       {}",
        result.config.llm.effective_model_label()
    );
    println!(
        "MCP write tools:       {}",
        result.config.serve.mcp.enable_write_tools
    );
}

fn print_semantic_edit_result(project_root: &Path, result: &SemanticConfigEditResult) {
    println!("Updated:                {}", result.path);
    println!("Created:                {}", result.created);
    print_semantic_status_fields(project_root, &result.config);
    println!("Analyze required:       true");
    println!("Restart required:       true");
}

/// Print the annotated default `loomweave.yaml`, optionally pre-selecting the
/// active LLM provider block.
fn run_example(provider: Option<&str>) -> Result<()> {
    let yaml = match provider {
        None | Some("openrouter" | "openrouter_api") => LOOMWEAVE_YAML_STUB.to_owned(),
        Some(provider @ ("codex_cli" | "codex_sidecar" | "claude_cli" | "claude_sidecar")) => {
            // The stub already carries every provider sub-block, so selecting a
            // provider is just swapping the active `provider:` line.
            let p = match provider {
                "codex_sidecar" => "codex_cli",
                "claude_sidecar" => "claude_cli",
                other => other,
            };
            LOOMWEAVE_YAML_STUB.replacen("  provider: openrouter", &format!("  provider: {p}"), 1)
        }
        Some(other) => bail!(
            "unknown --provider {other:?}; expected one of: openrouter, openrouter_api, \
             codex_cli, codex_sidecar, claude_cli, claude_sidecar"
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
    println!();
    print_semantic_status_fields(path, &config);
    Ok(())
}

fn run_semantic_status(path: &Path, explicit_config: Option<&Path>) -> Result<()> {
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
    println!("loomweave.yaml:         {source}");
    print_semantic_status_fields(path, &config);
    Ok(())
}

fn print_semantic_status_fields(project_root: &Path, config: &McpConfig) {
    let semantic = &config.semantic_search;
    let sidecar = project_root.join(".weft/loomweave/embeddings.db");
    let count = embedding_sidecar_count(&sidecar);
    let has_key = std::env::var(&semantic.api_key_env)
        .ok()
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let provider_available = semantic_provider_available(semantic, has_key).unwrap_or(false);
    println!("Semantic enabled:       {}", semantic.enabled);
    println!("Semantic provider:      {}", semantic.provider.as_str());
    println!("allow_live_provider:    {}", semantic.allow_live_provider);
    println!("Endpoint URL:           {}", semantic.endpoint_url);
    println!("Embedding model:        {}", semantic.model_id);
    println!("Dimensions:             {}", semantic.dimensions);
    println!(
        "API key env:            {} ({})",
        semantic.api_key_env,
        if has_key { "set" } else { "unset" }
    );
    println!("Embeddings sidecar:     {}", sidecar.display());
    match count {
        Ok(Some(count)) => println!("Sidecar vectors:        {count}"),
        Ok(None) => println!("Sidecar vectors:        absent"),
        Err(ref err) => println!("Sidecar vectors:        unavailable — {err}"),
    }
    println!("Provider available:     {provider_available}");
    println!(
        "Next action:            {}",
        semantic_next_action(semantic, has_key, count.ok().flatten())
    );
}

fn semantic_provider_available(
    semantic: &loomweave_federation::config::SemanticSearchConfig,
    has_key: bool,
) -> Result<bool> {
    if !semantic.enabled {
        return Ok(false);
    }
    match semantic.provider {
        SemanticProviderKind::Api => Ok(semantic.allow_live_provider && has_key),
        SemanticProviderKind::LocalOpenAi => {
            semantic
                .validate_endpoint_trust()
                .context("validate local semantic endpoint")?;
            Ok(true)
        }
    }
}

fn semantic_next_action(
    semantic: &loomweave_federation::config::SemanticSearchConfig,
    has_key: bool,
    sidecar_count: Option<i64>,
) -> String {
    if !semantic.enabled {
        return "enable semantic search, then run `loomweave analyze`".to_owned();
    }
    match semantic.provider {
        SemanticProviderKind::Api if !semantic.allow_live_provider => {
            return "set semantic_search.allow_live_provider: true for hosted API calls".to_owned();
        }
        SemanticProviderKind::Api if !has_key => {
            return format!("export ${} before analyzing", semantic.api_key_env);
        }
        SemanticProviderKind::LocalOpenAi => {
            if sidecar_count.unwrap_or(0) == 0 {
                return format!(
                    "start the local embeddings server at {}, then run `loomweave analyze`",
                    semantic.endpoint_url
                );
            }
        }
        _ => {}
    }
    if sidecar_count.unwrap_or(0) == 0 {
        "run `loomweave analyze` to populate semantic embeddings".to_owned()
    } else {
        "reconnect/restart `loomweave serve` after config changes; semantic search can use the sidecar".to_owned()
    }
}

fn embedding_sidecar_count(path: &Path) -> Result<Option<i64>> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.query_row("SELECT COUNT(*) FROM entity_embeddings", [], |row| {
        row.get(0)
    })
    .optional()
    .with_context(|| format!("count vectors in {}", path.display()))
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
