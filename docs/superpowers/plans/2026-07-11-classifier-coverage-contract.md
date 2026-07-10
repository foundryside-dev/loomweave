# Classifier Coverage Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Loomweave prove classifier support and enumeration completeness independently of observed tag cardinality, including supported classifiers with zero matches.

**Architecture:** Language-plugin manifests declare their static tag classifiers. Each analysis run persists a versioned per-plugin coverage record in `runs.stats`; the storage layer reads the latest run fail-closed, and tag-backed MCP tools combine that producer evidence with query pagination/scope/scan flags in an authoritative `classification` block. Existing entity/tag rows and response fields remain intact.

**Tech Stack:** Rust 1.88 workspace, TOML plugin manifests, Serde/JSON, SQLite `runs.stats`, MCP JSON-RPC catalog tools, Python extractor fixtures, Cargo Nextest, Ruff, Mypy, Pytest, Wardline.

---

## Execution preflight

The planning workspace contains unrelated concurrent changes in `CHANGELOG.md`, `crates/loomweave-cli/src/analyze.rs`, `crates/loomweave-cli/tests/analyze.rs`, `crates/loomweave-storage/src/commands.rs`, `crates/loomweave-storage/src/writer.rs`, and `crates/loomweave-storage/tests/writer_actor.rs`. Do not execute this plan in that dirty tree.

- [ ] **Step 1: Create an isolated worktree from the design commit**

Use the `superpowers:using-git-worktrees` skill. Record the parent branch before
creating the feature branch so closeout can integrate into the exact branch the
work came from:

```bash
cd /home/john/loomweave
PARENT_BRANCH=$(git branch --show-current)
test -n "$PARENT_BRANCH"
git worktree add .worktrees/classifier-coverage -b fix/classifier-coverage-contract "$PARENT_BRANCH"
cd /home/john/loomweave/.worktrees/classifier-coverage
PARENT_META=$(git rev-parse --git-path loomweave-parent-branch)
printf '%s\n' "$PARENT_BRANCH" > "$PARENT_META"
git status --short --branch
```

Expected: branch `fix/classifier-coverage-contract` with a clean worktree and
the path printed by `git rev-parse --git-path loomweave-parent-branch`
containing `main`. The metadata file lives under the worktree's Git
administrative directory and is not committed.

- [ ] **Step 2: Pin the tracker and baseline**

```bash
filigree show clarion-b5c50abb19
cargo nextest run -p loomweave-core -p loomweave-storage -p loomweave-mcp
```

Expected: the issue is `fixing`; the focused crate suites pass before new tests are introduced.

## File structure

- Create `crates/loomweave-core/src/classifier_coverage.rs` for shared versioned coverage types.
- Modify `crates/loomweave-core/src/plugin/manifest.rs` and built-in plugin manifests for classifier declarations.
- Create `crates/loomweave-cli/src/analyze/classifier_coverage.rs` and minimally wire `analyze.rs` to persist per-run coverage.
- Create `crates/loomweave-storage/src/classifier_coverage.rs` to read the latest run fail-closed.
- Create `crates/loomweave-mcp/src/catalogue/classification.rs` and wire tag facets to return authoritative state.
- Update catalog/tool tests, operator documentation, the workflow skill, and the changelog.

### Task 1: Declare plugin classifier support

**Files:**
- Create: `crates/loomweave-core/src/classifier_coverage.rs`
- Modify: `crates/loomweave-core/src/lib.rs`
- Modify: `crates/loomweave-core/src/plugin/manifest.rs`
- Modify: `plugins/python/plugin.toml`
- Modify: `crates/loomweave-plugin-rust/plugin.toml`
- Modify: `crates/loomweave-plugin-rust/src/serve.rs`
- Modify: `packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml`
- Test: `crates/loomweave-core/src/plugin/manifest.rs`
- Test: `plugins/python/tests/test_package.py`
- Test: `crates/loomweave-plugin-rust/src/lib.rs`

- [ ] **Step 1: Write failing manifest contract tests**

Extend the canonical manifest fixture with:

```toml
classifier_tags = ["http-route", "entry-point", "http-route"]
```

Add these assertions/tests in `manifest.rs`:

```rust
assert_eq!(manifest.ontology.classifier_tags, vec!["entry-point", "http-route"]);

#[test]
fn classifier_tags_default_empty_for_legacy_manifest() {
    let manifest = parse_manifest(manifest_without("classifier_tags").as_bytes()).unwrap();
    assert!(manifest.ontology.classifier_tags.is_empty());
}

#[test]
fn classifier_tags_reject_non_kebab_case_values() {
    let toml = manifest_with(r#"classifier_tags = ["HTTP_ROUTE"]"#);
    let error = parse_manifest(toml.as_bytes()).unwrap_err();
    assert!(matches!(error, ManifestError::GrammarViolation {
        field: "classifier_tags",
        value
    } if value == "HTTP_ROUTE"));
}
```

Update Python package tests to pin:

```python
assert manifest["ontology"]["classifier_tags"] == [
    "cli-command",
    "data-model",
    "entry-point",
    "exported-api",
    "framework-handler",
    "http-route",
    "public-surface",
    "test",
]
assert manifest["ontology"]["ontology_version"] == "0.12.0"
```

Update the Rust manifest test to pin the sorted set `allow-dead-code`, `cli-command`, `entry-point`, `exported-api`, `framework-handler`, `http-route`, `test` and ontology `0.9.0`. Update the handshake ontology constant in `crates/loomweave-plugin-rust/src/serve.rs` to `0.9.0`, then copy the canonical manifest byte-for-byte into the distribution wheel path:

```bash
cp crates/loomweave-plugin-rust/plugin.toml packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml
python scripts/check-rust-plugin-manifest-lockstep.py
```

- [ ] **Step 2: Run tests and verify RED**

```bash
cargo test -p loomweave-core plugin::manifest -- --nocapture
cargo test -p loomweave-plugin-rust manifest_parses -- --nocapture
uv run --project plugins/python pytest --no-cov plugins/python/tests/test_package.py -q
```

Expected failures: `Ontology` has no `classifier_tags` field and manifests carry no declaration.

- [ ] **Step 3: Add shared coverage types**

Create `classifier_coverage.rs`:

```rust
use serde::{Deserialize, Serialize};

pub const CLASSIFIER_COVERAGE_SCHEMA: &str = "loomweave.classifier-coverage.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifierCoverage {
    pub schema: String,
    pub source_walk_complete: bool,
    pub source_walk_skipped_entries: u64,
    pub plugins: Vec<PluginClassifierCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginClassifierCoverage {
    pub plugin_id: String,
    pub plugin_version: String,
    pub ontology_version: String,
    pub matched_files: u64,
    pub analyzed_files: u64,
    pub retained_files: u64,
    pub degraded_files: u64,
    pub status: PluginCoverageStatus,
    pub classifier_tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginCoverageStatus {
    Complete,
    Degraded,
    Failed,
    NotApplicable,
}
```

Export the module and types from `crates/loomweave-core/src/lib.rs`.

- [ ] **Step 4: Implement bounded manifest parsing**

Add to `Ontology`:

```rust
#[serde(default)]
pub classifier_tags: Vec<String>,
```

Add caps:

```rust
pub const MAX_CLASSIFIER_TAGS: usize = 256;
pub const MAX_CLASSIFIER_TAG_LEN: usize = 128;
```

Deserialize into `let mut manifest`, reject more than 256 tags, and validate each tag with:

```rust
let valid = !tag.is_empty()
    && tag.len() <= MAX_CLASSIFIER_TAG_LEN
    && tag.starts_with(|c: char| c.is_ascii_lowercase())
    && tag.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
if !valid {
    return Err(ManifestError::GrammarViolation {
        field: "classifier_tags",
        value: tag.clone(),
    });
}
```

Then canonicalize:

```rust
manifest.ontology.classifier_tags.sort();
manifest.ontology.classifier_tags.dedup();
```

- [ ] **Step 5: Declare classifiers, run GREEN, and commit**

Add the exact sorted Python/Rust sets from Step 1, bump their ontology versions, and state in comments that declarations do not create tag rows.

```bash
cargo test -p loomweave-core plugin::manifest -- --nocapture
cargo test -p loomweave-plugin-rust manifest_parses -- --nocapture
uv run --project plugins/python pytest --no-cov plugins/python/tests/test_package.py -q
git add crates/loomweave-core/src/classifier_coverage.rs crates/loomweave-core/src/lib.rs crates/loomweave-core/src/plugin/manifest.rs plugins/python/plugin.toml plugins/python/tests/test_package.py crates/loomweave-plugin-rust/plugin.toml crates/loomweave-plugin-rust/src/lib.rs crates/loomweave-plugin-rust/src/serve.rs packaging/rust-plugin-dist/wheel-data/data/share/loomweave/plugins/rust/plugin.toml
git commit -m "feat: declare plugin classifier capabilities"
```

### Task 2: Persist authoritative per-run coverage

**Files:**
- Create: `crates/loomweave-cli/src/analyze/classifier_coverage.rs`
- Modify: `crates/loomweave-cli/src/analyze.rs`
- Test: `crates/loomweave-cli/tests/analyze.rs`

- [ ] **Step 1: Add failing integration fixtures**

Give a synthetic plugin `classifier_tags = ["exported-api", "http-route"]`. After a successful clean-file run, query the latest `runs.stats` and assert:

```rust
let stats: String = conn.query_row(
    "SELECT stats FROM runs ORDER BY started_at DESC LIMIT 1",
    [],
    |row| row.get(0),
).unwrap();
let stats: serde_json::Value = serde_json::from_str(&stats).unwrap();
assert_eq!(stats["classifier_coverage"]["schema"], "loomweave.classifier-coverage.v1");
assert_eq!(stats["classifier_coverage"]["source_walk_complete"], true);
assert_eq!(stats["classifier_coverage"]["plugins"][0]["matched_files"], 1);
assert_eq!(stats["classifier_coverage"]["plugins"][0]["degraded_files"], 0);
assert_eq!(stats["classifier_coverage"]["plugins"][0]["status"], "complete");
assert_eq!(stats["classifier_coverage"]["plugins"][0]["classifier_tags"], serde_json::json!(["exported-api", "http-route"]));
```

Add a syntax-error fixture and assert `degraded_files=1`, `status="degraded"`, while the run remains completed.

- [ ] **Step 2: Run tests and verify RED**

```bash
cargo nextest run -p loomweave-cli -E 'test(analyze_persists_classifier_coverage) | test(analyze_marks_syntax_degraded_classifier_coverage)'
```

Expected: `stats.classifier_coverage` is absent.

- [ ] **Step 3: Add the coverage builder**

Create `analyze/classifier_coverage.rs`:

```rust
use std::collections::BTreeSet;
use loomweave_core::{ClassifierCoverage, Manifest, PluginClassifierCoverage, PluginCoverageStatus, CLASSIFIER_COVERAGE_SCHEMA};

pub(super) fn plugin_record(
    manifest: &Manifest,
    matched_files: usize,
    analyzed_files: usize,
    retained_files: usize,
    degraded_files: &BTreeSet<String>,
    failed: bool,
) -> PluginClassifierCoverage {
    let status = if matched_files == 0 {
        PluginCoverageStatus::NotApplicable
    } else if failed {
        PluginCoverageStatus::Failed
    } else if degraded_files.is_empty() {
        PluginCoverageStatus::Complete
    } else {
        PluginCoverageStatus::Degraded
    };
    PluginClassifierCoverage {
        plugin_id: manifest.plugin.plugin_id.clone(),
        plugin_version: manifest.plugin.version.clone(),
        ontology_version: manifest.ontology.ontology_version.clone(),
        matched_files: matched_files as u64,
        analyzed_files: analyzed_files as u64,
        retained_files: retained_files as u64,
        degraded_files: degraded_files.len() as u64,
        status,
        classifier_tags: manifest.ontology.classifier_tags.clone(),
    }
}

pub(super) fn run_coverage(skipped: u64, mut plugins: Vec<PluginClassifierCoverage>) -> ClassifierCoverage {
    plugins.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    ClassifierCoverage {
        schema: CLASSIFIER_COVERAGE_SCHEMA.to_owned(),
        source_walk_complete: skipped == 0,
        source_walk_skipped_entries: skipped,
        plugins,
    }
}
```

- [ ] **Step 4: Wire coverage into analysis**

Add `mod classifier_coverage;`. Keep `Vec<PluginClassifierCoverage>` beside the plugin index markers. Capture `matched_files` before the no-files branch, analyzed/retained counts after incremental partitioning, and a plugin-level `BTreeSet<String>` of syntax-degraded source paths.

Extend `PersistedPluginBatch` with:

```rust
degraded_source_files: BTreeSet<String>,
```

When `syntax_error_finding` returns a finding, parse `evidence.source_file_path` into that set before moving the finding. Merge batch sets per plugin. After `spawn_result`, append `plugin_record(..., spawn_result.is_err())`. Persist a `not-applicable` record before continuing when the plugin matched zero files.

Before committing the run:

```rust
let classifier_coverage = classifier_coverage::run_coverage(
    source_walk_skipped_entries,
    plugin_classifier_coverage,
);
```

Insert into both completed and soft-failed stats:

```rust
"classifier_coverage": classifier_coverage,
```

Hard failures remain unavailable because no authoritative graph was committed.

- [ ] **Step 5: Run GREEN and commit**

```bash
cargo nextest run -p loomweave-cli -E 'test(analyze_persists_classifier_coverage) | test(analyze_marks_syntax_degraded_classifier_coverage) | test(analyze_ontology_bump_forces_full_reanalysis)'
git add crates/loomweave-cli/src/analyze.rs crates/loomweave-cli/src/analyze/classifier_coverage.rs crates/loomweave-cli/tests/analyze.rs
git commit -m "feat(analyze): persist classifier coverage"
```

### Task 3: Read latest coverage fail-closed

**Files:**
- Create: `crates/loomweave-storage/src/classifier_coverage.rs`
- Modify: `crates/loomweave-storage/src/lib.rs`
- Test: `crates/loomweave-storage/src/classifier_coverage.rs`

- [ ] **Step 1: Write failing reader tests**

Using real migrations, seed: valid completed coverage; a newer failed run after an older completed run; completed coverage missing; completed coverage malformed. Assert only valid latest completed coverage is authoritative. Never fall back to the older completed run.

- [ ] **Step 2: Run tests and verify RED**

```bash
cargo test -p loomweave-storage classifier_coverage -- --nocapture
```

- [ ] **Step 3: Implement the reader**

Create:

```rust
use loomweave_core::{ClassifierCoverage, CLASSIFIER_COVERAGE_SCHEMA};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use crate::{Result, StorageError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestClassifierCoverage {
    pub run_id: Option<String>,
    pub run_status: Option<String>,
    pub coverage: Option<ClassifierCoverage>,
    pub unavailable_reason: Option<String>,
}

pub fn latest_classifier_coverage(conn: &Connection) -> Result<LatestClassifierCoverage> {
    let latest = conn.query_row(
        "SELECT id, status, stats FROM runs ORDER BY started_at DESC LIMIT 1",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
    ).optional().map_err(StorageError::from)?;
    let Some((run_id, run_status, stats)) = latest else {
        return Ok(LatestClassifierCoverage { run_id: None, run_status: None, coverage: None, unavailable_reason: Some("no analysis run is recorded".to_owned()) });
    };
    if run_status != "completed" {
        return Ok(LatestClassifierCoverage { run_id: Some(run_id), run_status: Some(run_status.clone()), coverage: None, unavailable_reason: Some(format!("latest analysis run status is {run_status}")) });
    }
    let stats: Value = match serde_json::from_str(&stats) {
        Ok(value) => value,
        Err(error) => return Ok(LatestClassifierCoverage { run_id: Some(run_id), run_status: Some(run_status), coverage: None, unavailable_reason: Some(format!("latest run stats are invalid JSON: {error}")) }),
    };
    let Some(raw) = stats.get("classifier_coverage") else {
        return Ok(LatestClassifierCoverage { run_id: Some(run_id), run_status: Some(run_status), coverage: None, unavailable_reason: Some("latest run has no classifier coverage metadata".to_owned()) });
    };
    match serde_json::from_value::<ClassifierCoverage>(raw.clone()) {
        Ok(coverage) if coverage.schema == CLASSIFIER_COVERAGE_SCHEMA => Ok(LatestClassifierCoverage { run_id: Some(run_id), run_status: Some(run_status), coverage: Some(coverage), unavailable_reason: None }),
        Ok(coverage) => Ok(LatestClassifierCoverage { run_id: Some(run_id), run_status: Some(run_status), coverage: None, unavailable_reason: Some(format!("unsupported classifier coverage schema {}", coverage.schema)) }),
        Err(error) => Ok(LatestClassifierCoverage { run_id: Some(run_id), run_status: Some(run_status), coverage: None, unavailable_reason: Some(format!("classifier coverage is invalid: {error}")) }),
    }
}
```

Export the module/function/types from `lib.rs`.

- [ ] **Step 4: Run GREEN and commit**

```bash
cargo test -p loomweave-storage classifier_coverage -- --nocapture
git add crates/loomweave-storage/src/classifier_coverage.rs crates/loomweave-storage/src/lib.rs
git commit -m "feat(storage): read classifier coverage"
```

### Task 4: Attach authoritative MCP classification

**Files:**
- Create: `crates/loomweave-mcp/src/catalogue/classification.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/mod.rs`
- Modify: `crates/loomweave-mcp/src/catalogue/faceted.rs`
- Test: `crates/loomweave-mcp/src/catalogue/classification.rs`
- Test: `crates/loomweave-mcp/tests/catalogue_tools.rs`

- [ ] **Step 1: Write failing pure and JSON-RPC tests**

Pure tests must cover supported-zero, all three truncation flags, partial mixed plugins, unsupported, missing coverage, and observed rows without coverage. Add `seed_classifier_coverage` to `catalogue_tools.rs` and this integration shape:

```rust
#[tokio::test]
async fn http_routes_are_supported_complete_when_python_matches_zero() {
    let (project, db, conn) = open_project();
    insert_entity(&conn, "python:module:plain", "module", "plain.py", Some((1, 1)));
    seed_classifier_coverage(&conn, json!({
        "schema": "loomweave.classifier-coverage.v1",
        "source_walk_complete": true,
        "source_walk_skipped_entries": 0,
        "plugins": [{
            "plugin_id": "python", "plugin_version": "1.4.1", "ontology_version": "0.12.0",
            "matched_files": 1, "analyzed_files": 1, "retained_files": 0,
            "degraded_files": 0, "status": "complete", "classifier_tags": ["http-route"]
        }]
    }));
    drop(conn);
    let env = call_tool(&state_for(project.path(), &db), "find_http_routes", json!({})).await;
    assert_eq!(env["result"]["page"]["total"], 0, "{env}");
    assert_eq!(env["result"]["classification"]["state"], "supported", "{env}");
    assert_eq!(env["result"]["classification"]["complete"], true, "{env}");
    assert_eq!(env["result"]["signal"]["available"], true, "{env}");
}
```

Add equivalent exported-API zero/nonzero tests, unsupported/degraded tests, and a `limit=1` positive result proving page truncation forces incomplete.

- [ ] **Step 2: Run tests and verify RED**

```bash
cargo nextest run -p loomweave-mcp -E 'test(http_routes_are_supported_complete_when_python_matches_zero) | test(exported_api_classifier_supports_zero_and_nonzero) | test(classifier_coverage_fails_closed)'
```

Expected: `classification` is absent and supported-zero has `signal.available=false`.

- [ ] **Step 3: Implement classification derivation**

Create `classification.rs` with a pure `classify_tag(latest, tag, response) -> Value`. It must:

```rust
let matches = response["page"]["total"].as_u64().unwrap_or(0);
let page_truncated = response["page"]["truncated"].as_bool().unwrap_or(true);
let scope_truncated = response["scope_truncated"].as_bool().unwrap_or(true);
let scan_truncated = response["scan_truncated"].as_bool().unwrap_or(true);
```

Return `unavailable` when current coverage is absent or no plugin matched a file. Partition active plugins by `classifier_tags.contains(tag)`. State is `unsupported` when supporting is empty, `supported` when unsupported is empty, otherwise `partial`. Compute:

```rust
let plugin_degraded = active.iter().any(|plugin| {
    plugin.status != PluginCoverageStatus::Complete || plugin.degraded_files > 0
});
let complete = state == "supported"
    && coverage.source_walk_complete
    && !plugin_degraded
    && !page_truncated
    && !scope_truncated
    && !scan_truncated;
```

Return the exact fields `schema`, `tag`, `state`, `complete`, `matches`, `supporting_plugins`, `unsupported_plugins`, `run_id`, `run_status`, and `reasons`.

- [ ] **Step 4: Attach classification and corrected signal**

In `tag_facet`, after page/facet creation:

```rust
let latest = loomweave_storage::latest_classifier_coverage(conn)?;
let classification = classify_tag(&latest, &tag, &response);
let available = classification["state"] == json!("supported");
let complete = classification["complete"].as_bool().unwrap_or(false);
let reason = if complete {
    format!("classifier {tag} is supported and this enumeration is complete")
} else {
    classification["reasons"].as_array()
        .and_then(|reasons| reasons.first())
        .and_then(Value::as_str)
        .unwrap_or("classifier enumeration is incomplete")
        .to_owned()
};
if let Some(object) = response.as_object_mut() {
    object.insert("classification".to_owned(), classification);
    object.insert("signal".to_owned(), json!({
        "available": available,
        "signal": "entity_tags",
        "complete": complete,
        "reason": reason,
    }));
}
```

Keep `known_tags` on empty pages as observed-instance diagnostics, but remove cardinality as the availability decision.

- [ ] **Step 5: Run GREEN and commit**

```bash
cargo test -p loomweave-mcp catalogue::classification -- --nocapture
cargo nextest run -p loomweave-mcp -E 'test(http_routes_are_supported_complete_when_python_matches_zero) | test(exported_api_classifier_supports_zero_and_nonzero) | test(classifier_coverage_fails_closed) | test(categorisation_shortcuts_are_honest_empty)'
git add crates/loomweave-mcp/src/catalogue/classification.rs crates/loomweave-mcp/src/catalogue/mod.rs crates/loomweave-mcp/src/catalogue/faceted.rs crates/loomweave-mcp/tests/catalogue_tools.rs
git commit -m "fix(mcp): distinguish supported-empty classifiers"
```

### Task 5: Document, verify, and validate Scrappack

**Files:**
- Modify: `crates/loomweave-mcp/src/lib.rs`
- Modify: `crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md`
- Modify: `docs/operator/language-support.md`
- Modify: `CHANGELOG.md`
- Test: `crates/loomweave-mcp/src/lib.rs`

- [ ] **Step 1: Write failing tool-description tests**

Require `entity_tag_list` and the tag-backed shortcut descriptions to mention `classification.state`, `classification.complete`, and supported-empty semantics.

```bash
cargo test -p loomweave-mcp tools_list -- --nocapture
```

Expected: descriptions only promise ambiguous honest-empty behavior.

- [ ] **Step 2: Update descriptions and migration guidance**

Use this exact rule:

```text
classification is authoritative for classifier support and enumeration completeness.
state=supported with complete=true may have page.total=0. known_tags lists observed
instances only and must not be used as a capability declaration. Any unavailable,
partial, or unsupported state or any truncation fails closed for whole-surface coverage.
```

Document Python `exported-api` as explicit `__all__` evidence and `http-route` as legitimately supported-empty in a non-web project. Add an Unreleased changelog entry naming `loomweave.classification.v1` and `loomweave.classifier-coverage.v1`.

- [ ] **Step 3: Run documentation tests and commit**

```bash
cargo test -p loomweave-mcp tools_list -- --nocapture
git add crates/loomweave-mcp/src/lib.rs crates/loomweave-mcp/assets/skills/loomweave-workflow/SKILL.md docs/operator/language-support.md CHANGELOG.md
git commit -m "docs: publish classifier completeness contract"
```

- [ ] **Step 4: Run focused and canonical verification**

```bash
cargo nextest run -p loomweave-core -p loomweave-storage -p loomweave-cli -p loomweave-mcp
uv run --project plugins/python pytest --no-cov plugins/python/tests/test_package.py plugins/python/tests/test_extractor.py -q
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
uv sync --project plugins/python --locked --extra dev
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
```

Expected: every command exits zero. Record exact test counts and skips.

- [ ] **Step 5: Run Wardline**

Manifest parsing is an external-input boundary:

```bash
wardline scan . --fail-on ERROR
```

Expected: exit 0. Exit 1 requires explain/fix-at-ingestion/rescan; exit 2 is a tool/configuration error and is not clean.

- [ ] **Step 6: Re-analyze Scrappack with the modified build**

```bash
cargo build --workspace --bins
uv sync --project plugins/python --locked --extra dev
export PATH="$PWD/plugins/python/.venv/bin:$PWD/target/debug:$PATH"
git -C /home/john/scrappack status --short
loomweave analyze /home/john/scrappack
git -C /home/john/scrappack status --short
```

Expected tracked Scrappack source status is identical before/after. Query the modified server for project status, entry points, exported APIs, HTTP routes, and all five requested tags. Expected snapshot if source has not changed:

```text
entry-point=5
cli-command=5
exported-api=0, classification.state=supported, complete=true
http-route=0, classification.state=supported, complete=true
page.truncated=false, scope_truncated=false, scan_truncated=false
source_walk_complete=true, python degraded_files=0
```

If source changed, report actual counts and judge the contract fields rather than historical cardinality.

### Task 6: Integrate into the parent branch and reinstall uv tools

**Files:**
- No source changes. This task integrates the verified commits and installs the built wheels.

- [ ] **Step 1: Rebase onto the recorded parent and re-run the gates**

Use `superpowers:finishing-a-development-branch`. From the feature worktree:

```bash
PARENT_META=$(git rev-parse --git-path loomweave-parent-branch)
PARENT_BRANCH=$(cat "$PARENT_META")
test "$PARENT_BRANCH" = "main"
git rebase "$PARENT_BRANCH"
git status --short --branch
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
plugins/python/.venv/bin/ruff check plugins/python
plugins/python/.venv/bin/ruff format --check plugins/python
plugins/python/.venv/bin/mypy --strict plugins/python
plugins/python/.venv/bin/pytest plugins/python
bash tests/e2e/sprint_1_walking_skeleton.sh
bash tests/e2e/sprint_2_mcp_surface.sh
bash tests/e2e/phase3_subsystems.sh
wardline scan . --fail-on ERROR
```

Expected: the rebase is clean and every gate exits zero against the current parent. If the parent moves again after this verification, repeat the rebase and this exact gate block before merging.

- [ ] **Step 2: Fast-forward the verified feature branch into its parent**

Do not stash, reset, or overwrite the parent worktree's concurrent edits. The merge is allowed only when the parent worktree is clean:

```bash
PARENT_WORKTREE=/home/john/loomweave
test -z "$(git -C "$PARENT_WORKTREE" status --porcelain)"
FEATURE_HEAD=$(git rev-parse HEAD)
git -C "$PARENT_WORKTREE" switch "$PARENT_BRANCH"
git -C "$PARENT_WORKTREE" merge --ff-only fix/classifier-coverage-contract
test "$(git -C "$PARENT_WORKTREE" rev-parse HEAD)" = "$FEATURE_HEAD"
git -C "$PARENT_WORKTREE" status --short --branch
```

Expected: the parent branch now points at the exact verified feature SHA and remains clean. If the cleanliness check fails, stop and report the modified paths; do not create a stash on the owner's behalf. If `--ff-only` fails because the parent advanced, return to Step 1.

- [ ] **Step 3: Build all three wheels from the merged parent**

Build from `/home/john/loomweave`, not from the soon-to-be-removed feature worktree:

```bash
cd /home/john/loomweave
INSTALL_DIST=$(mktemp -d)
uv build --wheel --out-dir "$INSTALL_DIST" crates/loomweave-cli
uv build --wheel --out-dir "$INSTALL_DIST" plugins/python
uv build --wheel --out-dir "$INSTALL_DIST" packaging/rust-plugin-dist
CLI_WHEEL=$(find "$INSTALL_DIST" -maxdepth 1 -type f -name 'loomweave-*.whl' ! -name 'loomweave_plugin_*' -print -quit)
PYTHON_WHEEL=$(find "$INSTALL_DIST" -maxdepth 1 -type f -name 'loomweave_plugin_python-*.whl' -print -quit)
RUST_WHEEL=$(find "$INSTALL_DIST" -maxdepth 1 -type f -name 'loomweave_plugin_rust-*.whl' -print -quit)
test -f "$CLI_WHEEL"
test -f "$PYTHON_WHEEL"
test -f "$RUST_WHEEL"
```

Expected: one local wheel for `loomweave`, `loomweave-plugin-python`, and `loomweave-plugin-rust`. Do not install a registry artifact with the same version number; the exact local wheel paths are the installation authority.

- [ ] **Step 4: Force-reinstall the local wheels into uv**

Install the plugin tools independently so their executables and neighboring shared-data manifests resolve from their own uv environments. Install the CLI with both local wheels supplied so its exact-version dependencies cannot resolve back to an older registry build:

```bash
uv tool install --force --reinstall "$PYTHON_WHEEL"
uv tool install --force --reinstall "$RUST_WHEEL"
uv tool install --force --reinstall "$CLI_WHEEL" --with "$PYTHON_WHEEL" --with "$RUST_WHEEL"
hash -r
uv tool list
```

Expected: `uv tool list` contains all three local packages at the workspace version. `--force --reinstall` replaces the existing managed executables and refreshes cached package data.

- [ ] **Step 5: Verify installed executables and packaged manifests**

```bash
UV_BIN=$(uv tool dir --bin)
UV_ROOT=$(uv tool dir)
test "$(dirname "$(command -v loomweave)")" = "$UV_BIN"
test "$(dirname "$(command -v loomweave-plugin-python)")" = "$UV_BIN"
test "$(dirname "$(command -v loomweave-plugin-rust)")" = "$UV_BIN"
loomweave --version
PYTHON_MANIFESTS=$(find "$UV_ROOT" -path '*/share/loomweave/plugins/python/plugin.toml' -type f)
RUST_MANIFESTS=$(find "$UV_ROOT" -path '*/share/loomweave/plugins/rust/plugin.toml' -type f)
test -n "$PYTHON_MANIFESTS"
test -n "$RUST_MANIFESTS"
while IFS= read -r manifest; do cmp plugins/python/plugin.toml "$manifest"; done <<< "$PYTHON_MANIFESTS"
while IFS= read -r manifest; do cmp crates/loomweave-plugin-rust/plugin.toml "$manifest"; done <<< "$RUST_MANIFESTS"
loomweave doctor
```

Expected: every executable is linked from uv's bin directory, every installed manifest is byte-identical to the merged parent source, and `loomweave doctor` discovers both plugins without a schema/version error.

- [ ] **Step 6: Prove the uv-installed build on Scrappack**

Clear the worktree-specific `PATH` override from Task 5 before this proof:

```bash
export PATH="$UV_BIN:$(printf '%s' "$PATH" | tr ':' '\n' | grep -v '/classifier-coverage/' | paste -sd ':' -)"
test "$(command -v loomweave)" = "$UV_BIN/loomweave"
git -C /home/john/scrappack status --short
loomweave analyze /home/john/scrappack
git -C /home/john/scrappack status --short
```

Expected: Scrappack's tracked status is identical before/after, the analysis log discovers the uv-installed Python plugin with the new ontology, and the resulting HTTP-route/exported-API responses remain supported-complete with zero matches. Capture the installed executable paths, `uv tool list`, plugin discovery lines, run coverage JSON, and MCP classification blocks in the Filigree comment.

- [ ] **Step 7: Record compatibility, close the issue, and remove the worktree**

Plainweave 1.2.1 is expected to remain `denominator_complete=false` until the consumer prompt at `docs/implementation/handoffs/2026-07-11-plainweave-classifier-coverage-prompt.md` is implemented. Include the exact latest `runs.stats.classifier_coverage` and MCP `classification` objects in the handoff.

```bash
cd /home/john/loomweave
git status --short --branch
git log --oneline 9d60b59..HEAD
git diff --check 9d60b59..HEAD
filigree add-comment clarion-b5c50abb19 "Implemented classifier declarations, per-run coverage, and authoritative MCP classification; full gates passed; merged the verified feature SHA onto main; force-reinstalled local CLI/Python/Rust wheels through uv; verified the uv-installed build on Scrappack; recorded Plainweave migration evidence."
filigree close clarion-b5c50abb19 --reason="Loomweave now distinguishes supported-zero, unsupported or unavailable, degraded, and truncated classifier enumeration; verified commits are on main and the exact local wheels are active in uv."
git worktree remove .worktrees/classifier-coverage
git branch -d fix/classifier-coverage-contract
```

Do not close or remove the worktree if any canonical gate fails, the parent merge is incomplete, uv still resolves an older artifact, Scrappack cannot prove supported-zero through the uv-installed build, or the final diff includes unrelated changes.
