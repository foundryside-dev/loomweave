//! Fail-closed reads of classifier coverage from the latest analysis run.

use loomweave_core::ClassifierCoverage;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;

use crate::Result;

/// Upper bound for the complete `runs.stats` JSON document inspected by the
/// classifier reader. This exceeds the maximum valid v1 coverage payload while
/// bounding allocation when a catalogue was corrupted out of band.
pub const MAX_CLASSIFIER_STATS_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
struct StatsEnvelope {
    classifier_coverage: Option<ClassifierCoverage>,
}

/// Classifier coverage associated with the deterministically latest run.
///
/// An unavailable value deliberately retains the latest run identity/status so
/// callers can explain why they refused to infer support. It never falls back
/// to an older completed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestClassifierCoverage {
    run_id: Option<String>,
    run_status: Option<String>,
    coverage: Option<ClassifierCoverage>,
    reason: Option<String>,
}

impl LatestClassifierCoverage {
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.coverage.is_some()
    }

    #[must_use]
    pub fn run_id(&self) -> Option<&str> {
        self.run_id.as_deref()
    }

    #[must_use]
    pub fn run_status(&self) -> Option<&str> {
        self.run_status.as_deref()
    }

    #[must_use]
    pub fn coverage(&self) -> Option<&ClassifierCoverage> {
        self.coverage.as_ref()
    }

    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    fn unavailable(
        run_id: Option<String>,
        run_status: Option<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            run_status,
            coverage: None,
            reason: Some(reason.into()),
        }
    }
}

/// Read and validate coverage from the latest run, ordered by the run's stable
/// `(started_at, id)` key. Only a terminal, completed run is eligible.
pub fn latest_classifier_coverage(conn: &Connection) -> Result<LatestClassifierCoverage> {
    let row = conn
        .query_row(
            "SELECT \
                 CASE WHEN typeof(id) = 'text' THEN id END, \
                 CASE WHEN typeof(started_at) = 'text' THEN started_at END, \
                 CASE WHEN typeof(status) = 'text' THEN status END, \
                 CASE WHEN typeof(stats) = 'text' \
                            AND length(CAST(stats AS BLOB)) <= ?1 THEN stats END, \
                 typeof(stats), length(CAST(stats AS BLOB)), \
                 CASE WHEN typeof(completed_at) = 'text' THEN completed_at END \
             FROM runs \
             ORDER BY started_at DESC, id DESC LIMIT 1",
            [MAX_CLASSIFIER_STATS_BYTES],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, usize>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;

    let Some((run_id, started_at, run_status, stats_json, stats_kind, stats_bytes, completed_at)) =
        row
    else {
        return Ok(LatestClassifierCoverage::unavailable(
            None,
            None,
            "no analysis run exists",
        ));
    };

    let Some(run_id) = run_id.filter(|value| !value.is_empty()) else {
        return Ok(LatestClassifierCoverage::unavailable(
            None,
            run_status,
            "latest analysis run has a malformed non-text or empty id",
        ));
    };
    if started_at.as_deref().is_none_or(str::is_empty) {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            run_status,
            "latest analysis run has a malformed non-text or empty started_at",
        ));
    }

    let Some(run_status) = run_status else {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            None,
            "latest analysis run has a malformed non-text status",
        ));
    };

    if run_status != "completed" {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            Some(run_status.clone()),
            format!("latest analysis run status is {run_status}, not completed"),
        ));
    }
    if completed_at.as_deref().is_none_or(str::is_empty) {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            Some(run_status),
            "latest completed analysis run has no completed_at timestamp",
        ));
    }

    if stats_kind != "text" {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            Some(run_status),
            "latest analysis run has malformed non-text stats",
        ));
    }
    if stats_bytes > MAX_CLASSIFIER_STATS_BYTES {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            Some(run_status),
            format!("latest analysis run stats exceed the {MAX_CLASSIFIER_STATS_BYTES}-byte limit"),
        ));
    }
    let Some(stats_json) = stats_json else {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            Some(run_status),
            "latest analysis run stats could not be read safely",
        ));
    };

    let stats: StatsEnvelope = match serde_json::from_str(&stats_json) {
        Ok(value) => value,
        Err(error) => {
            let reason = if error.is_syntax() || error.is_eof() {
                "latest analysis run has malformed stats JSON"
            } else {
                "latest analysis run has invalid classifier_coverage metadata"
            };
            return Ok(LatestClassifierCoverage::unavailable(
                Some(run_id),
                Some(run_status),
                reason,
            ));
        }
    };
    let Some(coverage) = stats.classifier_coverage else {
        return Ok(LatestClassifierCoverage::unavailable(
            Some(run_id),
            Some(run_status),
            "latest analysis run is missing classifier_coverage metadata",
        ));
    };

    Ok(LatestClassifierCoverage {
        run_id: Some(run_id),
        run_status: Some(run_status),
        coverage: Some(coverage),
        reason: None,
    })
}
