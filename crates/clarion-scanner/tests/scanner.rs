use clarion_scanner::{
    Baseline, BaselineError, DetectSecretsRule, EntropyTuning, HashedSecret, Scanner, load_baseline,
};
use sha1::{Digest, Sha1};

fn rules_for(input: &str) -> Vec<&'static str> {
    Scanner::new()
        .scan_bytes(input.as_bytes())
        .into_iter()
        .map(|detection| detection.rule_id)
        .collect()
}

fn assert_detects(input: &str, rule_id: &str) {
    let rules = rules_for(input);
    assert!(rules.contains(&rule_id), "{rule_id} not found in {rules:?}");
}

fn assert_not_detects(input: &str, rule_id: &str) {
    let rules = rules_for(input);
    assert!(
        !rules.contains(&rule_id),
        "{rule_id} unexpectedly found in {rules:?}"
    );
}

#[test]
fn detect_secrets_rule_owns_rule_id_strings() {
    assert_eq!(DetectSecretsRule::AwsAccessKey.rule_id(), "AwsAccessKeyId");
    assert_eq!(DetectSecretsRule::OpenAiApiKey.rule_id(), "OpenAiApiKey");
    assert_eq!(
        DetectSecretsRule::Base64HighEntropyString.rule_id(),
        "HighEntropyBase64"
    );
}

#[test]
fn named_patterns_detect_expected_credentials() {
    assert_detects(
        "aws_access_key_id = 'AKIAIOSFODNN7EXAMPLE'",
        "AwsAccessKeyId",
    );
    assert_detects(
        "aws_secret_access_key = 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY'",
        "AwsSecretAccessKey",
    );
    assert_detects(
        "token = 'ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ'",
        "GitHubPat",
    );
    assert_detects(
        &format!("token = 'github_pat_{}'", "a".repeat(82)),
        "GitHubFineGrainedPat",
    );
    assert_detects(
        "token = 'gho_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ'",
        "GitHubOAuth",
    );
    assert_detects(
        &format!("key = 'sk-ant-{}'", "A".repeat(90)),
        "AnthropicApiKey",
    );
    assert_detects(&format!("key = 'sk-{}'", "A".repeat(48)), "OpenAiApiKey");
    assert_detects("stripe = 'sk_live_abcdefghijklmnop'", "StripeApiKey");
    assert_detects("slack = 'xoxb-123456789012-abcdefghi'", "SlackToken");
    assert_detects(
        "jwt = 'eyJabcdef12345.eyJabcdef67890.eyJabcdef99999'",
        "JwtToken",
    );
    assert_detects("-----BEGIN RSA PRIVATE KEY-----", "PrivateKeyHeader");
    assert_detects("-----BEGIN DSA PRIVATE KEY-----", "PrivateKeyHeader");
    assert_detects("-----BEGIN OPENSSH PRIVATE KEY-----", "PrivateKeyHeader");
    assert_detects("-----BEGIN PRIVATE KEY-----", "PrivateKeyHeader");
    assert_detects("-----BEGIN PGP PRIVATE KEY BLOCK-----", "PrivateKeyHeader");
}

#[test]
fn named_patterns_ignore_near_misses() {
    assert_not_detects("AKIAIOSFODNN7EXAMPL", "AwsAccessKeyId");
    assert_not_detects("ghp_short", "GitHubPat");
    assert_not_detects("sk-not-long-enough", "OpenAiApiKey");
    assert_not_detects("-----BEGIN PUBLIC KEY-----", "PrivateKeyHeader");
}

#[test]
fn entropy_detection_has_expected_bounds() {
    assert_detects(
        "secret = AbCdEfGhIjKlMnOpQrStUvWxYz123456+/",
        "HighEntropyBase64",
    );
    assert_detects(
        "digest = 0123456789abcdefABCDEF0123456789abcdefABCDEF0123456789abcdef",
        "HighEntropyHex",
    );
    assert_not_detects(
        "uuid = 123e4567-e89b-12d3-a456-426614174000",
        "HighEntropyHex",
    );
    assert_detects(
        "checksum = 47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
        "HighEntropyBase64",
    );
}

#[test]
fn padded_base64_detection_hashes_the_full_literal_for_baselines() {
    let secret = "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    let source = format!("checksum = {secret}\n");
    let detections = Scanner::new().scan_bytes(source.as_bytes());
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "HighEntropyBase64")
        .expect("padded base64 detection")
        .clone();

    assert_eq!(detection.matched_len, secret.len());
    assert_eq!(hex20(detection.hashed_secret), sha1_hex(secret.as_bytes()));

    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "src/checksums.txt":
    - type: "Base64 High Entropy String"
      hashed_secret: "{}"
      line_number: 1
      is_secret: false
      justification: "Known non-secret digest checked into the fixture."
"#,
        sha1_hex(secret.as_bytes())
    ))
    .expect("baseline parses");

    let result = baseline.suppress(detections, std::path::Path::new("src/checksums.txt"));
    assert!(result.allowed.is_empty());
    assert_eq!(result.suppressed.len(), 1);
}

#[test]
fn contextual_credentials_detect_assignments_but_not_comments_or_hashes() {
    assert_detects(
        "password = \"correct-horse-battery-staple\"",
        "ContextualCredential",
    );
    assert_detects("api_key: \"abcDEF1234567890\"", "ContextualCredential");
    assert_detects(
        "SECRET_TOKEN := \"abcDEF1234567890\"",
        "ContextualCredential",
    );
    assert_not_detects(
        "password_hash = \"abcDEF1234567890\"",
        "ContextualCredential",
    );
    assert_not_detects("# password = \"abcDEF1234567890\"", "ContextualCredential");
    assert_detects("// password = \"abcDEF1234567890\"", "ContextualCredential");
    assert_detects(
        "/* password = \"abcDEF1234567890\" */",
        "ContextualCredential",
    );
}

#[test]
fn entropy_minimum_lengths_are_pinned() {
    assert_eq!(EntropyTuning::BASE64.min_len, 20);
    assert_eq!(EntropyTuning::HEX.min_len, 40);
}

#[test]
fn detection_records_line_and_sha1_hash_without_literal() {
    let detections = Scanner::new().scan_bytes(b"clean\nkey = 'AKIAIOSFODNN7EXAMPLE'\n");
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS key detection");
    assert_eq!(detection.line_number, 2);
    assert_eq!(detection.matched_len, "AKIAIOSFODNN7EXAMPLE".len());
    assert_ne!(detection.hashed_secret.as_bytes(), &[0u8; 20]);
}

#[test]
fn hashed_secret_round_trips_hex_display() {
    let hash = HashedSecret::from_hex("0123456789abcdef0123456789abcdef01234567")
        .expect("valid SHA-1 hex");

    assert_eq!(hash.to_string(), "0123456789abcdef0123456789abcdef01234567");
    assert!(HashedSecret::from_hex("not-a-sha1").is_err());
}

#[test]
fn scan_bytes_reports_offsets_in_original_non_utf8_buffer() {
    let input = b"\xff\xffkey = 'AKIAIOSFODNN7EXAMPLE'\n";
    let detections = Scanner::new().scan_bytes(input);
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS key detection");
    let expected_offset = input
        .windows(b"AKIAIOSFODNN7EXAMPLE".len())
        .position(|window| window == b"AKIAIOSFODNN7EXAMPLE")
        .expect("secret literal is in fixture");

    assert_eq!(detection.byte_offset, expected_offset);
    assert_eq!(
        hex20(detection.hashed_secret),
        sha1_hex(b"AKIAIOSFODNN7EXAMPLE")
    );
}

#[test]
fn baseline_suppresses_matching_detection_and_reports_fired_entry() {
    let scanner = Scanner::new();
    let detections = scanner.scan_bytes(b"key = 'AKIAIOSFODNN7EXAMPLE'\n");
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection")
        .clone();
    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Access Key"
      hashed_secret: "{}"
      line_number: 1
      is_secret: false
      justification: "Documented public AWS example key."
"#,
        hex20(detection.hashed_secret)
    ))
    .expect("baseline parses");

    let result = baseline.suppress(detections, std::path::Path::new("src/demo.py"));
    assert!(result.allowed.is_empty());
    assert_eq!(result.suppressed.len(), 1);
    assert_eq!(result.fired_entries.len(), 1);
}

#[test]
fn baseline_does_not_suppress_when_detector_type_differs() {
    let scanner = Scanner::new();
    let detections = scanner.scan_bytes(b"key = 'AKIAIOSFODNN7EXAMPLE'\n");
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection")
        .clone();
    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "GitHub Token"
      hashed_secret: "{}"
      line_number: 1
      is_secret: false
      justification: "Different detector type must not suppress this match."
"#,
        hex20(detection.hashed_secret)
    ))
    .expect("baseline parses");

    let result = baseline.suppress(detections, std::path::Path::new("src/demo.py"));
    assert_eq!(result.allowed.len(), 1);
    assert!(result.suppressed.is_empty());
    assert!(result.fired_entries.is_empty());
}

#[test]
fn baseline_rejects_unknown_detector_types() {
    let err = Baseline::from_yaml_str(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Acceess Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 42
      is_secret: false
      justification: "Typo should not silently lose suppression."
"#,
    )
    .expect_err("unknown detector type should fail");

    assert!(
        err.to_string().contains("unsupported detector type"),
        "unexpected error: {err}"
    );
}

#[test]
fn baseline_without_explicit_is_secret_false_does_not_suppress() {
    let scanner = Scanner::new();
    let detections = scanner.scan_bytes(b"key = 'AKIAIOSFODNN7EXAMPLE'\n");
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection")
        .clone();
    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Access Key"
      hashed_secret: "{}"
      line_number: 1
      justification: "The operator did not explicitly mark this as not secret."
"#,
        hex20(detection.hashed_secret)
    ))
    .expect("baseline parses");

    let result = baseline.suppress(detections, std::path::Path::new("src/demo.py"));
    assert_eq!(result.allowed.len(), 1);
    assert!(result.suppressed.is_empty());
    assert!(result.fired_entries.is_empty());
}

#[test]
fn baseline_does_not_suppress_same_suffix_in_sibling_directory() {
    let scanner = Scanner::new();
    let detections = scanner.scan_bytes(b"key = 'AKIAIOSFODNN7EXAMPLE'\n");
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection")
        .clone();
    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Access Key"
      hashed_secret: "{}"
      line_number: 1
      is_secret: false
      justification: "Only src/demo.py was reviewed."
"#,
        hex20(detection.hashed_secret)
    ))
    .expect("baseline parses");

    let result = baseline.suppress(detections, std::path::Path::new("vendor/src/demo.py"));
    assert_eq!(result.allowed.len(), 1);
    assert!(result.suppressed.is_empty());
    assert!(result.fired_entries.is_empty());
}

#[test]
fn baseline_is_secret_true_does_not_suppress() {
    let scanner = Scanner::new();
    let detections = scanner.scan_bytes(b"key = 'AKIAIOSFODNN7EXAMPLE'\n");
    let detection = detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection")
        .clone();
    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "demo.py":
    - type: "AWS Access Key"
      hashed_secret: "{}"
      line_number: 1
      is_secret: true
      justification: "Still a real secret."
"#,
        hex20(detection.hashed_secret)
    ))
    .expect("baseline parses");

    let result = baseline.suppress(detections, std::path::Path::new("demo.py"));
    assert_eq!(result.allowed.len(), 1);
    assert!(result.suppressed.is_empty());
}

#[test]
fn baseline_missing_justification_errors() {
    let err = Baseline::from_yaml_str(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Access Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 42
      is_secret: false
"#,
    )
    .expect_err("missing justification should fail");
    let BaselineError::MissingJustifications { entries } = err else {
        panic!("expected MissingJustifications, got {err:?}");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].line, 42);
}

#[test]
fn baseline_missing_justification_reports_all_entries() {
    let err = Baseline::from_yaml_str(
        r#"
version: "1.0"
results:
  "src/one.py":
    - type: "AWS Access Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 4
      is_secret: false
  "src/two.py":
    - type: "AWS Access Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 8
      is_secret: false
"#,
    )
    .expect_err("all missing justifications should be reported");
    let BaselineError::MissingJustifications { entries } = err else {
        panic!("expected MissingJustifications, got {err:?}");
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].line, 4);
    assert_eq!(entries[1].line, 8);
}

#[test]
fn baseline_rejects_absolute_result_paths() {
    let err = Baseline::from_yaml_str(
        r#"
version: "1.0"
results:
  "/tmp/secret.py":
    - type: "AWS Access Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 4
      is_secret: false
      justification: "Invalid path must not be accepted."
"#,
    )
    .expect_err("absolute baseline path should fail");
    assert!(
        err.to_string().contains("repository-relative"),
        "unexpected error: {err}"
    );
}

#[test]
fn baseline_rejects_parent_dir_result_paths() {
    let err = Baseline::from_yaml_str(
        r#"
version: "1.0"
results:
  "../secret.py":
    - type: "AWS Access Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 4
      is_secret: false
      justification: "Invalid path must not be accepted."
"#,
    )
    .expect_err("escaping baseline path should fail");
    assert!(
        err.to_string().contains("repository-relative"),
        "unexpected error: {err}"
    );
}

#[test]
fn absent_baseline_file_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let baseline = load_baseline(&dir.path().join(".clarion/secrets-baseline.yaml"))
        .expect("missing baseline is accepted");
    assert!(baseline.entries().is_empty());
}

#[test]
fn baseline_round_trips_through_yaml() {
    let raw = r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Access Key"
      hashed_secret: "0123456789abcdef0123456789abcdef01234567"
      line_number: 42
      is_secret: false
      justification: "Example key."
"#;
    let parsed = Baseline::from_yaml_str(raw).expect("parse baseline");
    let rendered = parsed.to_yaml_string().expect("serialize baseline");
    let reparsed = Baseline::from_yaml_str(&rendered).expect("reparse baseline");
    assert_eq!(parsed, reparsed);
}

/// Regression net for the gap noted in PR #11 review (clarion-55fc5aa885 §I6):
/// baseline suppression keys on (`hashed_secret`, `line_number`, `rule_type`).
/// A baseline entry at line 1 with one hash MUST NOT suppress a *different*
/// detection at line 1 — that would be a silent regression where a benign
/// stub gets replaced by a real secret at the same offset and the gate
/// stops firing.
#[test]
fn baseline_does_not_suppress_when_hash_drifts_at_same_line() {
    let scanner = Scanner::new();
    // Original benign content the operator baselined.
    let benign_detections = scanner.scan_bytes(b"key = 'AKIAIOSFODNN7EXAMPLE'\n");
    let benign = benign_detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection in benign content")
        .clone();
    // Operator commits a baseline acknowledging the benign secret on line 1.
    let baseline = Baseline::from_yaml_str(&format!(
        r#"
version: "1.0"
results:
  "src/demo.py":
    - type: "AWS Access Key"
      hashed_secret: "{}"
      line_number: 1
      is_secret: false
      justification: "Documented public AWS example key."
"#,
        hex20(benign.hashed_secret)
    ))
    .expect("baseline parses");

    // Later, the file is mutated: same line, same rule, *different* secret.
    let drifted_detections = scanner.scan_bytes(b"key = 'AKIAJONOTTHESAMEAS18'\n");
    let drifted = drifted_detections
        .iter()
        .find(|detection| detection.rule_id == "AwsAccessKeyId")
        .expect("AWS detection in drifted content")
        .clone();
    assert_ne!(
        benign.hashed_secret, drifted.hashed_secret,
        "test premise: drifted content must hash differently",
    );

    let result = baseline.suppress(vec![drifted], std::path::Path::new("src/demo.py"));
    assert_eq!(
        result.allowed.len(),
        1,
        "baseline must NOT suppress a drifted hash at the same line — that is \
         the security regression CLA-SEC-SECRET-DETECTED exists to catch",
    );
    assert!(result.suppressed.is_empty());
    assert!(result.fired_entries.is_empty());
}

/// Regression net for the gap noted in PR #11 review (clarion-55fc5aa885 §I7):
/// `HighEntropyHex` at 40 chars / entropy ≥ 3.0 will fire on git SHA-1 hashes,
/// blake3 hex digests, and lockfile integrity fields. The accepted v0.1 path
/// is *not* to tighten the rule (which would let real low-entropy secrets
/// through) but to document the operator-baseline workflow as the
/// resolution — exactly what this fixture asserts.
///
/// Tightening the entropy floor risks missing real secrets that happen to
/// look hash-like; the baseline path is per-(rule,file,line,hash) and is the
/// existing escape hatch ADR-013 §"Operator baseline" describes.
#[test]
fn high_entropy_hex_fires_on_lockfile_shas_but_baseline_suppresses_them() {
    // Lockfile integrity hash (npm-style sha512-truncated to hex) and a git
    // SHA-1. Both are 40-char hex, entropy ≈ log2(16) ≈ 4 — well above the
    // HighEntropyHex floor.
    let lockfile_payload = b"\"integrity\": \"a3f5e8c2b1d4f0967e8c2a1b5d3e0f4a6c8b2d1e\"";
    let git_sha = b"commit a3f5e8c2b1d4f0967e8c2a1b5d3e0f4a6c8b2d1e\n";

    let scanner = Scanner::new();
    let lockfile_detections = scanner.scan_bytes(lockfile_payload);
    let git_detections = scanner.scan_bytes(git_sha);
    let lockfile_hex = lockfile_detections
        .iter()
        .find(|detection| detection.rule_id == "HighEntropyHex")
        .cloned();
    let git_hex = git_detections
        .iter()
        .find(|detection| detection.rule_id == "HighEntropyHex")
        .cloned();

    let lockfile_hex = lockfile_hex.expect(
        "HighEntropyHex fires on lockfile integrity hash — the regression \
         net is the operator-baseline workflow, not a tightened rule",
    );
    let git_hex = git_hex.expect(
        "HighEntropyHex fires on git SHA-1 — the regression net is the \
         operator-baseline workflow, not a tightened rule",
    );

    // Operator commits a baseline pinning both the lockfile and git SHA on
    // their respective files. The same workflow scales to blake3 hex
    // digests and any other hash-like content.
    let baseline_yaml = format!(
        r#"
version: "1.0"
results:
  "package-lock.json":
    - type: "Hex High Entropy String"
      hashed_secret: "{lock_hash}"
      line_number: 1
      is_secret: false
      justification: "npm lockfile integrity hash, not a credential."
  "git-log.txt":
    - type: "Hex High Entropy String"
      hashed_secret: "{git_hash}"
      line_number: 1
      is_secret: false
      justification: "Git commit SHA, not a credential."
"#,
        lock_hash = hex20(lockfile_hex.hashed_secret),
        git_hash = hex20(git_hex.hashed_secret),
    );
    let baseline = Baseline::from_yaml_str(&baseline_yaml).expect("baseline parses");

    let lockfile_result = baseline.suppress(
        vec![lockfile_hex],
        std::path::Path::new("package-lock.json"),
    );
    let git_result = baseline.suppress(vec![git_hex], std::path::Path::new("git-log.txt"));

    assert!(
        lockfile_result.allowed.is_empty(),
        "operator-baseline must suppress the lockfile integrity-hash false positive",
    );
    assert!(
        git_result.allowed.is_empty(),
        "operator-baseline must suppress the git SHA false positive",
    );
    assert_eq!(lockfile_result.fired_entries.len(), 1);
    assert_eq!(git_result.fired_entries.len(), 1);
}

fn hex20(hash: HashedSecret) -> String {
    hash.to_string()
}

fn sha1_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::new();
    for byte in digest {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}
