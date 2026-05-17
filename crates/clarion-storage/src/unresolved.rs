//! Unresolved call-site storage helpers for inferred MCP dispatch.

use rusqlite::{Connection, params};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedCallSiteRecord {
    pub caller_entity_id: String,
    pub caller_content_hash: String,
    pub site_key: String,
    pub site_ordinal: i64,
    pub source_file_id: Option<String>,
    pub source_byte_start: i64,
    pub source_byte_end: i64,
    pub callee_expr: String,
    pub created_at: String,
}

pub fn replace_unresolved_call_sites_for_caller(
    conn: &Connection,
    caller_entity_id: &str,
    caller_content_hash: &str,
    sites: &[UnresolvedCallSiteRecord],
) -> Result<()> {
    conn.execute(
        "DELETE FROM entity_unresolved_call_sites WHERE caller_entity_id = ?1",
        params![caller_entity_id],
    )?;
    for site in sites {
        conn.execute(
            "INSERT INTO entity_unresolved_call_sites ( \
                caller_entity_id, caller_content_hash, site_key, site_ordinal, \
                source_file_id, source_byte_start, source_byte_end, callee_expr, created_at \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                caller_entity_id,
                caller_content_hash,
                site.site_key,
                site.site_ordinal,
                site.source_file_id,
                site.source_byte_start,
                site.source_byte_end,
                site.callee_expr,
                site.created_at,
            ],
        )?;
    }
    Ok(())
}
