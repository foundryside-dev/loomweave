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
use uuid::Uuid;

use super::SecretScanOutcome;

const SECRET_DETECTED: &str = "CLA-SEC-SECRET-DETECTED";

#[derive(Debug, Clone)]
pub(super) struct PendingFinding {
    pub(super) file_path: PathBuf,
    pub(super) rule_id: &'static str,
    pub(super) kind: FindingKind,
    pub(super) severity: FindingSeverity,
    pub(super) confidence: Option<f64>,
    pub(super) confidence_basis: Option<FindingConfidenceBasis>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindingConfidenceBasis {
    Pattern,
    Entropy,
    Baseline,
    BaselineSchema,
    OperatorOverride,
}

impl FindingConfidenceBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pattern => "pattern",
            Self::Entropy => "entropy",
            Self::Baseline => "baseline",
            Self::BaselineSchema => "baseline_schema",
            Self::OperatorOverride => "operator_override",
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
        let finding_id = Uuid::new_v4().to_string();
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
                    confidence: pending.confidence,
                    confidence_basis: pending
                        .confidence_basis
                        .map(FindingConfidenceBasis::as_str)
                        .map(str::to_owned),
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
            Some(0.6)
        } else {
            Some(1.0)
        },
        confidence_basis: if detection.category == SecretCategory::HighEntropy {
            Some(FindingConfidenceBasis::Entropy)
        } else {
            Some(FindingConfidenceBasis::Pattern)
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
    use super::{FindingConfidenceBasis, finding_entity_id, secret_detected_finding};
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

        assert_eq!(finding.confidence, Some(1.0));
        assert_eq!(
            finding.confidence_basis,
            Some(FindingConfidenceBasis::Pattern)
        );
    }
}
