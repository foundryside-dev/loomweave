//! LLM provider surface for WP6 and MCP on-demand tools.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const LEAF_SUMMARY_PROMPT_TEMPLATE_ID: &str = "leaf-v1";
pub const INFERRED_CALLS_PROMPT_VERSION: &str = "inferred-calls-v1";
const AGENT_PROVIDER_PROMPT_VERSION: &str = "loomweave-agent-provider-v1";
const CLAUDE_CLI_PRINT_PROMPT: &str = "You are Loomweave's local Claude Code LLM provider. Read the Loomweave provider prompt from stdin, complete that exact task, and return only the validated JSON object.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmPurpose {
    Summary,
    InferredEdges,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub purpose: LlmPurpose,
    pub model_id: String,
    pub prompt_id: String,
    pub prompt: String,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub model_id: String,
    pub output_json: String,
    pub input_tokens: u32,
    #[serde(default)]
    pub cached_input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachingModel {
    OpenAiChatCompletions,
}

#[derive(Debug, Error, PartialEq)]
pub enum LlmProviderError {
    #[error("recording fixture has no response for prompt {prompt_id:?} on model {model_id:?}")]
    MissingRecording { prompt_id: String, model_id: String },

    #[error("live LLM provider requires explicit opt-in")]
    LiveProviderNotAllowed,

    #[error("live OpenRouter provider requires an API key")]
    MissingApiKey,

    #[error("live OpenRouter HTTP request failed: {message}")]
    Http { message: String, retryable: bool },

    #[error("live OpenRouter returned status {status}: {message}")]
    Provider {
        status: u16,
        code: Option<Value>,
        message: String,
        metadata: Option<Value>,
        retryable: bool,
        retry_after_seconds: Option<u64>,
    },

    #[error("LLM CLI invocation failed: {message}")]
    Cli { message: String, retryable: bool },

    #[error("LLM CLI invocation timed out after {timeout_seconds} seconds")]
    Timeout { timeout_seconds: u64 },

    #[error("invalid live LLM provider response: {message}")]
    InvalidResponse { message: String, retryable: bool },

    #[error("invalid LLM provider configuration: {message}")]
    InvalidConfig { message: String },
}

impl LlmProviderError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::MissingRecording { .. }
            | Self::LiveProviderNotAllowed
            | Self::MissingApiKey
            | Self::InvalidConfig { .. } => false,
            Self::Http { retryable, .. }
            | Self::Provider { retryable, .. }
            | Self::Cli { retryable, .. }
            | Self::InvalidResponse { retryable, .. } => *retryable,
            Self::Timeout { .. } => true,
        }
    }
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError>;
    fn estimate_tokens(&self, request: &LlmRequest) -> u64;
    fn tier_to_model(&self, tier: &str) -> Option<&str>;
    fn caching_model(&self) -> CachingModel;
}

pub fn build_coding_agent_provider_prompt(request: &LlmRequest) -> String {
    format!(
        "Prompt contract: {prompt_version}\n\
         You are Loomweave's coding-agent LLM provider for repository graph enrichment.\n\
         Loomweave has already selected the source excerpt, entity metadata, unresolved call sites, and candidate graph context needed for this task.\n\
         Follow these rules exactly:\n\
         1. Use only the evidence inside <loomweave_request>. Do not inspect additional files, browse, run commands, edit files, or ask follow-up questions.\n\
         2. Return exactly one JSON object matching the structured-output schema supplied by the caller. Do not wrap it in Markdown or prose.\n\
         3. Reason privately if needed, but do not expose hidden reasoning. Put only concise evidence summaries in output fields that ask for rationale or relationships.\n\
         4. When evidence is absent, prefer empty strings for optional prose fields and empty arrays for collection fields instead of guessing.\n\
         5. Keep stable field names and JSON types; downstream Loomweave storage parses the response mechanically.\n\
         Task type: {task_type}\n\
         Prompt template: {prompt_id}\n\
         Task guidance:\n\
         {task_guidance}\n\
         <loomweave_request>\n\
         {prompt}\n\
         </loomweave_request>\n",
        prompt_version = AGENT_PROVIDER_PROMPT_VERSION,
        task_type = agent_task_type(&request.purpose),
        prompt_id = request.prompt_id,
        task_guidance = agent_task_guidance(&request.purpose),
        prompt = request.prompt
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub request: LlmRequest,
    pub response: LlmResponse,
}

#[derive(Debug)]
pub struct RecordingProvider {
    recordings: Vec<Recording>,
    invocations: Mutex<Vec<LlmRequest>>,
}

impl RecordingProvider {
    pub fn from_recordings(recordings: Vec<Recording>) -> Self {
        Self {
            recordings,
            invocations: Mutex::new(Vec::new()),
        }
    }

    pub fn invocations(&self) -> Vec<LlmRequest> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        self.recordings
            .iter()
            .find(|recording| recording.request == request)
            .map(|recording| recording.response.clone())
            .ok_or(LlmProviderError::MissingRecording {
                prompt_id: request.prompt_id,
                model_id: request.model_id,
            })
    }

    fn estimate_tokens(&self, _request: &LlmRequest) -> u64 {
        0
    }

    fn tier_to_model(&self, _tier: &str) -> Option<&str> {
        None
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::OpenAiChatCompletions
    }
}

#[derive(Clone)]
pub struct TrafficLoggingProvider {
    inner: Arc<dyn LlmProvider>,
    log_path: PathBuf,
    max_bytes: u64,
    /// In-process serialisation of rotate+append so this process's own
    /// concurrent `tools/call` dispatch cannot interleave partial JSON lines or
    /// race the rotation rename. Shared across clones (one lock per log file)
    /// via `Arc` (weft-ac59e8e730). This guards only THIS process's threads;
    /// cross-process exclusion (two `serve` processes sharing one log path) is
    /// provided by the advisory `flock` taken in `append_event` (L6), since an
    /// O_APPEND write larger than PIPE_BUF is not atomic across processes.
    write_lock: Arc<Mutex<()>>,
}

impl TrafficLoggingProvider {
    pub fn new(inner: Arc<dyn LlmProvider>, log_path: PathBuf) -> Self {
        Self::with_max_bytes(inner, log_path, DEFAULT_LLM_TRAFFIC_LOG_MAX_BYTES)
    }

    pub fn with_max_bytes(inner: Arc<dyn LlmProvider>, log_path: PathBuf, max_bytes: u64) -> Self {
        Self {
            inner,
            log_path,
            max_bytes: max_bytes.max(1),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    fn append_event(&self, event: &Value) -> Result<(), LlmProviderError> {
        // Serialise the whole event up front so the file sees exactly one
        // write_all of one complete line.
        //
        // O_APPEND alone does NOT give cross-process line atomicity here: the
        // POSIX atomicity guarantee for a concurrent append holds only up to
        // PIPE_BUF (4096 on Linux), and an error event carries a message capped
        // at 4096 bytes (`truncate_for_error`) PLUS the JSON envelope and
        // escaping, so a single line readily exceeds PIPE_BUF. A `write_all`
        // larger than that issues multiple `write()` syscalls, and a second
        // `serve` process sharing this log path could interleave its own writes
        // between them, corrupting the JSON line. The in-process `write_lock`
        // (a per-process `Arc<Mutex>`) cannot prevent that — it is invisible to
        // other processes. We take an advisory `flock(LOCK_EX)` on the log file
        // for the rotation + append so the exclusion is genuinely cross-process.
        // (weft-ac59e8e730 / L6.)
        let mut line =
            serde_json::to_string(event).map_err(|err| LlmProviderError::InvalidResponse {
                message: format!("serialize LLM traffic log event: {err}"),
                retryable: false,
            })?;
        line.push('\n');
        // In-process guard first: cheap, and it serialises rotation among this
        // process's own threads before the cross-process flock below.
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(parent) = self.log_path.parent() {
            fs::create_dir_all(parent).map_err(|err| LlmProviderError::Cli {
                message: format!(
                    "create LLM traffic log directory {}: {err}",
                    parent.display()
                ),
                retryable: false,
            })?;
        }
        // Acquire the cross-process lock BEFORE the rotation check so a peer
        // process cannot rotate (rename) out from under our size check, and
        // hold it across the append. The lock is taken on a dedicated lock
        // file (not the log itself) so it survives the rotation rename and is
        // never invalidated when the log is renamed away mid-flight.
        let lock_path = llm_traffic_lock_path(&self.log_path);
        let lock_file =
            OpenOptions::new()
                .create(true)
                .write(true)
                .open(&lock_path)
                .map_err(|err| LlmProviderError::Cli {
                    message: format!(
                        "open LLM traffic log lock {}: {err}",
                        lock_path.display()
                    ),
                    retryable: false,
                })?;
        FileExt::lock_exclusive(&lock_file).map_err(|err| LlmProviderError::Cli {
            message: format!("lock LLM traffic log {}: {err}", self.log_path.display()),
            retryable: false,
        })?;
        // `lock_file` (and thus the flock) is released when it drops at the end
        // of this function — after the append below completes.
        let append_result = (|| {
            self.rotate_if_needed()?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_path)
                .map_err(|err| LlmProviderError::Cli {
                    message: format!(
                        "open LLM traffic log {}: {err}",
                        self.log_path.display()
                    ),
                    retryable: false,
                })?;
            file.write_all(line.as_bytes())
                .map_err(|err| LlmProviderError::Cli {
                    message: format!(
                        "write LLM traffic log {}: {err}",
                        self.log_path.display()
                    ),
                    retryable: false,
                })
        })();
        // Best-effort explicit unlock; the drop would do it anyway.
        let _ = FileExt::unlock(&lock_file);
        append_result
    }

    fn rotate_if_needed(&self) -> Result<(), LlmProviderError> {
        let Ok(metadata) = fs::metadata(&self.log_path) else {
            return Ok(());
        };
        if metadata.len() < self.max_bytes {
            return Ok(());
        }
        let backup_path = llm_traffic_backup_path(&self.log_path);
        match fs::remove_file(&backup_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(LlmProviderError::Cli {
                    message: format!(
                        "remove old LLM traffic log backup {}: {err}",
                        backup_path.display()
                    ),
                    retryable: false,
                });
            }
        }
        match fs::rename(&self.log_path, &backup_path) {
            Ok(()) => Ok(()),
            // Another writer (e.g. a second serve process sharing the file)
            // rotated between our metadata check and the rename — the log is
            // already fresh, so this is success, not an error
            // (weft-ac59e8e730 TOCTOU).
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LlmProviderError::Cli {
                message: format!(
                    "rotate LLM traffic log {} to {}: {err}",
                    self.log_path.display(),
                    backup_path.display()
                ),
                retryable: false,
            }),
        }
    }
}

pub const DEFAULT_LLM_TRAFFIC_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

fn llm_traffic_backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.1", path.display()))
}

/// Sidecar lock file for the cross-process append flock (L6). A dedicated file
/// (not the log itself) so the lock survives the rotation rename — flock follows
/// the open description, and renaming the log away mid-append would otherwise
/// leave a peer locking a path no longer pointing at the live log.
fn llm_traffic_lock_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", path.display()))
}

#[async_trait]
impl LlmProvider for TrafficLoggingProvider {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        let provider = self.inner.name();
        let result = self.inner.invoke(request.clone()).await;
        let event = match &result {
            Ok(response) => llm_traffic_success_event(provider, &request, response),
            Err(err) => llm_traffic_error_event(provider, &request, err),
        };
        // Fire-and-forget: the diagnostics sidecar must never gate the call it
        // observes — propagating an append failure converted a successful
        // (already paid-for) LLM call into an error (weft-ac59e8e730).
        if let Err(log_err) = self.append_event(&event) {
            tracing::warn!(
                error = %log_err,
                path = %self.log_path.display(),
                "failed to append LLM traffic diagnostics event; returning the provider result unchanged"
            );
        }
        result
    }

    fn estimate_tokens(&self, request: &LlmRequest) -> u64 {
        self.inner.estimate_tokens(request)
    }

    fn tier_to_model(&self, tier: &str) -> Option<&str> {
        self.inner.tier_to_model(tier)
    }

    fn caching_model(&self) -> CachingModel {
        self.inner.caching_model()
    }
}

fn llm_traffic_base_event(provider: &str, request: &LlmRequest, outcome: &str) -> Value {
    serde_json::json!({
        "schema": "loomweave.llm.lookup.v1",
        "ts_unix_ms": unix_timestamp_millis(),
        "provider": provider,
        "purpose": request.purpose,
        "prompt_id": request.prompt_id,
        "request_model_id": request.model_id,
        "max_output_tokens": request.max_output_tokens,
        "outcome": outcome,
    })
}

fn llm_traffic_success_event(
    provider: &str,
    request: &LlmRequest,
    response: &LlmResponse,
) -> Value {
    let mut event = llm_traffic_base_event(provider, request, "success");
    event["response_model_id"] = Value::String(response.model_id.clone());
    event["usage"] = serde_json::json!({
        "input_tokens": response.input_tokens,
        "cached_input_tokens": response.cached_input_tokens,
        "output_tokens": response.output_tokens,
        "total_tokens": response.total_tokens,
        "cost_usd": response.cost_usd,
    });
    event
}

fn llm_traffic_error_event(provider: &str, request: &LlmRequest, err: &LlmProviderError) -> Value {
    let mut event = llm_traffic_base_event(provider, request, "error");
    event["error"] = serde_json::json!({
        "kind": llm_provider_error_kind(err),
        "retryable": err.retryable(),
        "message": truncate_for_error(&err.to_string()),
    });
    event
}

fn llm_provider_error_kind(err: &LlmProviderError) -> &'static str {
    match err {
        LlmProviderError::MissingRecording { .. } => "missing_recording",
        LlmProviderError::LiveProviderNotAllowed => "live_provider_not_allowed",
        LlmProviderError::MissingApiKey => "missing_api_key",
        LlmProviderError::Http { .. } => "http",
        LlmProviderError::Provider { .. } => "provider",
        LlmProviderError::Cli { .. } => "cli",
        LlmProviderError::Timeout { .. } => "timeout",
        LlmProviderError::InvalidResponse { .. } => "invalid_response",
        LlmProviderError::InvalidConfig { .. } => "invalid_config",
    }
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterProviderConfig {
    pub api_key: Option<String>,
    pub allow_live_provider: bool,
    pub model_id: String,
    pub endpoint_url: String,
    pub referer: String,
    pub title: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    model_id: String,
    api_key: String,
    endpoint_url: String,
    referer: String,
    title: String,
    timeout_seconds: u64,
}

impl OpenRouterProvider {
    pub fn from_config(config: OpenRouterProviderConfig) -> Result<Self, LlmProviderError> {
        if !config.allow_live_provider {
            return Err(LlmProviderError::LiveProviderNotAllowed);
        }
        let Some(api_key) = config.api_key.filter(|key| !key.trim().is_empty()) else {
            return Err(LlmProviderError::MissingApiKey);
        };
        if config.timeout_seconds == 0 {
            return Err(LlmProviderError::InvalidConfig {
                message: "OpenRouter timeout_seconds must be greater than zero".to_owned(),
            });
        }
        Ok(Self {
            model_id: config.model_id,
            api_key,
            endpoint_url: config.endpoint_url,
            referer: config.referer,
            title: config.title,
            timeout_seconds: config.timeout_seconds,
        })
    }

    fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.endpoint_url.trim_end_matches('/')
        )
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        let payload = serde_json::json!({
            "model": request.model_id,
            "max_tokens": request.max_output_tokens,
            "temperature": 0,
            "provider": {
                "require_parameters": true
            },
            "response_format": response_format_for_purpose(&request.purpose),
            "messages": [
                {
                    "role": "user",
                    "content": request.prompt
                }
            ]
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_seconds))
            .build()
            .map_err(|err| LlmProviderError::Http {
                message: err.to_string(),
                retryable: false,
            })?;
        let response = client
            .post(self.chat_completions_url())
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("HTTP-Referer", self.referer.as_str())
            .header("X-OpenRouter-Title", self.title.as_str())
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|err| LlmProviderError::Http {
                message: err.to_string(),
                retryable: true,
            })?;
        let status = response.status();
        let retry_after_seconds = retry_after_seconds(response.headers());
        let body = response
            .text()
            .await
            .map_err(|err| LlmProviderError::Http {
                message: err.to_string(),
                retryable: true,
            })?;
        if !status.is_success() {
            return Err(provider_error_from_body(
                status.as_u16(),
                retry_after_seconds,
                &body,
            ));
        }
        if let Ok(envelope) = serde_json::from_str::<OpenRouterErrorEnvelope>(&body) {
            return Err(provider_error_from_openrouter(
                envelope.error.status_code(status.as_u16()),
                Some(envelope.error.code.clone()),
                envelope.error.message,
                envelope.error.metadata,
                retry_after_seconds,
            ));
        }
        let completion: OpenRouterChatResponse =
            serde_json::from_str(&body).map_err(|err| LlmProviderError::InvalidResponse {
                message: err.to_string(),
                retryable: true,
            })?;
        let output_json = completion.output_text()?;
        let usage = completion
            .usage
            .ok_or_else(|| LlmProviderError::InvalidResponse {
                message: "response missing usage".to_owned(),
                retryable: true,
            })?;
        Ok(LlmResponse {
            model_id: completion.model,
            output_json,
            input_tokens: usage.prompt,
            cached_input_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .map_or(0, |details| details.cached_tokens),
            output_tokens: usage.completion,
            total_tokens: usage.total,
            cost_usd: usage.cost.unwrap_or(0.0),
        })
    }

    fn estimate_tokens(&self, request: &LlmRequest) -> u64 {
        u64::from(estimate_text_tokens(&request.prompt)) + u64::from(request.max_output_tokens)
    }

    fn tier_to_model(&self, tier: &str) -> Option<&str> {
        match tier {
            "summary" | "inferred_edges" => Some(self.model_id.as_str()),
            _ => None,
        }
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::OpenAiChatCompletions
    }
}

/// Resolve `executable` via `which::which` and return a typed CLI error if
/// it is missing on PATH or at the configured absolute path. Called from each
/// CLI provider's `from_config` so a typo in `loomweave.yaml` aborts at
/// `loomweave serve` startup rather than exploding on the first MCP request.
fn validate_cli_executable(label: &str, executable: &str) -> Result<(), LlmProviderError> {
    which::which(executable).map_err(|err| LlmProviderError::Cli {
        message: format!("{label} executable {executable:?} not resolvable: {err}"),
        retryable: false,
    })?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCliProviderConfig {
    pub executable: String,
    pub project_root: PathBuf,
    pub model_id: String,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub sandbox: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct CodexCliProvider {
    executable: String,
    project_root: PathBuf,
    model_id: String,
    model: Option<String>,
    profile: Option<String>,
    sandbox: String,
    timeout: Duration,
    timeout_seconds: u64,
}

impl CodexCliProvider {
    pub fn from_config(config: CodexCliProviderConfig) -> Result<Self, LlmProviderError> {
        if config.executable.trim().is_empty() {
            return Err(LlmProviderError::Cli {
                message: "Codex CLI executable must not be blank".to_owned(),
                retryable: false,
            });
        }
        if config.model_id.trim().is_empty() {
            return Err(LlmProviderError::Cli {
                message: "Codex CLI model_id must not be blank".to_owned(),
                retryable: false,
            });
        }
        if config.sandbox.trim().is_empty() {
            return Err(LlmProviderError::Cli {
                message: "Codex CLI sandbox must not be blank".to_owned(),
                retryable: false,
            });
        }
        if config.timeout_seconds == 0 {
            return Err(LlmProviderError::Cli {
                message: "Codex CLI timeout_seconds must be greater than zero".to_owned(),
                retryable: false,
            });
        }
        validate_cli_executable("Codex CLI", &config.executable)?;

        Ok(Self {
            executable: config.executable,
            project_root: config.project_root,
            model_id: config.model_id,
            model: config.model.filter(|model| !model.trim().is_empty()),
            profile: config.profile.filter(|profile| !profile.trim().is_empty()),
            sandbox: config.sandbox,
            timeout: Duration::from_secs(config.timeout_seconds),
            timeout_seconds: config.timeout_seconds,
        })
    }

    fn invoke_with_temp_files(
        &self,
        request: LlmRequest,
        output_path: &Path,
        schema_path: &Path,
    ) -> Result<LlmResponse, LlmProviderError> {
        let schema = codex_output_schema_for_purpose(&request.purpose);
        let schema_json = serde_json::to_vec_pretty(&schema).map_err(|err| {
            LlmProviderError::InvalidResponse {
                message: format!("serialize Codex output schema: {err}"),
                retryable: false,
            }
        })?;
        fs::write(schema_path, schema_json).map_err(|err| LlmProviderError::Cli {
            message: format!("write Codex output schema {}: {err}", schema_path.display()),
            retryable: false,
        })?;
        let provider_prompt = build_coding_agent_provider_prompt(&request);

        let mut command = Command::new(&self.executable);
        command
            .arg("exec")
            .arg("--sandbox")
            .arg(&self.sandbox)
            .arg("-c")
            .arg("approval_policy=\"never\"")
            .arg("--json")
            .arg("--cd")
            .arg(&self.project_root)
            .arg("--output-last-message")
            .arg(output_path)
            .arg("--output-schema")
            .arg(schema_path);
        if let Some(profile) = &self.profile {
            command.arg("--profile").arg(profile);
        }
        if let Some(model) = &self.model {
            command.arg("--model").arg(model);
        }
        command
            .arg("-")
            .current_dir(&self.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|err| LlmProviderError::Cli {
            message: format!("spawn Codex CLI {}: {err}", self.executable),
            retryable: false,
        })?;

        let stdout_reader = take_reader(&mut child.stdout, "stdout")?;
        let stderr_reader = take_reader(&mut child.stderr, "stderr")?;
        if let Err(err) = write_child_stdin(&mut child, &provider_prompt) {
            let _ = child.kill();
            return Err(err);
        }

        let status = wait_for_child(&mut child, self.timeout, self.timeout_seconds)?;
        let stdout = join_reader(stdout_reader, "stdout")?;
        let stderr = join_reader(stderr_reader, "stderr")?;

        if !status.success() {
            return Err(LlmProviderError::Cli {
                message: format!(
                    "codex exec exited with {status}: {}",
                    truncate_for_error(&String::from_utf8_lossy(&stderr))
                ),
                retryable: codex_status_retryable(status),
            });
        }

        let output_json =
            fs::read_to_string(output_path).map_err(|err| LlmProviderError::InvalidResponse {
                message: format!(
                    "read Codex output-last-message {}: {err}",
                    output_path.display()
                ),
                retryable: true,
            })?;
        let output_json = output_json.trim().to_owned();
        if output_json.is_empty() {
            return Err(LlmProviderError::InvalidResponse {
                message: "Codex output-last-message was empty".to_owned(),
                retryable: true,
            });
        }
        serde_json::from_str::<Value>(&output_json).map_err(|err| {
            LlmProviderError::InvalidResponse {
                message: format!("Codex output was not valid JSON: {err}"),
                retryable: true,
            }
        })?;

        let usage = parse_codex_jsonl_usage(&stdout);
        let input_tokens = usage
            .input_tokens
            .unwrap_or_else(|| estimate_text_tokens(&provider_prompt));
        let output_tokens = usage
            .output_tokens
            .unwrap_or_else(|| estimate_text_tokens(&output_json));
        let total_tokens = usage
            .total_tokens
            .unwrap_or_else(|| input_tokens.saturating_add(output_tokens));
        let cached_input_tokens = usage.cached_input_tokens.unwrap_or(0);

        Ok(LlmResponse {
            model_id: request.model_id,
            output_json,
            input_tokens,
            cached_input_tokens,
            output_tokens,
            total_tokens,
            cost_usd: 0.0,
        })
    }
}

#[async_trait]
impl LlmProvider for CodexCliProvider {
    fn name(&self) -> &'static str {
        "codex_cli"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let output_file = codex_temp_file("loomweave-codex-output", ".json")?;
            let schema_file = codex_temp_file("loomweave-codex-schema", ".json")?;
            this.invoke_with_temp_files(request, output_file.path(), schema_file.path())
        })
        .await
        .map_err(|err| LlmProviderError::Cli {
            message: format!("Codex CLI task failed to join: {err}"),
            retryable: true,
        })?
    }

    fn estimate_tokens(&self, request: &LlmRequest) -> u64 {
        u64::from(estimate_text_tokens(&build_coding_agent_provider_prompt(
            request,
        ))) + u64::from(request.max_output_tokens)
    }

    fn tier_to_model(&self, tier: &str) -> Option<&str> {
        match tier {
            "summary" | "inferred_edges" => Some(self.model_id.as_str()),
            _ => None,
        }
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::OpenAiChatCompletions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCliProviderConfig {
    pub executable: String,
    pub project_root: PathBuf,
    pub model_id: String,
    pub model: Option<String>,
    pub permission_mode: String,
    pub tools: Vec<String>,
    pub timeout_seconds: u64,
    pub max_turns: u32,
    pub no_session_persistence: bool,
    pub exclude_dynamic_system_prompt_sections: bool,
}

#[derive(Debug, Clone)]
pub struct ClaudeCliProvider {
    executable: String,
    project_root: PathBuf,
    model_id: String,
    model: Option<String>,
    permission_mode: String,
    tools: Vec<String>,
    timeout: Duration,
    timeout_seconds: u64,
    max_turns: u32,
    no_session_persistence: bool,
    exclude_dynamic_system_prompt_sections: bool,
}

impl ClaudeCliProvider {
    pub fn from_config(config: ClaudeCliProviderConfig) -> Result<Self, LlmProviderError> {
        if config.executable.trim().is_empty() {
            return Err(LlmProviderError::Cli {
                message: "Claude CLI executable must not be blank".to_owned(),
                retryable: false,
            });
        }
        if config.model_id.trim().is_empty() {
            return Err(LlmProviderError::Cli {
                message: "Claude CLI model_id must not be blank".to_owned(),
                retryable: false,
            });
        }
        if config.permission_mode.trim().is_empty() {
            return Err(LlmProviderError::Cli {
                message: "Claude CLI permission_mode must not be blank".to_owned(),
                retryable: false,
            });
        }
        if config.timeout_seconds == 0 {
            return Err(LlmProviderError::Cli {
                message: "Claude CLI timeout_seconds must be greater than zero".to_owned(),
                retryable: false,
            });
        }
        if config.max_turns == 0 {
            return Err(LlmProviderError::Cli {
                message: "Claude CLI max_turns must be greater than zero".to_owned(),
                retryable: false,
            });
        }
        validate_cli_executable("Claude CLI", &config.executable)?;

        Ok(Self {
            executable: config.executable,
            project_root: config.project_root,
            model_id: config.model_id,
            model: config.model.filter(|model| !model.trim().is_empty()),
            permission_mode: config.permission_mode,
            tools: config
                .tools
                .into_iter()
                .filter(|tool| !tool.trim().is_empty())
                .collect(),
            timeout: Duration::from_secs(config.timeout_seconds),
            timeout_seconds: config.timeout_seconds,
            max_turns: config.max_turns,
            no_session_persistence: config.no_session_persistence,
            exclude_dynamic_system_prompt_sections: config.exclude_dynamic_system_prompt_sections,
        })
    }
}

#[async_trait]
impl LlmProvider for ClaudeCliProvider {
    fn name(&self) -> &'static str {
        "claude_cli"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let schema = codex_output_schema_for_purpose(&request.purpose);
            let schema_json = serde_json::to_string(&schema).map_err(|err| {
                LlmProviderError::InvalidResponse {
                    message: format!("serialize Claude output schema: {err}"),
                    retryable: false,
                }
            })?;
            let provider_prompt = build_coding_agent_provider_prompt(&request);
            let mut command = Command::new(&this.executable);
            command
                .arg("-p")
                .arg(CLAUDE_CLI_PRINT_PROMPT)
                .arg("--output-format")
                .arg("json")
                .arg("--json-schema")
                .arg(schema_json)
                .arg("--permission-mode")
                .arg(&this.permission_mode)
                .arg("--max-turns")
                .arg(this.max_turns.to_string())
                .arg("--mcp-config")
                .arg(r#"{"mcpServers":{}}"#)
                .arg("--strict-mcp-config")
                .arg("--disable-slash-commands");
            if this.no_session_persistence {
                command.arg("--no-session-persistence");
            }
            if this.exclude_dynamic_system_prompt_sections {
                command.arg("--exclude-dynamic-system-prompt-sections");
            }
            if let Some(model) = &this.model {
                command.arg("--model").arg(model);
            }
            command.arg("--tools").arg(this.tools.join(","));
            command
                .current_dir(&this.project_root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let mut child = command.spawn().map_err(|err| LlmProviderError::Cli {
                message: format!("spawn Claude CLI {}: {err}", this.executable),
                retryable: false,
            })?;
            let stdout_reader = take_reader(&mut child.stdout, "stdout")?;
            let stderr_reader = take_reader(&mut child.stderr, "stderr")?;
            if let Err(err) = write_child_stdin(&mut child, &provider_prompt) {
                let _ = child.kill();
                return Err(err);
            }

            let status = wait_for_child(&mut child, this.timeout, this.timeout_seconds)?;
            let stdout = join_reader(stdout_reader, "stdout")?;
            let stderr = join_reader(stderr_reader, "stderr")?;
            if !status.success() {
                return Err(LlmProviderError::Cli {
                    message: format!(
                        "claude -p exited with {status}: {}",
                        truncate_for_error(&String::from_utf8_lossy(&stderr))
                    ),
                    retryable: cli_status_retryable(status),
                });
            }

            let parsed = parse_claude_cli_json_output(&stdout)?;
            let input_tokens = parsed
                .usage
                .input_tokens
                .unwrap_or_else(|| estimate_text_tokens(&provider_prompt));
            let output_tokens = parsed
                .usage
                .output_tokens
                .unwrap_or_else(|| estimate_text_tokens(&parsed.output_json));
            let total_tokens = parsed
                .usage
                .total_tokens
                .unwrap_or_else(|| input_tokens.saturating_add(output_tokens));
            let cached_input_tokens = parsed.usage.cached_input_tokens.unwrap_or(0);

            Ok(LlmResponse {
                model_id: request.model_id,
                output_json: parsed.output_json,
                input_tokens,
                cached_input_tokens,
                output_tokens,
                total_tokens,
                cost_usd: parsed.cost_usd.unwrap_or(0.0),
            })
        })
        .await
        .map_err(|err| LlmProviderError::Cli {
            message: format!("Claude CLI task failed to join: {err}"),
            retryable: true,
        })?
    }

    fn estimate_tokens(&self, request: &LlmRequest) -> u64 {
        u64::from(estimate_text_tokens(&build_coding_agent_provider_prompt(
            request,
        ))) + u64::from(request.max_output_tokens)
    }

    fn tier_to_model(&self, tier: &str) -> Option<&str> {
        match tier {
            "summary" | "inferred_edges" => Some(self.model_id.as_str()),
            _ => None,
        }
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::OpenAiChatCompletions
    }
}

fn response_format_for_purpose(purpose: &LlmPurpose) -> Value {
    match purpose {
        LlmPurpose::Summary => serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "loomweave_summary",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "purpose": {
                            "type": "string",
                            "description": "Why this entity exists in the local codebase."
                        },
                        "behavior": {
                            "type": "string",
                            "description": "What the entity does at leaf scope."
                        },
                        "relationships": {
                            "type": "string",
                            "description": "Important callers, callees, ownership, or adjacent entities visible from the prompt."
                        },
                        "risks": {
                            "type": "string",
                            "description": "Notable implementation risks, caveats, or empty string when none are visible."
                        }
                    },
                    "required": ["purpose", "behavior", "relationships", "risks"],
                    "additionalProperties": false
                }
            }
        }),
        LlmPurpose::InferredEdges => serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "loomweave_inferred_calls",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "edges": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "site_key": {
                                        "type": "string",
                                        "description": "The unresolved call-site key from the prompt."
                                    },
                                    "target_id": {
                                        "type": "string",
                                        "description": "The Loomweave entity id for the inferred callee."
                                    },
                                    "confidence": {
                                        "type": "number",
                                        "description": "Model confidence from 0.0 to 1.0."
                                    },
                                    "rationale": {
                                        "type": "string",
                                        "description": "Brief evidence for the inferred target."
                                    }
                                },
                                "required": ["site_key", "target_id", "confidence", "rationale"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["edges"],
                    "additionalProperties": false
                }
            }
        }),
    }
}

fn codex_output_schema_for_purpose(purpose: &LlmPurpose) -> Value {
    response_format_for_purpose(purpose)["json_schema"]["schema"].clone()
}

fn agent_task_type(purpose: &LlmPurpose) -> &'static str {
    match purpose {
        LlmPurpose::Summary => "leaf_summary",
        LlmPurpose::InferredEdges => "inferred_edges",
    }
}

fn agent_task_guidance(purpose: &LlmPurpose) -> &'static str {
    match purpose {
        LlmPurpose::Summary => {
            "- Produce a leaf-scope summary only for the requested entity.\n\
             - `purpose`, `behavior`, `relationships`, and `risks` must be strings.\n\
             - Do not summarize sibling entities except where direct caller/callee/ownership context is visible in the supplied excerpt.\n\
             - Use an empty `risks` string when no concrete implementation risk is visible."
        }
        LlmPurpose::InferredEdges => {
            "- Resolve only unresolved call sites listed in the request.\n\
             - Choose targets only from the supplied candidate entities JSON.\n\
             - Return no more edges than the request's max_edges instruction allows.\n\
             - Use confidence from 0.0 to 1.0 and include brief evidence in `rationale`.\n\
             - Return `{\"edges\":[]}` when the supplied evidence is insufficient."
        }
    }
}

type PipeReader = thread::JoinHandle<std::io::Result<Vec<u8>>>;

fn take_reader<R>(
    pipe: &mut Option<R>,
    pipe_name: &'static str,
) -> Result<PipeReader, LlmProviderError>
where
    R: Read + Send + 'static,
{
    let mut reader = pipe.take().ok_or_else(|| LlmProviderError::Cli {
        message: format!("child {pipe_name} was not captured"),
        retryable: false,
    })?;
    Ok(thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    }))
}

fn write_child_stdin(child: &mut Child, prompt: &str) -> Result<(), LlmProviderError> {
    let mut stdin = child.stdin.take().ok_or_else(|| LlmProviderError::Cli {
        message: "child stdin was not captured".to_owned(),
        retryable: false,
    })?;
    stdin
        .write_all(prompt.as_bytes())
        .map_err(|err| LlmProviderError::Cli {
            message: format!("write provider prompt to stdin: {err}"),
            retryable: true,
        })?;
    Ok(())
}

fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
    timeout_seconds: u64,
) -> Result<ExitStatus, LlmProviderError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LlmProviderError::Timeout { timeout_seconds });
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(err) => {
                return Err(LlmProviderError::Cli {
                    message: format!("poll provider process status: {err}"),
                    retryable: true,
                });
            }
        }
    }
}

fn join_reader(handle: PipeReader, pipe_name: &'static str) -> Result<Vec<u8>, LlmProviderError> {
    match handle.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(err)) => Err(LlmProviderError::Cli {
            message: format!("read provider {pipe_name}: {err}"),
            retryable: true,
        }),
        Err(_) => Err(LlmProviderError::Cli {
            message: format!("read provider {pipe_name}: reader thread panicked"),
            retryable: true,
        }),
    }
}

fn cli_status_retryable(status: ExitStatus) -> bool {
    status.code().is_none()
}

fn truncate_for_error(message: &str) -> String {
    const MAX_ERROR_CHARS: usize = 4096;
    if message.chars().count() <= MAX_ERROR_CHARS {
        return message.to_owned();
    }
    let mut truncated: String = message.chars().take(MAX_ERROR_CHARS).collect();
    truncated.push_str("... (truncated)");
    truncated
}

fn codex_status_retryable(status: ExitStatus) -> bool {
    cli_status_retryable(status)
}

fn codex_temp_file(
    prefix: &str,
    suffix: &str,
) -> Result<tempfile::NamedTempFile, LlmProviderError> {
    tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()
        .map_err(|err| LlmProviderError::Cli {
            message: format!("create Codex CLI temp file {prefix}: {err}"),
            retryable: true,
        })
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct LlmUsageSummary {
    input_tokens: Option<u32>,
    cached_input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    total_tokens: Option<u32>,
    cost_usd: Option<f64>,
}

impl LlmUsageSummary {
    fn add(&mut self, other: Self) {
        self.input_tokens = add_optional_u32(self.input_tokens, other.input_tokens);
        self.cached_input_tokens =
            add_optional_u32(self.cached_input_tokens, other.cached_input_tokens);
        self.output_tokens = add_optional_u32(self.output_tokens, other.output_tokens);
        self.total_tokens = add_optional_u32(self.total_tokens, other.total_tokens);
        self.cost_usd = add_optional_f64(self.cost_usd, other.cost_usd);
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ClaudeCliOutput {
    output_json: String,
    usage: LlmUsageSummary,
    cost_usd: Option<f64>,
}

fn add_optional_u32(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn add_optional_f64(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn parse_codex_jsonl_usage(stdout: &[u8]) -> LlmUsageSummary {
    let mut summary = LlmUsageSummary::default();
    let stdout_text = String::from_utf8_lossy(stdout);
    let mut malformed: u64 = 0;
    for line in stdout_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        match serde_json::from_str::<Value>(line) {
            Ok(event) => summary.add(usage_from_event(&event)),
            Err(err) => {
                malformed += 1;
                tracing::warn!(
                    error = %err,
                    snippet = %line.chars().take(80).collect::<String>(),
                    "Codex JSONL usage parser: skipping malformed line; token totals will be \
                     under-reported and `session_token_ceiling` enforcement may diverge from \
                     true accounting"
                );
            }
        }
    }
    if malformed > 0 {
        tracing::warn!(
            malformed = malformed,
            "Codex JSONL usage parser skipped {malformed} malformed line{suffix}; token totals \
             below are a lower bound only",
            suffix = if malformed == 1 { "" } else { "s" },
        );
    }
    summary
}

fn parse_claude_cli_json_output(stdout: &[u8]) -> Result<ClaudeCliOutput, LlmProviderError> {
    let stdout_text = String::from_utf8_lossy(stdout);
    let trimmed = stdout_text.trim();
    if trimmed.is_empty() {
        return Err(LlmProviderError::InvalidResponse {
            message: "Claude CLI returned empty stdout".to_owned(),
            retryable: true,
        });
    }
    let value = serde_json::from_str::<Value>(trimmed).map_err(|err| {
        LlmProviderError::InvalidResponse {
            message: format!("Claude CLI stdout was not JSON: {err}"),
            retryable: true,
        }
    })?;
    let events = match &value {
        Value::Array(events) => events.as_slice(),
        _ => std::slice::from_ref(&value),
    };
    let result_event = events
        .iter()
        .rev()
        .find(|event| event.get("type").and_then(Value::as_str) == Some("result"))
        .or_else(|| {
            events
                .iter()
                .rev()
                .find(|event| has_claude_structured_output(event))
        })
        .ok_or_else(|| LlmProviderError::InvalidResponse {
            message: "Claude CLI stdout had no `result` event or `structured_output`/\
                      `structuredOutput`/`result` payload; refusing to persist raw stdout \
                      as structured output"
                .to_owned(),
            retryable: true,
        })?;
    let usage = if result_event.get("usage").is_some() {
        usage_from_event(result_event)
    } else {
        let mut summary = LlmUsageSummary::default();
        for event in events {
            summary.add(usage_from_event(event));
        }
        summary
    };
    let cost_usd = result_event
        .get("total_cost_usd")
        .or_else(|| result_event.get("cost_usd"))
        .and_then(Value::as_f64)
        .or(usage.cost_usd);
    let output_json = claude_structured_output_json(result_event)?;
    Ok(ClaudeCliOutput {
        output_json,
        usage,
        cost_usd,
    })
}

fn has_claude_structured_output(value: &Value) -> bool {
    value.get("structured_output").is_some()
        || value.get("structuredOutput").is_some()
        || value.get("result").is_some()
}

fn claude_structured_output_json(value: &Value) -> Result<String, LlmProviderError> {
    let output = value
        .get("structured_output")
        .or_else(|| value.get("structuredOutput"))
        .or_else(|| value.get("result"))
        .ok_or_else(|| LlmProviderError::InvalidResponse {
            message: "Claude CLI event lacked `structured_output`/`structuredOutput`/`result` \
                      field; refusing to persist raw event as structured output"
                .to_owned(),
            retryable: true,
        })?;
    json_value_to_output_json(output, "Claude CLI structured output")
}

fn json_value_to_output_json(value: &Value, label: &str) -> Result<String, LlmProviderError> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(LlmProviderError::InvalidResponse {
                    message: format!("{label} was empty"),
                    retryable: true,
                });
            }
            serde_json::from_str::<Value>(trimmed).map_err(|err| {
                LlmProviderError::InvalidResponse {
                    message: format!("{label} string was not JSON: {err}"),
                    retryable: true,
                }
            })?;
            Ok(trimmed.to_owned())
        }
        Value::Object(_) | Value::Array(_) => {
            serde_json::to_string(value).map_err(|err| LlmProviderError::InvalidResponse {
                message: format!("serialize {label}: {err}"),
                retryable: false,
            })
        }
        _ => Err(LlmProviderError::InvalidResponse {
            message: format!("{label} was not an object, array, or JSON string"),
            retryable: true,
        }),
    }
}

fn usage_from_event(event: &Value) -> LlmUsageSummary {
    let raw_usage = event
        .get("usage")
        .or_else(|| event.get("msg").and_then(|msg| msg.get("usage")))
        .or_else(|| {
            event
                .get("message")
                .and_then(|message| message.get("usage"))
        });
    let Some(raw_usage) = raw_usage else {
        return LlmUsageSummary::default();
    };
    usage_from_usage_value(raw_usage)
}

fn usage_from_usage_value(raw_usage: &Value) -> LlmUsageSummary {
    let input_tokens = u32_from_value(raw_usage.get("input_tokens"))
        .or_else(|| u32_from_value(raw_usage.get("prompt_tokens")));
    let output_tokens = u32_from_value(raw_usage.get("output_tokens"))
        .or_else(|| u32_from_value(raw_usage.get("completion_tokens")));
    let total_tokens = u32_from_value(raw_usage.get("total_tokens")).or_else(|| {
        match (input_tokens, output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output)),
            _ => None,
        }
    });
    let cached_from_details = raw_usage
        .get("prompt_tokens_details")
        .and_then(|details| u32_from_value(details.get("cached_tokens")));
    let cached_input_tokens = u32_from_value(raw_usage.get("cached_input_tokens"))
        .or_else(|| u32_from_value(raw_usage.get("cache_read_input_tokens")))
        .or(cached_from_details);
    LlmUsageSummary {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        total_tokens,
        cost_usd: raw_usage
            .get("cost_usd")
            .or_else(|| raw_usage.get("cost"))
            .and_then(Value::as_f64),
    }
}

fn u32_from_value(value: Option<&Value>) -> Option<u32> {
    let value = value?;
    match value {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterChatResponse {
    model: String,
    choices: Vec<OpenRouterChoice>,
    usage: Option<OpenRouterUsage>,
}

impl OpenRouterChatResponse {
    fn output_text(&self) -> Result<String, LlmProviderError> {
        for choice in &self.choices {
            if let Some(error) = &choice.error {
                return Err(provider_error_from_openrouter(
                    error.status_code(200),
                    Some(error.code.clone()),
                    error.message.clone(),
                    error.metadata.clone(),
                    None,
                ));
            }
        }
        let text = self
            .choices
            .iter()
            .filter_map(|choice| choice.message.as_ref())
            .filter_map(|message| message.content.as_deref())
            .find(|content| !content.trim().is_empty())
            .ok_or_else(|| LlmProviderError::InvalidResponse {
                message: "response contained no assistant message content".to_owned(),
                retryable: true,
            })?;
        Ok(text.to_owned())
    }
}

#[derive(Debug, Deserialize)]
struct OpenRouterChoice {
    message: Option<OpenRouterMessage>,
    error: Option<OpenRouterErrorBody>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterUsage {
    #[serde(rename = "prompt_tokens")]
    prompt: u32,
    #[serde(rename = "completion_tokens")]
    completion: u32,
    #[serde(rename = "total_tokens")]
    total: u32,
    prompt_tokens_details: Option<OpenRouterPromptTokensDetails>,
    cost: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPromptTokensDetails {
    cached_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenRouterErrorEnvelope {
    error: OpenRouterErrorBody,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenRouterErrorBody {
    code: Value,
    message: String,
    metadata: Option<Value>,
}

impl OpenRouterErrorBody {
    fn status_code(&self, fallback: u16) -> u16 {
        self.code
            .as_u64()
            .and_then(|code| u16::try_from(code).ok())
            .unwrap_or(fallback)
    }
}

fn provider_error_from_body(
    status: u16,
    retry_after_seconds: Option<u64>,
    body: &str,
) -> LlmProviderError {
    match serde_json::from_str::<OpenRouterErrorEnvelope>(body) {
        Ok(envelope) => provider_error_from_openrouter(
            status,
            Some(envelope.error.code),
            envelope.error.message,
            envelope.error.metadata,
            retry_after_seconds,
        ),
        Err(_) => {
            provider_error_from_openrouter(status, None, body.to_owned(), None, retry_after_seconds)
        }
    }
}

fn provider_error_from_openrouter(
    status: u16,
    code: Option<Value>,
    message: String,
    metadata: Option<Value>,
    retry_after_seconds: Option<u64>,
) -> LlmProviderError {
    LlmProviderError::Provider {
        status,
        code,
        message,
        metadata,
        retryable: retryable_status(status),
        retry_after_seconds,
    }
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn estimate_text_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(4))
        .unwrap_or(u32::MAX)
        .max(1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub id: &'static str,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafSummaryPromptInput {
    pub entity_id: String,
    pub kind: String,
    pub name: String,
    pub guidance: String,
    pub source_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredCallsPromptInput {
    pub caller_entity_id: String,
    pub caller_source_excerpt: String,
    pub unresolved_call_sites_json: String,
    pub candidate_entities_json: String,
    pub max_edges: usize,
}

pub fn build_leaf_summary_prompt(input: &LeafSummaryPromptInput) -> PromptTemplate {
    let guidance = if input.guidance.trim().is_empty() {
        "No matching guidance."
    } else {
        input.guidance.as_str()
    };
    PromptTemplate {
        id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID,
        body: format!(
            "You are summarising one Loomweave entity at leaf scope only.\n\
             Entity id: {entity_id}\n\
             Kind: {kind}\n\
             Name: {name}\n\
             Matching guidance:\n{guidance}\n\
             Source excerpt:\n{source}\n\
             Return JSON with purpose, behavior, relationships, and risks fields.",
            entity_id = input.entity_id,
            kind = input.kind,
            name = input.name,
            guidance = guidance,
            source = input.source_excerpt,
        ),
    }
}

pub fn build_inferred_calls_prompt(input: &InferredCallsPromptInput) -> PromptTemplate {
    PromptTemplate {
        id: INFERRED_CALLS_PROMPT_VERSION,
        body: format!(
            "You are resolving unresolved Loomweave call sites for one caller.\n\
             Caller entity id: {caller}\n\
             Caller source excerpt:\n{source}\n\
             Unresolved call sites JSON:\n{sites}\n\
             Candidate entities JSON:\n{candidates}\n\
             Return JSON with an edges array containing no more than {max_edges} entries. \
             Each edge must contain site_key, target_id, confidence, and rationale.",
            caller = input.caller_entity_id,
            source = input.caller_source_excerpt,
            sites = input.unresolved_call_sites_json,
            candidates = input.candidate_entities_json,
            max_edges = input.max_edges,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recording_provider_replays_exact_request_shape() {
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "summarise python:function:demo.hello".to_owned(),
            max_output_tokens: 512,
        };
        let response = LlmResponse {
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            output_json: r#"{"purpose":"demo"}"#.to_owned(),
            input_tokens: 120,
            cached_input_tokens: 0,
            output_tokens: 24,
            total_tokens: 144,
            cost_usd: 0.0,
        };
        let provider = RecordingProvider::from_recordings(vec![Recording {
            request: request.clone(),
            response: response.clone(),
        }]);

        assert_eq!(provider.invoke(request.clone()).await.unwrap(), response);
        assert_eq!(provider.invocations(), vec![request.clone()]);

        let missing = provider
            .invoke(LlmRequest {
                prompt: "changed".to_owned(),
                ..request
            })
            .await
            .expect_err("request-shape drift should miss the recording");
        assert!(matches!(missing, LlmProviderError::MissingRecording { .. }));
    }

    #[tokio::test]
    async fn traffic_logging_provider_appends_success_metadata_without_exchange_contents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp
            .path()
            .join(".weft/loomweave/diagnostics/llm-traffic.jsonl");
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "summary-model".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "SECRET_PROMPT_DO_NOT_LOG".to_owned(),
            max_output_tokens: 512,
        };
        let response = LlmResponse {
            model_id: "summary-model".to_owned(),
            output_json: r#"{"purpose":"SECRET_OUTPUT_DO_NOT_LOG"}"#.to_owned(),
            input_tokens: 11,
            cached_input_tokens: 3,
            output_tokens: 7,
            total_tokens: 18,
            cost_usd: 0.25,
        };
        let inner = RecordingProvider::from_recordings(vec![Recording {
            request: request.clone(),
            response,
        }]);
        let provider = TrafficLoggingProvider::new(std::sync::Arc::new(inner), log_path.clone());

        provider
            .invoke(request)
            .await
            .expect("logged provider invoke");

        let log = std::fs::read_to_string(log_path).expect("read traffic log");
        assert!(
            !log.contains("SECRET_PROMPT_DO_NOT_LOG") && !log.contains("SECRET_OUTPUT_DO_NOT_LOG"),
            "traffic log must not include prompt or output contents: {log}"
        );
        let event: Value = serde_json::from_str(log.trim()).expect("traffic log JSON");
        assert_eq!(event["schema"], "loomweave.llm.lookup.v1");
        assert_eq!(event["provider"], "recording");
        assert_eq!(event["purpose"], "Summary");
        assert_eq!(event["prompt_id"], LEAF_SUMMARY_PROMPT_TEMPLATE_ID);
        assert_eq!(event["request_model_id"], "summary-model");
        assert_eq!(event["outcome"], "success");
        assert_eq!(event["usage"]["input_tokens"], 11);
        assert_eq!(event["usage"]["cached_input_tokens"], 3);
        assert_eq!(event["usage"]["output_tokens"], 7);
        assert_eq!(event["usage"]["total_tokens"], 18);
        assert_eq!(event["usage"]["cost_usd"], 0.25);
    }

    #[tokio::test]
    async fn traffic_logging_provider_appends_error_metadata_without_exchange_contents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp
            .path()
            .join(".weft/loomweave/diagnostics/llm-traffic.jsonl");
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "summary-model".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "SECRET_PROMPT_DO_NOT_LOG".to_owned(),
            max_output_tokens: 512,
        };
        let provider = TrafficLoggingProvider::new(
            std::sync::Arc::new(RecordingProvider::from_recordings(Vec::new())),
            log_path.clone(),
        );

        provider
            .invoke(request)
            .await
            .expect_err("missing recording should still be logged");

        let log = std::fs::read_to_string(log_path).expect("read traffic log");
        assert!(
            !log.contains("SECRET_PROMPT_DO_NOT_LOG"),
            "traffic log must not include prompt contents: {log}"
        );
        let event: Value = serde_json::from_str(log.trim()).expect("traffic log JSON");
        assert_eq!(event["provider"], "recording");
        assert_eq!(event["outcome"], "error");
        assert_eq!(event["error"]["kind"], "missing_recording");
        assert_eq!(event["error"]["retryable"], false);
    }

    #[tokio::test]
    async fn traffic_logging_provider_rotates_diagnostics_log_when_size_limit_is_reached() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp
            .path()
            .join(".weft/loomweave/diagnostics/llm-traffic.jsonl");
        let backup_path = PathBuf::from(format!("{}.1", log_path.display()));
        fs::create_dir_all(log_path.parent().expect("log parent")).expect("create log parent");
        fs::write(&log_path, "old diagnostic lookup\nold diagnostic lookup\n")
            .expect("seed oversized log");

        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "summary-model".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "SECRET_PROMPT_DO_NOT_LOG".to_owned(),
            max_output_tokens: 512,
        };
        let response = LlmResponse {
            model_id: "summary-model".to_owned(),
            output_json: r#"{"purpose":"SECRET_OUTPUT_DO_NOT_LOG"}"#.to_owned(),
            input_tokens: 11,
            cached_input_tokens: 0,
            output_tokens: 7,
            total_tokens: 18,
            cost_usd: 0.0,
        };
        let inner = RecordingProvider::from_recordings(vec![Recording {
            request: request.clone(),
            response,
        }]);
        let provider = TrafficLoggingProvider::with_max_bytes(
            std::sync::Arc::new(inner),
            log_path.clone(),
            32,
        );

        provider
            .invoke(request)
            .await
            .expect("logged provider invoke");

        let backup = fs::read_to_string(&backup_path).expect("read rotated diagnostics log");
        assert!(backup.contains("old diagnostic lookup"));
        let current = fs::read_to_string(&log_path).expect("read fresh diagnostics log");
        assert!(!current.contains("old diagnostic lookup"));
        assert!(
            !current.contains("SECRET_PROMPT_DO_NOT_LOG")
                && !current.contains("SECRET_OUTPUT_DO_NOT_LOG"),
            "rotated diagnostics log must still omit exchange contents: {current}"
        );
        let event: Value = serde_json::from_str(current.trim()).expect("traffic log JSON");
        assert_eq!(event["outcome"], "success");
    }

    #[tokio::test]
    async fn traffic_logging_failure_does_not_poison_successful_llm_result() {
        // weft-ac59e8e730: the diagnostics sidecar must never gate the call it
        // observes. Make the log path unwritable by occupying its parent path
        // with a regular file, so create_dir_all fails.
        let temp = tempfile::tempdir().expect("tempdir");
        let blocking_file = temp.path().join("diagnostics");
        fs::write(&blocking_file, "not a directory").expect("seed blocking file");
        let log_path = blocking_file.join("llm-traffic.jsonl");

        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "summary-model".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "hello".to_owned(),
            max_output_tokens: 512,
        };
        let response = LlmResponse {
            model_id: "summary-model".to_owned(),
            output_json: r#"{"purpose":"demo"}"#.to_owned(),
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 1,
            total_tokens: 2,
            cost_usd: 0.0,
        };
        let inner = RecordingProvider::from_recordings(vec![Recording {
            request: request.clone(),
            response: response.clone(),
        }]);
        let provider = TrafficLoggingProvider::new(std::sync::Arc::new(inner), log_path);

        let result = provider
            .invoke(request)
            .await
            .expect("a successful LLM call must survive a diagnostics write failure");
        assert_eq!(result, response);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_traffic_log_appends_emit_only_well_formed_json_lines() {
        // weft-ac59e8e730: unsynchronised appends interleaved partial JSON
        // lines under concurrent tools/call dispatch; rotation under load also
        // raced itself. Hammer one shared log (with a tiny rotation cap so
        // rotations actually collide) and require every surviving line to
        // parse as a complete event.
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("diagnostics/llm-traffic.jsonl");
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "summary-model".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "hello".to_owned(),
            max_output_tokens: 512,
        };
        let response = LlmResponse {
            model_id: "summary-model".to_owned(),
            output_json: r#"{"purpose":"demo"}"#.to_owned(),
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 1,
            total_tokens: 2,
            cost_usd: 0.0,
        };
        let inner = RecordingProvider::from_recordings(vec![Recording {
            request: request.clone(),
            response,
        }]);
        let provider = TrafficLoggingProvider::with_max_bytes(
            std::sync::Arc::new(inner),
            log_path.clone(),
            512, // force frequent rotations so the rotation race is exercised
        );

        let mut handles = Vec::new();
        for _ in 0..64 {
            let provider = provider.clone();
            let request = request.clone();
            handles.push(tokio::spawn(async move { provider.invoke(request).await }));
        }
        for handle in handles {
            handle
                .await
                .expect("join")
                .expect("every concurrent invoke must succeed");
        }

        let mut parsed_lines = 0usize;
        for path in [log_path.clone(), llm_traffic_backup_path(&log_path)] {
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            for line in contents.lines() {
                let event: Value = serde_json::from_str(line).unwrap_or_else(|err| {
                    panic!("interleaved/partial traffic log line {line:?}: {err}")
                });
                assert_eq!(event["schema"], "loomweave.llm.lookup.v1");
                parsed_lines += 1;
            }
        }
        assert!(
            parsed_lines > 0,
            "expected surviving well-formed events after concurrent appends"
        );
    }

    #[test]
    fn cross_process_large_line_appends_never_interleave() {
        // L6: O_APPEND is line-atomic across processes only up to PIPE_BUF
        // (4096 on Linux). A line longer than that is written in multiple
        // write() syscalls, and a SECOND process sharing the log path can
        // interleave between them — corrupting the JSON. The per-process
        // `write_lock` cannot prevent that. We model separate processes with
        // separate `TrafficLoggingProvider` instances (so their in-process
        // mutexes are DISTINCT — only the cross-process flock can serialise
        // them) writing oversized events concurrently from many threads. Every
        // surviving line must still parse: proof the flock — not the dead
        // PIPE_BUF assumption — provides the cross-process guarantee.
        let temp = tempfile::tempdir().expect("tempdir");
        let log_path = temp.path().join("diagnostics/llm-traffic.jsonl");

        // A payload far larger than PIPE_BUF so each append needs multiple
        // write() syscalls.
        let big = "x".repeat(64 * 1024);

        let make_provider = || {
            let inner = RecordingProvider::from_recordings(Vec::new());
            // A generous rotation cap so we test interleave, not rotation, here
            // (rotation racing is already covered by the sibling test).
            TrafficLoggingProvider::with_max_bytes(
                std::sync::Arc::new(inner),
                log_path.clone(),
                64 * 1024 * 1024,
            )
        };

        let mut handles = Vec::new();
        for proc in 0..8 {
            // DISTINCT provider per "process": distinct in-process write_lock.
            let provider = make_provider();
            let big = big.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..16 {
                    let event = serde_json::json!({
                        "schema": "loomweave.llm.lookup.v1",
                        "proc": proc,
                        "seq": i,
                        "payload": big,
                    });
                    provider
                        .append_event(&event)
                        .expect("append must succeed under cross-process contention");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("join writer thread");
        }

        let contents = fs::read_to_string(&log_path).expect("read log");
        let mut lines = 0usize;
        for line in contents.lines() {
            let event: Value = serde_json::from_str(line).unwrap_or_else(|err| {
                panic!(
                    "interleaved/partial oversized traffic line (len {}): {err}",
                    line.len()
                )
            });
            assert_eq!(event["schema"], "loomweave.llm.lookup.v1");
            lines += 1;
        }
        assert_eq!(
            lines,
            8 * 16,
            "every oversized append must land as exactly one well-formed line"
        );
    }

    #[test]
    fn prompt_templates_have_stable_versions_and_embed_context() {
        let summary = build_leaf_summary_prompt(&LeafSummaryPromptInput {
            entity_id: "python:function:demo.hello".to_owned(),
            kind: "function".to_owned(),
            name: "demo.hello".to_owned(),
            guidance: String::new(),
            source_excerpt: "def hello():\n    return 42\n".to_owned(),
        });
        assert_eq!(summary.id, LEAF_SUMMARY_PROMPT_TEMPLATE_ID);
        assert!(summary.body.contains("python:function:demo.hello"));
        assert!(summary.body.contains("Return JSON"));

        let inferred = build_inferred_calls_prompt(&InferredCallsPromptInput {
            caller_entity_id: "python:function:demo.via_dispatch".to_owned(),
            caller_source_excerpt: "return DISPATCH[key]()".to_owned(),
            unresolved_call_sites_json: r#"[{"site_key":"a"}]"#.to_owned(),
            candidate_entities_json: r#"[{"id":"python:function:demo.world"}]"#.to_owned(),
            max_edges: 8,
        });
        assert_eq!(inferred.id, INFERRED_CALLS_PROMPT_VERSION);
        assert!(inferred.body.contains("python:function:demo.via_dispatch"));
        assert!(inferred.body.contains("Return JSON"));
        assert!(inferred.body.contains("no more than 8 entries"));
    }

    #[test]
    fn coding_agent_provider_prompt_wraps_request_with_shared_contract() {
        let request = LlmRequest {
            purpose: LlmPurpose::InferredEdges,
            model_id: "agent-default".to_owned(),
            prompt_id: INFERRED_CALLS_PROMPT_VERSION.to_owned(),
            prompt: "Resolve call-site a from the supplied candidates".to_owned(),
            max_output_tokens: 2048,
        };

        let prompt = build_coding_agent_provider_prompt(&request);

        assert!(prompt.contains("Prompt contract: loomweave-agent-provider-v1"));
        assert!(prompt.contains("Task type: inferred_edges"));
        assert!(prompt.contains("Do not inspect additional files"));
        assert!(prompt.contains("Return exactly one JSON object"));
        assert!(prompt.contains("Choose targets only from the supplied candidate entities JSON"));
        assert!(prompt.contains("<loomweave_request>"));
        assert!(prompt.contains("Resolve call-site a from the supplied candidates"));
    }

    #[test]
    fn openrouter_provider_requires_explicit_live_opt_in_and_api_key() {
        let denied = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: false,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect_err("api key alone must not enable live calls");
        assert!(matches!(denied, LlmProviderError::LiveProviderNotAllowed));

        let missing_key = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: None,
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect_err("live opt-in without key should fail");
        assert!(matches!(missing_key, LlmProviderError::MissingApiKey));

        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect("live opt-in and key should construct provider");

        assert_eq!(provider.name(), "openrouter");
        assert_eq!(
            provider.tier_to_model("summary"),
            Some("anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            provider.tier_to_model("inferred_edges"),
            Some("anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            provider.caching_model(),
            CachingModel::OpenAiChatCompletions
        );

        let zero_timeout = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 0,
        });
        assert!(
            matches!(zero_timeout, Err(LlmProviderError::InvalidConfig { .. })),
            "zero timeout_seconds must be rejected"
        );
    }

    #[tokio::test]
    async fn openrouter_provider_invokes_chat_completions_and_extracts_usage_tokens() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("POST /api/v1/chat/completions HTTP/1.1"));
            assert!(request.contains("authorization: Bearer secret"));
            assert!(request.contains("http-referer: https://github.com/foundryside-dev/loomweave"));
            assert!(request.contains("x-openrouter-title: Loomweave"));
            assert!(request.contains(r#""model":"anthropic/claude-sonnet-4.6""#));
            assert!(request.contains(r#""max_tokens":512"#));
            assert!(request.contains("Summarize this function"));
            assert!(
                request.contains(r#""response_format":{"json_schema":{"name":"loomweave_summary""#)
            );
            assert!(request.contains(r#""strict":true"#));
            assert!(
                request.contains(r#""required":["purpose","behavior","relationships","risks"]"#)
            );

            let body = r#"{
                "id": "gen-01",
                "object": "chat.completion",
                "created": 1779000000,
                "model": "anthropic/claude-sonnet-4.6",
                "choices": [
                    {
                        "finish_reason": "stop",
                        "native_finish_reason": "stop",
                        "message": {"role": "assistant", "content": "{\"purpose\":\"demo\"}"}
                    }
                ],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 200, "total_tokens": 1200, "cost": 0.123}
            }"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: format!("http://{addr}/api/v1"),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect("test provider");

        let response = provider
            .invoke(LlmRequest {
                purpose: LlmPurpose::Summary,
                model_id: "anthropic/claude-sonnet-4.6".to_owned(),
                prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                prompt: "Summarize this function".to_owned(),
                max_output_tokens: 512,
            })
            .await
            .expect("invoke mocked OpenRouter");

        assert_eq!(response.output_json, r#"{"purpose":"demo"}"#);
        assert_eq!(response.input_tokens, 1000);
        assert_eq!(response.output_tokens, 200);
        assert_eq!(response.total_tokens, 1200);
        assert!((response.cost_usd - 0.123).abs() < f64::EPSILON);
        handle.join().expect("server thread");
    }

    #[tokio::test]
    async fn openrouter_provider_unwraps_error_envelope_with_retryability() {
        let auth_error = invoke_openrouter_once(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"code\":401,\"message\":\"Invalid credentials\",\"metadata\":{}}}",
        )
        .await
        .expect_err("401 should return provider error");
        assert!(matches!(
            auth_error,
            LlmProviderError::Provider {
                status: 401,
                retryable: false,
                ..
            }
        ));
        assert!(auth_error.to_string().contains("Invalid credentials"));

        let retryable = invoke_openrouter_once(
            "HTTP/1.1 503 Service Unavailable\r\nretry-after: 60\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"code\":503,\"message\":\"No provider available\",\"metadata\":{}}}",
        )
        .await
        .expect_err("503 should return provider error");
        assert!(matches!(
            retryable,
            LlmProviderError::Provider {
                status: 503,
                retryable: true,
                retry_after_seconds: Some(60),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn openrouter_provider_unwraps_choice_level_error() {
        let err = invoke_openrouter_once(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"id\":\"gen-01\",\"object\":\"chat.completion\",\"created\":1779000000,\"model\":\"anthropic/claude-sonnet-4.6\",\"choices\":[{\"finish_reason\":\"error\",\"native_finish_reason\":\"error\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"error\":{\"code\":502,\"message\":\"Provider disconnected\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":0,\"total_tokens\":1}}",
        )
        .await
        .expect_err("choice error should return provider error");

        assert!(matches!(
            err,
            LlmProviderError::Provider {
                status: 502,
                retryable: true,
                ..
            }
        ));
        assert!(err.to_string().contains("Provider disconnected"));
    }

    #[tokio::test]
    async fn openrouter_provider_uses_inferred_calls_schema_for_inferred_requests() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains(
                r#""response_format":{"json_schema":{"name":"loomweave_inferred_calls""#
            ));
            assert!(request.contains(r#""required":["edges"]"#));
            assert!(
                request.contains(r#""required":["site_key","target_id","confidence","rationale"]"#)
            );

            let body = r#"{
                "id": "gen-01",
                "object": "chat.completion",
                "created": 1779000000,
                "model": "anthropic/claude-sonnet-4.6",
                "choices": [
                    {
                        "finish_reason": "stop",
                        "native_finish_reason": "stop",
                        "message": {"role": "assistant", "content": "{\"edges\":[]}"}
                    }
                ],
                "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12, "cost": 0.004}
            }"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: format!("http://{addr}/api/v1"),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect("test provider");

        let response = provider
            .invoke(LlmRequest {
                purpose: LlmPurpose::InferredEdges,
                model_id: "anthropic/claude-sonnet-4.6".to_owned(),
                prompt_id: INFERRED_CALLS_PROMPT_VERSION.to_owned(),
                prompt: "Resolve calls".to_owned(),
                max_output_tokens: 512,
            })
            .await
            .expect("invoke mocked OpenRouter");

        assert_eq!(response.output_json, r#"{"edges":[]}"#);
        assert_eq!(response.total_tokens, 12);
        assert!((response.cost_usd - 0.004).abs() < f64::EPSILON);
        handle.join().expect("server thread");
    }

    #[tokio::test]
    async fn openrouter_provider_connection_error_is_retryable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
        let addr = listener.local_addr().expect("unused port addr");
        drop(listener);
        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: format!("http://{addr}/api/v1"),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect("test provider");

        let err = provider
            .invoke(sample_request())
            .await
            .expect_err("connection refused should be retryable");
        assert!(matches!(
            err,
            LlmProviderError::Http {
                retryable: true,
                ..
            }
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn codex_cli_provider_invokes_exec_with_schema_stdin_and_usage() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root).expect("project root");
        let fake_codex = temp.path().join("codex");
        let log_path = temp.path().join("codex.log");
        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
log="{log}"
out=""
schema=""
cd_arg=""
sandbox=""
model=""
profile=""
json=0
stdin_prompt=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    exec)
      echo "subcommand=exec" >> "$log"
      shift
      ;;
    --sandbox)
      sandbox="$2"
      shift 2
      ;;
    --cd)
      cd_arg="$2"
      shift 2
      ;;
    --output-last-message)
      out="$2"
      shift 2
      ;;
    --output-schema)
      schema="$2"
      shift 2
      ;;
    --model)
      model="$2"
      shift 2
      ;;
    --profile)
      profile="$2"
      shift 2
      ;;
    --json)
      json=1
      shift
      ;;
    -c)
      echo "config=$2" >> "$log"
      shift 2
      ;;
    -)
      stdin_prompt="$(cat)"
      shift
      ;;
    *)
      echo "arg=$1" >> "$log"
      shift
      ;;
  esac
done

test "$json" = "1"
test -n "$out"
test -s "$schema"
grep -q '"purpose"' "$schema"
grep -q '"behavior"' "$schema"
case "$stdin_prompt" in
  *"Summarize this function"*) ;;
  *) echo "missing prompt" >&2; exit 31 ;;
esac
case "$stdin_prompt" in
  *"Prompt contract: loomweave-agent-provider-v1"*"Do not inspect additional files"*) ;;
  *) echo "missing Loomweave agent prompt contract" >&2; exit 32 ;;
esac

echo "sandbox=$sandbox" >> "$log"
echo "cd=$cd_arg" >> "$log"
echo "model=$model" >> "$log"
echo "profile=$profile" >> "$log"
printf '%s\n' '{{"usage":{{"input_tokens":11,"cached_input_tokens":4,"output_tokens":7,"total_tokens":18}}}}'
printf '%s' '{{"purpose":"via codex","behavior":"ran fake CLI","relationships":"","risks":""}}' > "$out"
"#,
            log = log_path.display()
        );
        fs::write(&fake_codex, script).expect("write fake codex");
        let mut permissions = fs::metadata(&fake_codex).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).expect("chmod fake codex");

        let provider = CodexCliProvider::from_config(CodexCliProviderConfig {
            executable: fake_codex.display().to_string(),
            project_root: project_root.clone(),
            model_id: "codex-cli-default".to_owned(),
            model: Some("gpt-5.5".to_owned()),
            profile: Some("loomweave".to_owned()),
            sandbox: "read-only".to_owned(),
            timeout_seconds: 5,
        })
        .expect("construct Codex CLI provider");

        let response = provider
            .invoke(LlmRequest {
                purpose: LlmPurpose::Summary,
                model_id: "codex-cli-default".to_owned(),
                prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                prompt: "Summarize this function".to_owned(),
                max_output_tokens: 512,
            })
            .await
            .expect("invoke fake Codex CLI");

        assert_eq!(provider.name(), "codex_cli");
        assert_eq!(provider.tier_to_model("summary"), Some("codex-cli-default"));
        assert_eq!(response.model_id, "codex-cli-default");
        assert_eq!(
            response.output_json,
            r#"{"purpose":"via codex","behavior":"ran fake CLI","relationships":"","risks":""}"#
        );
        assert_eq!(response.input_tokens, 11);
        assert_eq!(response.cached_input_tokens, 4);
        assert_eq!(response.output_tokens, 7);
        assert_eq!(response.total_tokens, 18);
        assert!(response.cost_usd.abs() < f64::EPSILON);

        let log = fs::read_to_string(log_path).expect("read fake codex log");
        assert!(log.contains("subcommand=exec"));
        assert!(log.contains("config=approval_policy=\"never\""));
        assert!(log.contains("sandbox=read-only"));
        assert!(log.contains(&format!("cd={}", project_root.display())));
        assert!(log.contains("model=gpt-5.5"));
        assert!(log.contains("profile=loomweave"));
    }

    #[tokio::test]
    async fn codex_cli_provider_fallback_usage_counts_wrapped_prompt() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root).expect("project root");
        let fake_codex = temp.path().join("codex");
        let script = r#"#!/usr/bin/env bash
set -euo pipefail
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-last-message)
      out="$2"
      shift 2
      ;;
    --sandbox|--cd|--output-schema|--model|--profile|-c)
      shift 2
      ;;
    -)
      cat >/dev/null
      shift
      ;;
    *)
      shift
      ;;
  esac
done
printf '%s' '{"purpose":"via codex","behavior":"ran fake CLI","relationships":"","risks":""}' > "$out"
"#;
        fs::write(&fake_codex, script).expect("write fake codex");
        let mut permissions = fs::metadata(&fake_codex).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_codex, permissions).expect("chmod fake codex");

        let provider = CodexCliProvider::from_config(CodexCliProviderConfig {
            executable: fake_codex.display().to_string(),
            project_root,
            model_id: "codex-cli-default".to_owned(),
            model: None,
            profile: None,
            sandbox: "read-only".to_owned(),
            timeout_seconds: 5,
        })
        .expect("construct Codex CLI provider");
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "codex-cli-default".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "short raw prompt".to_owned(),
            max_output_tokens: 512,
        };
        let expected_input_tokens =
            estimate_text_tokens(&build_coding_agent_provider_prompt(&request));

        let response = provider
            .invoke(request)
            .await
            .expect("invoke fake Codex CLI");

        assert_eq!(response.input_tokens, expected_input_tokens);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn claude_cli_provider_invokes_print_mode_with_schema_and_usage() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root).expect("project root");
        let fake_claude = temp.path().join("claude");
        let log_path = temp.path().join("claude.log");
        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
log="{log}"
schema=""
format=""
model=""
permission_mode=""
tools="unset"
max_turns=""
no_session_persistence=0
exclude_dynamic=0
mcp_config=""
strict_mcp=0
slash_disabled=0
print_prompt=""
stdin_prompt=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -p|--print)
      print_prompt="$2"
      shift 2
      ;;
    --output-format)
      format="$2"
      shift 2
      ;;
    --json-schema)
      schema="$2"
      shift 2
      ;;
    --model)
      model="$2"
      shift 2
      ;;
    --permission-mode)
      permission_mode="$2"
      shift 2
      ;;
    --tools)
      tools="$2"
      shift 2
      ;;
    --max-turns)
      max_turns="$2"
      shift 2
      ;;
    --mcp-config)
      mcp_config="$2"
      shift 2
      ;;
    --strict-mcp-config)
      strict_mcp=1
      shift
      ;;
    --disable-slash-commands)
      slash_disabled=1
      shift
      ;;
    --no-session-persistence)
      no_session_persistence=1
      shift
      ;;
    --exclude-dynamic-system-prompt-sections)
      exclude_dynamic=1
      shift
      ;;
    *)
      echo "arg=$1" >> "$log"
      shift
      ;;
  esac
done
stdin_prompt="$(cat)"

test "$format" = "json"
case "$schema" in
  *'"purpose"'*'"behavior"'*) ;;
  *) echo "schema missing summary fields" >&2; exit 41 ;;
esac
case "$stdin_prompt" in
  *"Summarize this function"*) ;;
  *) echo "missing prompt" >&2; exit 42 ;;
esac
case "$stdin_prompt" in
  *"Prompt contract: loomweave-agent-provider-v1"*"Do not inspect additional files"*) ;;
  *) echo "missing Loomweave agent prompt contract" >&2; exit 43 ;;
esac

echo "print_prompt=$print_prompt" >> "$log"
echo "model=$model" >> "$log"
echo "permission_mode=$permission_mode" >> "$log"
echo "tools=$tools" >> "$log"
echo "max_turns=$max_turns" >> "$log"
echo "mcp_config=$mcp_config" >> "$log"
echo "strict_mcp=$strict_mcp" >> "$log"
echo "slash_disabled=$slash_disabled" >> "$log"
echo "no_session_persistence=$no_session_persistence" >> "$log"
echo "exclude_dynamic=$exclude_dynamic" >> "$log"
printf '%s\n' '{{"type":"result","subtype":"success","structured_output":{{"purpose":"via claude","behavior":"ran fake CLI","relationships":"","risks":""}},"usage":{{"input_tokens":13,"cached_input_tokens":5,"output_tokens":6,"total_tokens":19}},"total_cost_usd":0.25}}'
"#,
            log = log_path.display()
        );
        fs::write(&fake_claude, script).expect("write fake claude");
        let mut permissions = fs::metadata(&fake_claude).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_claude, permissions).expect("chmod fake claude");

        let provider = ClaudeCliProvider::from_config(ClaudeCliProviderConfig {
            executable: fake_claude.display().to_string(),
            project_root,
            model_id: "claude-code-default".to_owned(),
            model: Some("claude-sonnet-4-6".to_owned()),
            permission_mode: "plan".to_owned(),
            tools: vec!["Read".to_owned(), "Grep".to_owned()],
            timeout_seconds: 5,
            max_turns: 2,
            no_session_persistence: true,
            exclude_dynamic_system_prompt_sections: true,
        })
        .expect("construct Claude CLI provider");

        let response = provider
            .invoke(LlmRequest {
                purpose: LlmPurpose::Summary,
                model_id: "claude-code-default".to_owned(),
                prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                prompt: "Summarize this function".to_owned(),
                max_output_tokens: 512,
            })
            .await
            .expect("invoke fake Claude CLI");

        assert_eq!(provider.name(), "claude_cli");
        assert_eq!(
            provider.tier_to_model("summary"),
            Some("claude-code-default")
        );
        assert_eq!(response.model_id, "claude-code-default");
        assert_eq!(response.input_tokens, 13);
        assert_eq!(response.cached_input_tokens, 5);
        assert_eq!(response.output_tokens, 6);
        assert_eq!(response.total_tokens, 19);
        assert!((response.cost_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(
            serde_json::from_str::<Value>(&response.output_json).expect("response JSON"),
            serde_json::json!({
                "purpose": "via claude",
                "behavior": "ran fake CLI",
                "relationships": "",
                "risks": ""
            })
        );

        let log = fs::read_to_string(log_path).expect("read fake claude log");
        assert!(log.contains("print_prompt=You are Loomweave's local Claude Code LLM provider"));
        assert!(log.contains("model=claude-sonnet-4-6"));
        assert!(log.contains("permission_mode=plan"));
        assert!(log.contains("tools=Read,Grep"));
        assert!(log.contains("max_turns=2"));
        assert!(log.contains(r#"mcp_config={"mcpServers":{}}"#));
        assert!(log.contains("strict_mcp=1"));
        assert!(log.contains("slash_disabled=1"));
        assert!(log.contains("no_session_persistence=1"));
        assert!(log.contains("exclude_dynamic=1"));
    }

    #[tokio::test]
    async fn claude_cli_provider_fallback_usage_counts_wrapped_prompt() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root).expect("project root");
        let fake_claude = temp.path().join("claude");
        let script = r#"#!/usr/bin/env bash
set -euo pipefail
while [[ $# -gt 0 ]]; do
  case "$1" in
    -p|--print|--output-format|--json-schema|--permission-mode|--max-turns|--mcp-config|--tools|--model)
      shift 2
      ;;
    --strict-mcp-config|--disable-slash-commands|--no-session-persistence|--exclude-dynamic-system-prompt-sections)
      shift
      ;;
    *)
      shift
      ;;
  esac
done
cat >/dev/null
printf '%s\n' '{"type":"result","subtype":"success","structured_output":{"purpose":"via claude","behavior":"ran fake CLI","relationships":"","risks":""},"total_cost_usd":0.0}'
"#;
        fs::write(&fake_claude, script).expect("write fake claude");
        let mut permissions = fs::metadata(&fake_claude).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_claude, permissions).expect("chmod fake claude");

        let provider = ClaudeCliProvider::from_config(ClaudeCliProviderConfig {
            executable: fake_claude.display().to_string(),
            project_root,
            model_id: "claude-code-default".to_owned(),
            model: None,
            permission_mode: "plan".to_owned(),
            tools: Vec::new(),
            timeout_seconds: 5,
            max_turns: 2,
            no_session_persistence: true,
            exclude_dynamic_system_prompt_sections: true,
        })
        .expect("construct Claude CLI provider");
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "claude-code-default".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "short raw prompt".to_owned(),
            max_output_tokens: 512,
        };
        let expected_input_tokens =
            estimate_text_tokens(&build_coding_agent_provider_prompt(&request));

        let response = provider
            .invoke(request)
            .await
            .expect("invoke fake Claude CLI");

        assert_eq!(response.input_tokens, expected_input_tokens);
    }

    #[tokio::test]
    async fn claude_cli_provider_passes_empty_tools_arg_when_no_tools_are_configured() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        fs::create_dir(&project_root).expect("project root");
        let fake_claude = temp.path().join("claude");
        let log_path = temp.path().join("claude.log");
        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
log="{log}"
saw_tools=0
tools_value="<unset>"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tools)
      saw_tools=1
      tools_value="${{2-<missing>}}"
      shift 2
      ;;
    -p|--print|--output-format|--json-schema|--permission-mode|--max-turns|--mcp-config)
      shift 2
      ;;
    --strict-mcp-config|--disable-slash-commands|--no-session-persistence|--exclude-dynamic-system-prompt-sections)
      shift
      ;;
    *)
      shift
      ;;
  esac
done
cat >/dev/null
echo "saw_tools=$saw_tools" >> "$log"
echo "tools_value=[$tools_value]" >> "$log"
printf '%s\n' '{{"type":"result","subtype":"success","structured_output":{{"purpose":"via claude","behavior":"ran fake CLI","relationships":"","risks":""}},"usage":{{"input_tokens":1,"output_tokens":1,"total_tokens":2}},"total_cost_usd":0.0}}'
"#,
            log = log_path.display()
        );
        fs::write(&fake_claude, script).expect("write fake claude");
        let mut permissions = fs::metadata(&fake_claude).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_claude, permissions).expect("chmod fake claude");

        let provider = ClaudeCliProvider::from_config(ClaudeCliProviderConfig {
            executable: fake_claude.display().to_string(),
            project_root,
            model_id: "claude-code-default".to_owned(),
            model: None,
            permission_mode: "plan".to_owned(),
            tools: Vec::new(),
            timeout_seconds: 5,
            max_turns: 2,
            no_session_persistence: true,
            exclude_dynamic_system_prompt_sections: true,
        })
        .expect("construct Claude CLI provider");

        provider
            .invoke(LlmRequest {
                purpose: LlmPurpose::Summary,
                model_id: "claude-code-default".to_owned(),
                prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                prompt: "Summarize this function".to_owned(),
                max_output_tokens: 512,
            })
            .await
            .expect("invoke fake Claude CLI");

        let log = fs::read_to_string(log_path).expect("read fake claude log");
        assert!(
            log.contains("saw_tools=1"),
            "Claude CLI must always receive --tools so the default no-tools posture is mechanically enforced (ADR-013). log: {log}"
        );
        assert!(
            log.contains("tools_value=[]"),
            "Empty tools list must be passed as an explicit empty --tools value, not omitted. log: {log}"
        );
    }

    #[test]
    fn claude_cli_output_parser_accepts_event_array_and_cache_reads() {
        let stdout = br#"[
          {"type":"system","subtype":"init"},
          {"type":"assistant","message":{"usage":{"input_tokens":4,"output_tokens":3}}},
          {"type":"result","subtype":"success","structured_output":{"purpose":"array","behavior":"ok","relationships":"","risks":""},"total_cost_usd":0.75,"usage":{"input_tokens":7,"cache_read_input_tokens":25,"output_tokens":11,"total_tokens":43}}
        ]"#;

        let parsed = parse_claude_cli_json_output(stdout).expect("parse Claude event array");

        assert_eq!(
            serde_json::from_str::<Value>(&parsed.output_json).expect("output json"),
            serde_json::json!({
                "purpose": "array",
                "behavior": "ok",
                "relationships": "",
                "risks": ""
            })
        );
        assert_eq!(parsed.usage.input_tokens, Some(7));
        assert_eq!(parsed.usage.cached_input_tokens, Some(25));
        assert_eq!(parsed.usage.output_tokens, Some(11));
        assert_eq!(parsed.usage.total_tokens, Some(43));
        assert_eq!(parsed.cost_usd, Some(0.75));
    }

    #[test]
    fn claude_cli_parser_rejects_empty_stdout_as_retryable_invalid_response() {
        let err = parse_claude_cli_json_output(b"")
            .expect_err("empty stdout must surface a typed InvalidResponse");
        match err {
            LlmProviderError::InvalidResponse { message, retryable } => {
                assert!(retryable, "empty stdout should be retryable (transient)");
                assert!(
                    message.contains("empty stdout"),
                    "error message must name the failure mode: {message}"
                );
            }
            other => panic!("expected InvalidResponse for empty stdout, got: {other:?}"),
        }
    }

    #[test]
    fn claude_cli_parser_rejects_non_json_stdout() {
        let err = parse_claude_cli_json_output(b"not json")
            .expect_err("non-JSON stdout must surface a typed InvalidResponse");
        match err {
            LlmProviderError::InvalidResponse { message, retryable } => {
                assert!(retryable);
                assert!(
                    message.contains("not JSON"),
                    "error message must reference JSON parse failure: {message}"
                );
            }
            other => panic!("expected InvalidResponse for non-JSON stdout, got: {other:?}"),
        }
    }

    #[test]
    fn claude_cli_parser_refuses_raw_stdout_when_no_structured_output_or_result_event() {
        // Single event with neither `type=result` nor any of the structured
        // output fields. Pre-fix, the parser fell back to the raw event
        // payload and downstream consumers persisted it as a summary.
        // Post-fix (clarion-55fc5aa885 §C3), this must be a typed
        // InvalidResponse rather than silent garbage.
        let stdout =
            br#"{"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let err = parse_claude_cli_json_output(stdout).expect_err(
            "stdout without a `result` event or `structured_output` field must \
             surface InvalidResponse, not be persisted as a summary",
        );
        match err {
            LlmProviderError::InvalidResponse { message, retryable } => {
                assert!(retryable);
                assert!(
                    message.contains("no `result` event"),
                    "error must explain the missing-structured-output failure: {message}"
                );
            }
            other => {
                panic!("expected InvalidResponse for missing structured output, got: {other:?}")
            }
        }
    }

    #[test]
    fn claude_cli_parser_accepts_result_event_with_string_result_payload() {
        // The other arm `result_event.get("result")` selects a `result`
        // event that has a JSON-string `result` field. Exercise it to pin
        // the contract.
        let stdout = br#"{"type":"result","result":"{\"purpose\":\"string\",\"behavior\":\"ok\",\"relationships\":\"\",\"risks\":\"\"}"}"#;
        let parsed = parse_claude_cli_json_output(stdout).expect("parse result event");
        assert_eq!(
            serde_json::from_str::<Value>(&parsed.output_json).expect("output json"),
            serde_json::json!({
                "purpose": "string",
                "behavior": "ok",
                "relationships": "",
                "risks": ""
            })
        );
    }

    #[test]
    fn cli_status_retryable_treats_signal_kill_as_retryable() {
        use std::os::unix::process::ExitStatusExt;
        // ExitStatus::from_raw with a non-exit signal code yields code()=None.
        let killed = ExitStatus::from_raw(9);
        assert!(
            cli_status_retryable(killed),
            "child killed by signal must be treated as retryable so the orchestrator can retry"
        );
        assert!(
            codex_status_retryable(killed),
            "codex retryable check must agree with the CLI floor"
        );
    }

    #[test]
    fn cli_status_retryable_treats_clean_nonzero_exit_as_non_retryable() {
        use std::os::unix::process::ExitStatusExt;
        // raw status 0x100 → exit code 1, code()=Some(1)
        let exit_one = ExitStatus::from_raw(0x100);
        assert!(
            !cli_status_retryable(exit_one),
            "clean process exit must be treated as non-retryable: the CLI \
             rejected the request deterministically"
        );
        assert!(!codex_status_retryable(exit_one));
    }

    #[test]
    fn provider_trait_exposes_wp6_methods() {
        fn assert_trait<T: LlmProvider>(_: &T) {}
        let provider = RecordingProvider::from_recordings(Vec::new());
        assert_trait(&provider);
        assert_eq!(provider.name(), "recording");
        assert_eq!(provider.estimate_tokens(&sample_request()), 0);
        assert_eq!(
            provider.caching_model(),
            CachingModel::OpenAiChatCompletions
        );
    }

    fn sample_request() -> LlmRequest {
        LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "summary".to_owned(),
            max_output_tokens: 512,
        }
    }

    async fn invoke_openrouter_once(
        raw_response: &'static str,
    ) -> Result<LlmResponse, LlmProviderError> {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(raw_response.as_bytes())
                .expect("write response");
        });
        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: format!("http://{addr}/api/v1"),
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave".to_owned(),
            timeout_seconds: 30,
        })
        .expect("test provider");
        let result = provider.invoke(sample_request()).await;
        handle.join().expect("server thread");
        result
    }
}
