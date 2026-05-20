use std::path::Path;
use std::{fs, net::SocketAddr};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    #[serde(alias = "llm_policy")]
    pub llm: LlmConfig,
    pub integrations: IntegrationsConfig,
    pub serve: ServeConfig,
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
        reject_llm_policy_alias_collision(raw)?;
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
        if self.integrations.filigree.enabled && self.integrations.filigree.actor.trim().is_empty()
        {
            return Err(ConfigError::InvalidFiligreeActor {
                code: "CLA-CONFIG-FILIGREE-ACTOR-BLANK",
            });
        }
        self.serve.http.validate_loopback_trust()?;
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
    pub codex_cli: CodexCliConfig,
    pub claude_cli: ClaudeCliConfig,
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
            codex_cli: CodexCliConfig::default(),
            claude_cli: ClaudeCliConfig::default(),
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
    #[serde(rename = "codex_cli", alias = "codex")]
    CodexCli,
    #[serde(rename = "claude_cli", alias = "claude_code")]
    ClaudeCli,
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
            referer: "https://github.com/tachyon-beep/clarion".to_owned(),
            title: "Clarion".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct CodexCliConfig {
    pub executable: String,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub sandbox: CodexSandboxMode,
    pub timeout_seconds: u64,
}

impl Default for CodexCliConfig {
    fn default() -> Self {
        Self {
            executable: "codex".to_owned(),
            model: None,
            profile: None,
            sandbox: CodexSandboxMode::ReadOnly,
            timeout_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ClaudeCliConfig {
    pub executable: String,
    pub model: Option<String>,
    pub permission_mode: ClaudePermissionMode,
    pub tools: Vec<String>,
    pub timeout_seconds: u64,
    pub max_turns: u32,
    pub no_session_persistence: bool,
    pub exclude_dynamic_system_prompt_sections: bool,
}

impl Default for ClaudeCliConfig {
    fn default() -> Self {
        Self {
            executable: "claude".to_owned(),
            model: None,
            permission_mode: ClaudePermissionMode::Plan,
            tools: Vec::new(),
            timeout_seconds: 300,
            max_turns: 2,
            no_session_persistence: true,
            exclude_dynamic_system_prompt_sections: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ClaudePermissionMode {
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
}

impl ClaudePermissionMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct IntegrationsConfig {
    pub filigree: FiligreeConfig,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ServeConfig {
    pub http: HttpReadConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct HttpReadConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_socket_addr")]
    pub bind: SocketAddr,
    pub allow_non_loopback: bool,
    /// Name of the env var holding the inbound bearer token. When the env
    /// var is set, every `/api/v1/files`-family request must carry
    /// `Authorization: Bearer <that-value>`; the capabilities probe is
    /// always unauthenticated. When the env var is unset on a loopback
    /// bind, the surface stays unauthenticated (the v0.1 trust model).
    /// When the env var is unset on a non-loopback bind, `clarion serve`
    /// refuses to start (`CLA-CONFIG-HTTP-NO-AUTH`). Default
    /// `CLARION_LOOM_TOKEN` matches Filigree's pinned client default.
    pub token_env: String,
    /// Optional env var holding the Loom component identity HMAC secret.
    /// When configured, `clarion serve` refuses to start unless the env var
    /// exists and protected HTTP read routes require
    /// `X-Loom-Component: clarion:<hmac>`.
    pub identity_token_env: Option<String>,
}

impl Default for HttpReadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: SocketAddr::from(([127, 0, 0, 1], 9111)),
            allow_non_loopback: false,
            token_env: "CLARION_LOOM_TOKEN".to_owned(),
            identity_token_env: None,
        }
    }
}

impl HttpReadConfig {
    pub fn validate_loopback_trust(&self) -> Result<(), ConfigError> {
        if self.enabled && !self.allow_non_loopback && !self.is_loopback_bind() {
            return Err(ConfigError::NonLoopbackHttpBind {
                code: "CLA-CONFIG-HTTP-NON-LOOPBACK",
                bind: self.bind,
            });
        }
        Ok(())
    }

    /// Refuse to start a non-loopback HTTP read API when the inbound bearer
    /// token env var is unset. Loopback binds with the env var unset stay
    /// unauthenticated (v0.1 trust matrix); the failure case is the explicit
    /// `allow_non_loopback: true` opt-in plus an unset `token_env`.
    pub fn validate_auth_trust<F>(&self, env_lookup: F) -> Result<(), ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        if !self.enabled {
            return Ok(());
        }
        let has_identity_secret = match self.identity_token_env.as_deref() {
            Some(env_var) => {
                let has_secret = env_lookup(env_var)
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                if !has_secret {
                    return Err(ConfigError::MissingHttpIdentitySecret {
                        code: "CLA-CONFIG-HTTP-IDENTITY-MISSING",
                        token_env: env_var.to_owned(),
                    });
                }
                true
            }
            None => false,
        };
        if self.is_loopback_bind() {
            return Ok(());
        }
        if has_identity_secret {
            return Ok(());
        }
        let has_token = env_lookup(&self.token_env)
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        if has_token {
            return Ok(());
        }
        Err(ConfigError::NonLoopbackHttpNoAuth {
            code: "CLA-CONFIG-HTTP-NO-AUTH",
            bind: self.bind,
            token_env: self.token_env.clone(),
        })
    }

    #[must_use]
    pub fn is_loopback_bind(&self) -> bool {
        self.bind.ip().is_loopback()
    }
}

fn deserialize_socket_addr<'de, D>(deserializer: D) -> Result<SocketAddr, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    raw.parse()
        .map_err(|err| serde::de::Error::custom(format!("invalid serve.http.bind {raw:?}: {err}")))
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
    CodexCli,
    ClaudeCli,
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
        LlmProviderKind::CodexCli => {
            let live_env_opt_in = env_lookup("CLARION_LLM_LIVE").as_deref() == Some("1");
            if !config.llm.allow_live_provider && !live_env_opt_in {
                return Ok(ProviderSelection::Disabled);
            }
            Ok(ProviderSelection::CodexCli)
        }
        LlmProviderKind::ClaudeCli => {
            let live_env_opt_in = env_lookup("CLARION_LLM_LIVE").as_deref() == Some("1");
            if !config.llm.allow_live_provider && !live_env_opt_in {
                return Ok(ProviderSelection::Disabled);
            }
            Ok(ProviderSelection::ClaudeCli)
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

    #[error("{code}: integrations.filigree.actor must not be blank when Filigree is enabled")]
    InvalidFiligreeActor { code: &'static str },

    #[error(
        "{code}: serve.http.bind {bind} exposes the unauthenticated non-loopback Clarion HTTP read API; \
         bind to loopback (127.0.0.1 or ::1) or set serve.http.allow_non_loopback: true only on a trusted network"
    )]
    NonLoopbackHttpBind {
        code: &'static str,
        bind: SocketAddr,
    },

    #[error(
        "{code}: serve.http.bind {bind} is non-loopback and serve.http.allow_non_loopback is true, \
         but the inbound auth env var ${token_env} is unset; refusing to start an unauthenticated \
         HTTP read API on a routable interface. Set ${token_env} to a non-empty bearer token, \
         or bind to loopback."
    )]
    NonLoopbackHttpNoAuth {
        code: &'static str,
        bind: SocketAddr,
        token_env: String,
    },

    #[error(
        "{code}: serve.http.identity_token_env names ${token_env}, but that env var is unset; \
         refusing to start an HTTP read API with incomplete Loom component identity configuration."
    )]
    MissingHttpIdentitySecret {
        code: &'static str,
        token_env: String,
    },

    #[error(
        "{code}: clarion.yaml contains both `llm` and `llm_policy` top-level keys; \
         `llm_policy` is a serde alias for `llm` and serde silently discards one. \
         Pick one and remove the other."
    )]
    AmbiguousLlmKey { code: &'static str },
}

/// Reject configs that name both `llm` and `llm_policy` at the top level.
/// They alias the same field; serde-norway silently picks one and discards
/// the other, which is the classic copy-paste-migration pitfall. Detecting
/// the collision pre-parse turns a silent override into a typed error.
fn reject_llm_policy_alias_collision(raw: &str) -> Result<(), ConfigError> {
    let value: serde_norway::Value = match serde_norway::from_str(raw) {
        Ok(value) => value,
        // If the YAML doesn't even parse as a generic Value, let the typed
        // parse below produce the canonical Yaml error.
        Err(_) => return Ok(()),
    };
    let Some(mapping) = value.as_mapping() else {
        return Ok(());
    };
    let has_llm = mapping.contains_key("llm");
    let has_llm_policy = mapping.contains_key("llm_policy");
    if has_llm && has_llm_policy {
        return Err(ConfigError::AmbiguousLlmKey {
            code: "CLA-CONFIG-AMBIGUOUS-LLM-KEY",
        });
    }
    Ok(())
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
    fn rejects_both_llm_and_llm_policy_keys_present_together() {
        // Realistic migration-doc copy-paste case: operator copies the new
        // `llm_policy:` block but forgets to delete the old `llm:` block.
        // Serde-norway would silently pick one and discard the other.
        let err = McpConfig::from_yaml_str(
            r"
llm:
  enabled: false
  provider: recording
llm_policy:
  enabled: true
  provider: openrouter
  model_id: openai/gpt-4o-mini
",
        )
        .expect_err("ambiguous llm key must be rejected");

        match err {
            ConfigError::AmbiguousLlmKey { code } => {
                assert_eq!(code, "CLA-CONFIG-AMBIGUOUS-LLM-KEY");
            }
            other => panic!("expected AmbiguousLlmKey error, got: {other:?}"),
        }
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
            serve: ServeConfig::default(),
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
            serve: ServeConfig::default(),
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
    fn codex_cli_provider_requires_live_opt_in_but_no_api_key() {
        let cfg = McpConfig::from_yaml_str(
            r"
llm_policy:
  enabled: true
  provider: codex_cli
  allow_live_provider: true
  model_id: codex-cli-default
  codex_cli:
    executable: /tmp/fake-codex
    model: gpt-5.5
    profile: clarion
    sandbox: read-only
    timeout_seconds: 30
",
        )
        .expect("parse Codex CLI provider config");

        assert_eq!(cfg.llm.provider, LlmProviderKind::CodexCli);
        assert_eq!(cfg.llm.model_id, "codex-cli-default");
        assert_eq!(cfg.llm.codex_cli.executable, "/tmp/fake-codex");
        assert_eq!(cfg.llm.codex_cli.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(cfg.llm.codex_cli.profile.as_deref(), Some("clarion"));
        assert_eq!(cfg.llm.codex_cli.sandbox, CodexSandboxMode::ReadOnly);
        assert_eq!(cfg.llm.codex_cli.timeout_seconds, 30);

        let selected = select_provider_with_env(&cfg, |_| None).expect("provider selection");
        assert_eq!(selected, ProviderSelection::CodexCli);
    }

    #[test]
    fn codex_cli_provider_stays_disabled_without_live_opt_in() {
        let cfg = McpConfig {
            llm: LlmConfig {
                enabled: true,
                provider: LlmProviderKind::CodexCli,
                ..LlmConfig::default()
            },
            integrations: IntegrationsConfig::default(),
            serve: ServeConfig::default(),
        };

        let selected = select_provider_with_env(&cfg, |_| None).expect("provider selection");
        assert_eq!(selected, ProviderSelection::Disabled);

        let env_selected = select_provider_with_env(&cfg, |name| {
            (name == "CLARION_LLM_LIVE").then(|| "1".to_owned())
        })
        .expect("provider selection via env opt-in");
        assert_eq!(env_selected, ProviderSelection::CodexCli);
    }

    #[test]
    fn claude_cli_provider_requires_live_opt_in_but_no_api_key() {
        let cfg = McpConfig::from_yaml_str(
            r#"
llm_policy:
  enabled: true
  provider: claude_cli
  allow_live_provider: true
  model_id: claude-code-default
  claude_cli:
    executable: /tmp/fake-claude
    model: claude-sonnet-4-6
    permission_mode: plan
    tools: ["Read", "Glob", "Grep"]
    timeout_seconds: 45
    max_turns: 2
    no_session_persistence: true
"#,
        )
        .expect("parse Claude CLI provider config");

        assert_eq!(cfg.llm.provider, LlmProviderKind::ClaudeCli);
        assert_eq!(cfg.llm.model_id, "claude-code-default");
        assert_eq!(cfg.llm.claude_cli.executable, "/tmp/fake-claude");
        assert_eq!(
            cfg.llm.claude_cli.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            cfg.llm.claude_cli.permission_mode,
            ClaudePermissionMode::Plan
        );
        assert_eq!(cfg.llm.claude_cli.tools, vec!["Read", "Glob", "Grep"]);
        assert_eq!(cfg.llm.claude_cli.timeout_seconds, 45);
        assert_eq!(cfg.llm.claude_cli.max_turns, 2);
        assert!(cfg.llm.claude_cli.no_session_persistence);

        let selected = select_provider_with_env(&cfg, |_| None).expect("provider selection");
        assert_eq!(selected, ProviderSelection::ClaudeCli);
    }

    #[test]
    fn claude_cli_provider_stays_disabled_without_live_opt_in() {
        let cfg = McpConfig {
            llm: LlmConfig {
                enabled: true,
                provider: LlmProviderKind::ClaudeCli,
                ..LlmConfig::default()
            },
            integrations: IntegrationsConfig::default(),
            serve: ServeConfig::default(),
        };

        let selected = select_provider_with_env(&cfg, |_| None).expect("provider selection");
        assert_eq!(selected, ProviderSelection::Disabled);

        let env_selected = select_provider_with_env(&cfg, |name| {
            (name == "CLARION_LLM_LIVE").then(|| "1".to_owned())
        })
        .expect("provider selection via env opt-in");
        assert_eq!(env_selected, ProviderSelection::ClaudeCli);
    }

    #[test]
    fn http_bind_is_parsed_when_config_loads() {
        let cfg = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "127.0.0.1:0"
"#,
        )
        .expect("parse HTTP bind");

        assert_eq!(cfg.serve.http.bind, SocketAddr::from(([127, 0, 0, 1], 0)));
    }

    #[test]
    fn http_allow_non_loopback_defaults_false() {
        assert!(!McpConfig::default().serve.http.allow_non_loopback);
    }

    #[test]
    fn http_allow_non_loopback_is_parsed_when_config_loads() {
        let cfg = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "127.0.0.1:0"
    allow_non_loopback: true
"#,
        )
        .expect("parse HTTP allow_non_loopback");

        assert!(cfg.serve.http.allow_non_loopback);
    }

    #[test]
    fn http_identity_token_env_is_parsed_when_config_loads() {
        let cfg = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "127.0.0.1:0"
    identity_token_env: CLARION_TEST_IDENTITY
"#,
        )
        .expect("parse HTTP identity_token_env");

        assert_eq!(
            cfg.serve.http.identity_token_env.as_deref(),
            Some("CLARION_TEST_IDENTITY")
        );
    }

    #[test]
    fn enabled_non_loopback_http_bind_requires_allow_non_loopback() {
        let err = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "0.0.0.0:0"
"#,
        )
        .expect_err("enabled wildcard HTTP bind should require explicit opt-in");

        let message = err.to_string();
        assert!(
            message.contains("unauthenticated non-loopback"),
            "error should explain the unauthenticated non-loopback risk: {message}"
        );
        assert!(
            message.contains("allow_non_loopback"),
            "error should name the explicit opt-in: {message}"
        );
    }

    #[test]
    fn enabled_lan_http_bind_requires_allow_non_loopback() {
        let err = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "192.168.1.10:0"
"#,
        )
        .expect_err("enabled LAN HTTP bind should require explicit opt-in");

        assert!(matches!(err, ConfigError::NonLoopbackHttpBind { .. }));
    }

    #[test]
    fn enabled_ipv6_loopback_http_bind_is_allowed_by_default() {
        let cfg = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "[::1]:0"
"#,
        )
        .expect("IPv6 loopback HTTP bind should not require non-loopback opt-in");

        assert!(!cfg.serve.http.allow_non_loopback);
        assert!(cfg.serve.http.is_loopback_bind());
    }

    #[test]
    fn enabled_non_loopback_http_bind_allows_explicit_opt_in() {
        let cfg = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "0.0.0.0:0"
    allow_non_loopback: true
"#,
        )
        .expect("explicit opt-in should allow non-loopback HTTP bind");

        assert!(cfg.serve.http.allow_non_loopback);
    }

    #[test]
    fn invalid_http_bind_fails_config_load() {
        let err = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "not-a-socket"
"#,
        )
        .expect_err("invalid bind should fail");

        assert!(
            err.to_string().contains("invalid serve.http.bind"),
            "unexpected error: {err}"
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

    #[test]
    fn enabled_filigree_integration_rejects_blank_actor() {
        let err = McpConfig::from_yaml_str(
            r#"
integrations:
  filigree:
    enabled: true
    actor: "   "
"#,
        )
        .expect_err("blank Filigree actor should be rejected");

        assert!(err.to_string().contains("CLA-CONFIG-FILIGREE-ACTOR-BLANK"));
    }
}
