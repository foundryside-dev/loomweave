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
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
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
    FindingConfidence, FindingKind, FindingSeverity, PendingFinding, secret_detected_finding,
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
    per_file_outcomes: Vec<PerFileOutcome>,
    findings: Vec<PendingFinding>,
    finding_anchors: BTreeMap<PathBuf, String>,
    scanned_files: Arc<BTreeSet<PathBuf>>,
    // Subset of `scanned_files` restricted to paths matched by
    // `files::is_secret_scan_sidecar` (e.g. `.env`, `.env.*`, `*.env`).
    // These paths have no plugin coverage by design, so their `core:file`
    // anchor is the only catalog row tracking briefing_blocked + content_hash
    // for that path. The anchor-reconciliation pass in `anchors.rs` iterates
    // this set on every run to clear stale blocks once secrets are removed.
    scanned_sidecars: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PerFileOutcome {
    Clean {
        file: PathBuf,
    },
    Blocked {
        file: PathBuf,
        reason: BriefingBlockReason,
        findings: Vec<Detection>,
    },
    Overridden {
        file: PathBuf,
        findings: Vec<Detection>,
    },
}

impl SecretScanOutcome {
    #[cfg(test)]
    pub(crate) fn per_file_outcomes(&self) -> &[PerFileOutcome] {
        &self.per_file_outcomes
    }

    pub(crate) fn briefing_blocks_shared(&self) -> Arc<BTreeMap<PathBuf, BriefingBlockReason>> {
        Arc::new(
            self.per_file_outcomes
                .iter()
                .filter_map(|outcome| match outcome {
                    PerFileOutcome::Blocked { file, reason, .. } => Some((file.clone(), *reason)),
                    PerFileOutcome::Clean { .. } | PerFileOutcome::Overridden { .. } => None,
                })
                .collect(),
        )
    }

    pub(crate) fn briefing_block_for(&self, file: &Path) -> Option<BriefingBlockReason> {
        self.per_file_outcomes
            .iter()
            .find_map(|outcome| match outcome {
                PerFileOutcome::Blocked {
                    file: blocked_file,
                    reason,
                    ..
                } if blocked_file == file => Some(*reason),
                PerFileOutcome::Clean { .. }
                | PerFileOutcome::Blocked { .. }
                | PerFileOutcome::Overridden { .. } => None,
            })
    }

    pub(crate) fn scanned_files_shared(&self) -> Arc<BTreeSet<PathBuf>> {
        Arc::clone(&self.scanned_files)
    }

    pub(crate) fn scanned_sidecars(&self) -> &BTreeSet<PathBuf> {
        &self.scanned_sidecars
    }

    pub(crate) fn finding_files(&self) -> BTreeSet<PathBuf> {
        self.findings
            .iter()
            .map(|finding| finding.file_path.clone())
            .collect()
    }

    pub(crate) fn augment_stats(&self, stats: &mut serde_json::Value) {
        let override_files = self
            .per_file_outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                PerFileOutcome::Overridden { file, .. } => Some(file),
                PerFileOutcome::Clean { .. } | PerFileOutcome::Blocked { .. } => None,
            })
            .collect::<Vec<_>>();
        if override_files.is_empty() {
            return;
        }
        stats["secret_override_used"] = json!(true);
        stats["secret_override_files_affected"] = json!(
            override_files
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
    let mut scanned_sidecars = BTreeSet::new();
    let scans = scan_source_files_parallel(source_files, |buf| scanner.scan_bytes(buf))?;

    for scan in scans {
        let canonical_file = scan.canonical_file;
        scanned_files.insert(canonical_file.clone());
        if files::is_secret_scan_sidecar(&canonical_file) {
            scanned_sidecars.insert(canonical_file.clone());
        }
        let baseline_file = project_relative_path(project_root, &canonical_file);
        let SuppressionResult {
            allowed,
            fired_entries,
            ..
        } = baseline.suppress(scan.detections, &baseline_file);
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
            confidence: FindingConfidence::Baseline,
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
    let mut per_file_outcomes = Vec::new();
    let mut override_detections = BTreeMap::<PathBuf, Vec<Detection>>::new();
    findings.extend(baseline_matches);

    for (file, allowed) in per_file {
        if allowed.is_empty() {
            per_file_outcomes.push(PerFileOutcome::Clean { file });
            continue;
        }
        // ADR-013 §"Override — --allow-unredacted-secrets": each detection
        // emits its own `CLA-SEC-SECRET-DETECTED` finding regardless of
        // whether the operator subsequently overrode the block. The override
        // finding (`CLA-SEC-UNREDACTED-SECRETS-ALLOWED`) is additive — it
        // records the operator decision but does not replace the
        // per-(rule,file,line) audit row, so a security review running
        // `filigree list --rule-id=CLA-SEC-SECRET-DETECTED` enumerates the
        // full detection population.
        findings.extend(
            allowed
                .iter()
                .map(|detection| secret_detected_finding(&file, detection)),
        );
        if override_confirmed {
            override_detections.insert(file.clone(), allowed.clone());
            per_file_outcomes.push(PerFileOutcome::Overridden {
                file,
                findings: allowed,
            });
        } else {
            per_file_outcomes.push(PerFileOutcome::Blocked {
                file,
                reason: BriefingBlockReason::SecretPresent,
                findings: allowed,
            });
        }
    }

    for file in override_detections.keys() {
        let detections = override_detections.get(file).map_or(&[][..], Vec::as_slice);
        findings.push(PendingFinding {
            file_path: file.clone(),
            rule_id: SECRET_OVERRIDE_ALLOWED,
            kind: FindingKind::Defect,
            severity: FindingSeverity::Error,
            confidence: FindingConfidence::OperatorOverride,
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
        per_file_outcomes,
        findings,
        finding_anchors: BTreeMap::new(),
        scanned_files: Arc::new(scanned_files),
        scanned_sidecars,
    })
}

#[derive(Debug)]
struct SourceFileScan {
    canonical_file: PathBuf,
    detections: Vec<Detection>,
}

fn scan_source_files_parallel<F>(
    source_files: &[PathBuf],
    scan_bytes: F,
) -> Result<Vec<SourceFileScan>>
where
    F: Fn(&[u8]) -> Vec<Detection> + Sync,
{
    if source_files.is_empty() {
        return Ok(Vec::new());
    }

    let worker_count = source_files.len().min(
        thread::available_parallelism()
            .map_or(1, usize::from)
            .max(1),
    );
    let next_file = AtomicUsize::new(0);
    let results = (0..source_files.len())
        .map(|_| Mutex::new(None))
        .collect::<Vec<_>>();

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let next_file = &next_file;
            let results = &results;
            let scan_bytes = &scan_bytes;
            scope.spawn(move || {
                loop {
                    let idx = next_file.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = source_files.get(idx) else {
                        break;
                    };
                    let result = scan_one_source_file(file, scan_bytes);
                    *results[idx]
                        .lock()
                        .expect("parallel secret-scan result lock poisoned") = Some(result);
                }
            });
        }
    });

    results
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("parallel secret-scan result lock poisoned")
                .expect("parallel secret-scan worker did not fill result")
        })
        .collect()
}

fn scan_one_source_file<F>(file: &Path, scan_bytes: &F) -> Result<SourceFileScan>
where
    F: Fn(&[u8]) -> Vec<Detection> + Sync + ?Sized,
{
    let canonical_file = canonical_or_original(file);
    let buf = fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let detections = scan_bytes(&buf);
    Ok(SourceFileScan {
        canonical_file,
        detections,
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

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
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
    use super::{
        ConfirmToken, OverridePolicy, PerFileOutcome, SecretScanOptions, display_relative,
        pre_ingest, scan_source_files_parallel,
    };
    use std::sync::{Arc, Mutex};

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

    #[test]
    fn scan_source_files_parallel_scans_on_workers_and_preserves_input_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let files = (0..8)
            .map(|idx| {
                let path = tmp.path().join(format!("{idx}.txt"));
                std::fs::write(&path, format!("file-{idx}\n")).expect("write scan file");
                path
            })
            .collect::<Vec<_>>();
        let caller_thread = std::thread::current().id();
        let scan_threads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&scan_threads);

        let scans = scan_source_files_parallel(&files, |buf| {
            seen.lock()
                .expect("record thread")
                .push(std::thread::current().id());
            assert!(buf.starts_with(b"file-"));
            Vec::new()
        })
        .expect("parallel scan");

        assert_eq!(
            scans
                .iter()
                .map(|scan| scan.canonical_file.clone())
                .collect::<Vec<_>>(),
            files
                .iter()
                .map(|file| file.canonicalize().expect("canonical test file"))
                .collect::<Vec<_>>()
        );
        assert!(
            scan_threads
                .lock()
                .expect("scan threads")
                .iter()
                .any(|thread_id| *thread_id != caller_thread),
            "expected at least one scan to run on a worker thread"
        );
    }

    #[test]
    fn pre_ingest_records_file_outcomes_without_side_channel_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let clean = tmp.path().join("clean.txt");
        let secret = tmp.path().join("secret.txt");
        std::fs::write(&clean, b"hello\n").expect("write clean");
        std::fs::write(&secret, b"key = 'AKIAIOSFODNN7EXAMPLE'\n").expect("write secret");

        let outcome = pre_ingest(
            tmp.path(),
            &[clean.clone(), secret.clone()],
            &SecretScanOptions {
                override_policy: OverridePolicy::Preconfirmed(ConfirmToken),
            },
        )
        .expect("pre-ingest");

        assert!(matches!(
            outcome.per_file_outcomes()[0],
            PerFileOutcome::Clean { .. }
        ));
        let PerFileOutcome::Overridden { file, findings } = &outcome.per_file_outcomes()[1] else {
            panic!("secret file should be recorded as overridden");
        };
        assert_eq!(file, &secret.canonicalize().expect("canonical secret"));
        assert_eq!(findings.len(), 1);
        assert!(outcome.briefing_blocks_shared().is_empty());
    }
}
