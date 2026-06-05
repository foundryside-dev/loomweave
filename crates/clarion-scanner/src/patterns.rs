use regex::bytes::{Regex, RegexSet};

use crate::{
    DetectSecretsRule, Detection, SecretCategory, entropy::EntropyTuning, line_number_for_offset,
    sha1_digest,
};

/// Metadata for one named secret detector.
#[derive(Debug, Clone)]
pub struct PatternMeta {
    pub detect_secrets_type: DetectSecretsRule,
    pub category: SecretCategory,
    pub pattern: &'static str,
    capture_group: Option<usize>,
}

#[derive(Debug, Clone)]
struct CompiledPattern {
    meta: PatternMeta,
    regex: Regex,
}

/// Rust-native port of the ADR-013 v0.1 secret rule floor.
#[derive(Debug, Clone)]
pub struct Scanner {
    patterns: RegexSet,
    pattern_meta: Vec<PatternMeta>,
    compiled_patterns: Vec<CompiledPattern>,
    entropy_b64: EntropyTuning,
    entropy_hex: EntropyTuning,
    entropy_b64_re: Regex,
    entropy_hex_re: Regex,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// Build the default ADR-013 scanner.
    ///
    /// # Panics
    ///
    /// Panics only if one of the compiled-in regular expressions is invalid.
    #[must_use]
    pub fn new() -> Self {
        // Cache compiled Regexes and Scanner instance behind a OnceLock.
        // Cloning a `Regex` or `RegexSet` is extremely cheap (just bumping an Arc),
        // preventing redundant compilation overhead whenever a Scanner is instantiated.
        static DEFAULTS: std::sync::OnceLock<Scanner> = std::sync::OnceLock::new();
        DEFAULTS
            .get_or_init(|| {
                let pattern_meta = default_pattern_meta();
                let patterns = RegexSet::new(pattern_meta.iter().map(|meta| meta.pattern))
                    .expect("default secret patterns compile");
                let compiled_patterns = pattern_meta
                    .iter()
                    .cloned()
                    .map(|meta| CompiledPattern {
                        regex: Regex::new(meta.pattern).expect("default secret pattern compiles"),
                        meta,
                    })
                    .collect();
                Self {
                    patterns,
                    pattern_meta,
                    compiled_patterns,
                    entropy_b64: EntropyTuning::BASE64,
                    entropy_hex: EntropyTuning::HEX,
                    entropy_b64_re: Regex::new(r"[A-Za-z0-9+/]{20,}={0,2}")
                        .expect("base64 candidate regex compiles"),
                    entropy_hex_re: Regex::new(r"\b[a-fA-F0-9]{40,}\b")
                        .expect("hex candidate regex compiles"),
                }
            })
            .clone()
    }

    #[must_use]
    pub fn pattern_meta(&self) -> &[PatternMeta] {
        &self.pattern_meta
    }

    #[must_use]
    pub fn scan_bytes(&self, buf: &[u8]) -> Vec<Detection> {
        let set_matches = self.patterns.matches(buf);
        let mut detections = Vec::new();

        for (idx, compiled) in self.compiled_patterns.iter().enumerate() {
            if !set_matches.matched(idx) {
                continue;
            }
            for captures in compiled.regex.captures_iter(buf) {
                let Some(whole_match) = captures.get(0) else {
                    continue;
                };
                if compiled.meta.category == SecretCategory::ContextualCredential
                    && line_is_comment(buf, whole_match.start())
                {
                    continue;
                }
                let Some(secret_match) = compiled
                    .meta
                    .capture_group
                    .and_then(|group| captures.get(group))
                    .or(Some(whole_match))
                else {
                    continue;
                };
                detections.push(detection_from_match(
                    &compiled.meta,
                    buf,
                    secret_match.start(),
                    secret_match.end(),
                ));
            }
        }

        let named_ranges = detections
            .iter()
            .map(|detection| {
                (
                    detection.byte_offset,
                    detection.byte_offset + detection.matched_len,
                )
            })
            .collect::<Vec<_>>();
        self.scan_entropy(buf, &named_ranges, &mut detections);

        detections.sort_by_key(|d| (d.byte_offset, d.rule_id));
        detections
    }

    fn scan_entropy(
        &self,
        bytes: &[u8],
        named_ranges: &[(usize, usize)],
        detections: &mut Vec<Detection>,
    ) {
        for candidate in self.entropy_b64_re.find_iter(bytes) {
            let candidate_bytes = &bytes[candidate.start()..candidate.end()];
            if base64_candidate_has_boundaries(bytes, candidate.start(), candidate.end())
                && !range_overlaps(candidate.start(), candidate.end(), named_ranges)
                && self.entropy_b64.accepts(candidate_bytes)
            {
                detections.push(entropy_detection(
                    DetectSecretsRule::Base64HighEntropyString,
                    bytes,
                    candidate.start(),
                    candidate.end(),
                ));
            }
        }
        for candidate in self.entropy_hex_re.find_iter(bytes) {
            let candidate_bytes = &bytes[candidate.start()..candidate.end()];
            if !range_overlaps(candidate.start(), candidate.end(), named_ranges)
                && self.entropy_hex.accepts(candidate_bytes)
            {
                detections.push(entropy_detection(
                    DetectSecretsRule::HexHighEntropyString,
                    bytes,
                    candidate.start(),
                    candidate.end(),
                ));
            }
        }
    }
}

fn detection_from_match(meta: &PatternMeta, bytes: &[u8], start: usize, end: usize) -> Detection {
    let matched = &bytes[start..end];
    Detection {
        rule_id: meta.detect_secrets_type.rule_id(),
        detect_secrets_type: meta.detect_secrets_type,
        category: meta.category,
        byte_offset: start,
        line_number: line_number_for_offset(bytes, start),
        matched_len: end.saturating_sub(start),
        hashed_secret: sha1_digest(matched),
    }
}

fn entropy_detection(
    detect_secrets_type: DetectSecretsRule,
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Detection {
    Detection {
        rule_id: detect_secrets_type.rule_id(),
        detect_secrets_type,
        category: SecretCategory::HighEntropy,
        byte_offset: start,
        line_number: line_number_for_offset(bytes, start),
        matched_len: end.saturating_sub(start),
        hashed_secret: sha1_digest(&bytes[start..end]),
    }
}

fn default_pattern_meta() -> Vec<PatternMeta> {
    vec![
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::AwsAccessKey,
            category: SecretCategory::CloudCredential,
            pattern: r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::AwsSecretAccessKey,
            category: SecretCategory::CloudCredential,
            pattern: r#"(?i)\baws[^:=\n]{0,32}(?:secret|access)[^:=\n]{0,32}(?:=|:|:=)\s*["']?([A-Za-z0-9/+=]{40})["']?"#,
            capture_group: Some(1),
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::GitHubToken,
            category: SecretCategory::VcsCredential,
            pattern: r"\bghp_[A-Za-z0-9]{36}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::GitHubFineGrainedToken,
            category: SecretCategory::VcsCredential,
            pattern: r"\bgithub_pat_[A-Za-z0-9_]{82,}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::GitHubOAuthToken,
            category: SecretCategory::VcsCredential,
            pattern: r"\bgh[ousr]_[A-Za-z0-9]{36}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::AnthropicApiKey,
            category: SecretCategory::AiProviderCredential,
            pattern: r"\bsk-ant-[A-Za-z0-9_-]{90,}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::OpenAiApiKey,
            category: SecretCategory::AiProviderCredential,
            pattern: r"\bsk-(?:[A-Za-z0-9]{48}|(?:proj|svcacct)-[A-Za-z0-9_-]{20,})\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::StripeApiKey,
            category: SecretCategory::PaymentsCredential,
            pattern: r"\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{16,}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::SlackToken,
            category: SecretCategory::MessagingCredential,
            pattern: r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::JwtToken,
            category: SecretCategory::JwtToken,
            pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::PrivateKey,
            category: SecretCategory::PrivateKey,
            pattern: r"-----BEGIN (?:(?:RSA|EC|DSA|OPENSSH|ENCRYPTED) PRIVATE KEY|PRIVATE KEY|PGP PRIVATE KEY BLOCK)-----",
            capture_group: None,
        },
        PatternMeta {
            detect_secrets_type: DetectSecretsRule::KeywordDetector,
            category: SecretCategory::ContextualCredential,
            pattern: r#"(?i)(?:^|[^A-Za-z0-9_-])(?:password|passwd|secret[_-]?token|secret|token|api[_-]?key)\s*(?:=|:=|:)\s*["']([^"'\s]{8,})["']"#,
            capture_group: Some(1),
        },
    ]
}

fn range_overlaps(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(range_start, range_end)| start < *range_end && end > *range_start)
}

fn base64_candidate_has_boundaries(bytes: &[u8], start: usize, end: usize) -> bool {
    let before_ok = start == 0 || !is_base64_candidate_byte(bytes[start - 1]);
    let after_ok = end == bytes.len() || !is_base64_candidate_byte(bytes[end]);
    before_ok && after_ok
}

fn is_base64_candidate_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'='
    )
}

fn line_is_comment(bytes: &[u8], offset: usize) -> bool {
    // ADR-013's v0.1 rule floor is Python/.env-first, so only shell/Python
    // `#` comments are ignored here. Other language comment forms should use
    // an explicit baseline entry until their detector context is added.
    let line_start = bytes
        .get(..offset.min(bytes.len()))
        .and_then(|prefix| prefix.iter().rposition(|byte| *byte == b'\n'))
        .map_or(0, |pos| pos + 1);
    bytes[line_start..offset.min(bytes.len())]
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b'#')
}
