use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};

use loomweave_federation::filigree::FiligreeHttpClient;
use loomweave_federation::scan_results::ScanResultsRequest;

/// Translate SARIF findings from a file and post them to Filigree.
#[allow(clippy::too_many_lines, clippy::collapsible_if)]
pub fn run_import(file: &Path, scan_source_opt: Option<String>, project_path: &Path) -> Result<()> {
    let project_root = project_path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise path {}", project_path.display()))?;

    // Load MCP config
    let mcp_config = crate::analyze::load_mcp_config(&project_root, None);

    // Build Filigree HTTP client
    let client = FiligreeHttpClient::from_config(&mcp_config.integrations.filigree, |name| {
        std::env::var(name).ok()
    })
    .context("build Filigree HTTP client")?
    .ok_or_else(|| anyhow!("Filigree integration is disabled in loomweave.yaml"))?;

    // Read and parse SARIF file
    let content = fs::read_to_string(file)
        .with_context(|| format!("failed to read SARIF file: {}", file.display()))?;
    let sarif: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse SARIF JSON in file: {}", file.display()))?;

    // Extract tool name and determine scan_source
    let driver_name = sarif
        .get("runs")
        .and_then(|r| r.as_array())
        .and_then(|r| r.first())
        .and_then(|r| r.get("tool"))
        .and_then(|t| t.get("driver"))
        .and_then(|d| d.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown");

    let scan_source = match scan_source_opt {
        Some(src) => src,
        None => {
            if driver_name.eq_ignore_ascii_case("wardline") {
                "wardline".to_owned()
            } else {
                driver_name.to_lowercase()
            }
        }
    };

    // Parse findings
    let mut findings = Vec::new();
    if let Some(runs) = sarif.get("runs").and_then(|r| r.as_array()) {
        for run in runs {
            if let Some(results) = run.get("results").and_then(|r| r.as_array()) {
                for res in results {
                    let rule_id = res
                        .get("ruleId")
                        .and_then(|r| r.as_str())
                        .unwrap_or("unknown-rule")
                        .to_owned();
                    let message = res
                        .get("message")
                        .and_then(|m| m.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let level = res
                        .get("level")
                        .and_then(|l| l.as_str())
                        .unwrap_or("warning");
                    let severity = match level {
                        "error" => "high",
                        "warning" => "medium",
                        _ => "info",
                    };

                    // Physical location mapping
                    let mut path = None;
                    let mut line_start = None;
                    let mut line_end = None;

                    if let Some(locations) = res.get("locations").and_then(|l| l.as_array()) {
                        if let Some(loc) = locations.first() {
                            if let Some(phys_loc) = loc.get("physicalLocation") {
                                if let Some(al) = phys_loc.get("artifactLocation") {
                                    if let Some(uri) = al.get("uri").and_then(|u| u.as_str()) {
                                        path = Some(normalize_sarif_uri(uri, &project_root));
                                    }
                                }
                                if let Some(region) = phys_loc.get("region") {
                                    line_start = region.get("startLine").and_then(Value::as_i64);
                                    line_end = region
                                        .get("endLine")
                                        .and_then(Value::as_i64)
                                        .or(line_start);
                                }
                            }
                        }
                    }

                    let path = match path {
                        Some(p) if !p.is_empty() => p,
                        _ => continue, // skip findings with no path
                    };

                    let properties = res
                        .get("properties")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new()));
                    let partial_fingerprints = res.get("partialFingerprints").cloned();
                    let fingerprint = partial_fingerprints
                        .as_ref()
                        .and_then(select_partial_fingerprint);

                    let mut metadata = Map::new();
                    metadata.insert("kind".to_owned(), json!("defect"));
                    if let Some(partial_fingerprints) = partial_fingerprints {
                        metadata.insert("partial_fingerprints".to_owned(), partial_fingerprints);
                    }
                    if scan_source == "wardline" {
                        metadata.insert("wardline_properties".to_owned(), properties);
                    } else {
                        metadata.insert("sarif_properties".to_owned(), properties);
                    }

                    let mut wire_find = Map::new();
                    wire_find.insert("path".to_owned(), json!(path));
                    wire_find.insert("rule_id".to_owned(), json!(rule_id));
                    wire_find.insert("message".to_owned(), json!(message));
                    wire_find.insert("severity".to_owned(), json!(severity));
                    if let Some(fingerprint) = fingerprint {
                        wire_find.insert("fingerprint".to_owned(), json!(fingerprint));
                    }
                    if let Some(ls) = line_start {
                        wire_find.insert("line_start".to_owned(), json!(ls));
                    }
                    if let Some(le) = line_end {
                        wire_find.insert("line_end".to_owned(), json!(le));
                    }
                    wire_find.insert("metadata".to_owned(), Value::Object(metadata));

                    findings.push(Value::Object(wire_find));
                }
            }
        }
    }

    let total_findings = findings.len();
    tracing::info!(
        file = %file.display(),
        scan_source = %scan_source,
        findings_count = total_findings,
        "parsed SARIF findings"
    );

    let request = ScanResultsRequest {
        scan_source: scan_source.clone(),
        scan_run_id: None,
        mark_unseen: true,
        create_observations: false,
        complete_scan_run: true,
        findings,
    };

    // Run client POST in a separate thread to bypass nested tokio runtime panics in reqwest::blocking
    let thread_client = client.clone();
    let worker = std::thread::spawn(move || thread_client.post_scan_results(&request));
    let response = worker
        .join()
        .map_err(|_| anyhow!("SARIF post thread panicked"))?
        .map_err(|err| anyhow!("post findings to Filigree failed: {err}"))?;

    tracing::info!(
        scan_source = %scan_source,
        created = response.findings_created,
        updated = response.findings_updated,
        warnings = response.warnings.len(),
        "successfully imported SARIF findings to Filigree"
    );

    for warning in &response.warnings {
        tracing::warn!(warning = %warning, "Filigree intake warning");
    }

    println!(
        "Import complete: {} created, {} updated",
        response.findings_created, response.findings_updated
    );

    Ok(())
}

fn select_partial_fingerprint(partial_fingerprints: &Value) -> Option<String> {
    let object = partial_fingerprints.as_object()?;
    object
        .get("wardlineFingerprint/v1")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            object
                .iter()
                .filter_map(|(_, value)| value.as_str())
                .find(|value| !value.trim().is_empty())
                .map(str::to_owned)
        })
}

fn normalize_sarif_uri(uri: &str, project_root: &Path) -> String {
    let path = if let Some(stripped) = uri.strip_prefix("file://localhost/") {
        PathBuf::from(format!("/{stripped}"))
    } else if let Some(stripped) = uri.strip_prefix("file:///") {
        PathBuf::from(format!("/{stripped}"))
    } else if let Some(stripped) = uri.strip_prefix("file://") {
        PathBuf::from(stripped)
    } else {
        PathBuf::from(uri)
    };

    if path.is_absolute()
        && let Ok(relative) = path.strip_prefix(project_root)
    {
        return relative.to_string_lossy().into_owned();
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::normalize_sarif_uri;

    #[test]
    fn normalize_sarif_uri_relativizes_project_absolute_file_uri() {
        let root = std::path::Path::new("/home/john/project");
        assert_eq!(
            normalize_sarif_uri("file:///home/john/project/src/a.py", root),
            "src/a.py"
        );
    }

    #[test]
    fn normalize_sarif_uri_preserves_unresolved_absolute_file_uri() {
        let root = std::path::Path::new("/home/john/project");
        assert_eq!(
            normalize_sarif_uri("file:///tmp/other/src/a.py", root),
            "/tmp/other/src/a.py"
        );
    }

    #[test]
    fn normalize_sarif_uri_keeps_relative_uri_relative() {
        let root = std::path::Path::new("/home/john/project");
        assert_eq!(normalize_sarif_uri("src/a.py", root), "src/a.py");
    }
}
