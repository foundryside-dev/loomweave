//! Pre-ingest secret scanning for `clarion analyze`.
//!
//! Exit codes used by this module:
//! - 0: analysis may continue, with or without an explicit secret override.
//! - 1: ordinary hard failure reported through the caller's `anyhow::Result`.
//! - 78 (`EX_CONFIG`): `--allow-unredacted-secrets` was misconfigured or not
//!   confirmed; the caller must abort before `BeginRun`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

mod anchors;
mod baseline;
mod files;
mod findings;

use anyhow::{Context, Result};
use clarion_core::BriefingBlockReason;
use clarion_scanner::{Detection, Scanner, SuppressionResult};
use clarion_storage::{Writer, commands::EntityRecord};
pub(crate) use files::collect_scan_files;
use findings::{
    FindingConfidenceBasis, FindingKind, FindingSeverity, PendingFinding, secret_detected_finding,
};
use serde_json::json;

const SECRET_OVERRIDE_ALLOWED: &str = "CLA-SEC-UNREDACTED-SECRETS-ALLOWED";
const OVERRIDE_UNCONFIRMED: &str = "CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED";
const CONFIRM_TOKEN: &str = "yes-i-understand";

#[derive(Debug, Clone, Default)]
pub(crate) struct SecretScanOptions {
    pub(crate) override_policy: OverridePolicy,
}

impl SecretScanOptions {
    pub(crate) fn from_cli(
        allow_unredacted_secrets: bool,
        confirm_allow_unredacted_secrets: Option<String>,
    ) -> std::result::Result<Self, OverrideConfirmationError> {
        let override_policy = match (allow_unredacted_secrets, confirm_allow_unredacted_secrets) {
            (false, None) => OverridePolicy::Forbid,
            (true, None) => OverridePolicy::RequireInteractive,
            (true, Some(token)) if token == CONFIRM_TOKEN => {
                OverridePolicy::Preconfirmed(ConfirmToken)
            }
            (true | false, Some(_)) => {
                return Err(OverrideConfirmationError);
            }
        };
        Ok(Self { override_policy })
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) enum OverridePolicy {
    #[default]
    Forbid,
    RequireInteractive,
    Preconfirmed(ConfirmToken),
}

#[derive(Debug, Clone)]
pub(crate) struct ConfirmToken;

#[derive(Debug, Clone)]
pub(crate) struct OverrideConfirmationError;

impl std::fmt::Display for OverrideConfirmationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{OVERRIDE_UNCONFIRMED}: --allow-unredacted-secrets requires confirmation token {CONFIRM_TOKEN:?}"
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SecretScanOutcome {
    pub(crate) briefing_blocks: BTreeMap<PathBuf, BriefingBlockReason>,
    findings: Vec<PendingFinding>,
    finding_anchors: BTreeMap<PathBuf, String>,
    override_files: Vec<PathBuf>,
    scanned_files: BTreeSet<PathBuf>,
}

impl SecretScanOutcome {
    pub(crate) fn scanned_files(&self) -> &BTreeSet<PathBuf> {
        &self.scanned_files
    }

    pub(crate) fn finding_files(&self) -> BTreeSet<PathBuf> {
        self.findings
            .iter()
            .map(|finding| finding.file_path.clone())
            .collect()
    }

    pub(crate) fn augment_stats(&self, stats: &mut serde_json::Value) {
        if self.override_files.is_empty() {
            return;
        }
        stats["secret_override_used"] = json!(true);
        stats["secret_override_files_affected"] = json!(
            self.override_files
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
        );
    }

    pub(crate) fn remember_finding_anchors(&mut self, entities: &[(String, EntityRecord)]) {
        anchors::remember_finding_anchors(self, entities);
    }

    pub(crate) async fn persist_findings(
        &mut self,
        writer: &Writer,
        run_id: &str,
        project_root: &Path,
        started_at: &str,
    ) -> Result<()> {
        anchors::ensure_and_emit_findings(self, writer, run_id, project_root, started_at).await
    }
}

pub(crate) fn pre_ingest(
    project_root: &Path,
    source_files: &[PathBuf],
    options: &SecretScanOptions,
) -> Result<SecretScanOutcome> {
    let project_root = project_root
        .canonicalize()
        .with_context(|| format!("canonicalize project root {}", project_root.display()))?;
    let project_root = project_root.as_path();
    let scanner = Scanner::new();
    let (baseline, mut findings) = baseline::load_for_scan(project_root)?;
    let mut per_file = Vec::new();
    let mut all_allowed = Vec::new();
    let mut baseline_matches = Vec::new();
    let mut scanned_files = BTreeSet::new();

    for file in source_files {
        let canonical_file = canonical_or_original(file);
        scanned_files.insert(canonical_file.clone());
        let buf = fs::read(file).with_context(|| format!("read {}", file.display()))?;
        let detections = scanner.scan_bytes(&buf);
        let baseline_file = project_relative_path(project_root, &canonical_file);
        let SuppressionResult {
            allowed,
            fired_entries,
            ..
        } = baseline.suppress(detections, &baseline_file);
        if !allowed.is_empty() {
            all_allowed.extend(
                allowed
                    .iter()
                    .cloned()
                    .map(|detection| (canonical_file.clone(), detection)),
            );
        }
        baseline_matches.extend(fired_entries.into_iter().map(|entry| PendingFinding {
            file_path: normalize_project_path(project_root, &entry.file_path),
            rule_id: baseline::baseline_match_rule_id(),
            kind: FindingKind::Fact,
            severity: FindingSeverity::Info,
            confidence: Some(1.0),
            confidence_basis: Some(FindingConfidenceBasis::Baseline),
            message: format!(
                "Secret baseline entry matched {}:{}",
                entry.file_path.display(),
                entry.entry.line_number
            ),
            evidence: json!({
                "file_path": entry.file_path,
                "line_number": entry.entry.line_number,
                "rule": entry.entry.rule_type.as_str(),
            }),
        }));
        per_file.push((canonical_file, allowed));
    }

    let override_confirmed = confirm_override_if_needed(project_root, &all_allowed, options);
    let mut briefing_blocks = BTreeMap::new();
    let mut override_files = BTreeSet::new();
    let mut override_detections = BTreeMap::<PathBuf, Vec<Detection>>::new();
    findings.extend(baseline_matches);

    for (file, allowed) in per_file {
        if allowed.is_empty() {
            continue;
        }
        if override_confirmed {
            override_files.insert(file.clone());
            override_detections.insert(file, allowed);
        } else {
            briefing_blocks.insert(file.clone(), BriefingBlockReason::SecretPresent);
            findings.extend(
                allowed
                    .iter()
                    .map(|detection| secret_detected_finding(&file, detection)),
            );
        }
    }

    for file in &override_files {
        let detections = override_detections.get(file).map_or(&[][..], Vec::as_slice);
        findings.push(PendingFinding {
            file_path: file.clone(),
            rule_id: SECRET_OVERRIDE_ALLOWED,
            kind: FindingKind::Defect,
            severity: FindingSeverity::Error,
            confidence: Some(1.0),
            confidence_basis: Some(FindingConfidenceBasis::OperatorOverride),
            message: format!(
                "Operator allowed unredacted secrets in {}",
                display_relative(project_root, file)
            ),
            evidence: json!({
                "file_path": display_relative(project_root, file),
                "override_used": true,
                "detections": detections.iter().map(detection_audit_json).collect::<Vec<_>>(),
            }),
        });
    }

    Ok(SecretScanOutcome {
        briefing_blocks,
        findings,
        finding_anchors: BTreeMap::new(),
        override_files: override_files.into_iter().collect(),
        scanned_files,
    })
}

fn confirm_override_if_needed(
    project_root: &Path,
    allowed: &[(PathBuf, Detection)],
    options: &SecretScanOptions,
) -> bool {
    if allowed.is_empty() {
        return false;
    }
    match options.override_policy {
        OverridePolicy::Forbid => false,
        OverridePolicy::Preconfirmed(_) => true,
        OverridePolicy::RequireInteractive => {
            print_detection_list(project_root, allowed);
            if io::stdin().is_terminal() {
                eprint!("Type '{CONFIRM_TOKEN}' to proceed: ");
                let _ = io::stderr().flush();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() && input.trim() == CONFIRM_TOKEN {
                    return true;
                }
            }
            abort_unconfirmed_override();
        }
    }
}

fn print_detection_list(project_root: &Path, allowed: &[(PathBuf, Detection)]) {
    for (file, detection) in allowed {
        eprintln!(
            "{}:{} {}",
            display_relative(project_root, file),
            detection.line_number,
            detection.rule_id
        );
    }
}

fn detection_audit_json(detection: &Detection) -> serde_json::Value {
    json!({
        "rule_id": detection.rule_id,
        "rule": detection.detect_secrets_type.as_str(),
        "line_number": detection.line_number,
        "hashed_secret_hex": detection.hashed_secret.to_string(),
    })
}

fn abort_unconfirmed_override() -> ! {
    eprintln!("{OVERRIDE_UNCONFIRMED}: --allow-unredacted-secrets requires confirmation");
    std::process::exit(78);
}

pub(super) fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|err| {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "using non-canonical secret-scan path after canonicalization failed"
        );
        path.to_path_buf()
    })
}

fn normalize_project_path(root: &Path, path: &Path) -> PathBuf {
    canonical_or_original(&root.join(path))
}

fn project_relative_path(root: &Path, path: &Path) -> PathBuf {
    relative_path(root, path).unwrap_or_else(|| path.to_path_buf())
}

pub(super) fn display_relative(root: &Path, path: &Path) -> String {
    relative_path(root, path)
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

fn relative_path(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = canonical_or_original(root);
    let path = canonical_or_original(path);
    path.strip_prefix(root).ok().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::display_relative;

    #[cfg(unix)]
    #[test]
    fn display_relative_handles_noncanonical_root_with_canonical_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real_root = tmp.path().join("real");
        let link_root = tmp.path().join("link");
        std::fs::create_dir(&real_root).expect("create real root");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("symlink project root");
        let file = real_root.join(".env");
        std::fs::write(&file, b"token=example\n").expect("write fixture");
        let canonical_file = file.canonicalize().expect("canonical file");

        assert_eq!(display_relative(&link_root, &canonical_file), ".env");
    }
}
