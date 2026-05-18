use std::path::Path;

use anyhow::{Context, Result};
use clarion_scanner::{Baseline, BaselineError};
use serde_json::json;

use super::normalize_project_path;
use crate::secret_scan::findings::{
    FindingConfidenceBasis, FindingKind, FindingSeverity, PendingFinding,
};

const BASELINE_NO_JUSTIFICATION: &str = "CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION";
const BASELINE_MATCH: &str = "CLA-INFRA-SECRET-BASELINE-MATCH";

pub(super) fn load_for_scan(project_root: &Path) -> Result<(Baseline, Vec<PendingFinding>)> {
    let path = project_root.join(".clarion/secrets-baseline.yaml");
    match clarion_scanner::load_baseline(&path) {
        Ok(baseline) => Ok((baseline, Vec::new())),
        Err(BaselineError::MissingJustifications { entries }) => Ok((
            Baseline::empty(),
            entries
                .into_iter()
                .map(|entry| PendingFinding {
                    file_path: normalize_project_path(project_root, &entry.file),
                    rule_id: BASELINE_NO_JUSTIFICATION,
                    kind: FindingKind::Defect,
                    severity: FindingSeverity::Error,
                    confidence: Some(1.0),
                    confidence_basis: Some(FindingConfidenceBasis::BaselineSchema),
                    message: format!(
                        "Secret baseline entry missing justification at {}:{}",
                        entry.file.display(),
                        entry.line
                    ),
                    evidence: json!({"file_path": entry.file, "line_number": entry.line}),
                })
                .collect(),
        )),
        Err(err) => Err(err).context("load secret baseline"),
    }
}

pub(super) fn baseline_match_rule_id() -> &'static str {
    BASELINE_MATCH
}
