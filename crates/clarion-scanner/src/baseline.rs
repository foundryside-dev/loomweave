use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Detection, hex_decode_20};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    version: String,
    entries: BTreeMap<PathBuf, Vec<BaselineEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineEntry {
    pub rule_type: String,
    pub hashed_secret: [u8; 20],
    pub line_number: u32,
    pub is_secret: bool,
    pub justification: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineMatch {
    pub file_path: PathBuf,
    pub entry: BaselineEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineEntryIssue {
    pub file: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuppressionResult {
    pub allowed: Vec<Detection>,
    pub suppressed: Vec<Detection>,
    pub fired_entries: Vec<BaselineMatch>,
}

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    #[error("baseline version mismatch: expected 1.0, got {0}")]
    UnsupportedVersion(String),
    #[error("baseline entries missing required field 'justification'")]
    MissingJustifications { entries: Vec<BaselineEntryIssue> },
    #[error("baseline entry has invalid hashed_secret at {file}:{line}: {details}")]
    InvalidHash {
        file: PathBuf,
        line: u32,
        details: String,
    },
    #[error("baseline parse error: {0}")]
    Parse(#[from] serde_norway::Error),
    #[error("baseline I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn load_baseline(path: &Path) -> Result<Baseline, BaselineError> {
    match fs::read_to_string(path) {
        Ok(raw) => Baseline::from_yaml_str(&raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Baseline::empty()),
        Err(err) => Err(BaselineError::Io(err)),
    }
}

impl Baseline {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: "1.0".to_owned(),
            entries: BTreeMap::new(),
        }
    }

    pub fn from_yaml_str(raw: &str) -> Result<Self, BaselineError> {
        let parsed: RawBaseline = serde_norway::from_str(raw)?;
        Self::from_raw(parsed)
    }

    pub fn to_yaml_string(&self) -> Result<String, BaselineError> {
        let raw = RawBaseline::from(self);
        serde_norway::to_string(&raw).map_err(BaselineError::Parse)
    }

    #[must_use]
    pub fn entries(&self) -> &BTreeMap<PathBuf, Vec<BaselineEntry>> {
        &self.entries
    }

    #[must_use]
    pub fn suppress(&self, detections: Vec<Detection>, file: &Path) -> SuppressionResult {
        let entries = self.entries_for(file);
        let mut allowed = Vec::new();
        let mut suppressed = Vec::new();
        let mut fired_entries = Vec::new();
        let mut fired_keys = BTreeSet::new();

        'detections: for detection in detections {
            for (baseline_path, entry) in &entries {
                if entry.is_secret {
                    continue;
                }
                if entry.hashed_secret == detection.hashed_secret
                    && entry.line_number == detection.line_number
                {
                    let key = (
                        (*baseline_path).clone(),
                        entry.hashed_secret,
                        entry.line_number,
                    );
                    if fired_keys.insert(key) {
                        fired_entries.push(BaselineMatch {
                            file_path: (*baseline_path).clone(),
                            entry: (*entry).clone(),
                        });
                    }
                    suppressed.push(detection);
                    continue 'detections;
                }
            }
            allowed.push(detection);
        }

        SuppressionResult {
            allowed,
            suppressed,
            fired_entries,
        }
    }

    fn from_raw(raw: RawBaseline) -> Result<Self, BaselineError> {
        if raw.version != "1.0" {
            return Err(BaselineError::UnsupportedVersion(raw.version));
        }
        let mut missing_justifications = Vec::new();
        for (file, raw_entries) in &raw.results {
            for entry in raw_entries {
                if entry
                    .justification
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    missing_justifications.push(BaselineEntryIssue {
                        file: file.clone(),
                        line: entry.line_number,
                    });
                }
            }
        }
        if !missing_justifications.is_empty() {
            return Err(BaselineError::MissingJustifications {
                entries: missing_justifications,
            });
        }

        let mut entries = BTreeMap::new();
        for (file, raw_entries) in raw.results {
            let mut converted = Vec::new();
            for entry in raw_entries {
                let justification = entry.justification.unwrap_or_default();
                let hashed_secret = hex_decode_20(&entry.hashed_secret).map_err(|details| {
                    BaselineError::InvalidHash {
                        file: file.clone(),
                        line: entry.line_number,
                        details,
                    }
                })?;
                converted.push(BaselineEntry {
                    rule_type: entry.rule_type,
                    hashed_secret,
                    line_number: entry.line_number,
                    is_secret: entry.is_secret,
                    justification,
                });
            }
            entries.insert(file, converted);
        }
        Ok(Self {
            version: raw.version,
            entries,
        })
    }

    fn entries_for(&self, file: &Path) -> Vec<(&PathBuf, &BaselineEntry)> {
        self.entries
            .iter()
            .filter(|(candidate, _)| baseline_path_matches(file, candidate))
            .flat_map(|(path, entries)| entries.iter().map(move |entry| (path, entry)))
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RawBaseline {
    version: String,
    #[serde(default)]
    results: BTreeMap<PathBuf, Vec<RawBaselineEntry>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RawBaselineEntry {
    #[serde(rename = "type")]
    rule_type: String,
    hashed_secret: String,
    line_number: u32,
    #[serde(default)]
    is_secret: bool,
    justification: Option<String>,
}

impl From<&Baseline> for RawBaseline {
    fn from(value: &Baseline) -> Self {
        let results = value
            .entries
            .iter()
            .map(|(path, entries)| {
                (
                    path.clone(),
                    entries
                        .iter()
                        .map(|entry| RawBaselineEntry {
                            rule_type: entry.rule_type.clone(),
                            hashed_secret: crate::hex_encode(&entry.hashed_secret),
                            line_number: entry.line_number,
                            is_secret: entry.is_secret,
                            justification: Some(entry.justification.clone()),
                        })
                        .collect(),
                )
            })
            .collect();
        Self {
            version: value.version.clone(),
            results,
        }
    }
}

fn baseline_path_matches(file: &Path, baseline_path: &Path) -> bool {
    file == baseline_path || file.ends_with(baseline_path)
}
