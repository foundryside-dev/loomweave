//! LLM provider surface for WP6 and MCP on-demand tools.

use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

pub const LEAF_SUMMARY_PROMPT_TEMPLATE_ID: &str = "leaf-v1";
pub const INFERRED_CALLS_PROMPT_VERSION: &str = "inferred-calls-v1";
const ANTHROPIC_MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmPurpose {
    Summary,
    InferredEdges,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequest {
    pub purpose: LlmPurpose,
    pub model_id: String,
    pub prompt_id: String,
    pub prompt: String,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    pub model_id: String,
    pub output_json: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachingModel {
    AnthropicPromptCache,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LlmProviderError {
    #[error("recording fixture has no response for prompt {prompt_id:?} on model {model_id:?}")]
    MissingRecording { prompt_id: String, model_id: String },

    #[error("live Anthropic provider requires explicit opt-in")]
    LiveProviderNotAllowed,

    #[error("live Anthropic provider requires an API key")]
    MissingApiKey,

    #[error("live Anthropic HTTP request failed: {0}")]
    Http(String),

    #[error("live Anthropic returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("invalid live Anthropic response: {0}")]
    InvalidResponse(String),
}

pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError>;
    fn estimate_cost_usd(&self, request: &LlmRequest) -> f64;
    fn tier_to_model(&self, tier: &str) -> Option<&str>;
    fn caching_model(&self) -> CachingModel;
}

#[derive(Debug, Clone, PartialEq)]
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

    fn estimate_cost_usd(&self, _request: &LlmRequest) -> f64 {
        0.0
    }

    fn tier_to_model(&self, _tier: &str) -> Option<&str> {
        None
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::AnthropicPromptCache
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicProviderConfig {
    pub api_key: Option<String>,
    pub allow_live_provider: bool,
    pub summary_model_id: String,
    pub inferred_edges_model_id: String,
}

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    summary_model_id: String,
    inferred_edges_model_id: String,
    api_key: String,
    endpoint: String,
    client: reqwest::blocking::Client,
}

impl AnthropicProvider {
    pub fn from_config(config: AnthropicProviderConfig) -> Result<Self, LlmProviderError> {
        Self::from_config_with_endpoint(config, ANTHROPIC_MESSAGES_ENDPOINT.to_owned())
    }

    pub fn from_config_with_endpoint(
        config: AnthropicProviderConfig,
        endpoint: String,
    ) -> Result<Self, LlmProviderError> {
        if !config.allow_live_provider {
            return Err(LlmProviderError::LiveProviderNotAllowed);
        }
        let Some(api_key) = config.api_key.filter(|key| !key.trim().is_empty()) else {
            return Err(LlmProviderError::MissingApiKey);
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|err| LlmProviderError::Http(err.to_string()))?;
        Ok(Self {
            summary_model_id: config.summary_model_id,
            inferred_edges_model_id: config.inferred_edges_model_id,
            api_key,
            endpoint,
            client,
        })
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        let payload = serde_json::json!({
            "model": request.model_id,
            "max_tokens": request.max_output_tokens,
            "messages": [
                {
                    "role": "user",
                    "content": request.prompt
                }
            ]
        });
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", self.api_key.as_str())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .map_err(|err| LlmProviderError::Http(err.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .map_err(|err| LlmProviderError::Http(err.to_string()))?;
        if !status.is_success() {
            return Err(LlmProviderError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        let message: AnthropicMessageResponse = serde_json::from_str(&body)
            .map_err(|err| LlmProviderError::InvalidResponse(err.to_string()))?;
        let output_json = message.output_text()?;
        let cost_usd = cost_for_usage(
            &message.model,
            message.usage.input_tokens,
            message.usage.output_tokens,
        );
        Ok(LlmResponse {
            model_id: message.model,
            output_json,
            input_tokens: message.usage.input_tokens,
            output_tokens: message.usage.output_tokens,
            cost_usd,
        })
    }

    fn estimate_cost_usd(&self, request: &LlmRequest) -> f64 {
        let estimated_input_tokens = estimate_tokens(&request.prompt);
        cost_for_usage(
            &request.model_id,
            estimated_input_tokens,
            request.max_output_tokens,
        )
    }

    fn tier_to_model(&self, tier: &str) -> Option<&str> {
        match tier {
            "summary" => Some(self.summary_model_id.as_str()),
            "inferred_edges" => Some(self.inferred_edges_model_id.as_str()),
            _ => None,
        }
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::AnthropicPromptCache
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageResponse {
    model: String,
    content: Vec<AnthropicContentBlock>,
    usage: AnthropicUsage,
}

impl AnthropicMessageResponse {
    fn output_text(&self) -> Result<String, LlmProviderError> {
        let text = self
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("");
        if text.trim().is_empty() {
            return Err(LlmProviderError::InvalidResponse(
                "response contained no text blocks".to_owned(),
            ));
        }
        Ok(text)
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

fn estimate_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(4))
        .unwrap_or(u32::MAX)
        .max(1)
}

fn cost_for_usage(model_id: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (input_per_mtok, output_per_mtok) = model_pricing_usd_per_mtok(model_id);
    (f64::from(input_tokens) * input_per_mtok + f64::from(output_tokens) * output_per_mtok)
        / 1_000_000.0
}

fn model_pricing_usd_per_mtok(model_id: &str) -> (f64, f64) {
    let model = model_id.to_ascii_lowercase();
    if model.contains("haiku") {
        (1.0, 5.0)
    } else if model.contains("sonnet") {
        (3.0, 15.0)
    } else if model.contains("opus-4-1") || model.contains("opus-4-202") {
        (15.0, 75.0)
    } else if model.contains("opus") {
        (5.0, 25.0)
    } else {
        (3.0, 15.0)
    }
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
            model_id: "claude-haiku-4-5".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "summarise python:function:demo.hello".to_owned(),
            max_output_tokens: 512,
        };
        let response = LlmResponse {
            model_id: "claude-haiku-4-5".to_owned(),
            output_json: r#"{"purpose":"demo"}"#.to_owned(),
            input_tokens: 120,
            output_tokens: 24,
            cost_usd: 0.001,
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
    fn anthropic_provider_requires_explicit_live_opt_in_and_api_key() {
        let denied = AnthropicProvider::from_config(AnthropicProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: false,
            summary_model_id: "claude-haiku-4-5".to_owned(),
            inferred_edges_model_id: "claude-haiku-4-5".to_owned(),
        })
        .expect_err("api key alone must not enable live calls");
        assert!(matches!(denied, LlmProviderError::LiveProviderNotAllowed));

        let missing_key = AnthropicProvider::from_config(AnthropicProviderConfig {
            api_key: None,
            allow_live_provider: true,
            summary_model_id: "claude-haiku-4-5".to_owned(),
            inferred_edges_model_id: "claude-haiku-4-5".to_owned(),
        })
        .expect_err("live opt-in without key should fail");
        assert!(matches!(missing_key, LlmProviderError::MissingApiKey));

        let provider = AnthropicProvider::from_config(AnthropicProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            summary_model_id: "claude-haiku-4-5".to_owned(),
            inferred_edges_model_id: "claude-haiku-4-5".to_owned(),
        })
        .expect("live opt-in and key should construct provider");

        assert_eq!(provider.name(), "anthropic");
        assert_eq!(provider.tier_to_model("summary"), Some("claude-haiku-4-5"));
        assert_eq!(
            provider.tier_to_model("inferred_edges"),
            Some("claude-haiku-4-5")
        );
        assert_eq!(provider.caching_model(), CachingModel::AnthropicPromptCache);
    }

    #[test]
    fn anthropic_provider_invokes_messages_api_and_prices_usage() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("POST /v1/messages HTTP/1.1"));
            assert!(request.contains("x-api-key: secret"));
            assert!(request.contains("anthropic-version: 2023-06-01"));
            assert!(request.contains(r#""model":"claude-haiku-4-5""#));
            assert!(request.contains(r#""max_tokens":512"#));
            assert!(request.contains("Summarize this function"));

            let body = r#"{
                "id": "msg_01",
                "type": "message",
                "role": "assistant",
                "model": "claude-haiku-4-5",
                "content": [
                    {"type": "text", "text": "{\"purpose\":\"demo\"}"}
                ],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1000, "output_tokens": 200}
            }"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let provider = AnthropicProvider::from_config_with_endpoint(
            AnthropicProviderConfig {
                api_key: Some("secret".to_owned()),
                allow_live_provider: true,
                summary_model_id: "claude-haiku-4-5".to_owned(),
                inferred_edges_model_id: "claude-haiku-4-5".to_owned(),
            },
            format!("http://{addr}/v1/messages"),
        )
        .expect("test provider");

        let response = provider
            .invoke(LlmRequest {
                purpose: LlmPurpose::Summary,
                model_id: "claude-haiku-4-5".to_owned(),
                prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                prompt: "Summarize this function".to_owned(),
                max_output_tokens: 512,
            })
            .expect("invoke mocked Anthropic");

        assert_eq!(response.output_json, r#"{"purpose":"demo"}"#);
        assert_eq!(response.input_tokens, 1000);
        assert_eq!(response.output_tokens, 200);
        assert!((response.cost_usd - 0.002).abs() < f64::EPSILON);
        handle.join().expect("server thread");
    }

    #[test]
    fn provider_trait_exposes_wp6_methods() {
        fn assert_trait<T: LlmProvider>(_: &T) {}
        let provider = RecordingProvider::from_recordings(Vec::new());
        assert_trait(&provider);
        assert_eq!(provider.name(), "recording");
        assert!((provider.estimate_cost_usd(&sample_request()) - 0.0).abs() < f64::EPSILON);
        assert_eq!(provider.caching_model(), CachingModel::AnthropicPromptCache);
    }

    fn sample_request() -> LlmRequest {
        LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: "claude-haiku-4-5".to_owned(),
            prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
            prompt: "summary".to_owned(),
            max_output_tokens: 512,
        }
    }
}
