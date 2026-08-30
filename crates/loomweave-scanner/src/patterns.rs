use regex::bytes::{Regex, RegexSet};

use crate::{
    DetectSecretsRule, Detection, SecretCategory, entropy::EntropyTuning, line_number_for_offset,
    sha1_digest,
};

/// Inline allow-markers honoured by [`Scanner::scan_bytes_partitioned`]: a
/// detection whose line carries one of these substrings (byte-exact,
/// case-sensitive — conventionally a trailing comment) is partitioned out as
/// operator-allowed instead of reported. The first form is Loomweave-native;
/// the second matches detect-secrets' `# pragma: allowlist secret` so
/// operator habits from that tool transfer (ADR-013 amendment 2026-08-31).
pub const INLINE_ALLOW_MARKERS: [&str; 2] =
    ["secret-scan: allow-this-line", "pragma: allowlist secret"];

/// Hex-candidate lengths that are exactly a common digest's hex encoding:
/// SHA-1 (40), SHA-224 (56), SHA-256/BLAKE3 (64), SHA-384 (96),
/// SHA-512/BLAKE2b (128). MD5 (32) sits below the 40-char entropy floor and
/// never becomes a candidate.
const DIGEST_HEX_LENGTHS: [usize; 5] = [40, 56, 64, 96, 128];

/// Case-insensitive line-context keywords that mark an exact-digest-length
/// hex candidate as a digest fixture rather than a secret. Every keyword
/// contains at least one non-hex letter, so none can match inside the hex
/// candidate itself. Deliberately narrow: `hmac` is absent (an HMAC *key* is
/// a secret), and `integrity`/`commit` contexts stay on the baseline path.
const DIGEST_CONTEXT_KEYWORDS: [&str; 7] = [
    "sha",
    "blake",
    "digest",
    "checksum",
    "fingerprint",
    "etag",
    "hash",
];

/// Result of [`Scanner::scan_bytes_partitioned`]: `detections` are
/// actionable; `inline_allowed` were suppressed by an inline allow-marker on
/// their own line and are returned so callers can audit the operator
/// decision (mirroring the baseline-match audit trail).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartitionedScan {
    pub detections: Vec<Detection>,
    pub inline_allowed: Vec<Detection>,
}

/// Metadata for one named secret detector.
#[derive(Debug, Clone)]
pub struct PatternMeta {
    pub detect_secrets_type: DetectSecretsRule,
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
            entropy_b64_re: Regex::new(r"[A-Za-z0-9+/]{20,}={0,2}")
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

    /// [`Self::scan_bytes`], then partition out detections whose line carries
    /// one of [`INLINE_ALLOW_MARKERS`]. `scan_bytes` itself stays
    /// policy-free (raw detection); this is the policy-aware entry point the
    /// pre-ingest scan uses.
    #[must_use]
    pub fn scan_bytes_partitioned(&self, buf: &[u8]) -> PartitionedScan {
        let (inline_allowed, detections) = self
            .scan_bytes(buf)
            .into_iter()
            .partition(|detection| line_has_inline_allow_marker(buf, detection.byte_offset));
        PartitionedScan {
            detections,
            inline_allowed,
        }
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
                && !hex_candidate_is_digest_fixture(bytes, candidate.start(), candidate.end())
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

/// An exact-digest-length hex candidate whose line names a digest context is
/// a digest fixture, not a secret (ADR-013 amendment 2026-08-31). This only
/// gates the entropy rule: a digest-keyword line whose literal is a real
/// credential assignment still fires the named/keyword detectors, whose
/// matches were excluded from entropy candidacy before this check runs.
fn hex_candidate_is_digest_fixture(bytes: &[u8], start: usize, end: usize) -> bool {
    if !DIGEST_HEX_LENGTHS.contains(&end.saturating_sub(start)) {
        return false;
    }
    let (line_start, line_end) = line_bounds(bytes, start);
    let line_lower = bytes[line_start..line_end].to_ascii_lowercase();
    DIGEST_CONTEXT_KEYWORDS
        .iter()
        .any(|keyword| contains_subslice(&line_lower, keyword.as_bytes()))
}

fn line_has_inline_allow_marker(bytes: &[u8], offset: usize) -> bool {
    let (line_start, line_end) = line_bounds(bytes, offset);
    let line = &bytes[line_start..line_end];
    INLINE_ALLOW_MARKERS
        .iter()
        .any(|marker| contains_subslice(line, marker.as_bytes()))
}

/// `[start, end)` of the line containing `offset`, excluding the newline.
fn line_bounds(bytes: &[u8], offset: usize) -> (usize, usize) {
    let offset = offset.min(bytes.len());
    let start = bytes[..offset]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |pos| pos + 1);
    let end = bytes[offset..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |pos| offset + pos);
    (start, end)
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
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
