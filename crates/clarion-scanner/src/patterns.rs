use regex::{Regex, RegexSet};

use crate::{
    Detection, SecretCategory, entropy::EntropyTuning, line_number_for_offset, sha1_digest,
};

/// Metadata for one named secret detector.
#[derive(Debug, Clone)]
pub struct PatternMeta {
    pub rule_id: &'static str,
    pub detect_secrets_type: &'static str,
    pub category: SecretCategory,
    pub pattern: &'static str,
    capture_group: Option<usize>,
}

#[derive(Debug)]
struct CompiledPattern {
    meta: PatternMeta,
    regex: Regex,
}

/// Rust-native port of the ADR-013 v0.1 secret rule floor.
#[derive(Debug)]
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
            entropy_b64_re: Regex::new(r"\b[A-Za-z0-9+/]{20,}={0,2}\b")
                .expect("base64 candidate regex compiles"),
            entropy_hex_re: Regex::new(r"\b[a-fA-F0-9]{40,}\b")
                .expect("hex candidate regex compiles"),
        }
    }

    #[must_use]
    pub fn pattern_meta(&self) -> &[PatternMeta] {
        &self.pattern_meta
    }

    #[must_use]
    pub fn scan_bytes(&self, buf: &[u8]) -> Vec<Detection> {
        let source = String::from_utf8_lossy(buf);
        let bytes = source.as_bytes();
        let _set_matches = self.patterns.matches(&source);
        let mut detections = Vec::new();

        for compiled in &self.compiled_patterns {
            for captures in compiled.regex.captures_iter(&source) {
                let Some(whole_match) = captures.get(0) else {
                    continue;
                };
                if compiled.meta.category == SecretCategory::ContextualCredential
                    && line_is_comment(bytes, whole_match.start())
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
                    bytes,
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
        self.scan_entropy(bytes, &named_ranges, &mut detections);

        detections.sort_by_key(|d| (d.byte_offset, d.rule_id));
        detections
    }

    fn scan_entropy(
        &self,
        bytes: &[u8],
        named_ranges: &[(usize, usize)],
        detections: &mut Vec<Detection>,
    ) {
        let source = String::from_utf8_lossy(bytes);
        for candidate in self.entropy_b64_re.find_iter(&source) {
            let candidate_bytes = &source.as_bytes()[candidate.start()..candidate.end()];
            if looks_like_sha256_base64(&source, candidate.start(), candidate.end()) {
                continue;
            }
            if !range_overlaps(candidate.start(), candidate.end(), named_ranges)
                && self.entropy_b64.accepts(candidate_bytes)
            {
                detections.push(entropy_detection(
                    "HighEntropyBase64",
                    bytes,
                    candidate.start(),
                    candidate.end(),
                ));
            }
        }
        for candidate in self.entropy_hex_re.find_iter(&source) {
            let candidate_bytes = &source.as_bytes()[candidate.start()..candidate.end()];
            if !range_overlaps(candidate.start(), candidate.end(), named_ranges)
                && self.entropy_hex.accepts(candidate_bytes)
            {
                detections.push(entropy_detection(
                    "HighEntropyHex",
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
        rule_id: meta.rule_id,
        category: meta.category,
        byte_offset: start,
        line_number: line_number_for_offset(bytes, start),
        matched_len: end.saturating_sub(start),
        hashed_secret: sha1_digest(matched),
    }
}

fn entropy_detection(rule_id: &'static str, bytes: &[u8], start: usize, end: usize) -> Detection {
    Detection {
        rule_id,
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
            rule_id: "AwsAccessKeyId",
            detect_secrets_type: "AWS Access Key",
            category: SecretCategory::CloudCredential,
            pattern: r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "AwsSecretAccessKey",
            detect_secrets_type: "AWS Secret Access Key",
            category: SecretCategory::CloudCredential,
            pattern: r#"(?i)\baws[^:=\n]{0,32}(?:secret|access)[^:=\n]{0,32}(?:=|:|:=)\s*["']?([A-Za-z0-9/+=]{40})["']?"#,
            capture_group: Some(1),
        },
        PatternMeta {
            rule_id: "GitHubPat",
            detect_secrets_type: "GitHub Token",
            category: SecretCategory::VcsCredential,
            pattern: r"\bghp_[A-Za-z0-9]{36}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "GitHubFineGrainedPat",
            detect_secrets_type: "GitHub Fine-Grained Token",
            category: SecretCategory::VcsCredential,
            pattern: r"\bgithub_pat_[A-Za-z0-9_]{82,}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "GitHubOAuth",
            detect_secrets_type: "GitHub OAuth Token",
            category: SecretCategory::VcsCredential,
            pattern: r"\bgh[ousr]_[A-Za-z0-9]{36}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "AnthropicApiKey",
            detect_secrets_type: "Anthropic API Key",
            category: SecretCategory::AiProviderCredential,
            pattern: r"\bsk-ant-[A-Za-z0-9_-]{90,}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "OpenAiApiKey",
            detect_secrets_type: "OpenAI API Key",
            category: SecretCategory::AiProviderCredential,
            pattern: r"\bsk-[A-Za-z0-9]{48}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "StripeApiKey",
            detect_secrets_type: "Stripe API Key",
            category: SecretCategory::PaymentsCredential,
            pattern: r"\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{16,}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "SlackToken",
            detect_secrets_type: "Slack Token",
            category: SecretCategory::MessagingCredential,
            pattern: r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "JwtToken",
            detect_secrets_type: "JWT Token",
            category: SecretCategory::JwtToken,
            pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "PrivateKeyHeader",
            detect_secrets_type: "Private Key",
            category: SecretCategory::PrivateKey,
            pattern: r"-----BEGIN (?:RSA|EC|DSA|OPENSSH|PGP|ENCRYPTED) PRIVATE KEY-----",
            capture_group: None,
        },
        PatternMeta {
            rule_id: "ContextualCredential",
            detect_secrets_type: "Keyword Detector",
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

fn line_is_comment(bytes: &[u8], offset: usize) -> bool {
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

fn looks_like_sha256_base64(source: &str, start: usize, end: usize) -> bool {
    let candidate = &source[start..end];
    (candidate.len() == 44 && candidate.ends_with('='))
        || (candidate.len() == 43 && source.as_bytes().get(end) == Some(&b'='))
}
