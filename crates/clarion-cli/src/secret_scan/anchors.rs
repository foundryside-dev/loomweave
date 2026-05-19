use std::{fs, path::Path};

use anyhow::{Context, Result};
use clarion_storage::{
    Writer,
    commands::{EntityRecord, WriterCmd},
};

use super::{SecretScanOutcome, canonical_or_original, display_relative};
use crate::secret_scan::findings::emit_findings;

pub(super) fn remember_finding_anchors(
    outcome: &mut SecretScanOutcome,
    entities: &[(String, EntityRecord)],
) {
    for (id, record) in entities {
        let Some(path) = record.source_file_path.as_deref() else {
            continue;
        };
        let key = canonical_or_original(Path::new(path));
        if record.kind == "module" || !outcome.finding_anchors.contains_key(&key) {
            outcome.finding_anchors.insert(key, id.clone());
        }
    }
}

pub(super) async fn ensure_and_emit_findings(
    outcome: &mut SecretScanOutcome,
    writer: &Writer,
    run_id: &str,
    project_root: &Path,
    started_at: &str,
) -> Result<()> {
    ensure_finding_anchors(outcome, writer, project_root, started_at).await?;
    emit_findings(
        writer,
        run_id,
        started_at,
        outcome,
        &outcome.finding_anchors,
    )
    .await
}

async fn ensure_finding_anchors(
    outcome: &mut SecretScanOutcome,
    writer: &Writer,
    project_root: &Path,
    started_at: &str,
) -> Result<()> {
    for file in outcome.finding_files() {
        let key = canonical_or_original(&file);
        if outcome.finding_anchors.contains_key(&key) {
            continue;
        }
        let id = secret_finding_anchor_id(project_root, &key);
        let relative = display_relative(project_root, &key);
        let short_name = key
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&relative)
            .to_owned();
        let mut properties = serde_json::Map::new();
        properties.insert("finding_anchor".to_owned(), serde_json::json!(true));
        if let Some(reason) = outcome.briefing_blocks.get(&key) {
            properties.insert(
                "briefing_blocked".to_owned(),
                serde_json::Value::String(reason.as_str().to_owned()),
            );
        }
        let record = EntityRecord {
            id: id.clone(),
            plugin_id: "core".to_owned(),
            kind: "file".to_owned(),
            name: relative,
            short_name,
            parent_id: None,
            source_file_id: None,
            source_file_path: Some(key.display().to_string()),
            source_byte_start: None,
            source_byte_end: None,
            source_line_start: None,
            source_line_end: None,
            properties_json: serde_json::Value::Object(properties).to_string(),
            content_hash: file_content_hash(&key),
            summary_json: None,
            wardline_json: None,
            first_seen_commit: None,
            last_seen_commit: None,
            created_at: started_at.to_owned(),
            updated_at: started_at.to_owned(),
        };
        writer
            .send_wait(|ack| WriterCmd::InsertEntity {
                entity: Box::new(record),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertEntity for secret finding anchor {id}"))?;
        outcome.finding_anchors.insert(key, id);
    }
    Ok(())
}

fn secret_finding_anchor_id(project_root: &Path, file: &Path) -> String {
    let relative = display_relative(project_root, file);
    let digest = blake3::hash(relative.as_bytes()).to_hex().to_string();
    format!("core:file:{digest}")
}

fn file_content_hash(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(blake3::hash(&bytes).to_hex().to_string()),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "secret-scan finding-anchor: content-hash read failed; entity briefing-block \
                 lookups may fail for this path because the finding anchor will land without a \
                 content_hash"
            );
            None
        }
    }
}
