//! Cache helpers for MCP LLM-backed tools.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, StorageError};

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryCacheKey {
    pub entity_id: String,
    pub content_hash: String,
    pub prompt_template_id: String,
    pub model_tier: String,
    pub guidance_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryCacheEntry {
    pub key: SummaryCacheKey,
    pub summary_json: String,
    pub cost_usd: f64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub caller_count: i64,
    pub fan_out: i64,
    pub stale_semantic: bool,
    pub created_at: String,
    pub last_accessed_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InferredEdgeCacheKey {
    pub caller_entity_id: String,
    pub caller_content_hash: String,
    pub model_id: String,
    pub prompt_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InferredEdgeCacheEntry {
    pub key: InferredEdgeCacheKey,
    pub result_json: String,
    pub cost_usd: f64,
    pub token_count: i64,
    pub created_at: String,
    pub last_accessed_at: String,
}

pub fn upsert_summary_cache(conn: &Connection, entry: &SummaryCacheEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO summary_cache ( \
            entity_id, content_hash, prompt_template_id, model_tier, \
            guidance_fingerprint, summary_json, cost_usd, tokens_input, \
            tokens_output, created_at, last_accessed_at, caller_count, \
            fan_out, stale_semantic \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint) \
         DO UPDATE SET \
            summary_json = excluded.summary_json, \
            cost_usd = excluded.cost_usd, \
            tokens_input = excluded.tokens_input, \
            tokens_output = excluded.tokens_output, \
            created_at = excluded.created_at, \
            last_accessed_at = excluded.last_accessed_at, \
            caller_count = excluded.caller_count, \
            fan_out = excluded.fan_out, \
            stale_semantic = excluded.stale_semantic",
        params![
            entry.key.entity_id,
            entry.key.content_hash,
            entry.key.prompt_template_id,
            entry.key.model_tier,
            entry.key.guidance_fingerprint,
            entry.summary_json,
            entry.cost_usd,
            entry.tokens_input,
            entry.tokens_output,
            entry.created_at,
            entry.last_accessed_at,
            entry.caller_count,
            entry.fan_out,
            bool_as_i64(entry.stale_semantic),
        ],
    )?;
    Ok(())
}

pub fn summary_cache_lookup(
    conn: &Connection,
    key: &SummaryCacheKey,
) -> Result<Option<SummaryCacheEntry>> {
    conn.query_row(
        "SELECT entity_id, content_hash, prompt_template_id, model_tier, \
                guidance_fingerprint, summary_json, cost_usd, tokens_input, \
                tokens_output, created_at, last_accessed_at, caller_count, \
                fan_out, stale_semantic \
         FROM summary_cache \
         WHERE entity_id = ?1 AND content_hash = ?2 AND prompt_template_id = ?3 \
           AND model_tier = ?4 AND guidance_fingerprint = ?5",
        params![
            key.entity_id,
            key.content_hash,
            key.prompt_template_id,
            key.model_tier,
            key.guidance_fingerprint,
        ],
        map_summary_cache_entry,
    )
    .optional()
    .map_err(StorageError::from)
}

pub fn touch_summary_cache(
    conn: &Connection,
    key: &SummaryCacheKey,
    last_accessed_at: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE summary_cache \
         SET last_accessed_at = ?6 \
         WHERE entity_id = ?1 AND content_hash = ?2 AND prompt_template_id = ?3 \
           AND model_tier = ?4 AND guidance_fingerprint = ?5",
        params![
            key.entity_id,
            key.content_hash,
            key.prompt_template_id,
            key.model_tier,
            key.guidance_fingerprint,
            last_accessed_at,
        ],
    )?;
    Ok(changed > 0)
}

pub fn upsert_inferred_edge_cache(conn: &Connection, entry: &InferredEdgeCacheEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO inferred_edge_cache ( \
            caller_entity_id, caller_content_hash, model_id, prompt_version, \
            result_json, cost_usd, token_count, created_at, last_accessed_at \
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(caller_entity_id, caller_content_hash, model_id, prompt_version) \
         DO UPDATE SET \
            result_json = excluded.result_json, \
            cost_usd = excluded.cost_usd, \
            token_count = excluded.token_count, \
            created_at = excluded.created_at, \
            last_accessed_at = excluded.last_accessed_at",
        params![
            entry.key.caller_entity_id,
            entry.key.caller_content_hash,
            entry.key.model_id,
            entry.key.prompt_version,
            entry.result_json,
            entry.cost_usd,
            entry.token_count,
            entry.created_at,
            entry.last_accessed_at,
        ],
    )?;
    Ok(())
}

pub fn inferred_edge_cache_lookup(
    conn: &Connection,
    key: &InferredEdgeCacheKey,
) -> Result<Option<InferredEdgeCacheEntry>> {
    conn.query_row(
        "SELECT caller_entity_id, caller_content_hash, model_id, prompt_version, \
                result_json, cost_usd, token_count, created_at, last_accessed_at \
         FROM inferred_edge_cache \
         WHERE caller_entity_id = ?1 AND caller_content_hash = ?2 \
           AND model_id = ?3 AND prompt_version = ?4",
        params![
            key.caller_entity_id,
            key.caller_content_hash,
            key.model_id,
            key.prompt_version,
        ],
        map_inferred_edge_cache_entry,
    )
    .optional()
    .map_err(StorageError::from)
}

pub fn touch_inferred_edge_cache(
    conn: &Connection,
    key: &InferredEdgeCacheKey,
    last_accessed_at: &str,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE inferred_edge_cache \
         SET last_accessed_at = ?5 \
         WHERE caller_entity_id = ?1 AND caller_content_hash = ?2 \
           AND model_id = ?3 AND prompt_version = ?4",
        params![
            key.caller_entity_id,
            key.caller_content_hash,
            key.model_id,
            key.prompt_version,
            last_accessed_at,
        ],
    )?;
    Ok(changed > 0)
}

fn map_summary_cache_entry(row: &Row<'_>) -> rusqlite::Result<SummaryCacheEntry> {
    Ok(SummaryCacheEntry {
        key: SummaryCacheKey {
            entity_id: row.get(0)?,
            content_hash: row.get(1)?,
            prompt_template_id: row.get(2)?,
            model_tier: row.get(3)?,
            guidance_fingerprint: row.get(4)?,
        },
        summary_json: row.get(5)?,
        cost_usd: row.get(6)?,
        tokens_input: row.get(7)?,
        tokens_output: row.get(8)?,
        created_at: row.get(9)?,
        last_accessed_at: row.get(10)?,
        caller_count: row.get(11)?,
        fan_out: row.get(12)?,
        stale_semantic: row.get::<_, i64>(13)? != 0,
    })
}

fn map_inferred_edge_cache_entry(row: &Row<'_>) -> rusqlite::Result<InferredEdgeCacheEntry> {
    Ok(InferredEdgeCacheEntry {
        key: InferredEdgeCacheKey {
            caller_entity_id: row.get(0)?,
            caller_content_hash: row.get(1)?,
            model_id: row.get(2)?,
            prompt_version: row.get(3)?,
        },
        result_json: row.get(4)?,
        cost_usd: row.get(5)?,
        token_count: row.get(6)?,
        created_at: row.get(7)?,
        last_accessed_at: row.get(8)?,
    })
}

fn bool_as_i64(value: bool) -> i64 {
    i64::from(value)
}
