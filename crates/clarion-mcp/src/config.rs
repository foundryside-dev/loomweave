use std::fs;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    #[serde(alias = "llm_policy")]
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
        let config: Self =
            serde_norway::from_str(raw).map_err(|err| ConfigError::Yaml(err.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.llm.provider == LlmProviderKind::Anthropic
            || self.llm.anthropic_api_key_env.is_some()
        {
            return Err(ConfigError::DeprecatedProvider {
                code: "CLA-CONFIG-DEPRECATED-PROVIDER",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub provider: LlmProviderKind,
    pub allow_live_provider: bool,
    pub session_token_ceiling: u64,
    pub model_id: String,
    pub openrouter: OpenRouterConfig,
    pub recording_fixture_path: Option<String>,
    pub max_inferred_edges_per_caller: u32,
    pub cache_max_age_days: u32,
    pub anthropic_api_key_env: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: LlmProviderKind::OpenRouter,
            allow_live_provider: false,
            session_token_ceiling: 1_000_000,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            openrouter: OpenRouterConfig::default(),
            recording_fixture_path: None,
            max_inferred_edges_per_caller: 8,
            cache_max_age_days: 180,
            anthropic_api_key_env: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    #[serde(rename = "openrouter", alias = "open_router")]
    OpenRouter,
    Anthropic,
    Recording,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct OpenRouterConfig {
    pub endpoint_url: String,
    pub api_key_env: String,
    pub attribution: OpenRouterAttributionConfig,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            api_key_env: "OPENROUTER_API_KEY".to_owned(),
            attribution: OpenRouterAttributionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct OpenRouterAttributionConfig {
    pub referer: String,
    pub title: String,
}

impl Default for OpenRouterAttributionConfig {
    fn default() -> Self {
        Self {
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
        }
    }
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
    OpenRouter { api_key_env: String },
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
        LlmProviderKind::Anthropic => Err(ConfigError::DeprecatedProvider {
            code: "CLA-CONFIG-DEPRECATED-PROVIDER",
        }),
        LlmProviderKind::OpenRouter => {
            let live_env_opt_in = env_lookup("CLARION_LLM_LIVE").as_deref() == Some("1");
            if !config.llm.allow_live_provider && !live_env_opt_in {
                return Ok(ProviderSelection::Disabled);
            }

            let env_var = config.llm.openrouter.api_key_env.clone();
            let has_key = env_lookup(&env_var)
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
            if !has_key {
                return Err(ConfigError::MissingOpenRouterApiKey { env_var });
            }

            Ok(ProviderSelection::OpenRouter {
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

    #[error("live OpenRouter provider selected but API key env var {env_var} is missing")]
    MissingOpenRouterApiKey { env_var: String },

    #[error(
        "{code}: llm.provider=anthropic is deprecated; use llm_policy.provider: openrouter with llm_policy.openrouter.api_key_env and llm_policy.model_id"
    )]
    DeprecatedProvider { code: &'static str },
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
  provider: openrouter
  session_token_ceiling: 250000
  model_id: anthropic/claude-sonnet-4.6
  openrouter:
    endpoint_url: http://localhost:4000/api/v1
    api_key_env: TEST_OPENROUTER_KEY
    attribution:
      referer: https://example.invalid/clarion
      title: Clarion Test
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
        assert_eq!(cfg.llm.provider, LlmProviderKind::OpenRouter);
        assert_eq!(cfg.llm.session_token_ceiling, 250_000);
        assert_eq!(cfg.llm.model_id, "anthropic/claude-sonnet-4.6");
        assert_eq!(
            cfg.llm.openrouter.endpoint_url,
            "http://localhost:4000/api/v1"
        );
        assert_eq!(cfg.llm.openrouter.api_key_env, "TEST_OPENROUTER_KEY");
        assert_eq!(
            cfg.llm.openrouter.attribution.referer,
            "https://example.invalid/clarion"
        );
        assert_eq!(cfg.llm.openrouter.attribution.title, "Clarion Test");
        assert_eq!(cfg.llm.max_inferred_edges_per_caller, 3);
        assert_eq!(cfg.llm.cache_max_age_days, 7);
        assert!(cfg.integrations.filigree.enabled);
        assert_eq!(cfg.integrations.filigree.base_url, "http://127.0.0.1:9999");
        assert_eq!(cfg.integrations.filigree.actor, "clarion-test");
        assert_eq!(cfg.integrations.filigree.token_env, "TEST_FILIGREE_TOKEN");
        assert_eq!(cfg.integrations.filigree.timeout_seconds, 2);
    }

    #[test]
    fn accepts_llm_policy_alias_for_operator_config() {
        let cfg = McpConfig::from_yaml_str(
            r"
llm_policy:
  enabled: true
  provider: openrouter
  model_id: openai/gpt-4o-mini
",
        )
        .expect("parse config");

        assert!(cfg.llm.enabled);
        assert_eq!(cfg.llm.provider, LlmProviderKind::OpenRouter);
        assert_eq!(cfg.llm.model_id, "openai/gpt-4o-mini");
    }

    #[test]
    fn api_key_alone_does_not_select_live_provider() {
        let cfg = McpConfig {
            llm: LlmConfig {
                enabled: true,
                provider: LlmProviderKind::OpenRouter,
                ..LlmConfig::default()
            },
            integrations: IntegrationsConfig::default(),
        };

        let selected = select_provider_with_env(&cfg, |name| {
            (name == "OPENROUTER_API_KEY").then(|| "secret".to_owned())
        })
        .expect("provider selection");

        assert_eq!(selected, ProviderSelection::Disabled);
    }

    #[test]
    fn live_provider_requires_config_or_env_opt_in_and_api_key() {
        let cfg = McpConfig {
            llm: LlmConfig {
                enabled: true,
                provider: LlmProviderKind::OpenRouter,
                allow_live_provider: true,
                ..LlmConfig::default()
            },
            integrations: IntegrationsConfig::default(),
        };

        let missing = select_provider_with_env(&cfg, |_| None).expect_err("missing key");
        assert!(matches!(
            missing,
            ConfigError::MissingOpenRouterApiKey { ref env_var }
            if env_var == "OPENROUTER_API_KEY"
        ));

        let selected = select_provider_with_env(&cfg, |name| {
            (name == "OPENROUTER_API_KEY").then(|| "secret".to_owned())
        })
        .expect("provider selection");
        assert_eq!(
            selected,
            ProviderSelection::OpenRouter {
                api_key_env: "OPENROUTER_API_KEY".to_owned()
            }
        );
    }

    #[test]
    fn old_anthropic_provider_shape_reports_deprecated_provider() {
        let err = McpConfig::from_yaml_str(
            r"
llm:
  enabled: true
  provider: anthropic
  anthropic_api_key_env: ANTHROPIC_API_KEY
",
        )
        .expect_err("old provider shape should be rejected");

        assert!(matches!(err, ConfigError::DeprecatedProvider { .. }));
        assert!(err.to_string().contains("CLA-CONFIG-DEPRECATED-PROVIDER"));
        assert!(err.to_string().contains("provider: openrouter"));
    }
}
