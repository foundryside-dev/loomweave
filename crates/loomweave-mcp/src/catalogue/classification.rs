//! Classifier-declaration evidence for tag-backed catalogue responses.

use loomweave_core::PluginCoverageStatus;
use loomweave_storage::LatestClassifierCoverage;
use serde::Serialize;
use serde_json::{Value, json};

const CLASSIFICATION_SCHEMA: &str = "loomweave.classification.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClassificationState {
    Supported,
    Partial,
    Unsupported,
    Unavailable,
}

/// Derive declaration support and enumeration completeness from the current
/// run's validated coverage plus this response's bounded-page metadata.
pub(crate) fn classify_tag(
    latest: &LatestClassifierCoverage,
    tag: &str,
    response: &Value,
) -> Value {
    let matches = response
        .pointer("/page/total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut reasons = Vec::new();
    let Some(coverage) = latest.coverage() else {
        reasons.push(
            latest
                .reason()
                .unwrap_or("classifier coverage is unavailable")
                .to_owned(),
        );
        return classification_value(
            latest,
            tag,
            ClassificationState::Unavailable,
            false,
            matches,
            &[],
            &[],
            &reasons,
        );
    };

    let mut supporting_plugins = Vec::new();
    let mut unsupported_plugins = Vec::new();
    let mut active: Vec<_> = coverage
        .plugins()
        .iter()
        .filter(|plugin| {
            plugin.matched_files() > 0 && plugin.status() != PluginCoverageStatus::NotApplicable
        })
        .collect();
    active.sort_by(|left, right| left.plugin_id().cmp(right.plugin_id()));
    for plugin in &active {
        if plugin
            .classifier_tags()
            .iter()
            .any(|declared| declared == tag)
        {
            supporting_plugins.push(plugin.plugin_id().to_owned());
        } else {
            unsupported_plugins.push(plugin.plugin_id().to_owned());
        }
    }
    supporting_plugins.sort();
    unsupported_plugins.sort();

    let state = if active.is_empty() {
        reasons.push("no active source plugin matched files in the latest analysis run".to_owned());
        ClassificationState::Unavailable
    } else if supporting_plugins.is_empty() {
        reasons.push(format!(
            "no active source plugin declares classifier tag {tag:?}"
        ));
        ClassificationState::Unsupported
    } else if unsupported_plugins.is_empty() {
        ClassificationState::Supported
    } else {
        reasons.push(format!(
            "classifier tag {tag:?} is not declared by every active source plugin"
        ));
        ClassificationState::Partial
    };

    let mut complete = state == ClassificationState::Supported;
    if !coverage.plugin_discovery_complete() {
        complete = false;
        reasons.push("source plugin discovery was incomplete".to_owned());
    }
    if !coverage.source_walk_complete() {
        complete = false;
        reasons.push("source walk was incomplete".to_owned());
    }
    for plugin in &active {
        if plugin.status() != PluginCoverageStatus::Complete || plugin.degraded_files() > 0 {
            complete = false;
            reasons.push(format!(
                "active plugin {:?} classifier coverage status is {:?}",
                plugin.plugin_id(),
                plugin.status()
            ));
        }
    }

    let enumeration_reasons = incomplete_enumeration_reasons(response, matches);
    if !enumeration_reasons.is_empty() {
        complete = false;
        reasons.extend(enumeration_reasons);
    }

    classification_value(
        latest,
        tag,
        state,
        complete,
        matches,
        &supporting_plugins,
        &unsupported_plugins,
        &reasons,
    )
}

pub(crate) fn attach_tag_classification(
    response: &mut Value,
    latest: &LatestClassifierCoverage,
    tag: &str,
) {
    let observed_reason = response
        .pointer("/signal/reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let classification = classify_tag(latest, tag, response);
    let available = classification["state"] == json!("supported");
    let complete = classification["complete"].as_bool().unwrap_or(false);
    let classification_reason = classification["reasons"]
        .as_array()
        .map(|reasons| {
            reasons
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| {
            "classifier declaration and enumeration evidence are complete".to_owned()
        });
    let reason = observed_reason
        .map(|observed| format!("{observed}; {classification_reason}"))
        .unwrap_or(classification_reason);
    if let Some(object) = response.as_object_mut() {
        object.insert("classification".to_owned(), classification);
        object.insert(
            "signal".to_owned(),
            json!({
                "available": available,
                "complete": complete,
                "signal": "entity_tags",
                "reason": reason,
            }),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn classification_value(
    latest: &LatestClassifierCoverage,
    tag: &str,
    state: ClassificationState,
    complete: bool,
    matches: u64,
    supporting_plugins: &[String],
    unsupported_plugins: &[String],
    reasons: &[String],
) -> Value {
    json!({
        "schema": CLASSIFICATION_SCHEMA,
        "tag": tag,
        "state": state,
        "complete": complete,
        "matches": matches,
        "supporting_plugins": supporting_plugins,
        "unsupported_plugins": unsupported_plugins,
        "run_id": latest.run_id(),
        "run_status": latest.run_status(),
        "reasons": reasons,
    })
}

fn incomplete_enumeration_reasons(response: &Value, matches: u64) -> Vec<String> {
    let mut reasons = Vec::new();
    let offset = response
        .pointer("/page/offset")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let returned = response
        .pointer("/page/returned")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let page_truncated = response
        .pointer("/page/truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let scope_truncated = response
        .get("scope_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let scan_truncated = response
        .get("scan_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if offset != 0 {
        reasons.push("enumeration starts at a nonzero page offset".to_owned());
    }
    if returned != matches {
        reasons.push("enumeration returned fewer rows than the total match count".to_owned());
    }
    if page_truncated {
        reasons.push("enumeration page is truncated".to_owned());
    }
    if scope_truncated {
        reasons.push("scope resolution is truncated".to_owned());
    }
    if scan_truncated {
        reasons.push("tag scan is truncated".to_owned());
    }
    reasons
}

#[cfg(test)]
mod tests {
    use loomweave_storage::{latest_classifier_coverage, schema};
    use rusqlite::{Connection, params};

    use super::*;

    fn latest() -> LatestClassifierCoverage {
        let mut conn = Connection::open_in_memory().expect("open database");
        schema::apply_migrations(&mut conn).expect("migrate database");
        let stats = json!({
            "classifier_coverage": {
                "schema": "loomweave.classifier-coverage.v1",
                "source_walk_complete": true,
                "source_walk_skipped_entries": 0,
                "plugin_discovery_complete": true,
                "plugin_discovery_errors": 0,
                "plugin_discovery_error_samples": [],
                "plugins": [{
                    "plugin_id": "python",
                    "plugin_version": "1.0.0",
                    "ontology_version": "1.0.0",
                    "matched_files": 1,
                    "analyzed_files": 1,
                    "retained_files": 0,
                    "degraded_files": 0,
                    "status": "complete",
                    "classifier_tags": ["test"]
                }]
            }
        })
        .to_string();
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('run-1', '2026-07-12T00:00:00Z', '2026-07-12T00:01:00Z', \
                     '{}', ?1, 'completed')",
            params![stats],
        )
        .expect("insert run");
        latest_classifier_coverage(&conn).expect("read coverage")
    }

    fn complete_response() -> Value {
        json!({
            "page": {
                "total": 1,
                "offset": 0,
                "limit": 50,
                "returned": 1,
                "truncated": false,
            },
            "scope_truncated": false,
            "scan_truncated": false,
        })
    }

    #[test]
    fn every_outer_truncation_marker_fails_closed() {
        for marker in ["scope_truncated", "scan_truncated"] {
            let mut response = complete_response();
            response[marker] = json!(true);
            let classification = classify_tag(&latest(), "test", &response);
            assert_eq!(classification["state"], "supported", "{marker}");
            assert_eq!(classification["complete"], false, "{marker}");
            assert!(
                classification["reasons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|reason| reason.as_str().unwrap().contains("truncated")),
                "{marker}: {classification}"
            );
        }
    }

    #[test]
    fn missing_enumeration_metadata_fails_closed() {
        let classification = classify_tag(&latest(), "test", &json!({}));
        assert_eq!(classification["state"], "supported");
        assert_eq!(classification["complete"], false);
    }
}
