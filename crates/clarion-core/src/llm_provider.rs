//! LLM provider surface for WP6 and MCP on-demand tools.

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const LEAF_SUMMARY_PROMPT_TEMPLATE_ID: &str = "leaf-v1";
pub const INFERRED_CALLS_PROMPT_VERSION: &str = "inferred-calls-v1";

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

    #[error("live OpenRouter provider requires explicit opt-in")]
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

    #[error("invalid live OpenRouter response: {message}")]
    InvalidResponse { message: String, retryable: bool },
}

impl LlmProviderError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::MissingRecording { .. } | Self::LiveProviderNotAllowed | Self::MissingApiKey => {
                false
            }
            Self::Http { retryable, .. }
            | Self::Provider { retryable, .. }
            | Self::InvalidResponse { retryable, .. } => *retryable,
        }
    }
}

pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError>;
    fn estimate_tokens(&self, request: &LlmRequest) -> u64;
    fn tier_to_model(&self, tier: &str) -> Option<&str>;
    fn caching_model(&self) -> CachingModel;
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

impl LlmProvider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterProviderConfig {
    pub api_key: Option<String>,
    pub allow_live_provider: bool,
    pub model_id: String,
    pub endpoint_url: String,
    pub referer: String,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct OpenRouterProvider {
    model_id: String,
    api_key: String,
    endpoint_url: String,
    referer: String,
    title: String,
    client: reqwest::blocking::Client,
}

impl OpenRouterProvider {
    pub fn from_config(config: OpenRouterProviderConfig) -> Result<Self, LlmProviderError> {
        if !config.allow_live_provider {
            return Err(LlmProviderError::LiveProviderNotAllowed);
        }
        let Some(api_key) = config.api_key.filter(|key| !key.trim().is_empty()) else {
            return Err(LlmProviderError::MissingApiKey);
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|err| LlmProviderError::Http {
                message: err.to_string(),
                retryable: false,
            })?;
        Ok(Self {
            model_id: config.model_id,
            api_key,
            endpoint_url: config.endpoint_url,
            referer: config.referer,
            title: config.title,
            client,
        })
    }

    fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.endpoint_url.trim_end_matches('/')
        )
    }
}

impl LlmProvider for OpenRouterProvider {
    fn name(&self) -> &'static str {
        "openrouter"
    }

    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        let payload = serde_json::json!({
            "model": request.model_id,
            "max_completion_tokens": request.max_output_tokens,
            "temperature": 0,
            "messages": [
                {
                    "role": "user",
                    "content": request.prompt
                }
            ]
        });
        let response = self
            .client
            .post(self.chat_completions_url())
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("HTTP-Referer", self.referer.as_str())
            .header("X-OpenRouter-Title", self.title.as_str())
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .map_err(|err| LlmProviderError::Http {
                message: err.to_string(),
                retryable: true,
            })?;
        let status = response.status();
        let retry_after_seconds = retry_after_seconds(response.headers());
        let body = response.text().map_err(|err| LlmProviderError::Http {
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
            output_tokens: usage.completion,
            total_tokens: usage.total,
            cost_usd: 0.0,
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
    pub source_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredCallsPromptInput {
    pub caller_entity_id: String,
    pub caller_source_excerpt: String,
    pub unresolved_call_sites_json: String,
    pub candidate_entities_json: String,
}

pub fn build_leaf_summary_prompt(input: &LeafSummaryPromptInput) -> PromptTemplate {
    PromptTemplate {
        id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID,
        body: format!(
            "You are summarising one Clarion entity at leaf scope only.\n\
             Entity id: {entity_id}\n\
             Kind: {kind}\n\
             Name: {name}\n\
             Source excerpt:\n{source}\n\
             Return JSON with purpose, behavior, relationships, and risks fields.",
            entity_id = input.entity_id,
            kind = input.kind,
            name = input.name,
            source = input.source_excerpt,
        ),
    }
}

pub fn build_inferred_calls_prompt(input: &InferredCallsPromptInput) -> PromptTemplate {
    PromptTemplate {
        id: INFERRED_CALLS_PROMPT_VERSION,
        body: format!(
            "You are resolving unresolved Clarion call sites for one caller.\n\
             Caller entity id: {caller}\n\
             Caller source excerpt:\n{source}\n\
             Unresolved call sites JSON:\n{sites}\n\
             Candidate entities JSON:\n{candidates}\n\
             Return JSON with an edges array containing site_key, target_id, confidence, and rationale.",
            caller = input.caller_entity_id,
            source = input.caller_source_excerpt,
            sites = input.unresolved_call_sites_json,
            candidates = input.candidate_entities_json,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_provider_replays_exact_request_shape() {
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
            output_tokens: 24,
            total_tokens: 144,
            cost_usd: 0.0,
        };
        let provider = RecordingProvider::from_recordings(vec![Recording {
            request: request.clone(),
            response: response.clone(),
        }]);

        assert_eq!(provider.invoke(request.clone()).unwrap(), response);
        assert_eq!(provider.invocations(), vec![request.clone()]);

        let missing = provider
            .invoke(LlmRequest {
                prompt: "changed".to_owned(),
                ..request
            })
            .expect_err("request-shape drift should miss the recording");
        assert!(matches!(missing, LlmProviderError::MissingRecording { .. }));
    }

    #[test]
    fn prompt_templates_have_stable_versions_and_embed_context() {
        let summary = build_leaf_summary_prompt(&LeafSummaryPromptInput {
            entity_id: "python:function:demo.hello".to_owned(),
            kind: "function".to_owned(),
            name: "demo.hello".to_owned(),
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
        });
        assert_eq!(inferred.id, INFERRED_CALLS_PROMPT_VERSION);
        assert!(inferred.body.contains("python:function:demo.via_dispatch"));
        assert!(inferred.body.contains("Return JSON"));
    }

    #[test]
    fn openrouter_provider_requires_explicit_live_opt_in_and_api_key() {
        let denied = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: false,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
        })
        .expect_err("api key alone must not enable live calls");
        assert!(matches!(denied, LlmProviderError::LiveProviderNotAllowed));

        let missing_key = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: None,
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
        })
        .expect_err("live opt-in without key should fail");
        assert!(matches!(missing_key, LlmProviderError::MissingApiKey));

        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: "https://openrouter.ai/api/v1".to_owned(),
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
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
    }

    #[test]
    fn openrouter_provider_invokes_chat_completions_and_extracts_usage_tokens() {
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
            assert!(request.contains("http-referer: https://github.com/qacona/clarion"));
            assert!(request.contains("x-openrouter-title: Clarion"));
            assert!(request.contains(r#""model":"anthropic/claude-sonnet-4.6""#));
            assert!(request.contains(r#""max_completion_tokens":512"#));
            assert!(request.contains("Summarize this function"));

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
                "usage": {"prompt_tokens": 1000, "completion_tokens": 200, "total_tokens": 1200}
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
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
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
            .expect("invoke mocked OpenRouter");

        assert_eq!(response.output_json, r#"{"purpose":"demo"}"#);
        assert_eq!(response.input_tokens, 1000);
        assert_eq!(response.output_tokens, 200);
        assert_eq!(response.total_tokens, 1200);
        assert!((response.cost_usd - 0.0).abs() < f64::EPSILON);
        handle.join().expect("server thread");
    }

    #[test]
    fn openrouter_provider_unwraps_error_envelope_with_retryability() {
        let auth_error = invoke_openrouter_once(
            "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":{\"code\":401,\"message\":\"Invalid credentials\",\"metadata\":{}}}",
        )
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

    #[test]
    fn openrouter_provider_unwraps_choice_level_error() {
        let err = invoke_openrouter_once(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"id\":\"gen-01\",\"object\":\"chat.completion\",\"created\":1779000000,\"model\":\"anthropic/claude-sonnet-4.6\",\"choices\":[{\"finish_reason\":\"error\",\"native_finish_reason\":\"error\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"error\":{\"code\":502,\"message\":\"Provider disconnected\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":0,\"total_tokens\":1}}",
        )
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

    #[test]
    fn openrouter_provider_connection_error_is_retryable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind unused port");
        let addr = listener.local_addr().expect("unused port addr");
        drop(listener);
        let provider = OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: format!("http://{addr}/api/v1"),
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
        })
        .expect("test provider");

        let err = provider
            .invoke(sample_request())
            .expect_err("connection refused should be retryable");
        assert!(matches!(
            err,
            LlmProviderError::Http {
                retryable: true,
                ..
            }
        ));
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

    fn invoke_openrouter_once(raw_response: &'static str) -> Result<LlmResponse, LlmProviderError> {
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
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion".to_owned(),
        })
        .expect("test provider");
        let result = provider.invoke(sample_request());
        handle.join().expect("server thread");
        result
    }
}
