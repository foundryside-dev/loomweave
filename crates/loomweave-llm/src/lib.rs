//! loomweave-llm — LLM + embedding provider traits, concrete providers, and the
//! outbound HTTP/CLI transport for Loomweave summaries and embeddings.
//!
//! Extracted from `loomweave-core` (PRD-0001, clarion-141e9c08c8) so the
//! plugin-supervisor + SEI crate does not link an outbound HTTP client.

pub mod embedding_provider;
pub mod llm_provider;

pub use embedding_provider::{
    ApiEmbeddingProvider, ApiEmbeddingProviderConfig, EmbeddingProvider, EmbeddingProviderError,
    EmbeddingRecording, RecordingEmbeddingProvider,
};
pub use llm_provider::{
    CachingModel, ClaudeCliProvider, ClaudeCliProviderConfig, CodexCliProvider,
    CodexCliProviderConfig, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig,
    PromptTemplate, Recording, RecordingProvider, TrafficLoggingProvider,
    build_coding_agent_provider_prompt, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
