//! loomweave-core — domain types, identifiers, and provider traits.
//!
//! # Re-export policy (ticket clarion-29acbcd042)
//!
//! Only facade types that external callers need are re-exported at the crate
//! root. Implementation types (`Frame`, `TransportError`, `RequestEnvelope`, etc.)
//! remain accessible via `loomweave_core::plugin::transport::*` and siblings.

pub mod embedding_provider;
pub mod entity_id;
pub mod errors;
pub mod hardened_git;
pub mod llm_provider;
pub mod plugin;
pub mod store;

pub use embedding_provider::{
    ApiEmbeddingProvider, ApiEmbeddingProviderConfig, EmbeddingProvider, EmbeddingProviderError,
    EmbeddingRecording, RecordingEmbeddingProvider,
};
pub use entity_id::{EntityId, EntityIdError, entity_id};
pub use errors::{HttpErrorCode, McpErrorCode};
pub use hardened_git::{hardened_git_command, list_untracked_files};
pub use llm_provider::{
    CachingModel, ClaudeCliProvider, ClaudeCliProviderConfig, CodexCliProvider,
    CodexCliProviderConfig, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig,
    PromptTemplate, Recording, RecordingProvider, build_coding_agent_provider_prompt,
    build_inferred_calls_prompt, build_leaf_summary_prompt,
};
pub use plugin::{
    // host (Task 6) — facade for callers that spawn/connect plugins
    AcceptedEdge,
    AcceptedEntity,
    AnalyzeFileOutcome,
    AnalyzeFileStats,
    BriefingBlockReason,
    CapExceeded,
    // breaker (Task 7) — callers drive crash-loop accounting
    CrashLoopBreaker,
    CrashLoopState,
    // discovery (Task 5) — callers enumerate plugins
    DiscoveredPlugin,
    DiscoveryError,
    EdgeConfidence,
    FINDING_DISABLED_CRASH_LOOP,
    HostError,
    HostFinding,
    // jail / limits errors — callers may want to match on these
    JailError,
    // manifest (Task 1) — callers parse manifests from disk
    Manifest,
    ManifestError,
    OntologyEntityRole,
    PluginHost,
    UnresolvedCallSite,
    discover,
    parse_manifest,
};
