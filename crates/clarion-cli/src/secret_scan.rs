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

mod findings;

use anyhow::{Context, Result};
use clarion_scanner::{Baseline, BaselineError, Detection, Scanner, SuppressionResult};
pub(crate) use findings::emit_findings;
use findings::{PendingFinding, secret_detected_finding};
use serde_json::json;

const SECRET_OVERRIDE_ALLOWED: &str = "CLA-SEC-UNREDACTED-SECRETS-ALLOWED";
const BASELINE_NO_JUSTIFICATION: &str = "CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION";
const BASELINE_MATCH: &str = "CLA-INFRA-SECRET-BASELINE-MATCH";
const OVERRIDE_UNCONFIRMED: &str = "CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED";
const CONFIRM_TOKEN: &str = "yes-i-understand";

#[derive(Debug, Clone, Default)]
pub(crate) struct SecretScanOptions {
    pub(crate) allow_unredacted_secrets: bool,
    pub(crate) confirm_allow_unredacted_secrets: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SecretScanOutcome {
    pub(crate) briefing_blocks: BTreeMap<PathBuf, String>,
    findings: Vec<PendingFinding>,
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
}

pub(crate) fn pre_ingest(
    project_root: &Path,
    source_files: &[PathBuf],
    options: &SecretScanOptions,
) -> Result<SecretScanOutcome> {
    let scanner = Scanner::new();
    let (baseline, mut findings) = load_baseline_for_scan(project_root)?;
    let mut per_file = Vec::new();
    let mut all_allowed = Vec::new();
    let mut baseline_matches = Vec::new();
    let mut scanned_files = BTreeSet::new();

    for file in source_files {
        let canonical_file = canonical_or_original(file);
        scanned_files.insert(canonical_file.clone());
        let buf = fs::read(file).with_context(|| format!("read {}", file.display()))?;
        let detections = scanner.scan_bytes(&buf);
        let SuppressionResult {
            allowed,
            fired_entries,
            ..
        } = baseline.suppress(detections, file);
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
            rule_id: BASELINE_MATCH,
            kind: "fact",
            severity: "INFO",
            confidence: Some(1.0),
            confidence_basis: Some("baseline"),
            message: format!(
                "Secret baseline entry matched {}:{}",
                entry.file_path.display(),
                entry.entry.line_number
            ),
            evidence: json!({
                "file_path": entry.file_path,
                "line_number": entry.entry.line_number,
                "rule": entry.entry.rule_type,
            }),
        }));
        per_file.push((canonical_file, allowed));
    }

    let override_confirmed = confirm_override_if_needed(&all_allowed, options);
    let mut briefing_blocks = BTreeMap::new();
    let mut override_files = BTreeSet::new();
    findings.extend(baseline_matches);

    for (file, allowed) in per_file {
        if allowed.is_empty() {
            continue;
        }
        if override_confirmed {
            override_files.insert(file);
        } else {
            briefing_blocks.insert(file.clone(), "secret_present".to_owned());
            findings.extend(
                allowed
                    .iter()
                    .map(|detection| secret_detected_finding(&file, detection)),
            );
        }
    }

    for file in &override_files {
        findings.push(PendingFinding {
            file_path: file.clone(),
            rule_id: SECRET_OVERRIDE_ALLOWED,
            kind: "defect",
            severity: "ERROR",
            confidence: Some(1.0),
            confidence_basis: Some("operator_override"),
            message: format!(
                "Operator allowed unredacted secrets in {}",
                display_relative(project_root, file)
            ),
            evidence: json!({
                "file_path": display_relative(project_root, file),
                "override_used": true,
            }),
        });
    }

    Ok(SecretScanOutcome {
        briefing_blocks,
        findings,
        override_files: override_files.into_iter().collect(),
        scanned_files,
    })
}

fn load_baseline_for_scan(project_root: &Path) -> Result<(Baseline, Vec<PendingFinding>)> {
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
                    kind: "defect",
                    severity: "ERROR",
                    confidence: Some(1.0),
                    confidence_basis: Some("baseline_schema"),
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

fn confirm_override_if_needed(
    allowed: &[(PathBuf, Detection)],
    options: &SecretScanOptions,
) -> bool {
    if allowed.is_empty() || !options.allow_unredacted_secrets {
        return false;
    }
    if options.confirm_allow_unredacted_secrets.as_deref() == Some(CONFIRM_TOKEN) {
        return true;
    }
    if options.confirm_allow_unredacted_secrets.is_some() {
        abort_unconfirmed_override();
    }
    if io::stdin().is_terminal() {
        for (file, detection) in allowed {
            eprintln!(
                "{}:{} {}",
                file.display(),
                detection.line_number,
                detection.rule_id
            );
        }
        eprint!("Type '{CONFIRM_TOKEN}' to proceed: ");
        let _ = io::stderr().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() && input.trim() == CONFIRM_TOKEN {
            return true;
        }
    }
    abort_unconfirmed_override();
}

fn abort_unconfirmed_override() -> ! {
    eprintln!("{OVERRIDE_UNCONFIRMED}: --allow-unredacted-secrets requires confirmation");
    std::process::exit(78);
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_project_path(root: &Path, path: &Path) -> PathBuf {
    canonical_or_original(&root.join(path))
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
