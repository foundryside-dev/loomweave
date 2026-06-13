use std::path::Path;
use std::{
    fs,
    net::{IpAddr, SocketAddr},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Config schema version marker. Accepted and currently informational; it
    /// exists so a versioned `loomweave.yaml` (the install stub writes
    /// `version: 1`) still parses under `deny_unknown_fields`.
    pub version: u32,
    #[serde(alias = "llm_policy")]
    pub llm: LlmConfig,
    pub semantic_search: SemanticSearchConfig,
    pub integrations: IntegrationsConfig,
    pub serve: ServeConfig,
    /// Tolerated-and-ignored sibling section. The same `loomweave.yaml` is
    /// parsed by two structs: `AnalyzeConfig` (loomweave-cli) owns the top-level
    /// `analysis:` clustering block, while `McpConfig` owns `integrations` and is
    /// consulted at finding-emission time. Because `McpConfig` is
    /// `deny_unknown_fields` (so typos in the fields it *does* own fail loudly —
    /// agent-first-feedback §2), it must still declare `analysis` or it rejects
    /// any config carrying that documented section, silently disabling Filigree
    /// emission via `load_mcp_config`'s default-on-error fallback. Captured as an
    /// opaque value and never read here; `AnalyzeConfig` is the typed owner.
    #[serde(default)]
    pub analysis: serde_norway::Value,
}

fn default_config_version() -> u32 {
    1
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            llm: LlmConfig::default(),
            semantic_search: SemanticSearchConfig::default(),
            integrations: IntegrationsConfig::default(),
            serve: ServeConfig::default(),
            analysis: serde_norway::Value::Null,
        }
    }
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

    /// Parse the document for *structure* (schema shape + alias-collision
    /// guard) plus the trust of ONLY the section just edited, WITHOUT the
    /// cross-section trust validation that whole-config `validate()` runs.
    ///
    /// The `config` tool is the recovery surface for a federation that has
    /// drifted into an untrusted state, and `validate()` is whole-config: it
    /// rejects on a stale value in a section the caller never touched. That
    /// turns the recovery surface into a trap — `config llm set --disable` was
    /// blocked by a stale enabled non-loopback `semantic_search`, and
    /// `config semantic set --disable` (the very action that clears the
    /// offending state) was blocked by a stale enabled non-loopback
    /// `serve.http` (L2). An edit to one section must never be gated by
    /// another. The just-edited section IS still trust-validated (so
    /// re-enabling a non-loopback endpoint in the same edit is still refused);
    /// any cross-section trust issue that genuinely remains surfaces at the
    /// next full load (`serve` / `doctor` / `config check`).
    fn from_yaml_str_section_scoped(
        raw: &str,
        edited: EditedSection,
    ) -> Result<Self, ConfigError> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        reject_llm_policy_alias_collision(raw)?;
        let config: Self =
            serde_norway::from_str(raw).map_err(|err| ConfigError::Yaml(err.to_string()))?;
        match edited {
            EditedSection::SemanticSearch => config.semantic_search.validate_endpoint_trust()?,
            EditedSection::Llm => {}
        }
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.integrations.filigree.enabled && self.integrations.filigree.actor.trim().is_empty()
        {
            return Err(ConfigError::InvalidFiligreeActor {
                code: "LMWV-CONFIG-FILIGREE-ACTOR-BLANK",
            });
        }
        self.semantic_search.validate_endpoint_trust()?;
        self.serve.http.validate_loopback_trust()?;
        Ok(())
    }

    /// Non-fatal diagnostics about the *effective* LLM state, for surfacing at
    /// `serve` startup, in `loomweave doctor`, and in `loomweave config check`.
    ///
    /// These never fail config load — `enabled: false` is the legitimate
    /// safe default — they only explain why a configured provider may be inert,
    /// so a misconfiguration announces itself instead of silently disabling
    /// summaries (the agent-first-feedback §2.1 failure mode).
    #[must_use]
    pub fn llm_warnings(&self) -> Vec<String> {
        let llm = &self.llm;
        let provider = llm.provider.as_str();
        let mut warnings = Vec::new();
        if !llm.enabled {
            if llm.allow_live_provider {
                warnings.push(format!(
                    "llm_policy.provider={provider} with allow_live_provider=true but \
                     enabled=false → live summaries are off and entity_summary_get is \
                     cache-only. Set llm_policy.enabled: true to enable."
                ));
            }
        } else if !llm.allow_live_provider {
            warnings.push(format!(
                "llm_policy.enabled=true with provider={provider} but \
                 allow_live_provider=false → live summaries are off (unless \
                 LOOMWEAVE_LLM_LIVE=1 is set); entity_summary_get is cache-only. Set \
                 llm_policy.allow_live_provider: true to enable live calls."
            ));
        } else {
            // Live path is on: warn about an unpinned coding-agent model, which
            // inherits the local CLI default and can be an expensive tier
            // (agent-first-feedback §2.6).
            match llm.provider {
                LlmProviderKind::ClaudeCli if llm.claude_cli.model.is_none() => warnings.push(
                    "llm_policy.claude_cli.model is unset → summaries inherit the local \
                     `claude` CLI default model, which may be an expensive tier. Pin \
                     llm_policy.claude_cli.model to control per-summary cost."
                        .to_owned(),
                ),
                LlmProviderKind::CodexCli if llm.codex_cli.model.is_none() => warnings.push(
                    "llm_policy.codex_cli.model is unset → summaries inherit the local \
                     `codex` CLI default model. Pin llm_policy.codex_cli.model to control \
                     per-summary cost."
                        .to_owned(),
                ),
                _ => {}
            }
        }
        warnings
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
        }
    }
}

impl LlmConfig {
    /// Human-readable label for the model summaries will actually use, for
    /// diagnostics (`serve` startup, `doctor`, `config check`). A coding-agent
    /// CLI with an unpinned `model` inherits the local CLI's default, which this
    /// names explicitly rather than rendering as a bare null.
    #[must_use]
    pub fn effective_model_label(&self) -> String {
        match self.provider {
            LlmProviderKind::OpenRouter => self.model_id.clone(),
            LlmProviderKind::ClaudeCli => self
                .claude_cli
                .model
                .clone()
                .unwrap_or_else(|| "(local claude CLI default)".to_owned()),
            LlmProviderKind::CodexCli => self
                .codex_cli
                .model
                .clone()
                .unwrap_or_else(|| "(local codex CLI default)".to_owned()),
            LlmProviderKind::Recording => "(recording fixture)".to_owned(),
        }
    }
}

/// Semantic-search (embeddings) policy for `search_semantic` (`WS5b` / ADR-040).
/// **Opt-in, off by default** — mirrors [`LlmConfig`]; Weft is local-first, so
/// nothing here makes a hosted embedding service required. When `enabled` is
/// false the `search_semantic` tool degrades honestly to "not enabled".
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticSearchConfig {
    pub enabled: bool,
    pub provider: SemanticProviderKind,
    /// Explicit opt-in to the live API provider (in addition to `enabled`).
    pub allow_live_provider: bool,
    /// Embedding model id; embeddings are cache-keyed by this.
    pub model_id: String,
    /// Vector dimensionality (must match the model).
    pub dimensions: usize,
    /// `OpenAI`-compatible base URL (`/embeddings` is appended).
    pub endpoint_url: String,
    /// Env var holding the API key for the live provider.
    pub api_key_env: String,
    pub timeout_seconds: u64,
    /// Per-session embedding token ceiling for cost governance.
    pub session_token_ceiling: u64,
}

impl Default for SemanticSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: SemanticProviderKind::Api,
            allow_live_provider: false,
            model_id: "text-embedding-3-small".to_owned(),
            dimensions: 1536,
            endpoint_url: "https://api.openai.com/v1".to_owned(),
            api_key_env: "OPENAI_API_KEY".to_owned(),
            timeout_seconds: 60,
            session_token_ceiling: 5_000_000,
        }
    }
}

impl SemanticSearchConfig {
    /// Loopback-trust gate for the local OpenAI-compatible provider.
    ///
    /// Gated on `enabled` (matching [`HttpReadConfig::validate_loopback_trust`]):
    /// a disabled semantic block can never reach the endpoint, so its
    /// `endpoint_url` must not fail config load — otherwise
    /// `semantic_search.enabled: false` plus a stale non-loopback endpoint
    /// hard-fails `loomweave serve` AND traps recovery, because
    /// `update_semantic_config_file` re-parses before writing, so even
    /// `config semantic set --disable` was rejected (weft-ac59e8e730). The edit
    /// paths now parse via `from_yaml_str_section_scoped` (L2), which runs this
    /// gate only for the edited section and skips cross-section trust so an edit
    /// to one section is never trapped by a stale value in another.
    pub fn validate_endpoint_trust(&self) -> Result<(), ConfigError> {
        if !self.enabled || self.provider != SemanticProviderKind::LocalOpenAi {
            return Ok(());
        }
        let url = reqwest::Url::parse(&self.endpoint_url).map_err(|source| {
            ConfigError::InvalidSemanticEndpoint {
                code: "LMWV-CONFIG-SEMANTIC-ENDPOINT-URL",
                endpoint_url: self.endpoint_url.clone(),
                parse_error: source.to_string(),
            }
        })?;
        if matches!(url.scheme(), "http" | "https") && semantic_url_is_loopback(&url) {
            return Ok(());
        }
        Err(ConfigError::NonLoopbackSemanticEndpoint {
            code: "LMWV-CONFIG-SEMANTIC-NON-LOOPBACK",
            endpoint_url: self.endpoint_url.clone(),
        })
    }
}

fn semantic_url_is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.localdomain")
    {
        return true;
    }
    // `host_str()` keeps the URL-syntax brackets on an IPv6 host (`[::1]`),
    // which `IpAddr::from_str` rejects — strip them so IPv6 loopback is
    // recognised (weft-ac59e8e730).
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticProviderKind {
    #[serde(rename = "api", alias = "openai", alias = "openai_api")]
    Api,
    #[serde(rename = "local_openai", alias = "local", alias = "openai_local")]
    LocalOpenAi,
}

impl SemanticProviderKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::LocalOpenAi => "local_openai",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            "api" | "openai" | "openai_api" => Ok(Self::Api),
            "local_openai" | "local" | "openai_local" => Ok(Self::LocalOpenAi),
            other => Err(ConfigError::InvalidSemanticProvider {
                provider: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    #[serde(rename = "openrouter", alias = "open_router", alias = "openrouter_api")]
    OpenRouter,
    #[serde(rename = "codex_cli", alias = "codex", alias = "codex_sidecar")]
    CodexCli,
    #[serde(rename = "claude_cli", alias = "claude_code", alias = "claude_sidecar")]
    ClaudeCli,
    Recording,
}

impl LlmProviderKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::CodexCli => "codex_cli",
            Self::ClaudeCli => "claude_cli",
            Self::Recording => "recording",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            "openrouter" | "open_router" | "openrouter_api" => Ok(Self::OpenRouter),
            "codex_cli" | "codex" | "codex_sidecar" => Ok(Self::CodexCli),
            "claude_cli" | "claude_code" | "claude_sidecar" => Ok(Self::ClaudeCli),
            "recording" => Ok(Self::Recording),
            other => Err(ConfigError::InvalidLlmProvider {
                provider: other.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmConfigPatch {
    pub enabled: Option<bool>,
    pub provider: Option<LlmProviderKind>,
    pub allow_live_provider: Option<bool>,
    pub enable_write_tools: Option<bool>,
    pub model_id: Option<String>,
    pub codex_model: Option<String>,
    pub claude_model: Option<String>,
    pub openrouter_api_key_env: Option<String>,
    pub openrouter_endpoint_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmConfigEditResult {
    pub path: String,
    pub created: bool,
    pub config: McpConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticConfigPatch {
    pub enabled: Option<bool>,
    pub provider: Option<SemanticProviderKind>,
    pub allow_live_provider: Option<bool>,
    pub model_id: Option<String>,
    pub dimensions: Option<usize>,
    pub endpoint_url: Option<String>,
    pub api_key_env: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub session_token_ceiling: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticConfigEditResult {
    pub path: String,
    pub created: bool,
    pub config: McpConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenRouterConfig {
    pub endpoint_url: String,
    pub api_key_env: String,
    pub attribution: OpenRouterAttributionConfig,
    pub timeout_seconds: u64,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            api_key_env: "OPENROUTER_API_KEY".to_owned(),
            attribution: OpenRouterAttributionConfig::default(),
            timeout_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenRouterAttributionConfig {
    pub referer: String,
    pub title: String,
}

impl Default for OpenRouterAttributionConfig {
    fn default() -> Self {
        Self {
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct IntegrationsConfig {
    pub filigree: FiligreeConfig,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    pub mcp: McpServeConfig,
    pub http: HttpReadConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct McpServeConfig {
    /// Enable MCP tools that can mutate state, spawn processes, or call an LLM.
    /// Default false: `loomweave serve` exposes consult-mode read tools unless an
    /// operator explicitly opts into write-capable MCP operations.
    pub enable_write_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpReadConfig {
    pub enabled: bool,
    /// Bind address for the HTTP read API. `None` (the default) auto-selects a
    /// per-project deterministic port on `127.0.0.1` (ADR-044). `Some(addr)` is
    /// honored verbatim (operator override).
    #[serde(default, deserialize_with = "deserialize_optional_socket_addr")]
    pub bind: Option<SocketAddr>,
    pub allow_non_loopback: bool,
    /// Name of the env var holding the inbound bearer token. When the env
    /// var is set, every `/api/v1/files`-family request must carry
    /// `Authorization: Bearer <that-value>`; the capabilities probe is
    /// always unauthenticated. When the env var is unset on a loopback
    /// bind, the surface stays unauthenticated (the v0.1 trust model).
    /// When the env var is unset on a non-loopback bind, `loomweave serve`
    /// refuses to start (`LMWV-CONFIG-HTTP-NO-AUTH`). Default
    /// `WEFT_TOKEN` matches Filigree's pinned client default.
    pub token_env: String,
    /// Optional env var holding the Weft component identity HMAC secret.
    /// When configured, `loomweave serve` refuses to start unless the env var
    /// exists and protected HTTP read routes require
    /// `X-Weft-Component: loomweave:<hmac>`.
    pub identity_token_env: Option<String>,
    /// Enable the Wardline taint-store WRITE API (POST /api/wardline/taint-facts).
    /// Default false — `serve` is read-only unless explicitly opted in (ADR-036).
    /// When true, `serve` spawns an optional ADR-011 writer-actor.
    #[serde(default)]
    pub wardline_taint_write: bool,
}

impl Default for HttpReadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: None,
            allow_non_loopback: false,
            token_env: "WEFT_TOKEN".to_owned(),
            identity_token_env: None,
            wardline_taint_write: false,
        }
    }
}

impl HttpReadConfig {
    pub fn validate_loopback_trust(&self) -> Result<(), ConfigError> {
        if self.enabled && !self.allow_non_loopback && !self.is_loopback_bind() {
            // is_loopback_bind() is true for None, so reaching here implies Some(non-loopback).
            if let Some(bind) = self.bind {
                return Err(ConfigError::NonLoopbackHttpBind {
                    code: "LMWV-CONFIG-HTTP-NON-LOOPBACK",
                    bind,
                });
            }
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
                        code: "LMWV-CONFIG-HTTP-IDENTITY-MISSING",
                        token_env: env_var.to_owned(),
                    });
                }
                true
            }
            None => false,
        };
        // None (auto-select) always binds 127.0.0.1, so it is loopback.
        let Some(bind_addr) = self.bind else {
            return Ok(());
        };
        if bind_addr.ip().is_loopback() {
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
            code: "LMWV-CONFIG-HTTP-NO-AUTH",
            bind: bind_addr,
            token_env: self.token_env.clone(),
        })
    }

    /// `None` (auto-select) always binds `127.0.0.1`, so it is loopback.
    #[must_use]
    pub fn is_loopback_bind(&self) -> bool {
        self.bind.is_none_or(|addr| addr.ip().is_loopback())
    }
}

fn deserialize_optional_socket_addr<'de, D>(deserializer: D) -> Result<Option<SocketAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(raw) => raw.parse().map(Some).map_err(|err| {
            serde::de::Error::custom(format!("invalid serve.http.bind {raw:?}: {err}"))
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FiligreeConfig {
    pub enabled: bool,
    pub base_url: String,
    pub actor: String,
    /// Name of the environment variable holding the Filigree bearer token.
    /// Defaults to `WEFT_FEDERATION_TOKEN` (Weft-suite federation plumbing).
    /// The legacy `FILIGREE_API_TOKEN` name is still honoured as a deprecated
    /// fallback at token-resolution time — see `FiligreeHttpClient::from_config`.
    pub token_env: String,
    pub timeout_seconds: u64,
    /// Whether `loomweave analyze` POSTs its findings to Filigree's
    /// `POST /api/v1/scan-results` intake on completion (WP9-B,
    /// REQ-FINDING-03). Emission is a one-way Loomweave→Filigree data egress, so
    /// it is its own explicit opt-in: it requires both `enabled` *and* this
    /// flag, and **both default `false`**. Enabling the integration for the
    /// read side (`issues_for` reverse-lookup) therefore does not silently
    /// start outbound emission — the operator opts into the write direction
    /// separately by setting `emit_findings: true`.
    pub emit_findings: bool,
    /// Age threshold (days) for `loomweave analyze --prune-unseen` (REQ-FINDING-06):
    /// findings Filigree has marked `unseen_in_latest` and that are older than
    /// this are soft-archived (`fixed`) by the retention sweep. Default 30.
    /// Only consulted when `--prune-unseen` is passed; the sweep itself is
    /// opt-in per invocation, not on by default.
    pub prune_unseen_days: u32,
}

impl Default for FiligreeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://127.0.0.1:8766".to_owned(),
            actor: "loomweave-mcp".to_owned(),
            token_env: "WEFT_FEDERATION_TOKEN".to_owned(),
            timeout_seconds: 5,
            emit_findings: false,
            prune_unseen_days: 30,
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
        LlmProviderKind::OpenRouter => {
            let live_env_opt_in = env_lookup("LOOMWEAVE_LLM_LIVE").as_deref() == Some("1");
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
            let live_env_opt_in = env_lookup("LOOMWEAVE_LLM_LIVE").as_deref() == Some("1");
            if !config.llm.allow_live_provider && !live_env_opt_in {
                return Ok(ProviderSelection::Disabled);
            }
            Ok(ProviderSelection::CodexCli)
        }
        LlmProviderKind::ClaudeCli => {
            let live_env_opt_in = env_lookup("LOOMWEAVE_LLM_LIVE").as_deref() == Some("1");
            if !config.llm.allow_live_provider && !live_env_opt_in {
                return Ok(ProviderSelection::Disabled);
            }
            Ok(ProviderSelection::ClaudeCli)
        }
    }
}

/// Which config section an edit-path touched — selects the section-scoped
/// trust validation in [`McpConfig::from_yaml_str_section_scoped`] (L2).
#[derive(Debug, Clone, Copy)]
enum EditedSection {
    Llm,
    SemanticSearch,
}

#[allow(clippy::similar_names)] // path/patch are both the precise domain terms
pub fn update_llm_config_file(
    path: &Path,
    patch: &LlmConfigPatch,
) -> Result<LlmConfigEditResult, ConfigError> {
    let (mut document, created) = if path.exists() {
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if raw.trim().is_empty() {
            (versioned_empty_document(), false)
        } else {
            reject_llm_policy_alias_collision(&raw)?;
            (
                serde_norway::from_str::<serde_norway::Value>(&raw)
                    .map_err(|err| ConfigError::Yaml(err.to_string()))?,
                false,
            )
        }
    } else {
        (versioned_empty_document(), true)
    };

    apply_llm_patch(&mut document, patch)?;
    let rendered =
        serde_norway::to_string(&document).map_err(|err| ConfigError::Yaml(err.to_string()))?;
    // Editing the llm section must not be trapped by a stale value in an
    // unrelated section (L2): validate structure (+ this section's own trust,
    // of which llm has none), not cross-section trust.
    let parsed = McpConfig::from_yaml_str_section_scoped(&rendered, EditedSection::Llm)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    fs::write(path, rendered).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(LlmConfigEditResult {
        path: path.display().to_string(),
        created,
        config: parsed,
    })
}

#[allow(clippy::similar_names)] // path/patch are both the precise domain terms
pub fn update_semantic_config_file(
    path: &Path,
    patch: &SemanticConfigPatch,
) -> Result<SemanticConfigEditResult, ConfigError> {
    let (mut document, created) = if path.exists() {
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        if raw.trim().is_empty() {
            (versioned_empty_document(), false)
        } else {
            reject_llm_policy_alias_collision(&raw)?;
            (
                serde_norway::from_str::<serde_norway::Value>(&raw)
                    .map_err(|err| ConfigError::Yaml(err.to_string()))?,
                false,
            )
        }
    } else {
        (versioned_empty_document(), true)
    };

    apply_semantic_patch(&mut document, patch)?;
    let rendered =
        serde_norway::to_string(&document).map_err(|err| ConfigError::Yaml(err.to_string()))?;
    // Editing the semantic_search section must not be trapped by a stale value
    // in an unrelated section (L2) — and crucially `--disable` is itself the
    // recovery action. Validate structure + THIS section's own endpoint trust
    // (so re-enabling a non-loopback endpoint is still refused), but never
    // cross-section trust.
    let parsed =
        McpConfig::from_yaml_str_section_scoped(&rendered, EditedSection::SemanticSearch)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.display().to_string(),
            source,
        })?;
    }
    fs::write(path, rendered).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(SemanticConfigEditResult {
        path: path.display().to_string(),
        created,
        config: parsed,
    })
}

fn versioned_empty_document() -> serde_norway::Value {
    let mut mapping = serde_norway::Mapping::new();
    mapping.insert(
        serde_norway::Value::String("version".to_owned()),
        serde_norway::Value::Number(1.into()),
    );
    serde_norway::Value::Mapping(mapping)
}

fn apply_llm_patch(
    document: &mut serde_norway::Value,
    patch: &LlmConfigPatch,
) -> Result<(), ConfigError> {
    let root = mapping_mut(document)?;
    let llm_key = if root.contains_key("llm_policy") {
        "llm_policy"
    } else if root.contains_key("llm") {
        "llm"
    } else {
        "llm_policy"
    };
    let llm = child_mapping_mut(root, llm_key)?;
    if let Some(enabled) = patch.enabled {
        set_bool(llm, "enabled", enabled);
    }
    if let Some(provider) = patch.provider {
        set_string(llm, "provider", provider.as_str());
    }
    if let Some(allow_live_provider) = patch.allow_live_provider {
        set_bool(llm, "allow_live_provider", allow_live_provider);
    }
    if let Some(model_id) = patch.model_id.as_deref() {
        set_non_empty_string(llm, "model_id", model_id)?;
    }
    if let Some(model) = patch.codex_model.as_deref() {
        let codex = child_mapping_mut(llm, "codex_cli")?;
        set_non_empty_string(codex, "model", model)?;
    }
    if let Some(model) = patch.claude_model.as_deref() {
        let claude = child_mapping_mut(llm, "claude_cli")?;
        set_non_empty_string(claude, "model", model)?;
    }
    if let Some(api_key_env) = patch.openrouter_api_key_env.as_deref() {
        let openrouter = child_mapping_mut(llm, "openrouter")?;
        set_non_empty_string(openrouter, "api_key_env", api_key_env)?;
    }
    if let Some(endpoint_url) = patch.openrouter_endpoint_url.as_deref() {
        let openrouter = child_mapping_mut(llm, "openrouter")?;
        set_non_empty_string(openrouter, "endpoint_url", endpoint_url)?;
    }
    if let Some(enable_write_tools) = patch.enable_write_tools {
        let serve = child_mapping_mut(root, "serve")?;
        let mcp = child_mapping_mut(serve, "mcp")?;
        set_bool(mcp, "enable_write_tools", enable_write_tools);
    }
    Ok(())
}

fn apply_semantic_patch(
    document: &mut serde_norway::Value,
    patch: &SemanticConfigPatch,
) -> Result<(), ConfigError> {
    let root = mapping_mut(document)?;
    let semantic = child_mapping_mut(root, "semantic_search")?;
    if let Some(enabled) = patch.enabled {
        set_bool(semantic, "enabled", enabled);
    }
    if let Some(provider) = patch.provider {
        set_string(semantic, "provider", provider.as_str());
    }
    if let Some(allow_live_provider) = patch.allow_live_provider {
        set_bool(semantic, "allow_live_provider", allow_live_provider);
    }
    if let Some(model_id) = patch.model_id.as_deref() {
        set_non_empty_string(semantic, "model_id", model_id)?;
    }
    if let Some(dimensions) = patch.dimensions {
        if dimensions == 0 {
            return Err(ConfigError::Yaml(
                "dimensions must be greater than zero".to_owned(),
            ));
        }
        set_usize(semantic, "dimensions", dimensions)?;
    }
    if let Some(endpoint_url) = patch.endpoint_url.as_deref() {
        set_non_empty_string(semantic, "endpoint_url", endpoint_url)?;
    }
    if let Some(api_key_env) = patch.api_key_env.as_deref() {
        set_non_empty_string(semantic, "api_key_env", api_key_env)?;
    }
    if let Some(timeout_seconds) = patch.timeout_seconds {
        if timeout_seconds == 0 {
            return Err(ConfigError::Yaml(
                "timeout_seconds must be greater than zero".to_owned(),
            ));
        }
        set_u64(semantic, "timeout_seconds", timeout_seconds)?;
    }
    if let Some(session_token_ceiling) = patch.session_token_ceiling {
        set_u64(semantic, "session_token_ceiling", session_token_ceiling)?;
    }
    Ok(())
}

fn mapping_mut(value: &mut serde_norway::Value) -> Result<&mut serde_norway::Mapping, ConfigError> {
    value
        .as_mapping_mut()
        .ok_or_else(|| ConfigError::Yaml("loomweave.yaml root must be a YAML mapping".to_owned()))
}

fn child_mapping_mut<'a>(
    mapping: &'a mut serde_norway::Mapping,
    key: &str,
) -> Result<&'a mut serde_norway::Mapping, ConfigError> {
    let key_value = serde_norway::Value::String(key.to_owned());
    if !mapping.contains_key(&key_value) {
        mapping.insert(
            key_value.clone(),
            serde_norway::Value::Mapping(serde_norway::Mapping::new()),
        );
    }
    mapping
        .get_mut(&key_value)
        .and_then(serde_norway::Value::as_mapping_mut)
        .ok_or_else(|| ConfigError::Yaml(format!("{key} must be a YAML mapping")))
}

fn set_bool(mapping: &mut serde_norway::Mapping, key: &str, value: bool) {
    mapping.insert(
        serde_norway::Value::String(key.to_owned()),
        serde_norway::Value::Bool(value),
    );
}

fn set_string(mapping: &mut serde_norway::Mapping, key: &str, value: &str) {
    mapping.insert(
        serde_norway::Value::String(key.to_owned()),
        serde_norway::Value::String(value.to_owned()),
    );
}

fn set_u64(mapping: &mut serde_norway::Mapping, key: &str, value: u64) -> Result<(), ConfigError> {
    let number = serde_norway::to_value(value).map_err(|err| ConfigError::Yaml(err.to_string()))?;
    mapping.insert(serde_norway::Value::String(key.to_owned()), number);
    Ok(())
}

fn set_usize(
    mapping: &mut serde_norway::Mapping,
    key: &str,
    value: usize,
) -> Result<(), ConfigError> {
    let number = serde_norway::to_value(value).map_err(|err| ConfigError::Yaml(err.to_string()))?;
    mapping.insert(serde_norway::Value::String(key.to_owned()), number);
    Ok(())
}

fn set_non_empty_string(
    mapping: &mut serde_norway::Mapping,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Yaml(format!("{key} must not be blank")));
    }
    set_string(mapping, key, value);
    Ok(())
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

    #[error("{code}: integrations.filigree.actor must not be blank when Filigree is enabled")]
    InvalidFiligreeActor { code: &'static str },

    #[error(
        "{code}: serve.http.bind {bind} exposes the unauthenticated non-loopback Loomweave HTTP read API; \
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
         refusing to start an HTTP read API with incomplete Weft component identity configuration."
    )]
    MissingHttpIdentitySecret {
        code: &'static str,
        token_env: String,
    },

    #[error(
        "{code}: loomweave.yaml contains both `llm` and `llm_policy` top-level keys; \
         `llm_policy` is a serde alias for `llm` and serde silently discards one. \
         Pick one and remove the other."
    )]
    AmbiguousLlmKey { code: &'static str },

    #[error(
        "unknown LLM provider {provider:?}; expected one of: openrouter, openrouter_api, codex_cli, codex_sidecar, claude_cli, claude_sidecar"
    )]
    InvalidLlmProvider { provider: String },

    #[error(
        "unknown semantic search provider {provider:?}; expected one of: api, openai, openai_api, local_openai, local, openai_local"
    )]
    InvalidSemanticProvider { provider: String },

    #[error(
        "{code}: semantic_search.endpoint_url {endpoint_url:?} is not a valid URL: {parse_error}"
    )]
    InvalidSemanticEndpoint {
        code: &'static str,
        endpoint_url: String,
        parse_error: String,
    },

    #[error(
        "{code}: semantic_search.provider=local_openai requires semantic_search.endpoint_url to be http(s) on localhost or a loopback IP; got {endpoint_url:?}"
    )]
    NonLoopbackSemanticEndpoint {
        code: &'static str,
        endpoint_url: String,
    },
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
            code: "LMWV-CONFIG-AMBIGUOUS-LLM-KEY",
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
      referer: https://example.invalid/loomweave
      title: Loomweave Test
  max_inferred_edges_per_caller: 3
  cache_max_age_days: 7
integrations:
  filigree:
    enabled: true
    base_url: "http://127.0.0.1:9999"
    actor: "loomweave-test"
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
            "https://example.invalid/loomweave"
        );
        assert_eq!(cfg.llm.openrouter.attribution.title, "Loomweave Test");
        assert_eq!(cfg.llm.openrouter.timeout_seconds, 300); // default — not set in YAML
        assert_eq!(cfg.llm.max_inferred_edges_per_caller, 3);
        assert_eq!(cfg.llm.cache_max_age_days, 7);
        assert!(cfg.integrations.filigree.enabled);
        assert_eq!(cfg.integrations.filigree.base_url, "http://127.0.0.1:9999");
        assert_eq!(cfg.integrations.filigree.actor, "loomweave-test");
        assert_eq!(cfg.integrations.filigree.token_env, "TEST_FILIGREE_TOKEN");
        assert_eq!(cfg.integrations.filigree.timeout_seconds, 2);
    }

    #[test]
    fn filigree_emission_is_opt_in_independent_of_enabled() {
        // clarion-a26de2f368: outbound finding emission is a one-way egress and
        // must not piggyback on enabling Filigree for read enrichment. Both
        // knobs default false so flipping `enabled` for `issues_for` never
        // silently starts POSTing findings.
        let defaults = FiligreeConfig::default();
        assert!(!defaults.enabled);
        assert!(
            !defaults.emit_findings,
            "emit_findings must default false (explicit write opt-in)"
        );

        // Turning on the read side alone leaves emission off.
        let read_only = McpConfig::from_yaml_str(
            r"
integrations:
  filigree:
    enabled: true
",
        )
        .expect("parse config");
        assert!(read_only.integrations.filigree.enabled);
        assert!(
            !read_only.integrations.filigree.emit_findings,
            "enabling Filigree for reads must not turn on outbound emission"
        );
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
    fn accepts_operator_facing_llm_provider_mode_aliases() {
        let cases = [
            ("openrouter_api", LlmProviderKind::OpenRouter),
            ("codex_sidecar", LlmProviderKind::CodexCli),
            ("claude_sidecar", LlmProviderKind::ClaudeCli),
        ];

        for (provider, expected) in cases {
            let cfg = McpConfig::from_yaml_str(&format!(
                "llm_policy:\n  enabled: true\n  provider: {provider}\n"
            ))
            .unwrap_or_else(|err| panic!("provider alias {provider:?} should parse: {err}"));
            assert_eq!(cfg.llm.provider, expected);
        }
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
                assert_eq!(code, "LMWV-CONFIG-AMBIGUOUS-LLM-KEY");
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
            ..McpConfig::default()
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
            ..McpConfig::default()
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
    profile: loomweave
    sandbox: read-only
    timeout_seconds: 30
",
        )
        .expect("parse Codex CLI provider config");

        assert_eq!(cfg.llm.provider, LlmProviderKind::CodexCli);
        assert_eq!(cfg.llm.model_id, "codex-cli-default");
        assert_eq!(cfg.llm.codex_cli.executable, "/tmp/fake-codex");
        assert_eq!(cfg.llm.codex_cli.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(cfg.llm.codex_cli.profile.as_deref(), Some("loomweave"));
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
            ..McpConfig::default()
        };

        let selected = select_provider_with_env(&cfg, |_| None).expect("provider selection");
        assert_eq!(selected, ProviderSelection::Disabled);

        let env_selected = select_provider_with_env(&cfg, |name| {
            (name == "LOOMWEAVE_LLM_LIVE").then(|| "1".to_owned())
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
            ..McpConfig::default()
        };

        let selected = select_provider_with_env(&cfg, |_| None).expect("provider selection");
        assert_eq!(selected, ProviderSelection::Disabled);

        let env_selected = select_provider_with_env(&cfg, |name| {
            (name == "LOOMWEAVE_LLM_LIVE").then(|| "1".to_owned())
        })
        .expect("provider selection via env opt-in");
        assert_eq!(env_selected, ProviderSelection::ClaudeCli);
    }

    #[test]
    fn disabled_semantic_block_with_non_loopback_local_endpoint_still_loads() {
        // weft-ac59e8e730: the loopback-trust gate must be conditioned on
        // `enabled` — a disabled block can never reach the endpoint, and
        // failing here hard-failed `loomweave serve` / `config status` AND
        // trapped recovery (`config semantic set --disable` re-parses the
        // file before writing).
        let cfg = McpConfig::from_yaml_str(
            r"
semantic_search:
  enabled: false
  provider: local_openai
  endpoint_url: http://192.168.1.50:11434/v1
",
        )
        .expect("disabled semantic block must load regardless of endpoint trust");
        assert!(!cfg.semantic_search.enabled);
        assert_eq!(
            cfg.semantic_search.provider,
            SemanticProviderKind::LocalOpenAi
        );
    }

    #[test]
    fn enabled_local_semantic_provider_still_rejects_non_loopback_endpoint() {
        let err = McpConfig::from_yaml_str(
            r"
semantic_search:
  enabled: true
  provider: local_openai
  endpoint_url: http://192.168.1.50:11434/v1
",
        )
        .expect_err("enabled local provider with a non-loopback endpoint must fail");
        assert!(
            err.to_string()
                .contains("LMWV-CONFIG-SEMANTIC-NON-LOOPBACK"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn semantic_set_disable_succeeds_on_non_loopback_local_endpoint_config() {
        // weft-ac59e8e730 recovery path: `config semantic set --disable` on a
        // file whose enabled local provider points at a non-loopback endpoint
        // must be able to write the disabled state.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loomweave.yaml");
        fs::write(
            &path,
            "semantic_search:\n  enabled: false\n  provider: local_openai\n  endpoint_url: http://192.168.1.50:11434/v1\n",
        )
        .expect("seed config");

        let result = update_semantic_config_file(
            &path,
            &SemanticConfigPatch {
                enabled: Some(false),
                ..SemanticConfigPatch::default()
            },
        )
        .expect("disabling semantic search must succeed despite the stale endpoint");
        assert!(!result.config.semantic_search.enabled);

        // And re-enabling against the same stale endpoint is still refused.
        let err = update_semantic_config_file(
            &path,
            &SemanticConfigPatch {
                enabled: Some(true),
                ..SemanticConfigPatch::default()
            },
        )
        .expect_err("re-enabling with a non-loopback local endpoint must fail");
        assert!(
            err.to_string()
                .contains("LMWV-CONFIG-SEMANTIC-NON-LOOPBACK"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn llm_disable_succeeds_despite_stale_non_loopback_semantic_search() {
        // L2 cross-section recovery-trap: editing the llm section was rejected
        // by whole-config `validate()` because an UNRELATED, stale
        // semantic_search block was enabled with a non-loopback endpoint. An
        // edit to one section must never be gated by another.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loomweave.yaml");
        fs::write(
            &path,
            "version: 1\n\
             llm_policy:\n  enabled: true\n\
             semantic_search:\n  enabled: true\n  provider: local_openai\n  endpoint_url: http://192.168.1.50:11434/v1\n",
        )
        .expect("seed config");

        let result = update_llm_config_file(
            &path,
            &LlmConfigPatch {
                enabled: Some(false),
                ..LlmConfigPatch::default()
            },
        )
        .expect("disabling llm must succeed despite the stale non-loopback semantic endpoint");
        assert!(!result.config.llm.enabled);
    }

    #[test]
    fn semantic_disable_succeeds_despite_stale_non_loopback_serve_http() {
        // L2 cross-section recovery-trap (the worst case): `config semantic set
        // --disable` is the very recovery action, yet it was rejected by a
        // stale enabled non-loopback `serve.http` in a DIFFERENT section.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("loomweave.yaml");
        fs::write(
            &path,
            "version: 1\n\
             semantic_search:\n  enabled: true\n  provider: local_openai\n  endpoint_url: http://127.0.0.1:11434/v1\n\
             serve:\n  http:\n    enabled: true\n    bind: 192.168.1.50:8080\n",
        )
        .expect("seed config");

        let result = update_semantic_config_file(
            &path,
            &SemanticConfigPatch {
                enabled: Some(false),
                ..SemanticConfigPatch::default()
            },
        )
        .expect("disabling semantic must succeed despite the stale non-loopback serve.http");
        assert!(!result.config.semantic_search.enabled);
    }

    #[test]
    fn ipv6_loopback_semantic_endpoint_is_trusted() {
        // weft-ac59e8e730 (minor): url::host_str() returns the bracketed form
        // for IPv6 hosts, which IpAddr parsing rejected, so `http://[::1]:...`
        // was wrongly refused as non-loopback.
        let cfg = SemanticSearchConfig {
            enabled: true,
            provider: SemanticProviderKind::LocalOpenAi,
            endpoint_url: "http://[::1]:11434/v1".to_owned(),
            ..SemanticSearchConfig::default()
        };
        cfg.validate_endpoint_trust()
            .expect("IPv6 loopback endpoint must satisfy the loopback-trust gate");

        let non_loopback = SemanticSearchConfig {
            endpoint_url: "http://[2001:db8::1]:11434/v1".to_owned(),
            ..cfg
        };
        non_loopback
            .validate_endpoint_trust()
            .expect_err("non-loopback IPv6 endpoint must still be refused");
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

        assert_eq!(
            cfg.serve.http.bind,
            Some(SocketAddr::from(([127, 0, 0, 1], 0)))
        );
    }

    #[test]
    fn http_allow_non_loopback_defaults_false() {
        assert!(!McpConfig::default().serve.http.allow_non_loopback);
    }

    #[test]
    fn mcp_write_tools_default_false_and_can_be_enabled() {
        assert!(!McpConfig::default().serve.mcp.enable_write_tools);

        let cfg = McpConfig::from_yaml_str(
            r"
serve:
  mcp:
    enable_write_tools: true
",
        )
        .expect("parse MCP write-tool policy");

        assert!(cfg.serve.mcp.enable_write_tools);
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
    identity_token_env: LOOMWEAVE_TEST_IDENTITY
"#,
        )
        .expect("parse HTTP identity_token_env");

        assert_eq!(
            cfg.serve.http.identity_token_env.as_deref(),
            Some("LOOMWEAVE_TEST_IDENTITY")
        );
    }

    #[test]
    fn http_wardline_taint_write_defaults_false() {
        assert!(!McpConfig::default().serve.http.wardline_taint_write);
    }

    #[test]
    fn http_wardline_taint_write_is_parsed_when_config_loads() {
        let cfg = McpConfig::from_yaml_str(
            r#"
serve:
  http:
    enabled: true
    bind: "127.0.0.1:0"
    wardline_taint_write: true
"#,
        )
        .expect("parse HTTP wardline_taint_write");

        assert!(cfg.serve.http.wardline_taint_write);
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
    fn http_bind_defaults_to_none_auto_select() {
        // ADR-044: the installer no longer pins a port; an unset bind means
        // "auto-select a per-project deterministic port and publish it".
        assert_eq!(HttpReadConfig::default().bind, None);
    }

    #[test]
    fn http_bind_none_is_treated_as_loopback() {
        // Auto-select always binds 127.0.0.1, so an absent bind is loopback and
        // must satisfy the loopback-trust gate without allow_non_loopback.
        let cfg = HttpReadConfig {
            enabled: true,
            bind: None,
            ..HttpReadConfig::default()
        };
        assert!(cfg.is_loopback_bind());
        assert!(cfg.validate_loopback_trust().is_ok());
    }

    #[test]
    fn http_explicit_bind_still_parses() {
        let cfg = McpConfig::from_yaml_str(
            "serve:\n  http:\n    enabled: true\n    bind: \"127.0.0.1:9412\"\n",
        )
        .expect("parse explicit bind");
        assert_eq!(
            cfg.serve.http.bind,
            Some(SocketAddr::from(([127, 0, 0, 1], 9412)))
        );
    }

    #[test]
    fn http_bind_none_passes_auth_trust_validation() {
        let cfg = HttpReadConfig {
            enabled: true,
            bind: None,
            ..HttpReadConfig::default()
        };
        assert!(cfg.validate_auth_trust(|_| None).is_ok());
    }

    #[test]
    fn http_bind_explicit_null_is_treated_as_auto_select() {
        let cfg = McpConfig::from_yaml_str("serve:\n  http:\n    enabled: true\n    bind: ~\n")
            .expect("explicit YAML null should parse as auto-select");
        assert_eq!(cfg.serve.http.bind, None);
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

        assert!(err.to_string().contains("LMWV-CONFIG-FILIGREE-ACTOR-BLANK"));
    }

    #[test]
    fn version_marker_is_accepted() {
        let cfg = McpConfig::from_yaml_str("version: 1\n").expect("version marker should parse");
        assert_eq!(cfg.version, 1);
        // Omitting it falls back to the default schema version.
        assert_eq!(McpConfig::default().version, 1);
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let err = McpConfig::from_yaml_str("not_a_real_section: true\n")
            .expect_err("unknown top-level key should be rejected");
        let msg = err.to_string();
        assert!(matches!(err, ConfigError::Yaml(_)), "got: {msg}");
        assert!(msg.contains("not_a_real_section"), "got: {msg}");
    }

    #[test]
    fn tolerates_analysis_section_without_disabling_filigree_emission() {
        // clarion-1d405be546: the same loomweave.yaml is parsed by AnalyzeConfig
        // (which owns the top-level `analysis:` clustering block) and by McpConfig
        // (which owns `integrations.filigree`, consulted at emission time). Under
        // deny_unknown_fields, McpConfig must still PARSE a config that carries a
        // sibling `analysis:` section — otherwise load_mcp_config's
        // default-on-error fallback silently sets filigree.enabled = false and
        // emission is skipped with no surfaced error.
        let cfg = McpConfig::from_yaml_str(
            r"
analysis:
  clustering:
    min_cluster_size: 2
integrations:
  filigree:
    enabled: true
    emit_findings: true
    actor: loomweave-test
",
        )
        .expect("config carrying both analysis: and integrations.filigree: must load");
        assert!(
            cfg.integrations.filigree.enabled,
            "a sibling analysis: section must not disable Filigree"
        );
        assert!(
            cfg.integrations.filigree.emit_findings,
            "a sibling analysis: section must not disable finding emission"
        );
    }

    #[test]
    fn unknown_nested_key_under_claude_cli_is_rejected() {
        // The exact agent-first-feedback §2.1 bug: `model_id` placed inside
        // claude_cli (whose field is `model`) was silently dropped. With
        // deny_unknown_fields it must now fail loudly, naming the key.
        let err = McpConfig::from_yaml_str(
            r"
llm_policy:
  enabled: true
  provider: claude_cli
  allow_live_provider: true
  claude_cli:
    model_id: claude-sonnet-4-6
",
        )
        .expect_err("misplaced key under claude_cli should be rejected");
        let msg = err.to_string();
        assert!(matches!(err, ConfigError::Yaml(_)), "got: {msg}");
        assert!(msg.contains("model_id"), "got: {msg}");
    }

    #[test]
    fn fully_specified_live_provider_behind_disabled_emits_warning() {
        // enabled omitted (defaults false) but allow_live_provider set: a config
        // that looks live but is inert. Must load (disabled is a legitimate
        // default) AND warn.
        let cfg = McpConfig::from_yaml_str(
            r"
llm_policy:
  provider: claude_cli
  allow_live_provider: true
",
        )
        .expect("configured-but-disabled provider should still load");
        assert!(!cfg.llm.enabled);
        let warnings = cfg.llm_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("enabled=false")),
            "expected an enabled=false warning, got: {warnings:?}"
        );
    }

    #[test]
    fn enabled_without_allow_live_provider_emits_warning() {
        let cfg = McpConfig::from_yaml_str(
            r"
llm_policy:
  enabled: true
  provider: claude_cli
",
        )
        .expect("enabled-without-opt-in should load");
        let warnings = cfg.llm_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("allow_live_provider=false")),
            "expected an allow_live_provider=false warning, got: {warnings:?}"
        );
    }

    #[test]
    fn unpinned_claude_cli_model_on_live_path_warns_about_cost() {
        let cfg = McpConfig::from_yaml_str(
            r"
llm_policy:
  enabled: true
  provider: claude_cli
  allow_live_provider: true
",
        )
        .expect("live claude_cli without a pinned model should load");
        let warnings = cfg.llm_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("claude_cli.model")),
            "expected an unpinned-model cost warning, got: {warnings:?}"
        );
    }

    #[test]
    fn healthy_live_config_emits_no_warnings() {
        let cfg = McpConfig::from_yaml_str(
            r"
llm_policy:
  enabled: true
  provider: claude_cli
  allow_live_provider: true
  claude_cli:
    model: claude-sonnet-4-6
",
        )
        .expect("healthy live config should load");
        assert!(
            cfg.llm_warnings().is_empty(),
            "expected no warnings, got: {:?}",
            cfg.llm_warnings()
        );
    }
}
