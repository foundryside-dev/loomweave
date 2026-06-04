//! `clarion sarif import` integration tests.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;

use assert_cmd::Command;

fn clarion_bin() -> Command {
    let mut cmd = Command::cargo_bin("clarion").expect("clarion binary");
    cmd.env(
        "CLARION_CODEX_CONFIG",
        std::env::temp_dir().join(format!(
            "clarion-test-codex-config-{}.toml",
            std::process::id()
        )),
    );
    cmd
}

#[test]
#[allow(clippy::too_many_lines)]
fn sarif_import_posts_findings_to_mock_filigree() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("local addr");

    // Spawn a thread to receive the HTTP request from clarion sarif import
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut request = vec![0_u8; 8192];
        let read = stream.read(&mut request).expect("read request");
        let request_str = String::from_utf8_lossy(&request[..read]);

        assert!(request_str.contains("POST /api/v1/scan-results HTTP/1.1"));
        assert!(request_str.contains("authorization: Bearer my-mock-token"));
        assert!(request_str.contains("x-filigree-actor: my-actor"));

        // Verify finding content in request body
        assert!(
            request_str.contains("\"scan_source\":\"semgrep\""),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"path\":\"src/lib.rs\""),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"rule_id\":\"semgrep-rule-1\""),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"severity\":\"high\""),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"line_start\":42"),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"line_end\":45"),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"sarif_properties\":{\"confidence\":\"HIGH\"}"),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"fingerprint\":\"a-fingerprint\""),
            "first deterministic partial fingerprint should be promoted: {request_str}"
        );
        assert!(
            request_str.contains(
                "\"partial_fingerprints\":{\"aKey\":\"a-fingerprint\",\"zKey\":\"z-fingerprint\"}"
            ),
            "full SARIF partialFingerprints object should be preserved in metadata: {request_str}"
        );
        assert!(
            request_str.contains("\"kind\":\"defect\""),
            "body: {request_str}"
        );

        let body = r#"{"files_created":1,"files_updated":0,"findings_created":1,"findings_updated":0,"new_finding_ids":["f-abc"],"observations_created":0,"observations_failed":0,"warnings":[]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write response");
    });

    let dir = tempfile::tempdir().unwrap();

    // Write a mock clarion.yaml config
    let config_content = format!(
        r#"
integrations:
  filigree:
    enabled: true
    base_url: "http://{addr}"
    actor: "my-actor"
    token_env: "TEST_FILIGREE_TOKEN"
"#
    );
    fs::write(dir.path().join("clarion.yaml"), config_content).unwrap();

    // Create a dummy .clarion dir so it passes the project layout checks
    fs::create_dir_all(dir.path().join(".clarion")).unwrap();

    // Write a mock SARIF file
    let sarif_content = r#"{
      "version": "2.1.0",
      "runs": [
        {
          "tool": {
            "driver": {
              "name": "Semgrep",
              "version": "1.0"
            }
          },
          "results": [
            {
              "ruleId": "semgrep-rule-1",
              "message": {
                "text": "suspicious pattern detected"
              },
              "level": "error",
              "locations": [
                {
                  "physicalLocation": {
                    "artifactLocation": {
                      "uri": "src/lib.rs"
                    },
                    "region": {
                      "startLine": 42,
                      "endLine": 45
                    }
                  }
                }
              ],
              "partialFingerprints": {
                "zKey": "z-fingerprint",
                "aKey": "a-fingerprint"
              },
              "properties": {
                "confidence": "HIGH"
              }
            }
          ]
        }
      ]
    }"#;
    let sarif_path = dir.path().join("semgrep.sarif");
    fs::write(&sarif_path, sarif_content).unwrap();

    // Run the cli command
    clarion_bin()
        .env("TEST_FILIGREE_TOKEN", "my-mock-token")
        .args(["sarif", "import"])
        .arg(&sarif_path)
        .arg("--path")
        .arg(dir.path())
        .assert()
        .success();

    handle.join().unwrap();
}

#[test]
fn sarif_import_prefers_wardline_partial_fingerprint() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("local addr");

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut request = vec![0_u8; 8192];
        let read = stream.read(&mut request).expect("read request");
        let request_str = String::from_utf8_lossy(&request[..read]);

        assert!(
            request_str.contains("\"scan_source\":\"wardline-sarif\""),
            "body: {request_str}"
        );
        assert!(
            request_str.contains("\"fingerprint\":\"wardline-fp\""),
            "wardlineFingerprint/v1 should win over lexicographic fallback: {request_str}"
        );
        assert!(
            request_str.contains(
                "\"partial_fingerprints\":{\"aKey\":\"fallback-fp\",\"wardlineFingerprint/v1\":\"wardline-fp\"}"
            ),
            "full partialFingerprints object should be preserved: {request_str}"
        );

        let body = r#"{"files_created":1,"files_updated":0,"findings_created":1,"findings_updated":0,"new_finding_ids":["f-abc"],"observations_created":0,"observations_failed":0,"warnings":[]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write response");
    });

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("clarion.yaml"),
        format!(
            r#"
integrations:
  filigree:
    enabled: true
    base_url: "http://{addr}"
    actor: "my-actor"
    token_env: "TEST_FILIGREE_TOKEN"
"#
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".clarion")).unwrap();
    let sarif_content = r#"{
      "version": "2.1.0",
      "runs": [{
        "tool": {"driver": {"name": "Wardline", "version": "1.0"}},
        "results": [{
          "ruleId": "PY-WL-101",
          "message": {"text": "tainted flow"},
          "level": "error",
          "locations": [{
            "physicalLocation": {
              "artifactLocation": {"uri": "sampleapp/trust_flow.py"},
              "region": {"startLine": 41, "endLine": 44}
            }
          }],
          "partialFingerprints": {
            "aKey": "fallback-fp",
            "wardlineFingerprint/v1": "wardline-fp"
          },
          "properties": {"qualname": "sampleapp.trust_flow.unsafe_account_key"}
        }]
      }]
    }"#;
    let sarif_path = dir.path().join("wardline.sarif");
    fs::write(&sarif_path, sarif_content).unwrap();

    clarion_bin()
        .env("TEST_FILIGREE_TOKEN", "my-mock-token")
        .args(["sarif", "import"])
        .arg(&sarif_path)
        .args(["--scan-source", "wardline-sarif"])
        .arg("--path")
        .arg(dir.path())
        .assert()
        .success();

    handle.join().unwrap();
}
