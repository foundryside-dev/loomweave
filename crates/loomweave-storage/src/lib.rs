//! loomweave-storage — `SQLite` layer, writer-actor, reader pool.
//!
//! All mutations route through the writer actor (a single `tokio::task`
//! owning the sole write `rusqlite::Connection`). Readers come from a
//! `deadpool-sqlite` pool. See ADR-011.

pub mod cache;
pub mod commands;
pub mod embeddings;
pub mod error;
pub mod findings;
pub mod glob;
pub mod guidance;
pub mod integrity;
pub mod pragma;
pub mod prior_index;
pub mod query;
pub mod reader;
pub mod retry;
pub mod runs;
pub mod schema;
pub mod sei;
pub mod unresolved;
pub mod wardline_taint;
pub mod writer;

pub use cache::{
    InferredEdgeCacheEntry, InferredEdgeCacheKey, SummaryCacheEntry, SummaryCacheKey,
    delete_summary_cache_for_entity, inferred_edge_cache_key_id, inferred_edge_cache_lookup,
    summary_cache_lookup, touch_inferred_edge_cache, touch_summary_cache,
    upsert_inferred_edge_cache, upsert_summary_cache,
};
pub use commands::{
    EdgeRecord, EntityRecord, FindingRecord, InferredCallEdgeRecord, InferredEdgeWriteStats,
    RunStatus, WriterCmd,
};
pub use embeddings::{EmbeddingKey, EmbeddingStore, StoredEmbedding, embeddings_db_path};
pub use error::{Result, StorageError};
pub use findings::sweep_stale_findings;
pub use glob::glob_match;
pub use guidance::{
    GUIDANCE_PROPOSAL_MARKER, GuidanceProposal, GuidanceSheet, GuidanceSheetInput, MatchFacts,
    PortableSheet, PromotedGuidanceSheet, RuleVerdict, delete_guidance_sheet, get_guidance_sheet,
    guidance_sheet_is_expired, guidance_sheet_is_stale, guidance_sheet_matches_entity,
    import_portable_sheet, insert_guidance_sheet, invalidate_summaries_for_sheet,
    list_guidance_sheets, rule_match, slugify_guidance_name, upsert_guidance_sheet,
};
pub use prior_index::{
    PluginIndexMarker, PriorIndexEntry, clear_prior_index, load_plugin_index_markers,
    load_prior_index, previously_analyzed_files, prior_locators_by_file, replace_prior_index,
    replace_prior_index_and_markers, upsert_plugin_index_marker, upsert_prior_index_entry,
};
pub use query::{
    CallEdgeMatch, CanonicalProjectPath, ContainedEntities, EntityRow, EntitySubsystem,
    EntityVisibility, FindingForEmitRow, LocatorCollision, ModuleDependencyEdge,
    PRE_INGEST_SECRET_SCAN_RULE_IDS, RELATION_EDGE_KINDS, ReferenceDirection, ReferenceEdgeMatch,
    RelationEdgeMatch, ResolvedFile, ResolvedFileCatalogEntry, RolledUpReferenceEdge,
    SubsystemMember, UnresolvedCallSiteRow, ancestor_chain, call_edges_from, call_edges_targeting,
    candidate_entities_for_unresolved_sites, child_entity_ids, contained_entity_ids,
    containing_module_id, current_file_hash, duplicate_locator_collision, edge_total,
    entities_by_churn, entities_by_kind, entities_by_tag, entities_containing_line,
    entities_for_churn_candidates, entities_targeted_by_unresolved_call_sites,
    entities_with_wardline_facts, entity_at_line, entity_briefing_block_reason, entity_by_id,
    entity_ids_in_namespace, entity_total, entity_visibility, existing_entity_ids, find_entities,
    findings_for_emit, import_edges_for_entity, known_entity_kinds, known_entity_tags,
    live_unresolved_call_sites_exist, module_dependency_edges, module_reference_rollup,
    normalize_source_path, preferred_finding_anchor_by_file, reference_edges_for_entity,
    relation_edges_for_entity, resolve_entity_ref, resolve_file, resolve_file_catalog_entry,
    stored_secret_finding_anchor_by_file, subsystem_for_member, subsystem_members,
    subsystem_of_entity, subsystem_total, tags_for_entity, unresolved_call_sites_for_caller,
    unresolved_caller_count_for_target, unresolved_callers_for_target,
};
pub use reader::ReaderPool;
pub use retry::{RetryPolicy, begin_immediate};
pub use runs::mark_stale_running_runs_failed;
pub use sei::{
    BindingStatus, GitRename, GitRenameSource, LineageEvent, NewEntityDescriptor, SEI_PREFIX,
    SeiBinding, SeiBindingRecord, SeiDecision, SeiLineageEntry, SeiLineageRow, SeiLookupResult,
    SeiRecord, alive_binding_for_locator, alive_bindings_snapshot, append_sei_lineage,
    has_any_alive_binding, is_reserved_sei, mint_sei, orphan_sei_binding, orphaned_bindings,
    prior_analyzed_commit, rebind_or_mint, resolve_locator, resolve_sei, sei_for_locator,
    sei_lineage, set_entity_signature, upsert_sei_binding,
};
pub use unresolved::{UnresolvedCallSiteRecord, replace_unresolved_call_sites_for_caller};
pub use wardline_taint::{
    Resolution, TaintFact, TaintFactRow, get_taint_facts, get_taint_facts_by_sei,
    resolve_qualnames_all_kinds, resolve_wardline_qualname, resolve_wardline_qualnames,
    resolve_wardline_qualnames_for_plugin, seis_for_locators, upsert_taint_fact,
};
pub use writer::{
    DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, Writer, known_scan_time_edge_kinds,
};
