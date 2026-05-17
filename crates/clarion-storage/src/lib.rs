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
pub mod schema;
pub mod writer;

pub use cache::{
    InferredEdgeCacheEntry, InferredEdgeCacheKey, SummaryCacheEntry, SummaryCacheKey,
    inferred_edge_cache_lookup, summary_cache_lookup, touch_inferred_edge_cache,
    touch_summary_cache, upsert_inferred_edge_cache, upsert_summary_cache,
};
pub use commands::{EdgeRecord, EntityRecord, RunStatus, WriterCmd};
pub use error::{Result, StorageError};
pub use query::{
    CallEdgeMatch, ContainedEntities, EntityRow, call_edges_from, call_edges_targeting,
    child_entity_ids, contained_entity_ids, entity_at_line, entity_by_id, find_entities,
    normalize_source_path,
};
pub use reader::ReaderPool;
pub use writer::{DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, Writer};
