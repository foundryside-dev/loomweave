//! Versioned classifier coverage persisted with analysis-run statistics.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

/// Schema identifier for [`ClassifierCoverage`] records in `runs.stats`.
pub const CLASSIFIER_COVERAGE_SCHEMA: &str = "loomweave.classifier-coverage.v1";

/// Conservative wire caps. These bound untrusted persisted JSON before it is
/// used to classify catalogue completeness.
pub const MAX_CLASSIFIER_COVERAGE_PLUGINS: usize = 256;
pub const MAX_CLASSIFIER_TAGS: usize = 256;
pub const MAX_CLASSIFIER_TAG_LEN: usize = 128;
pub const MAX_PLUGIN_DISCOVERY_ERROR_SAMPLES: usize = 16;
pub const MAX_PLUGIN_DISCOVERY_ERROR_SAMPLE_LEN: usize = 512;
pub const MAX_PLUGIN_ID_LEN: usize = 128;
pub const MAX_PLUGIN_VERSION_LEN: usize = 128;

/// Validation failure for an in-memory or deserialized coverage record.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid classifier coverage: {message}")]
pub struct ClassifierCoverageError {
    message: String,
}

impl ClassifierCoverageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Authoritative classifier coverage for one analysis run.
///
/// Fields are private so every in-memory instance passes the same validation
/// as untrusted persisted JSON. Use [`ClassifierCoverage::try_new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassifierCoverage {
    schema: String,
    source_walk_complete: bool,
    source_walk_skipped_entries: u64,
    plugin_discovery_complete: bool,
    plugin_discovery_errors: u64,
    plugin_discovery_error_samples: Vec<String>,
    plugins: Vec<PluginClassifierCoverage>,
}

impl ClassifierCoverage {
    pub fn try_new(
        source_walk_complete: bool,
        source_walk_skipped_entries: u64,
        plugin_discovery_complete: bool,
        plugin_discovery_errors: u64,
        plugin_discovery_error_samples: Vec<String>,
        plugins: Vec<PluginClassifierCoverage>,
    ) -> Result<Self, ClassifierCoverageError> {
        let coverage = Self {
            schema: CLASSIFIER_COVERAGE_SCHEMA.to_owned(),
            source_walk_complete,
            source_walk_skipped_entries,
            plugin_discovery_complete,
            plugin_discovery_errors,
            plugin_discovery_error_samples,
            plugins,
        };
        coverage.validate()?;
        Ok(coverage)
    }

    pub fn schema(&self) -> &str {
        &self.schema
    }

    pub fn source_walk_complete(&self) -> bool {
        self.source_walk_complete
    }

    pub fn source_walk_skipped_entries(&self) -> u64 {
        self.source_walk_skipped_entries
    }

    pub fn plugin_discovery_complete(&self) -> bool {
        self.plugin_discovery_complete
    }

    pub fn plugin_discovery_errors(&self) -> u64 {
        self.plugin_discovery_errors
    }

    pub fn plugin_discovery_error_samples(&self) -> &[String] {
        &self.plugin_discovery_error_samples
    }

    pub fn plugins(&self) -> &[PluginClassifierCoverage] {
        &self.plugins
    }

    fn validate(&self) -> Result<(), ClassifierCoverageError> {
        if self.schema != CLASSIFIER_COVERAGE_SCHEMA {
            return Err(ClassifierCoverageError::new(format!(
                "schema must be {CLASSIFIER_COVERAGE_SCHEMA:?}, got {:?}",
                self.schema
            )));
        }
        if self.plugins.len() > MAX_CLASSIFIER_COVERAGE_PLUGINS {
            return Err(ClassifierCoverageError::new(format!(
                "plugins has {} entries; maximum is {MAX_CLASSIFIER_COVERAGE_PLUGINS}",
                self.plugins.len()
            )));
        }
        let mut plugin_ids = BTreeSet::new();
        for plugin in &self.plugins {
            if !plugin_ids.insert(plugin.plugin_id.as_str()) {
                return Err(ClassifierCoverageError::new(format!(
                    "duplicate plugin_id {:?}",
                    plugin.plugin_id
                )));
            }
        }
        if self.plugin_discovery_error_samples.len() > MAX_PLUGIN_DISCOVERY_ERROR_SAMPLES {
            return Err(ClassifierCoverageError::new(format!(
                "plugin_discovery_error_samples has {} entries; maximum is {MAX_PLUGIN_DISCOVERY_ERROR_SAMPLES}",
                self.plugin_discovery_error_samples.len()
            )));
        }
        if self.plugin_discovery_error_samples.len() as u64 > self.plugin_discovery_errors {
            return Err(ClassifierCoverageError::new(
                "plugin_discovery_error_samples cannot outnumber plugin_discovery_errors",
            ));
        }
        for sample in &self.plugin_discovery_error_samples {
            if sample.is_empty() || sample.len() > MAX_PLUGIN_DISCOVERY_ERROR_SAMPLE_LEN {
                return Err(ClassifierCoverageError::new(format!(
                    "plugin discovery sample length must be 1..={MAX_PLUGIN_DISCOVERY_ERROR_SAMPLE_LEN} bytes"
                )));
            }
        }
        if self.plugin_discovery_complete != (self.plugin_discovery_errors == 0) {
            return Err(ClassifierCoverageError::new(
                "plugin_discovery_complete must be true exactly when plugin_discovery_errors is zero",
            ));
        }
        let expected_source_walk_complete =
            self.plugin_discovery_complete && self.source_walk_skipped_entries == 0;
        if self.source_walk_complete != expected_source_walk_complete {
            return Err(ClassifierCoverageError::new(
                "source_walk_complete must reflect clean plugin discovery and zero skipped entries",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClassifierCoverage {
    schema: String,
    source_walk_complete: bool,
    source_walk_skipped_entries: u64,
    plugin_discovery_complete: bool,
    plugin_discovery_errors: u64,
    plugin_discovery_error_samples: Vec<String>,
    plugins: Vec<PluginClassifierCoverage>,
}

impl<'de> Deserialize<'de> for ClassifierCoverage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawClassifierCoverage::deserialize(deserializer)?;
        let coverage = Self {
            schema: raw.schema,
            source_walk_complete: raw.source_walk_complete,
            source_walk_skipped_entries: raw.source_walk_skipped_entries,
            plugin_discovery_complete: raw.plugin_discovery_complete,
            plugin_discovery_errors: raw.plugin_discovery_errors,
            plugin_discovery_error_samples: raw.plugin_discovery_error_samples,
            plugins: raw.plugins,
        };
        coverage.validate().map_err(de::Error::custom)?;
        Ok(coverage)
    }
}

/// Validated coverage produced by one source plugin during an analysis run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginClassifierCoverage {
    plugin_id: String,
    plugin_version: String,
    ontology_version: String,
    matched_files: u64,
    analyzed_files: u64,
    retained_files: u64,
    degraded_files: u64,
    status: PluginCoverageStatus,
    classifier_tags: Vec<String>,
}

/// Construction input for [`PluginClassifierCoverage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginClassifierCoverageInput {
    pub plugin_id: String,
    pub plugin_version: String,
    pub ontology_version: String,
    pub matched_files: u64,
    pub analyzed_files: u64,
    pub retained_files: u64,
    pub degraded_files: u64,
    pub status: PluginCoverageStatus,
    pub classifier_tags: Vec<String>,
}

impl PluginClassifierCoverage {
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn plugin_version(&self) -> &str {
        &self.plugin_version
    }

    pub fn ontology_version(&self) -> &str {
        &self.ontology_version
    }

    pub fn matched_files(&self) -> u64 {
        self.matched_files
    }

    pub fn analyzed_files(&self) -> u64 {
        self.analyzed_files
    }

    pub fn retained_files(&self) -> u64 {
        self.retained_files
    }

    pub fn degraded_files(&self) -> u64 {
        self.degraded_files
    }

    pub fn status(&self) -> PluginCoverageStatus {
        self.status
    }

    pub fn classifier_tags(&self) -> &[String] {
        &self.classifier_tags
    }

    fn validate(&self) -> Result<(), ClassifierCoverageError> {
        if self.plugin_id.len() > MAX_PLUGIN_ID_LEN
            || !crate::entity_id::validate_kind_grammar(&self.plugin_id)
        {
            return Err(ClassifierCoverageError::new(format!(
                "plugin_id must match [a-z][a-z0-9_]* and be at most {MAX_PLUGIN_ID_LEN} bytes"
            )));
        }
        validate_version("plugin_version", &self.plugin_version)?;
        validate_version("ontology_version", &self.ontology_version)?;
        if self.classifier_tags.len() > MAX_CLASSIFIER_TAGS {
            return Err(ClassifierCoverageError::new(format!(
                "classifier_tags has {} entries; maximum is {MAX_CLASSIFIER_TAGS}",
                self.classifier_tags.len()
            )));
        }
        for tag in &self.classifier_tags {
            if !classifier_tag_is_valid(tag) {
                return Err(ClassifierCoverageError::new(format!(
                    "classifier_tags contains invalid tag {tag:?}"
                )));
            }
        }
        if !self
            .classifier_tags
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        {
            return Err(ClassifierCoverageError::new(
                "classifier_tags must be sorted and unique",
            ));
        }

        let covered_files = self
            .analyzed_files
            .checked_add(self.retained_files)
            .ok_or_else(|| ClassifierCoverageError::new("status/count total overflow"))?;
        if covered_files > self.matched_files || self.degraded_files > self.analyzed_files {
            return Err(ClassifierCoverageError::new(
                "status/counts require analyzed + retained <= matched and degraded <= analyzed",
            ));
        }
        match self.status {
            PluginCoverageStatus::NotApplicable
                if self.matched_files == 0
                    && self.analyzed_files == 0
                    && self.retained_files == 0
                    && self.degraded_files == 0 =>
            {
                Ok(())
            }
            PluginCoverageStatus::Complete
                if self.matched_files > 0
                    && covered_files == self.matched_files
                    && self.degraded_files == 0 =>
            {
                Ok(())
            }
            PluginCoverageStatus::Degraded
                if self.matched_files > 0
                    && covered_files == self.matched_files
                    && self.degraded_files > 0 =>
            {
                Ok(())
            }
            PluginCoverageStatus::Failed if self.matched_files > 0 => Ok(()),
            _ => Err(ClassifierCoverageError::new(format!(
                "status {:?} is inconsistent with file counts",
                self.status
            ))),
        }
    }
}

impl TryFrom<PluginClassifierCoverageInput> for PluginClassifierCoverage {
    type Error = ClassifierCoverageError;

    fn try_from(input: PluginClassifierCoverageInput) -> Result<Self, Self::Error> {
        let plugin = Self {
            plugin_id: input.plugin_id,
            plugin_version: input.plugin_version,
            ontology_version: input.ontology_version,
            matched_files: input.matched_files,
            analyzed_files: input.analyzed_files,
            retained_files: input.retained_files,
            degraded_files: input.degraded_files,
            status: input.status,
            classifier_tags: input.classifier_tags,
        };
        plugin.validate()?;
        Ok(plugin)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPluginClassifierCoverage {
    plugin_id: String,
    plugin_version: String,
    ontology_version: String,
    matched_files: u64,
    analyzed_files: u64,
    retained_files: u64,
    degraded_files: u64,
    status: PluginCoverageStatus,
    classifier_tags: Vec<String>,
}

impl<'de> Deserialize<'de> for PluginClassifierCoverage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawPluginClassifierCoverage::deserialize(deserializer)?;
        Self::try_from(PluginClassifierCoverageInput {
            plugin_id: raw.plugin_id,
            plugin_version: raw.plugin_version,
            ontology_version: raw.ontology_version,
            matched_files: raw.matched_files,
            analyzed_files: raw.analyzed_files,
            retained_files: raw.retained_files,
            degraded_files: raw.degraded_files,
            status: raw.status,
            classifier_tags: raw.classifier_tags,
        })
        .map_err(de::Error::custom)
    }
}

/// Closed outcome set for a plugin's classifier pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginCoverageStatus {
    Complete,
    Degraded,
    Failed,
    NotApplicable,
}

pub(crate) fn classifier_tag_is_valid(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_CLASSIFIER_TAG_LEN
        && tag.as_bytes()[0].is_ascii_lowercase()
        && tag.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn validate_version(field: &str, value: &str) -> Result<(), ClassifierCoverageError> {
    if value.is_empty()
        || value.len() > MAX_PLUGIN_VERSION_LEN
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(ClassifierCoverageError::new(format!(
            "{field} must be 1..={MAX_PLUGIN_VERSION_LEN} printable ASCII bytes"
        )));
    }
    Ok(())
}
