//! clarion-storage — `SQLite` layer, writer-actor, reader pool.
//!
//! All mutations route through the writer actor (a single `tokio::task`
//! owning the sole write `rusqlite::Connection`). Readers come from a
//! `deadpool-sqlite` pool. See ADR-011.

pub mod cache;
pub mod commands;
pub mod error;
pub mod pragma;
pub mod query;
pub mod reader;
pub mod retry;
pub mod schema;
pub mod unresolved;
pub mod wardline_taint;
pub mod writer;

pub use cache::{
    InferredEdgeCacheEntry, InferredEdgeCacheKey, SummaryCacheEntry, SummaryCacheKey,
    inferred_edge_cache_key_id, inferred_edge_cache_lookup, summary_cache_lookup,
    touch_inferred_edge_cache, touch_summary_cache, upsert_inferred_edge_cache,
    upsert_summary_cache,
};
pub use commands::{
    EdgeRecord, EntityRecord, FindingRecord, InferredCallEdgeRecord, InferredEdgeWriteStats,
    RunStatus, WriterCmd,
};
pub use error::{Result, StorageError};
pub use query::{
    CallEdgeMatch, CanonicalProjectPath, ContainedEntities, EntityRow, EntitySubsystem,
    FindingForEmitRow, ModuleDependencyEdge, ReferenceDirection, ReferenceEdgeMatch, ResolvedFile,
    ResolvedFileCatalogEntry, RolledUpReferenceEdge, SubsystemMember, UnresolvedCallSiteRow,
    ancestor_chain, call_edges_from, call_edges_targeting, candidate_entities_for_unresolved_sites,
    child_entity_ids, contained_entity_ids, containing_module_id, entities_containing_line,
    entity_at_line, entity_briefing_block_reason, entity_by_id, existing_entity_ids, find_entities,
    findings_for_emit, import_edges_for_entity, module_dependency_edges, module_reference_rollup,
    normalize_source_path, reference_edges_for_entity, resolve_file, resolve_file_catalog_entry,
    subsystem_for_member, subsystem_members, subsystem_of_entity, unresolved_call_sites_for_caller,
    unresolved_callers_for_target,
};
pub use reader::ReaderPool;
pub use retry::{RetryPolicy, begin_immediate};
pub use unresolved::{UnresolvedCallSiteRecord, replace_unresolved_call_sites_for_caller};
pub use wardline_taint::{
    Resolution, ResolutionConfidence, TaintFact, TaintFactRow, get_taint_facts,
    resolve_wardline_qualname, resolve_wardline_qualnames, upsert_taint_fact,
};
pub use writer::{
    DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, Writer, known_scan_time_edge_kinds,
};
