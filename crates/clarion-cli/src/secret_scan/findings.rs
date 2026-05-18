use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clarion_scanner::Detection;
use clarion_storage::{
    Writer,
    commands::{FindingRecord, WriterCmd},
};
use serde_json::json;
use uuid::Uuid;

use super::SecretScanOutcome;

const SECRET_DETECTED: &str = "CLA-SEC-SECRET-DETECTED";

#[derive(Debug, Clone)]
pub(super) struct PendingFinding {
    pub(super) file_path: PathBuf,
    pub(super) rule_id: &'static str,
    pub(super) kind: &'static str,
    pub(super) severity: &'static str,
    pub(super) confidence: Option<f64>,
    pub(super) confidence_basis: Option<&'static str>,
    pub(super) message: String,
    pub(super) evidence: serde_json::Value,
}

pub(crate) async fn emit_findings(
    writer: &Writer,
    run_id: &str,
    started_at: &str,
    outcome: &SecretScanOutcome,
    entity_anchors: &BTreeMap<PathBuf, String>,
) -> Result<()> {
    for pending in &outcome.findings {
        let entity_id =
            finding_entity_id(&pending.file_path, entity_anchors).with_context(|| {
                format!("anchor secret finding for {}", pending.file_path.display())
            })?;
        let finding_id = Uuid::new_v4().to_string();
        writer
            .send_wait(|ack| WriterCmd::InsertFinding {
                finding: Box::new(FindingRecord {
                    id: finding_id.clone(),
                    tool: "clarion".to_owned(),
                    tool_version: env!("CARGO_PKG_VERSION").to_owned(),
                    run_id: run_id.to_owned(),
                    rule_id: pending.rule_id.to_owned(),
                    kind: pending.kind.to_owned(),
                    severity: pending.severity.to_owned(),
                    confidence: pending.confidence,
                    confidence_basis: pending.confidence_basis.map(str::to_owned),
                    entity_id,
                    related_entities_json: "[]".to_owned(),
                    message: pending.message.clone(),
                    evidence_json: pending.evidence.to_string(),
                    properties_json: "{}".to_owned(),
                    supports_json: "[]".to_owned(),
                    supported_by_json: "[]".to_owned(),
                    created_at: started_at.to_owned(),
                    updated_at: started_at.to_owned(),
                }),
                ack,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("InsertFinding {finding_id}"))?;
    }
    Ok(())
}

pub(super) fn secret_detected_finding(file: &Path, detection: &Detection) -> PendingFinding {
    PendingFinding {
        file_path: file.to_path_buf(),
        rule_id: SECRET_DETECTED,
        kind: "defect",
        severity: "ERROR",
        confidence: if detection.rule_id.starts_with("HighEntropy") {
            Some(0.6)
        } else {
            Some(1.0)
        },
        confidence_basis: if detection.rule_id.starts_with("HighEntropy") {
            Some("entropy")
        } else {
            Some("pattern")
        },
        message: format!(
            "{} detected in {}:{}",
            detection.rule_id,
            file.display(),
            detection.line_number
        ),
        evidence: json!({
            "file_path": file,
            "line_number": detection.line_number,
            "rule": detection.rule_id,
            "hashed_secret_hex": hex20(detection.hashed_secret),
        }),
    }
}

fn finding_entity_id(file_path: &Path, anchors: &BTreeMap<PathBuf, String>) -> Option<String> {
    anchors.get(file_path).cloned().or_else(|| {
        anchors
            .iter()
            .find(|(candidate, _)| candidate.ends_with(file_path))
            .map(|(_, entity_id)| entity_id.clone())
    })
}

fn hex20(bytes: [u8; 20]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(40);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}
