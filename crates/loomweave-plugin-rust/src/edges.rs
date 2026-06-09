//! Wire-shaped non-structural edges (Phase 1b).
//!
//! Keeps edge-shaping out of `extract.rs`: the walk decides WHICH edges exist
//! and supplies the ids + spans; this module owns the JSON layout the host
//! deserialises into `RawEdge` (`crates/loomweave-core/src/plugin/host.rs`).
//!
//! **`imports` is anchored** (ADR-026 decision 3): unlike the structural
//! `contains` edge (NULL byte offsets), it carries the `use` statement's source
//! byte span (`source_byte_start`/`source_byte_end`) and so may NOT be
//! `inferred` confidence. The two resolving outcomes map to the two non-inferred
//! tiers: a uniquely-resolved path is `resolved`; a glob / multi-kind candidate
//! is `ambiguous` (`EdgeConfidence::Ambiguous`, accepted on anchored edges and
//! kept by default `confidence >= resolved` queries).
use serde_json::{Value, json};

use crate::spans::SourceRange;

/// An anchored `imports` edge from the `use`-bearing module entity to the
/// resolved target, carrying the `use` statement's byte span.
///
/// `confidence` is the resolver's outcome rendered as the wire string —
/// `"resolved"` for a unique in-project target, `"ambiguous"` for a glob /
/// multi-kind candidate. NEVER `"inferred"` (an anchored edge may not be).
#[must_use]
pub fn imports_edge(from_id: &str, to_id: &str, confidence: &str, span: &SourceRange) -> Value {
    json!({
        "kind": "imports",
        "from_id": from_id,
        "to_id": to_id,
        "source_byte_start": span.byte_start,
        "source_byte_end": span.byte_end,
        "confidence": confidence,
    })
}
