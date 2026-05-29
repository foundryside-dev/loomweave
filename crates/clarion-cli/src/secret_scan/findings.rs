use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clarion_scanner::{Detection, SecretCategory};
use clarion_storage::{
    Writer,
    commands::{FindingRecord, WriterCmd},
};
use serde_json::json;

use super::SecretScanOutcome;

const SECRET_DETECTED: &str = "CLA-SEC-SECRET-DETECTED";

#[derive(Debug, Clone)]
pub(super) struct PendingFinding {
    pub(super) file_path: PathBuf,
    pub(super) rule_id: &'static str,
    pub(super) kind: FindingKind,
    pub(super) severity: FindingSeverity,
    pub(super) confidence: FindingConfidence,
    pub(super) message: String,
    pub(super) evidence: serde_json::Value,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindingKind {
    Defect,
    Fact,
    Classification,
    Metric,
    Suggestion,
}

impl FindingKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Defect => "defect",
            Self::Fact => "fact",
            Self::Classification => "classification",
            Self::Metric => "metric",
            Self::Suggestion => "suggestion",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindingSeverity {
    Info,
    Warn,
    Error,
    Critical,
    None,
}

impl FindingSeverity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Critical => "CRITICAL",
            Self::None => "NONE",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum FindingConfidence {
    ScoredPattern(f64),
    ScoredEntropy(f64),
    Baseline,
    OperatorOverride,
    Schema,
    Unknown,
}

impl FindingConfidence {
    fn value(self) -> Option<f64> {
        match self {
            Self::ScoredPattern(value) | Self::ScoredEntropy(value) => Some(value),
            Self::Baseline | Self::OperatorOverride | Self::Schema => Some(1.0),
            Self::Unknown => None,
        }
    }

    fn basis(self) -> Option<&'static str> {
        match self {
            Self::ScoredPattern(_) => Some("pattern"),
            Self::ScoredEntropy(_) => Some("entropy"),
            Self::Baseline => Some("baseline"),
            Self::OperatorOverride => Some("operator_override"),
            Self::Schema => Some("baseline_schema"),
            Self::Unknown => None,
        }
    }
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
        // Deterministic, run-scoped id so a `--resume` re-walk regenerates the
        // SAME id and `InsertFinding`'s upsert is idempotent (REQ-FINDING-05).
        // A random UUID would instead create a duplicate finding row on every
        // resume (the id never collides, so the upsert never fires). The digest
        // covers the anchor entity, rule, and evidence (file + line + hashed
        // secret), which uniquely identify a detection within a run.
        let discriminator = blake3::hash(
            format!(
                "{entity_id}\u{0}{}\u{0}{}",
                pending.rule_id, pending.evidence
            )
            .as_bytes(),
        )
        .to_hex();
        let finding_id = format!("core:finding:{run_id}:secret:{discriminator}");
        writer
            .send_wait(|ack| WriterCmd::InsertFinding {
                finding: Box::new(FindingRecord {
                    id: finding_id.clone(),
                    tool: "clarion".to_owned(),
                    tool_version: env!("CARGO_PKG_VERSION").to_owned(),
                    run_id: run_id.to_owned(),
                    rule_id: pending.rule_id.to_owned(),
                    kind: pending.kind.as_str().to_owned(),
                    severity: pending.severity.as_str().to_owned(),
                    confidence: pending.confidence.value(),
                    confidence_basis: pending.confidence.basis().map(str::to_owned),
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
        kind: FindingKind::Defect,
        severity: FindingSeverity::Error,
        confidence: if detection.category == SecretCategory::HighEntropy {
            FindingConfidence::ScoredEntropy(0.6)
        } else {
            FindingConfidence::ScoredPattern(1.0)
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
            "hashed_secret_hex": detection.hashed_secret.to_string(),
        }),
    }
}

fn finding_entity_id(file_path: &Path, anchors: &BTreeMap<PathBuf, String>) -> Option<String> {
    anchors.get(file_path).cloned()
}

#[cfg(test)]
mod tests {
    use super::{FindingConfidence, finding_entity_id, secret_detected_finding};
    use clarion_scanner::{DetectSecretsRule, Detection, HashedSecret, SecretCategory};
    use std::{collections::BTreeMap, path::PathBuf};

    #[test]
    fn finding_entity_id_requires_exact_anchor_path() {
        let mut anchors = BTreeMap::new();
        anchors.insert(
            PathBuf::from("/repo/vendor/lib/.env"),
            "core:file:vendor".to_owned(),
        );

        assert_eq!(
            finding_entity_id(PathBuf::from("lib/.env").as_path(), &anchors),
            None
        );
    }

    #[test]
    fn confidence_basis_uses_detection_category_not_rule_id_prefix() {
        let detection = Detection {
            rule_id: "HighEntropyNamedPattern",
            detect_secrets_type: DetectSecretsRule::AwsAccessKey,
            category: SecretCategory::CloudCredential,
            byte_offset: 0,
            line_number: 7,
            matched_len: 20,
            hashed_secret: HashedSecret::from_bytes([1u8; 20]),
        };

        let finding = secret_detected_finding(PathBuf::from("demo.sec").as_path(), &detection);

        assert_eq!(finding.confidence, FindingConfidence::ScoredPattern(1.0));
    }
}
