use clarion_scanner::{Baseline, BaselineError, Scanner, load_baseline};
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

fn hex20(bytes: [u8; 20]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::new();
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
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
