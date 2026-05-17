use std::fs;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    pub llm: LlmConfig,
    pub integrations: IntegrationsConfig,
}

impl McpConfig {
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_yaml_str(&raw)
    }

    pub fn from_yaml_str(raw: &str) -> Result<Self, ConfigError> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_yml::from_str(raw).map_err(|err| ConfigError::Yaml(err.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: LlmProviderKind,
    pub allow_live_provider: bool,
    pub session_cost_ceiling_usd: f64,
    pub summary_model_id: String,
    pub inferred_edges_model_id: String,
    pub max_inferred_edges_per_caller: u32,
    pub cache_max_age_days: u32,
    pub anthropic_api_key_env: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: LlmProviderKind::Anthropic,
            allow_live_provider: false,
            session_cost_ceiling_usd: 10.0,
            summary_model_id: "claude-haiku-4-5".to_owned(),
            inferred_edges_model_id: "claude-haiku-4-5".to_owned(),
            max_inferred_edges_per_caller: 8,
            cache_max_age_days: 180,
            anthropic_api_key_env: "ANTHROPIC_API_KEY".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    Anthropic,
    Recording,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct IntegrationsConfig {
    pub filigree: FiligreeConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct FiligreeConfig {
    pub enabled: bool,
    pub base_url: String,
    pub actor: String,
    pub token_env: String,
    pub timeout_seconds: u64,
}

impl Default for FiligreeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://127.0.0.1:8766".to_owned(),
            actor: "clarion-mcp".to_owned(),
            token_env: "FILIGREE_API_TOKEN".to_owned(),
            timeout_seconds: 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSelection {
    Disabled,
    Recording,
    Anthropic { api_key_env: String },
}

pub fn select_provider_with_env<F>(
    config: &McpConfig,
    env_lookup: F,
) -> Result<ProviderSelection, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    if !config.llm.enabled {
        return Ok(ProviderSelection::Disabled);
    }

    match config.llm.provider {
        LlmProviderKind::Recording => Ok(ProviderSelection::Recording),
        LlmProviderKind::Anthropic => {
            let live_env_opt_in = env_lookup("CLARION_LLM_LIVE").as_deref() == Some("1");
            if !config.llm.allow_live_provider && !live_env_opt_in {
                return Ok(ProviderSelection::Disabled);
            }

            let env_var = config.llm.anthropic_api_key_env.clone();
            let has_key = env_lookup(&env_var)
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            if !has_key {
                return Err(ConfigError::MissingAnthropicApiKey { env_var });
            }

            Ok(ProviderSelection::Anthropic {
                api_key_env: env_var,
            })
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read MCP config {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid MCP config: {0}")]
    Yaml(String),

    #[error("live Anthropic provider selected but API key env var {env_var} is missing")]
    MissingAnthropicApiKey { env_var: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mcp_llm_and_filigree_config() {
        let cfg = McpConfig::from_yaml_str(
            r#"
llm:
  enabled: true
  provider: recording
  session_cost_ceiling_usd: 2.5
  summary_model_id: claude-test-summary
  inferred_edges_model_id: claude-test-infer
  max_inferred_edges_per_caller: 3
  cache_max_age_days: 7
integrations:
  filigree:
    enabled: true
    base_url: "http://127.0.0.1:9999"
    actor: "clarion-test"
    token_env: TEST_FILIGREE_TOKEN
    timeout_seconds: 2
"#,
        )
        .expect("parse config");

        assert!(cfg.llm.enabled);
        assert_eq!(cfg.llm.provider, LlmProviderKind::Recording);
        assert!((cfg.llm.session_cost_ceiling_usd - 2.5).abs() < f64::EPSILON);
        assert_eq!(cfg.llm.summary_model_id, "claude-test-summary");
        assert_eq!(cfg.llm.inferred_edges_model_id, "claude-test-infer");
        assert_eq!(cfg.llm.max_inferred_edges_per_caller, 3);
        assert_eq!(cfg.llm.cache_max_age_days, 7);
        assert!(cfg.integrations.filigree.enabled);
        assert_eq!(cfg.integrations.filigree.base_url, "http://127.0.0.1:9999");
        assert_eq!(cfg.integrations.filigree.actor, "clarion-test");
        assert_eq!(cfg.integrations.filigree.token_env, "TEST_FILIGREE_TOKEN");
        assert_eq!(cfg.integrations.filigree.timeout_seconds, 2);
    }

    #[test]
    fn api_key_alone_does_not_select_live_provider() {
        let cfg = McpConfig {
            llm: LlmConfig {
                enabled: true,
                provider: LlmProviderKind::Anthropic,
                ..LlmConfig::default()
            },
            integrations: IntegrationsConfig::default(),
        };

        let selected = select_provider_with_env(&cfg, |name| {
            (name == "ANTHROPIC_API_KEY").then(|| "secret".to_owned())
        })
        .expect("provider selection");

        assert_eq!(selected, ProviderSelection::Disabled);
    }

    #[test]
    fn live_provider_requires_config_or_env_opt_in_and_api_key() {
        let cfg = McpConfig {
            llm: LlmConfig {
                enabled: true,
                provider: LlmProviderKind::Anthropic,
                allow_live_provider: true,
                ..LlmConfig::default()
            },
            integrations: IntegrationsConfig::default(),
        };

        let missing = select_provider_with_env(&cfg, |_| None).expect_err("missing key");
        assert!(matches!(
            missing,
            ConfigError::MissingAnthropicApiKey { ref env_var }
            if env_var == "ANTHROPIC_API_KEY"
        ));

        let selected = select_provider_with_env(&cfg, |name| {
            (name == "ANTHROPIC_API_KEY").then(|| "secret".to_owned())
        })
        .expect("provider selection");
        assert_eq!(
            selected,
            ProviderSelection::Anthropic {
                api_key_env: "ANTHROPIC_API_KEY".to_owned()
            }
        );
    }
}
