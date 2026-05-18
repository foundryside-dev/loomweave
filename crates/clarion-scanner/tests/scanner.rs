use clarion_scanner::{Baseline, BaselineError, Scanner, load_baseline};

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
    assert_detects("-----BEGIN OPENSSH PRIVATE KEY-----", "PrivateKeyHeader");
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
    assert_not_detects(
        "checksum = 47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
        "HighEntropyBase64",
    );
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
    assert_ne!(detection.hashed_secret, [0u8; 20]);
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

    let result = baseline.suppress(detections, std::path::Path::new("/repo/src/demo.py"));
    assert!(result.allowed.is_empty());
    assert_eq!(result.suppressed.len(), 1);
    assert_eq!(result.fired_entries.len(), 1);
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
    assert!(matches!(
        err,
        BaselineError::MissingJustification { line: 42, .. }
    ));
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

fn hex20(bytes: [u8; 20]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::new();
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}
