# Flow B — Read-Time Wardline Finding Reconciliation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When `issues_for` / `orientation_pack` runs for an entity, surface the Wardline findings Filigree holds for that entity, reconciled by qualname — enrich-only, no new Filigree route.

**Architecture:** A new `wardline_reconcile` module does pure qualname matching (`metadata.wardline.qualname` == the entity_id's segment-3 qualname). The Filigree HTTP client (`filigree.rs`) gains a two-hop read (`GET /api/loom/files?path_prefix=` → Filigree `file_id`, then `GET /api/loom/findings?scan_source=wardline&file_id=`). The two MCP tools call the client, reconcile, and attach a `wardline_findings` section. If Filigree is unreachable the section degrades to `unavailable`; the tool never fails.

**Tech Stack:** Rust, `reqwest::blocking`, `serde`/`serde_json`, `cargo nextest`. Spec: `docs/superpowers/specs/2026-05-30-clarion-consume-wardline-data-design.md`. Tracked by `clarion-71f995b88a`.

---

## File Structure

- **Create** `crates/clarion-mcp/src/wardline_reconcile.rs` — pure reconciliation: qualname extraction from an entity_id, qualname extraction from a finding's metadata, the `ResolutionConfidence` enum, and `reconcile_for_entity`. No I/O; fully unit-testable.
- **Modify** `crates/clarion-mcp/src/filigree.rs` — add the `WardlineFinding` / `LoomFileRecord` wire types + parsers, the `loom_files_url` / `loom_findings_url` builders, a private `get_json` helper, and `FiligreeLookup::wardline_findings_for_path` (default `Ok(vec![])`; HTTP client does the two-hop).
- **Modify** `crates/clarion-mcp/src/lib.rs` — register `mod wardline_reconcile`; build the `wardline_findings` section in `tool_issues_for` and `tool_orientation_pack`.
- **Modify** `docs/federation/contracts.md` — pin the two consumed loom read routes.

Each task is independently committable. Run the full crate gate after the last code task: `cargo fmt --all -- --check`, `cargo clippy -p clarion-mcp --all-targets -- -D warnings`, `cargo nextest run -p clarion-mcp`.

---

## Task 1: Wardline + loom-file wire types and parsers

**Files:**
- Modify: `crates/clarion-mcp/src/filigree.rs` (add types near the other `Deserialize` structs, ~line 38; tests in the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing parse tests**

Add to `mod tests` in `crates/clarion-mcp/src/filigree.rs`:

```rust
#[test]
fn parses_loom_findings_list_envelope() {
    let resp = parse_wardline_findings_response(
        r#"{"items":[
            {"finding_id":"f-1","file_id":"file-9","severity":"high","status":"open",
             "scan_source":"wardline","rule_id":"WLN-TAINT-001","message":"tainted sink",
             "suggestion":"","scan_run_id":"r-1","line_start":12,"line_end":12,
             "fingerprint":"fp-abc","issue_id":null,"seen_count":1,
             "metadata":{"wardline":{"qualname":"demo.Foo.bar","kind":"DEFECT"}},
             "data_warnings":[]}
        ],"has_more":false}"#,
    )
    .expect("parse findings list");
    assert_eq!(resp.items.len(), 1);
    let f = &resp.items[0];
    assert_eq!(f.rule_id, "WLN-TAINT-001");
    assert_eq!(f.fingerprint.as_deref(), Some("fp-abc"));
    assert_eq!(f.line_start, Some(12));
    assert_eq!(
        f.metadata.get("wardline").and_then(|w| w.get("qualname")).and_then(|q| q.as_str()),
        Some("demo.Foo.bar")
    );
}

#[test]
fn parses_loom_files_list_envelope() {
    let resp = parse_loom_files_response(
        r#"{"items":[
            {"file_id":"file-9","path":"src/demo.py","language":"python","file_type":"source"},
            {"file_id":"file-10","path":"src/demo_helpers.py","language":"python","file_type":"source"}
        ],"has_more":false}"#,
    )
    .expect("parse files list");
    assert_eq!(resp.items.len(), 2);
    assert_eq!(resp.items[0].file_id, "file-9");
    assert_eq!(resp.items[0].path, "src/demo.py");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p clarion-mcp filigree::tests::parses_loom`
Expected: FAIL to compile — `parse_wardline_findings_response`, `parse_loom_files_response`, and the types are not defined.

- [ ] **Step 3: Add the types and parsers**

Add to `crates/clarion-mcp/src/filigree.rs` (after `EntityAssociation`, ~line 38). Extra fields in the Filigree rows are ignored by serde, so this reads only the subset Clarion surfaces:

```rust
/// One Wardline finding as Clarion surfaces it — the subset of Filigree's
/// `ScanFindingLoom` (`GET /api/loom/findings`) used for read-time
/// reconciliation. Unknown fields are ignored so Filigree can grow the row.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WardlineFinding {
    pub rule_id: String,
    pub message: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub line_start: Option<i64>,
    #[serde(default)]
    pub line_end: Option<i64>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
    /// The finding's `metadata` object; `metadata.wardline.qualname` is the
    /// reconciliation key. Defaults to JSON null when absent.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WardlineFindingsResponse {
    #[serde(default)]
    pub items: Vec<WardlineFinding>,
}

/// One row of `GET /api/loom/files` — only the fields needed to map a path to
/// Filigree's `file_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoomFileRecord {
    pub file_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoomFilesResponse {
    #[serde(default)]
    pub items: Vec<LoomFileRecord>,
}

pub fn parse_wardline_findings_response(
    body: &str,
) -> Result<WardlineFindingsResponse, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
}

pub fn parse_loom_files_response(body: &str) -> Result<LoomFilesResponse, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p clarion-mcp filigree::tests::parses_loom`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/clarion-mcp/src/filigree.rs
git commit -m "feat(mcp): Wardline finding + loom-file wire types (Flow B, clarion-71f995b88a)"
```

---

## Task 2: Pure qualname reconciliation module

**Files:**
- Create: `crates/clarion-mcp/src/wardline_reconcile.rs`
- Modify: `crates/clarion-mcp/src/lib.rs` (add `mod wardline_reconcile;` next to the other `mod` declarations near the top)

- [ ] **Step 1: Write the failing tests**

Create `crates/clarion-mcp/src/wardline_reconcile.rs` with only the tests first:

```rust
//! Reconcile Wardline findings to Clarion entities by qualname (Flow B).
//!
//! `metadata.wardline.qualname` is the pre-composed dotted name, which for a
//! function/method entity is byte-identical to the entity_id's segment-3
//! `canonical_qualified_name` (proven by `fixtures/entity_id.json`). Matching is
//! therefore a local string compare against Clarion's own catalog — no oracle.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filigree::WardlineFinding;

    fn finding(qualname: Option<&str>) -> WardlineFinding {
        let metadata = match qualname {
            Some(q) => serde_json::json!({ "wardline": { "qualname": q } }),
            None => serde_json::json!({ "wardline": { "kind": "DEFECT" } }),
        };
        WardlineFinding {
            rule_id: "WLN-X".to_owned(),
            message: "m".to_owned(),
            severity: Some("high".to_owned()),
            status: Some("open".to_owned()),
            line_start: Some(1),
            line_end: Some(1),
            fingerprint: Some("fp".to_owned()),
            file_id: Some("file-1".to_owned()),
            metadata,
        }
    }

    #[test]
    fn extracts_segment_three_qualname_incl_locals_and_nested() {
        assert_eq!(entity_qualname("python:function:demo.Foo.bar"), Some("demo.Foo.bar"));
        assert_eq!(
            entity_qualname("python:function:demo.outer.<locals>.inner"),
            Some("demo.outer.<locals>.inner")
        );
        assert_eq!(entity_qualname("python:function:hello"), Some("hello"));
        assert_eq!(entity_qualname("python:module:"), None); // empty qualname
        assert_eq!(entity_qualname("notanid"), None);
    }

    #[test]
    fn exact_match_binds_other_qualname_is_none() {
        let r = reconcile_for_entity(
            "python:function:demo.Foo.bar",
            vec![finding(Some("demo.Foo.bar")), finding(Some("demo.other"))],
        );
        assert_eq!(r.matched.len(), 1);
        assert_eq!(r.matched[0].resolution_confidence, ResolutionConfidence::Exact);
        assert_eq!(r.omitted_no_qualname, 0);
    }

    #[test]
    fn missing_qualname_is_counted_omitted_not_matched() {
        let r = reconcile_for_entity("python:function:demo.Foo.bar", vec![finding(None)]);
        assert!(r.matched.is_empty());
        assert_eq!(r.omitted_no_qualname, 1);
    }

    #[test]
    fn unparseable_entity_id_yields_empty_no_panic() {
        let r = reconcile_for_entity("notanid", vec![finding(Some("demo.Foo.bar"))]);
        assert!(r.matched.is_empty());
        assert_eq!(r.omitted_no_qualname, 0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p clarion-mcp wardline_reconcile`
Expected: FAIL to compile — module not registered, symbols undefined.

- [ ] **Step 3: Implement the module**

Add `mod wardline_reconcile;` to `crates/clarion-mcp/src/lib.rs` (with the other module declarations), then prepend the implementation above the `#[cfg(test)]` block in `wardline_reconcile.rs`:

```rust
use crate::filigree::WardlineFinding;

/// A Wardline finding's resolution against a Clarion entity. v1 produces only
/// `Exact` (byte-equal qualname) or `None`. `Heuristic` is reserved for a future
/// best-effort normalization pass and is never returned yet — kept in the enum
/// so the wire shape is stable when it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolutionConfidence {
    Exact,
    Heuristic,
    None,
}

#[derive(Debug, Clone)]
pub struct MatchedFinding {
    pub finding: WardlineFinding,
    pub resolution_confidence: ResolutionConfidence,
}

#[derive(Debug, Clone, Default)]
pub struct ReconcileResult {
    pub matched: Vec<MatchedFinding>,
    pub omitted_no_qualname: usize,
}

/// The entity_id's segment-3 `canonical_qualified_name`.
/// `python:function:demo.Foo.bar` -> `Some("demo.Foo.bar")`. `None` when the id
/// lacks three `{plugin}:{kind}:{qualname}` segments or the qualname is empty.
pub fn entity_qualname(entity_id: &str) -> Option<&str> {
    let mut parts = entity_id.splitn(3, ':');
    let _plugin = parts.next()?;
    let _kind = parts.next()?;
    let qualname = parts.next()?;
    (!qualname.is_empty()).then_some(qualname)
}

/// The dotted qualname a finding targets, from `metadata.wardline.qualname`.
/// `None` when the key is absent (malformed / non-Python) — counted as omitted.
fn finding_qualname(finding: &WardlineFinding) -> Option<&str> {
    finding.metadata.get("wardline")?.get("qualname")?.as_str()
}

fn resolution_confidence(entity_qn: &str, finding_qn: &str) -> ResolutionConfidence {
    if entity_qn == finding_qn {
        ResolutionConfidence::Exact
    } else {
        ResolutionConfidence::None
    }
}

/// Filter `findings` to those that resolve to `entity_id`, tagging each with its
/// confidence. Findings with no `wardline.qualname` are counted in
/// `omitted_no_qualname`, never dropped silently.
pub fn reconcile_for_entity(entity_id: &str, findings: Vec<WardlineFinding>) -> ReconcileResult {
    let Some(target) = entity_qualname(entity_id) else {
        return ReconcileResult::default();
    };
    let mut result = ReconcileResult::default();
    for finding in findings {
        match finding_qualname(&finding) {
            Some(qn) => {
                let confidence = resolution_confidence(target, qn);
                if confidence != ResolutionConfidence::None {
                    result.matched.push(MatchedFinding { finding, resolution_confidence: confidence });
                }
            }
            None => result.omitted_no_qualname += 1,
        }
    }
    result
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p clarion-mcp wardline_reconcile`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/clarion-mcp/src/wardline_reconcile.rs crates/clarion-mcp/src/lib.rs
git commit -m "feat(mcp): pure Wardline qualname reconciliation module (Flow B)"
```

---

## Task 3: Filigree client two-hop fetch

**Files:**
- Modify: `crates/clarion-mcp/src/filigree.rs` (URL builders near `entity_associations_url` ~line 286; `get_json` helper + trait method + HTTP impl; mock-server test in `mod tests`)

- [ ] **Step 1: Write the failing URL-builder + mock-server tests**

Add to `mod tests` in `crates/clarion-mcp/src/filigree.rs`:

```rust
#[test]
fn builds_loom_url_builders_with_encoding() {
    assert_eq!(
        loom_files_url("http://127.0.0.1:8542/", "wardline", "src/demo.py"),
        "http://127.0.0.1:8542/api/loom/files?scan_source=wardline&path_prefix=src%2Fdemo.py"
    );
    assert_eq!(
        loom_findings_url("http://127.0.0.1:8542/", "wardline", "file-9"),
        "http://127.0.0.1:8542/api/loom/findings?scan_source=wardline&file_id=file-9"
    );
}

#[test]
fn wardline_findings_for_path_does_two_hops_and_exact_path_filter() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || {
        // Hop 1: GET /api/loom/files — path_prefix matches two files; the
        // exact-path filter must pick file-9, not the helpers file.
        let (mut s1, _) = listener.accept().expect("accept files");
        let mut buf = [0_u8; 4096];
        let n = s1.read(&mut buf).expect("read files req");
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(req.contains("GET /api/loom/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"));
        let body = r#"{"items":[{"file_id":"file-9","path":"src/demo.py","language":"python","file_type":"source"},{"file_id":"file-10","path":"src/demo.py.bak","language":"python","file_type":"source"}],"has_more":false}"#;
        write!(s1, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}", body.len(), body).unwrap();

        // Hop 2: GET /api/loom/findings for file-9.
        let (mut s2, _) = listener.accept().expect("accept findings");
        let n = s2.read(&mut buf).expect("read findings req");
        let req = String::from_utf8_lossy(&buf[..n]);
        assert!(req.contains("GET /api/loom/findings?scan_source=wardline&file_id=file-9 HTTP/1.1"));
        let body = r#"{"items":[{"finding_id":"f-1","file_id":"file-9","severity":"high","status":"open","scan_source":"wardline","rule_id":"WLN-TAINT-001","message":"sink","suggestion":"","scan_run_id":"r-1","line_start":12,"line_end":12,"fingerprint":"fp","issue_id":null,"seen_count":1,"metadata":{"wardline":{"qualname":"demo.Foo.bar"}},"data_warnings":[]}],"has_more":false}"#;
        write!(s2, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}", body.len(), body).unwrap();
    });
    let client = detail_test_client(addr);
    let findings = client
        .wardline_findings_for_path("src/demo.py")
        .expect("two-hop fetch");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule_id, "WLN-TAINT-001");
    handle.join().expect("server thread");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p clarion-mcp filigree::tests::wardline_findings_for_path filigree::tests::builds_loom_url`
Expected: FAIL to compile — `loom_files_url`, `loom_findings_url`, and `wardline_findings_for_path` undefined.

- [ ] **Step 3: Add URL builders**

Add to `crates/clarion-mcp/src/filigree.rs` near `entity_associations_url`:

```rust
pub fn loom_files_url(base_url: &str, scan_source: &str, path_prefix: &str) -> String {
    format!(
        "{}/api/loom/files?scan_source={}&path_prefix={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(scan_source),
        percent_encode_query_value(path_prefix)
    )
}

pub fn loom_findings_url(base_url: &str, scan_source: &str, file_id: &str) -> String {
    format!(
        "{}/api/loom/findings?scan_source={}&file_id={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(scan_source),
        percent_encode_query_value(file_id)
    )
}
```

- [ ] **Step 4: Add a `get_json` helper and the trait method**

Add a private helper to `impl FiligreeHttpClient` (DRYs the header/auth/status/parse the existing methods inline):

```rust
    /// GET `url` with the standard actor + bearer headers and parse the body as
    /// `T`. A non-success status is surfaced as `HttpStatus` so the caller can
    /// stop hammering a down endpoint.
    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, FiligreeClientError> {
        let mut request = self.client.get(url).header("accept", "application/json");
        if !self.actor.trim().is_empty() {
            request = request.header("x-filigree-actor", self.actor.as_str());
        }
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(FiligreeClientError::Request)?;
        let status = response.status();
        let body = response.text().map_err(FiligreeClientError::Request)?;
        if !status.is_success() {
            return Err(FiligreeClientError::HttpStatus { status: status.as_u16(), body });
        }
        serde_json::from_str(&body)
            .map_err(|e| FiligreeClientError::Contract(FiligreeContractError::from(e)))
    }
```

Add the method to the `FiligreeLookup` trait (default returns empty so storage-only servers and existing test doubles keep compiling, exactly like `issue_detail`):

```rust
    /// Wardline findings for a source file, for read-time reconciliation
    /// (Flow B). Two-hop: resolve `path` → Filigree `file_id`, then fetch that
    /// file's `scan_source=wardline` findings. Returns an empty list when no
    /// Wardline-touched file exists at `path`. Default impl returns empty (no
    /// Filigree); the HTTP client overrides it. Transport / non-success HTTP is
    /// surfaced as `Err` so the caller degrades the section to `unavailable`.
    fn wardline_findings_for_path(
        &self,
        _path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        Ok(Vec::new())
    }
```

- [ ] **Step 5: Implement the two-hop in the HTTP client**

Add to `impl FiligreeLookup for FiligreeHttpClient`:

```rust
    fn wardline_findings_for_path(
        &self,
        path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        // Hop 1: path -> Filigree file_id. path_prefix is a prefix filter, so
        // take only the row whose path is byte-exact.
        let files: LoomFilesResponse =
            self.get_json(&loom_files_url(&self.base_url, "wardline", path))?;
        let Some(file_id) = files.items.into_iter().find(|f| f.path == path).map(|f| f.file_id)
        else {
            return Ok(Vec::new());
        };
        // Hop 2: file_id -> wardline findings.
        let findings: WardlineFindingsResponse =
            self.get_json(&loom_findings_url(&self.base_url, "wardline", &file_id))?;
        Ok(findings.items)
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run -p clarion-mcp filigree::tests::wardline_findings_for_path filigree::tests::builds_loom_url`
Expected: PASS (2 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/clarion-mcp/src/filigree.rs
git commit -m "feat(mcp): Filigree two-hop wardline_findings_for_path (loom files + findings)"
```

---

## Task 4: Wire the `wardline_findings` section into `issues_for`

**Files:**
- Modify: `crates/clarion-mcp/src/lib.rs` (`tool_issues_for`, ~line 1007–1126; add a helper `wardline_section_for_entity`; integration test in the lib.rs test module)

- [ ] **Step 1: Write the failing integration tests**

In the `lib.rs` test module, find the existing `FiligreeLookup` test double used by `issues_for` tests (search for `impl FiligreeLookup for`). Add a double that returns canned findings and one that errors, then:

```rust
#[tokio::test]
async fn issues_for_attaches_exact_wardline_findings() {
    // Build a server whose single entity is python:function:demo.hello at src/demo.py.
    let server = issues_for_test_server_with_entity(
        "python:function:demo.hello",
        "src/demo.py",
    );
    let server = server.with_filigree_client(Arc::new(FakeWardline {
        findings: vec![
            wf("demo.hello", "WLN-TAINT-001"), // exact -> attached
            wf("demo.other", "WLN-TAINT-002"), // different entity -> not attached
            wf_no_qualname("WLN-METRIC-001"),  // omitted
        ],
    }));
    let out = server.tool_issues_for(&args_id("python:function:demo.hello")).await.unwrap();
    let section = &out["wardline_findings"];
    assert_eq!(section["result_kind"], "matched");
    assert_eq!(section["items"].as_array().unwrap().len(), 1);
    assert_eq!(section["items"][0]["rule_id"], "WLN-TAINT-001");
    assert_eq!(section["items"][0]["resolution_confidence"], "exact");
    assert_eq!(section["omitted_no_qualname"], 1);
}

#[tokio::test]
async fn issues_for_degrades_when_wardline_fetch_errors() {
    let server = issues_for_test_server_with_entity("python:function:demo.hello", "src/demo.py")
        .with_filigree_client(Arc::new(ErroringWardline));
    let out = server.tool_issues_for(&args_id("python:function:demo.hello")).await.unwrap();
    assert_eq!(out["wardline_findings"]["result_kind"], "unavailable");
    assert!(out["wardline_findings"]["items"].as_array().unwrap().is_empty());
}
```

Add the test doubles + helpers near the existing `issues_for` test doubles (adapt names to the existing harness — `issues_for_test_server_with_entity`, `args_id`, and a single-entity server builder will already exist or be trivially derived from the current `issues_for` tests; reuse them):

```rust
fn wf(qualname: &str, rule_id: &str) -> crate::filigree::WardlineFinding {
    crate::filigree::WardlineFinding {
        rule_id: rule_id.to_owned(),
        message: "m".to_owned(),
        severity: Some("high".to_owned()),
        status: Some("open".to_owned()),
        line_start: Some(1),
        line_end: Some(1),
        fingerprint: Some("fp".to_owned()),
        file_id: Some("file-1".to_owned()),
        metadata: serde_json::json!({ "wardline": { "qualname": qualname } }),
    }
}
fn wf_no_qualname(rule_id: &str) -> crate::filigree::WardlineFinding {
    let mut f = wf("x", rule_id);
    f.metadata = serde_json::json!({ "wardline": { "kind": "METRIC" } });
    f
}

struct FakeWardline { findings: Vec<crate::filigree::WardlineFinding> }
impl crate::filigree::FiligreeLookup for FakeWardline {
    fn associations_for(&self, _id: &str)
        -> Result<crate::filigree::EntityAssociationsResponse, crate::filigree::FiligreeClientError> {
        Ok(crate::filigree::EntityAssociationsResponse { associations: vec![] })
    }
    fn wardline_findings_for_path(&self, _path: &str)
        -> Result<Vec<crate::filigree::WardlineFinding>, crate::filigree::FiligreeClientError> {
        Ok(self.findings.clone())
    }
}

struct ErroringWardline;
impl crate::filigree::FiligreeLookup for ErroringWardline {
    fn associations_for(&self, _id: &str)
        -> Result<crate::filigree::EntityAssociationsResponse, crate::filigree::FiligreeClientError> {
        Ok(crate::filigree::EntityAssociationsResponse { associations: vec![] })
    }
    fn wardline_findings_for_path(&self, _path: &str)
        -> Result<Vec<crate::filigree::WardlineFinding>, crate::filigree::FiligreeClientError> {
        Err(crate::filigree::FiligreeClientError::HttpStatus { status: 503, body: "down".to_owned() })
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p clarion-mcp issues_for_attaches_exact_wardline_findings issues_for_degrades_when_wardline_fetch_errors`
Expected: FAIL — `out["wardline_findings"]` is null (section not built yet).

- [ ] **Step 3: Add the section helper**

Add a free function to `crates/clarion-mcp/src/lib.rs` (near the other envelope helpers, e.g. by `issues_unavailable`):

```rust
/// Build the `wardline_findings` enrich section for one entity. Enrich-only:
/// a fetch error degrades to `result_kind: "unavailable"` rather than failing
/// the tool.
fn wardline_section_for_entity(
    client: &std::sync::Arc<dyn crate::filigree::FiligreeLookup>,
    entity_id: &str,
    source_file_path: Option<&str>,
) -> Value {
    let Some(path) = source_file_path else {
        return serde_json::json!({ "result_kind": "no_matches", "items": [], "omitted_no_qualname": 0 });
    };
    match client.wardline_findings_for_path(path) {
        Ok(findings) => {
            let result = crate::wardline_reconcile::reconcile_for_entity(entity_id, findings);
            let items: Vec<Value> = result
                .matched
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "rule_id": m.finding.rule_id,
                        "message": m.finding.message,
                        "severity": m.finding.severity,
                        "status": m.finding.status,
                        "line_start": m.finding.line_start,
                        "line_end": m.finding.line_end,
                        "fingerprint": m.finding.fingerprint,
                        "resolution_confidence": m.resolution_confidence,
                    })
                })
                .collect();
            let result_kind = if items.is_empty() { "no_matches" } else { "matched" };
            serde_json::json!({
                "result_kind": result_kind,
                "items": items,
                "omitted_no_qualname": result.omitted_no_qualname,
            })
        }
        Err(err) => serde_json::json!({
            "result_kind": "unavailable",
            "items": [],
            "omitted_no_qualname": 0,
            "reason": err.to_string(),
        }),
    }
}
```

- [ ] **Step 4: Call it from `tool_issues_for`**

In `tool_issues_for`, after `accumulator.apply_issue_details(&details);` (line ~1119), build the envelope, then attach the section for the **requested** entity (the one whose `id` matches the `id` argument), running the blocking client call off-thread:

```rust
        let mut envelope = accumulator.into_envelope(
            read.entity_cap_truncated,
            requests_total,
            detail_requests_total,
            &endpoint,
        );
        // Flow B: attach Wardline findings reconciled to the requested entity.
        if let Some(entity) = read.entities.iter().find(|e| e.id == read.requested_id) {
            let client = client.clone();
            let entity_id = entity.id.clone();
            let path = entity.source_file_path.clone();
            let section = tokio::task::spawn_blocking(move || {
                wardline_section_for_entity(&client, &entity_id, path.as_deref())
            })
            .await
            .unwrap_or_else(|err| serde_json::json!({
                "result_kind": "unavailable", "items": [], "omitted_no_qualname": 0,
                "reason": format!("wardline task failed: {err}"),
            }));
            if let Value::Object(map) = &mut envelope {
                map.insert("wardline_findings".to_owned(), section);
            }
        }
        Ok(envelope)
```

Note: if `read` exposes the requested id under a different field name than `requested_id`, use that; if it does not retain it, capture the `entity_id` argument into a local before the `read_issues_for_entities` call (it is `required_str(arguments, "id")?` at the top) and match on that local instead.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p clarion-mcp issues_for_attaches_exact_wardline_findings issues_for_degrades_when_wardline_fetch_errors`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/clarion-mcp/src/lib.rs
git commit -m "feat(mcp): attach reconciled wardline_findings section to issues_for (Flow B)"
```

---

## Task 5: Wire the section into `orientation_pack`

**Files:**
- Modify: `crates/clarion-mcp/src/lib.rs` (`tool_orientation_pack`; integration test)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn orientation_pack_includes_wardline_findings() {
    let server = orientation_pack_test_server_with_entity("python:function:demo.hello", "src/demo.py")
        .with_filigree_client(Arc::new(FakeWardline { findings: vec![wf("demo.hello", "WLN-TAINT-001")] }));
    // orientation_pack resolves by file+line or entity; use the entity form.
    let out = server.tool_orientation_pack(&args_entity("python:function:demo.hello")).await.unwrap();
    assert_eq!(out["wardline_findings"]["result_kind"], "matched");
    assert_eq!(out["wardline_findings"]["items"][0]["rule_id"], "WLN-TAINT-001");
}
```

Reuse the `FakeWardline` / `wf` helpers from Task 4. `orientation_pack_test_server_with_entity` and `args_entity` mirror the existing `orientation_pack` test setup — derive them from the current orientation_pack tests.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p clarion-mcp orientation_pack_includes_wardline_findings`
Expected: FAIL — `wardline_findings` key absent from the pack.

- [ ] **Step 3: Attach the section in `tool_orientation_pack`**

Locate where `tool_orientation_pack` assembles its final `Value` (the packet with `entity`, neighbors, paths, issues, health). After the packet object is built and before returning, attach the section for the pack's primary entity — only when a Filigree client is configured (the pack runs on storage-only servers too):

```rust
        if let (Some(client), Some(primary)) = (self.filigree_client.clone(), primary_entity.as_ref()) {
            let entity_id = primary.id.clone();
            let path = primary.source_file_path.clone();
            let section = tokio::task::spawn_blocking(move || {
                wardline_section_for_entity(&client, &entity_id, path.as_deref())
            })
            .await
            .unwrap_or_else(|err| serde_json::json!({
                "result_kind": "unavailable", "items": [], "omitted_no_qualname": 0,
                "reason": format!("wardline task failed: {err}"),
            }));
            if let Value::Object(map) = &mut packet {
                map.insert("wardline_findings".to_owned(), section);
            }
        }
```

Adapt `primary_entity` / `packet` to the actual local variable names in `tool_orientation_pack` (the primary `EntityRow` and the mutable packet `Value`). If the packet is built as an immutable `json!({...})`, bind it to `let mut packet = ...;` first.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p clarion-mcp orientation_pack_includes_wardline_findings`
Expected: PASS.

- [ ] **Step 5: Run the full crate gate**

```bash
cargo fmt --all -- --check
cargo clippy -p clarion-mcp --all-targets -- -D warnings
cargo nextest run -p clarion-mcp
```
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/clarion-mcp/src/lib.rs
git commit -m "feat(mcp): attach reconciled wardline_findings section to orientation_pack (Flow B)"
```

---

## Task 6: Pin the consumed routes in the federation contract

**Files:**
- Modify: `docs/federation/contracts.md` (new "Consumed Filigree route" subsection after the issue-detail one, ~line 406+)

- [ ] **Step 1: Add the contract section**

Append after the existing "Consumed Filigree route: issue detail (enrichment)" section:

````markdown
## Consumed Filigree route: Wardline findings (read-time reconciliation)

Flow B (read-time Wardline finding reconciliation) consumes two existing Filigree
*loom* read routes — no new route is requested. Both are enrich-only: if either
is unreachable the `wardline_findings` section degrades to
`result_kind: "unavailable"` and the tool returns normally.

1. `GET /api/loom/files?scan_source=wardline&path_prefix=<rel-path>` → unified
   `ListResponse[FileRecordLoom]` (`{items, has_more, next_offset?}`). Clarion
   takes the item whose `path` is byte-exact (the filter is a prefix) to obtain
   Filigree's `file_id`. Pinned by Filigree `tests/fixtures/contracts/loom/files.json`.
2. `GET /api/loom/findings?scan_source=wardline&file_id=<file_id>` →
   `ListResponse[ScanFindingLoom]`. Clarion reads `rule_id`, `message`,
   `severity`, `status`, `line_start/line_end`, `fingerprint`, and `metadata`
   (the reconciliation key `metadata.wardline.qualname`). Pinned by Filigree
   `tests/fixtures/contracts/loom/findings.json`.

Reconciliation: `metadata.wardline.qualname` is matched byte-exact against the
entity_id's segment-3 `canonical_qualified_name` (`python:function:<qualname>`),
per the §"Wardline qualname normalization" contract. A match is
`resolution_confidence: exact`; an unresolved qualname is `none`. (`heuristic` is
reserved.) `POST /api/v1/files:resolve` is **not** used here — it is a route
Clarion *exposes*, not one it consumes.
````

- [ ] **Step 2: Commit**

```bash
git add docs/federation/contracts.md
git commit -m "docs(federation): pin the two consumed loom routes for Flow B reconciliation"
```

---

## Self-Review

**Spec coverage** (against `2026-05-30-clarion-consume-wardline-data-design.md`):
- §3 read-time lazy reconciliation → Tasks 2 (match), 4/5 (attach). ✓
- §3 `resolution_confidence` tiers → Task 2 (`Exact`/`None`; `Heuristic` reserved, documented). ✓
- §4 two-hop via existing loom routes → Task 3. ✓
- §5 enrich-only / no fabrication / omitted count → Tasks 2 (omitted), 4 (degrade test + no_matches). ✓
- §6 hermetic tests → injected `FakeWardline`/`ErroringWardline` (Tasks 4/5), mock TcpListener (Task 3). ✓
- §10.3 pin consumed routes in contracts.md → Task 6. ✓
- Kind handling (functions/methods only) → falls out of `entity_qualname` matching `python:function:` ids; class/module ids simply never match a function-qualname finding. Documented in Task 2 module doc. ✓

**Placeholder scan:** No TBD/TODO; every code/test step carries real code and an exact `cargo nextest` command with expected result. The two adaptation notes (Task 4 `requested_id`, Task 5 `primary_entity`/`packet` names) are explicit fallback instructions, not placeholders.

**Type consistency:** `WardlineFinding` (fields used identically in Tasks 1/2/3/4). `ResolutionConfidence` serializes lowercase → asserted as `"exact"` in Task 4. `reconcile_for_entity` / `ReconcileResult` / `MatchedFinding` consistent Task 2 → 4. `wardline_findings_for_path` signature identical in trait default (Task 3), HTTP impl (Task 3), and test doubles (Task 4). Section shape (`result_kind` / `items` / `omitted_no_qualname`) identical across the helper (Task 4), both call sites (Tasks 4/5), and the contract doc (Task 6).

**Out of plan scope (tracker-only, per spec §7):** Flow A re-scope of `clarion-1f6241b329` / `clarion-22acf15fd7` — already recorded as comments; not a build task here.
