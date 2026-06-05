//! Plugin-host facade.
//!
//! Submodules are added per WP2 task:
//!   - `manifest`   — Task 1: `plugin.toml` parser + validator (L5, ADR-021/ADR-022).
//!   - `protocol`   — Task 2: JSON-RPC 2.0 typed envelopes + param/result structs (L4).
//!   - `transport`  — Task 2: LSP-style Content-Length framing (L4).
//!   - `jail`       — Task 4: path-jail enforcement (ADR-021 §2a).
//!   - `limits`     — Task 4: core-enforced ceilings and circuit-breakers (ADR-021 §2b–§2d).
//!   - `discovery`  — Task 5: `$PATH` scanning for `loomweave-plugin-*` executables (L9, ADR-021 §L9).
//!   - `host`       — Task 6: plugin-host supervisor (ADR-021 §Layer 2, ADR-022, UQ-WP2-11).
//!   - `host_findings` — `HostFinding` subcodes and constructors used by `host`.
//!   - `breaker`    — Task 7: crash-loop breaker (ADR-002 + UQ-WP2-10).

pub mod breaker;
pub mod discovery;
pub mod host;
mod host_findings;
pub mod jail;
pub mod limits;
pub mod manifest;
#[cfg(test)]
pub(crate) mod mock;
pub mod protocol;
pub mod transport;

pub use breaker::{CrashLoopBreaker, CrashLoopState, FINDING_DISABLED_CRASH_LOOP};
pub use discovery::{DiscoveredPlugin, DiscoveryError, discover, discover_on_path};
pub use host::{
    AcceptedEdge, AcceptedEntity, AnalyzeFileOutcome, BriefingBlockReason, HostError, HostFinding,
    PluginHost, RawEdge, RawEntity,
};
pub use jail::{JailError, jail, jail_to_string};
pub use limits::{
    BreakerState, CapExceeded, ContentLengthCeiling, DEFAULT_MAX_NOFILE, DEFAULT_MAX_NPROC,
    DEFAULT_MAX_RSS_MIB, EntityCountCap, FINDING_DISABLED_PATH_ESCAPE, FINDING_ENTITY_CAP,
    FINDING_FRAME_OVERSIZE, FINDING_OOM_KILLED, FINDING_PATH_ESCAPE, PathEscapeBreaker,
    apply_prlimit_as, apply_prlimit_nofile_nproc, effective_rss_mib,
};
pub use manifest::{Manifest, ManifestError, OntologyEntityRole, parse_manifest};
// `make_notification` and `make_request` are intentionally omitted —
// they're `pub(crate)` because they panic on serde failure for a
// property (well-formed param types) that external callers cannot
// guarantee. External consumers should build envelopes directly and
// handle the serde error.
pub use protocol::{
    AnalyzeFileFinding, AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, EdgeConfidence,
    ExitNotification, InitializeParams, InitializeResult, InitializedNotification, JsonRpcVersion,
    NotificationEnvelope, ProtocolError, RequestEnvelope, ResponseEnvelope, ResponsePayload,
    ShutdownParams, ShutdownResult, UnresolvedCallSite,
};
pub use transport::{Frame, TransportError, read_frame, write_frame};
