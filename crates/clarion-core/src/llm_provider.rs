//! LLM provider surface for WP6 and MCP on-demand tools.

use std::sync::Mutex;

use thiserror::Error;

pub const LEAF_SUMMARY_PROMPT_TEMPLATE_ID: &str = "leaf-v1";
pub const INFERRED_CALLS_PROMPT_VERSION: &str = "inferred-calls-v1";

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

    #[error("live Anthropic invocation is not available in this build")]
    LiveInvocationUnavailable,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicProvider {
    summary_model_id: String,
    inferred_edges_model_id: String,
}

impl AnthropicProvider {
    pub fn from_config(config: AnthropicProviderConfig) -> Result<Self, LlmProviderError> {
        if !config.allow_live_provider {
            return Err(LlmProviderError::LiveProviderNotAllowed);
        }
        if config
            .api_key
            .as_deref()
            .is_none_or(|key| key.trim().is_empty())
        {
            return Err(LlmProviderError::MissingApiKey);
        }
        Ok(Self {
            summary_model_id: config.summary_model_id,
            inferred_edges_model_id: config.inferred_edges_model_id,
        })
    }
}

impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn invoke(&self, _request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        Err(LlmProviderError::LiveInvocationUnavailable)
    }

    fn estimate_cost_usd(&self, _request: &LlmRequest) -> f64 {
        0.0
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
             Return JSON with an edges array containing target_id, confidence, and rationale.",
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
