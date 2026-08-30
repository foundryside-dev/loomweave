use loomweave_core::{
    CLASSIFIER_COVERAGE_SCHEMA, ClassifierCoverage, PluginClassifierCoverage,
    PluginClassifierCoverageInput, PluginCoverageStatus,
};

fn valid_coverage_json() -> serde_json::Value {
    serde_json::json!({
        "schema": CLASSIFIER_COVERAGE_SCHEMA,
        "source_walk_complete": true,
        "source_walk_skipped_entries": 0,
        "plugin_discovery_complete": true,
        "plugin_discovery_errors": 0,
        "plugin_discovery_error_samples": [],
        "plugins": [{
            "plugin_id": "python",
            "plugin_version": "1.5.1",
            "ontology_version": "0.12.0",
            "matched_files": 1,
            "analyzed_files": 1,
            "retained_files": 0,
            "degraded_files": 0,
            "status": "complete",
            "classifier_tags": ["http-route"]
        }]
    })
}

#[test]
fn classifier_coverage_uses_the_versioned_schema_and_closed_status_values() {
    let coverage: ClassifierCoverage = serde_json::from_value(valid_coverage_json()).unwrap();

    assert_eq!(coverage.schema(), CLASSIFIER_COVERAGE_SCHEMA);
    assert_eq!(
        coverage.plugins()[0].status(),
        PluginCoverageStatus::Complete
    );
}

#[test]
fn classifier_coverage_rejects_unknown_fields() {
    let mut value = valid_coverage_json();
    value["unexpected"] = serde_json::json!(true);

    let error = serde_json::from_value::<ClassifierCoverage>(value).unwrap_err();

    assert!(error.to_string().contains("unknown field"), "{error}");
}

#[test]
fn plugin_classifier_coverage_rejects_unknown_fields() {
    let mut value = valid_coverage_json();
    value["plugins"][0]["unexpected"] = serde_json::json!(true);

    let error = serde_json::from_value::<ClassifierCoverage>(value).unwrap_err();

    assert!(error.to_string().contains("unknown field"), "{error}");
}

#[test]
fn plugin_classifier_coverage_rejects_unknown_status() {
    let mut value = valid_coverage_json();
    value["plugins"][0]["status"] = serde_json::json!("skipped");

    let error = serde_json::from_value::<ClassifierCoverage>(value).unwrap_err();

    assert!(error.to_string().contains("unknown variant"), "{error}");
}

#[test]
fn plugin_coverage_not_applicable_has_an_explicit_wire_literal() {
    let mut value = valid_coverage_json();
    value["plugins"][0]["matched_files"] = serde_json::json!(0);
    value["plugins"][0]["analyzed_files"] = serde_json::json!(0);
    value["plugins"][0]["status"] = serde_json::json!("not-applicable");

    let coverage: ClassifierCoverage = serde_json::from_value(value).unwrap();
    let encoded = serde_json::to_value(&coverage).unwrap();

    assert_eq!(
        coverage.plugins()[0].status(),
        PluginCoverageStatus::NotApplicable
    );
    assert_eq!(encoded["plugins"][0]["status"], "not-applicable");
}

#[test]
fn classifier_coverage_rejects_an_unknown_schema_version() {
    let mut value = valid_coverage_json();
    value["schema"] = serde_json::json!("loomweave.classifier-coverage.v2");

    assert_rejected(value, "schema");
}

#[test]
fn classifier_coverage_rejects_more_than_256_plugins() {
    let mut value = valid_coverage_json();
    let template = value["plugins"][0].clone();
    value["plugins"] = serde_json::Value::Array(
        (0..257)
            .map(|index| {
                let mut plugin = template.clone();
                plugin["plugin_id"] = serde_json::json!(format!("plugin_{index}"));
                plugin
            })
            .collect(),
    );

    assert_rejected(value, "plugins");
}

#[test]
fn classifier_coverage_rejects_duplicate_plugin_ids() {
    let mut value = valid_coverage_json();
    let duplicate = value["plugins"][0].clone();
    value["plugins"].as_array_mut().unwrap().push(duplicate);

    assert_rejected(value, "duplicate plugin_id");
}

#[test]
fn classifier_coverage_rejects_inconsistent_discovery_and_source_walk_state() {
    for (complete, errors, samples, source_complete, skipped) in [
        (true, 1, vec!["bad manifest"], false, 0),
        (false, 0, vec![], false, 0),
        (false, 1, vec!["bad manifest"], true, 0),
        (true, 0, vec![], true, 1),
    ] {
        let mut value = valid_coverage_json();
        value["plugin_discovery_complete"] = serde_json::json!(complete);
        value["plugin_discovery_errors"] = serde_json::json!(errors);
        value["plugin_discovery_error_samples"] = serde_json::json!(samples);
        value["source_walk_complete"] = serde_json::json!(source_complete);
        value["source_walk_skipped_entries"] = serde_json::json!(skipped);

        assert_rejected(value, "complete");
    }
}

#[test]
fn classifier_coverage_bounds_discovery_error_samples() {
    let mut too_many = valid_coverage_json();
    too_many["plugin_discovery_complete"] = serde_json::json!(false);
    too_many["plugin_discovery_errors"] = serde_json::json!(17);
    too_many["plugin_discovery_error_samples"] =
        serde_json::json!((0..17).map(|i| format!("error {i}")).collect::<Vec<_>>());
    too_many["source_walk_complete"] = serde_json::json!(false);
    assert_rejected(too_many, "samples");

    let mut too_long = valid_coverage_json();
    too_long["plugin_discovery_complete"] = serde_json::json!(false);
    too_long["plugin_discovery_errors"] = serde_json::json!(1);
    too_long["plugin_discovery_error_samples"] = serde_json::json!(["x".repeat(513)]);
    too_long["source_walk_complete"] = serde_json::json!(false);
    assert_rejected(too_long, "sample");

    let mut more_samples_than_errors = valid_coverage_json();
    more_samples_than_errors["plugin_discovery_complete"] = serde_json::json!(false);
    more_samples_than_errors["plugin_discovery_errors"] = serde_json::json!(1);
    more_samples_than_errors["plugin_discovery_error_samples"] =
        serde_json::json!(["first", "second"]);
    more_samples_than_errors["source_walk_complete"] = serde_json::json!(false);
    assert_rejected(more_samples_than_errors, "outnumber");
}

#[test]
fn plugin_coverage_bounds_identifiers_versions_and_classifier_tags() {
    for (field, invalid) in [
        ("plugin_id", String::new()),
        ("plugin_id", "x".repeat(129)),
        ("plugin_id", "invalid-plugin".to_owned()),
        ("plugin_version", String::new()),
        ("plugin_version", "1".repeat(129)),
        ("ontology_version", String::new()),
        ("ontology_version", "1".repeat(129)),
    ] {
        let mut value = valid_coverage_json();
        value["plugins"][0][field] = serde_json::json!(invalid);
        assert_rejected(value, field);
    }

    let mut too_many = valid_coverage_json();
    too_many["plugins"][0]["classifier_tags"] = serde_json::json!(
        (0..257)
            .map(|index| format!("tag-{index}"))
            .collect::<Vec<_>>()
    );
    assert_rejected(too_many, "classifier_tags");

    for tags in [
        vec!["x".repeat(129)],
        vec!["HTTP_ROUTE".to_owned()],
        vec!["http-route".to_owned(), "entry-point".to_owned()],
        vec!["http-route".to_owned(), "http-route".to_owned()],
    ] {
        let mut value = valid_coverage_json();
        value["plugins"][0]["classifier_tags"] = serde_json::json!(tags);
        assert_rejected(value, "classifier_tags");
    }
}

#[test]
fn plugin_coverage_rejects_invalid_status_and_count_combinations() {
    for (matched, analyzed, retained, degraded, status) in [
        (0, 0, 0, 0, "complete"),
        (0, 0, 0, 0, "failed"),
        (1, 0, 0, 0, "not-applicable"),
        (0, 1, 0, 0, "not-applicable"),
        (1, 2, 0, 0, "failed"),
        (1, 0, 0, 1, "failed"),
        (2, 1, 0, 0, "complete"),
        (1, 1, 0, 1, "complete"),
        (1, 1, 0, 0, "degraded"),
        (2, 1, 0, 1, "degraded"),
    ] {
        let mut value = valid_coverage_json();
        let plugin = &mut value["plugins"][0];
        plugin["matched_files"] = serde_json::json!(matched);
        plugin["analyzed_files"] = serde_json::json!(analyzed);
        plugin["retained_files"] = serde_json::json!(retained);
        plugin["degraded_files"] = serde_json::json!(degraded);
        plugin["status"] = serde_json::json!(status);

        assert_rejected(value, "status");
    }
}

#[test]
fn plugin_coverage_accepts_each_valid_status_and_count_shape() {
    for (matched, analyzed, retained, degraded, status) in [
        (2, 1, 1, 0, "complete"),
        (2, 2, 0, 1, "degraded"),
        (2, 1, 0, 1, "failed"),
        (0, 0, 0, 0, "not-applicable"),
    ] {
        let mut value = valid_coverage_json();
        let plugin = &mut value["plugins"][0];
        plugin["matched_files"] = serde_json::json!(matched);
        plugin["analyzed_files"] = serde_json::json!(analyzed);
        plugin["retained_files"] = serde_json::json!(retained);
        plugin["degraded_files"] = serde_json::json!(degraded);
        plugin["status"] = serde_json::json!(status);

        serde_json::from_value::<ClassifierCoverage>(value)
            .unwrap_or_else(|error| panic!("valid {status} record was rejected: {error}"));
    }
}

#[test]
fn validated_constructors_reject_invalid_records_and_build_canonical_coverage() {
    let plugin = PluginClassifierCoverage::try_from(PluginClassifierCoverageInput {
        plugin_id: "python".to_owned(),
        plugin_version: "1.5.1".to_owned(),
        ontology_version: "0.12.0".to_owned(),
        matched_files: 1,
        analyzed_files: 1,
        retained_files: 0,
        degraded_files: 0,
        status: PluginCoverageStatus::Complete,
        classifier_tags: vec!["http-route".to_owned()],
    })
    .unwrap();
    let coverage = ClassifierCoverage::try_new(true, 0, true, 0, vec![], vec![plugin]).unwrap();

    assert_eq!(coverage.schema(), CLASSIFIER_COVERAGE_SCHEMA);

    let invalid_coverage =
        ClassifierCoverage::try_new(true, 0, false, 1, vec!["bad manifest".to_owned()], vec![]);
    assert!(invalid_coverage.is_err());

    let invalid = PluginClassifierCoverage::try_from(PluginClassifierCoverageInput {
        plugin_id: "python".to_owned(),
        plugin_version: "1.5.1".to_owned(),
        ontology_version: "0.12.0".to_owned(),
        matched_files: 0,
        analyzed_files: 0,
        retained_files: 0,
        degraded_files: 0,
        status: PluginCoverageStatus::Complete,
        classifier_tags: vec!["http-route".to_owned()],
    });
    assert!(invalid.is_err());
}

fn assert_rejected(value: serde_json::Value, expected: &str) {
    let error = serde_json::from_value::<ClassifierCoverage>(value).unwrap_err();
    assert!(
        error.to_string().contains(expected),
        "expected {expected:?} in {error}"
    );
}
