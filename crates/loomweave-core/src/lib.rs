//! loomweave-core — domain types, identifiers, and the sandboxed plugin host.
//!
//! # Re-export policy (ticket clarion-29acbcd042)
//!
//! Only facade types that external callers need are re-exported at the crate
//! root. Implementation types (`Frame`, `TransportError`, `RequestEnvelope`, etc.)
//! remain accessible via `loomweave_core::plugin::transport::*` and siblings.

pub mod classifier_coverage;
pub mod dotenv;
pub mod entity_id;
pub mod errors;
pub mod hardened_git;
pub mod ontology;
pub mod plugin;
pub mod store;
pub mod worktree;

pub use classifier_coverage::{
    CLASSIFIER_COVERAGE_SCHEMA, ClassifierCoverage, ClassifierCoverageError,
    PluginClassifierCoverage, PluginClassifierCoverageInput, PluginCoverageStatus,
};
pub use entity_id::{EntityId, EntityIdError, entity_id};
pub use errors::{HttpErrorCode, McpErrorCode};
pub use hardened_git::{
    GIT_PROBE_STDERR_TAIL_BYTES, GitProbeError, GitProbeLimits, GitProbeOutput, TrackedState,
    hardened_git_command, list_untracked_files, operator_global_git_config_command, run_git_probe,
    run_git_probe_default, tracked_state,
};
pub use plugin::process_tree::{descendant_pids, kill_process_tree};
pub use plugin::{
    // host (Task 6) — facade for callers that spawn/connect plugins
    AcceptedEdge,
    AcceptedEntity,
    AnalyzeFileOutcome,
    AnalyzeFileStats,
    BriefingBlockReason,
    CapExceeded,
    CoverageStatus,
    // breaker (Task 7) — callers drive crash-loop accounting
    CrashLoopBreaker,
    CrashLoopState,
    // discovery (Task 5) — callers enumerate plugins
    DUPLICATE_LOCATOR_RULE_ID,
    DiscoveredPlugin,
    DiscoveryError,
    EdgeConfidence,
    FINDING_DISABLED_CRASH_LOOP,
    FacetCoverage,
    HostError,
    HostFinding,
    // jail / limits errors — callers may want to match on these
    JailError,
    // manifest (Task 1) — callers parse manifests from disk
    Manifest,
    ManifestError,
    OntologyEntityRole,
    PluginHost,
    ResolutionCoverage,
    UnresolvedCallSite,
    discover,
    parse_manifest,
    // interpreter (clarion-5cf9643de9) — `analyze` keys its incremental skip
    // on the resolver environment a language-server plugin resolved against.
    resolver_environment_for,
};
