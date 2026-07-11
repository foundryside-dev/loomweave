use std::collections::BTreeSet;

use anyhow::{Context, Result};
use loomweave_core::classifier_coverage::{
    MAX_PLUGIN_DISCOVERY_ERROR_SAMPLE_LEN, MAX_PLUGIN_DISCOVERY_ERROR_SAMPLES,
};
use loomweave_core::{
    ClassifierCoverage, Manifest, PluginClassifierCoverage, PluginClassifierCoverageInput,
    PluginCoverageStatus,
};

/// Mutable per-plugin accounting. `analyzed_files` is incremented only after a
/// complete file batch has crossed the writer boundary successfully.
pub(super) struct PluginCoverageAccumulator {
    plugin_id: String,
    plugin_version: String,
    ontology_version: String,
    classifier_tags: Vec<String>,
    matched_files: u64,
    analyzed_files: u64,
    retained_files: u64,
    degraded_source_files: BTreeSet<String>,
    failed: bool,
}

impl PluginCoverageAccumulator {
    pub(super) fn new(manifest: &Manifest, matched_files: usize) -> Self {
        Self {
            plugin_id: manifest.plugin.plugin_id.clone(),
            plugin_version: manifest.plugin.version.clone(),
            ontology_version: manifest.ontology.ontology_version.clone(),
            classifier_tags: manifest.ontology.classifier_tags.clone(),
            matched_files: u64::try_from(matched_files).unwrap_or(u64::MAX),
            analyzed_files: 0,
            retained_files: 0,
            degraded_source_files: BTreeSet::new(),
            failed: false,
        }
    }

    pub(super) fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(super) fn record_retained(&mut self, count: usize) {
        self.retained_files = self
            .retained_files
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }

    pub(super) fn record_completed_file(&mut self, degraded_source_files: BTreeSet<String>) {
        self.analyzed_files = self.analyzed_files.saturating_add(1);
        self.degraded_source_files.extend(degraded_source_files);
    }

    pub(super) fn record_analyzed_degraded_source(&mut self, source_file: String) {
        self.degraded_source_files.insert(source_file);
    }

    pub(super) fn mark_failed(&mut self) {
        self.failed = true;
    }

    fn finish(self) -> Result<PluginClassifierCoverage> {
        let covered_files = self.analyzed_files.saturating_add(self.retained_files);
        let status = if self.matched_files == 0 {
            PluginCoverageStatus::NotApplicable
        } else if self.failed || covered_files != self.matched_files {
            PluginCoverageStatus::Failed
        } else if self.degraded_source_files.is_empty() {
            PluginCoverageStatus::Complete
        } else {
            PluginCoverageStatus::Degraded
        };
        PluginClassifierCoverage::try_from(PluginClassifierCoverageInput {
            plugin_id: self.plugin_id,
            plugin_version: self.plugin_version,
            ontology_version: self.ontology_version,
            matched_files: self.matched_files,
            analyzed_files: self.analyzed_files,
            retained_files: self.retained_files,
            degraded_files: u64::try_from(self.degraded_source_files.len()).unwrap_or(u64::MAX),
            status,
            classifier_tags: self.classifier_tags,
        })
        .context("build plugin classifier coverage")
    }
}

pub(super) fn run_coverage(
    source_walk_skipped_entries: u64,
    discovery_errors: &[String],
    mut plugins: Vec<PluginCoverageAccumulator>,
) -> Result<ClassifierCoverage> {
    plugins.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    let plugins = plugins
        .into_iter()
        .map(PluginCoverageAccumulator::finish)
        .collect::<Result<Vec<_>>>()?;
    let discovery_error_count = u64::try_from(discovery_errors.len()).unwrap_or(u64::MAX);
    let mut ordered_errors = discovery_errors.iter().collect::<Vec<_>>();
    ordered_errors.sort_unstable();
    let samples = ordered_errors
        .into_iter()
        .take(MAX_PLUGIN_DISCOVERY_ERROR_SAMPLES)
        .map(|sample| truncate_utf8(sample, MAX_PLUGIN_DISCOVERY_ERROR_SAMPLE_LEN))
        .filter(|sample| !sample.is_empty())
        .collect();
    let discovery_complete = discovery_errors.is_empty();
    ClassifierCoverage::try_new(
        discovery_complete && source_walk_skipped_entries == 0,
        source_walk_skipped_entries,
        discovery_complete,
        discovery_error_count,
        samples,
        plugins,
    )
    .context("build classifier coverage")
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use loomweave_core::plugin::manifest::parse_manifest;

    use super::*;

    fn manifest() -> Manifest {
        parse_manifest(
            br#"
[plugin]
name = "fixture"
plugin_id = "fixture"
version = "1.0.0"
protocol_version = "1.0"
executable = "fixture"
language = "fixture"
extensions = ["fx"]

[capabilities.runtime]
expected_max_rss_mb = 64
expected_entities_per_file = 10
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["module"]
edge_kinds = []
rule_id_prefix = "LMWV-FIX-"
ontology_version = "1.0.0"
classifier_tags = ["entry-point"]
"#,
        )
        .unwrap()
    }

    #[test]
    fn source_walk_skips_make_coverage_incomplete() {
        let coverage =
            run_coverage(2, &[], vec![PluginCoverageAccumulator::new(&manifest(), 0)]).unwrap();

        assert!(!coverage.source_walk_complete());
        assert_eq!(coverage.source_walk_skipped_entries(), 2);
    }

    #[test]
    fn active_unprocessed_plugin_finishes_failed() {
        let coverage =
            run_coverage(0, &[], vec![PluginCoverageAccumulator::new(&manifest(), 1)]).unwrap();

        assert_eq!(coverage.plugins()[0].status(), PluginCoverageStatus::Failed);
        assert_eq!(coverage.plugins()[0].analyzed_files(), 0);
    }

    #[test]
    fn discovery_error_samples_are_sorted_bounded_and_utf8_safe() {
        let mut errors = (1..=20)
            .rev()
            .map(|index| format!("{index:02}-ordinary"))
            .collect::<Vec<_>>();
        errors.push(format!("00-{}", "é".repeat(300)));

        let coverage = run_coverage(0, &errors, Vec::new()).unwrap();
        let samples = coverage.plugin_discovery_error_samples();
        assert_eq!(coverage.plugin_discovery_errors(), 21);
        assert_eq!(samples.len(), MAX_PLUGIN_DISCOVERY_ERROR_SAMPLES);
        assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(samples[0].len(), 511);
        assert!(samples[0].ends_with('é'));
        assert!(
            samples
                .iter()
                .all(|sample| sample.len() <= MAX_PLUGIN_DISCOVERY_ERROR_SAMPLE_LEN)
        );
    }
}
