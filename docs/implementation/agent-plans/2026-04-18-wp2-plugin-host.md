# WP2 — Plugin Host + Hybrid Authority Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the Sprint 1 walking-skeleton plugin host — JSON-RPC over Content-Length framed stdio (L4), `plugin.toml` manifest parser (L5), core-enforced minimums (L6, all four controls per ADR-021 §2), plugin discovery (L9), crash-loop breaker, and wired `clarion analyze` that spawns a mock plugin and persists entities through the WP1 writer-actor.

**Architecture:** Plugin host lives inside `clarion-core` under a new `plugin/` module (per WP2 §3). Subprocesses are managed via `tokio::process::Command` with piped stdio; framing is hand-rolled LSP-style Content-Length on top of `tokio::io::AsyncRead`/`AsyncWrite` (UQ-WP2-02 resolution). ADR-021 §2d's `RLIMIT_AS` is applied inside `CommandExt::pre_exec` — the only unsafe call site in the workspace, gated by `#[allow(unsafe_code)]` with a safety comment. A fresh workspace crate `clarion-mock-plugin` supplies the fixture binary that the integration-test subprocess spawns.

**Tech Stack:** Additions to the WP1 floor — `toml = "0.8"` (manifest parsing); `nix = { version = "0.28", features = ["resource"] }` (`setrlimit(RLIMIT_AS)`); `tokio` features expanded with `"process"` + `"io-util"`; `tokio-util = { version = "0.7", features = ["codec"] }` is **not** adopted (hand-rolled framing keeps the dependency surface small per UQ-WP2-02). New workspace member `crates/clarion-mock-plugin/` (test fixture binary). All ADR-023 gates (fmt / pedantic clippy / nextest / doc / deny / build dev + release) remain green on every commit.

**Source spec:** `docs/implementation/sprint-1/wp2-plugin-host.md`. If this plan and the spec disagree, the spec is authoritative on *what* to build; this plan is authoritative on *how* to build it step-by-step.

**ADR anchors:** ADR-002 (plugin transport JSON-RPC over Content-Length framed stdio), ADR-021 (plugin authority hybrid — declared capabilities + four core-enforced minimums), ADR-022 (core/plugin ontology boundary — reserved kinds, rule-ID namespaces, identifier grammar), ADR-003 (3-segment EntityId — reused verbatim from WP1 for identity-mismatch rejection), ADR-011 (writer-actor — reused verbatim from WP1 for entity persistence), ADR-023 (tooling baseline — applied to every WP2 commit).

**Resolved UQs before starting:**

- **UQ-WP2-01** — Plugin discovery: PATH scan for `clarion-plugin-*` binaries + neighboring `plugin.toml` fallback to `<install-prefix>/share/clarion/plugins/<name>/plugin.toml`. Sprint 1 hard-codes no env var overrides; `clarion.yaml` discovery config surface is WP6. **Resolved by Task 5.**
- **UQ-WP2-02** — JSON-RPC library: hand-rolled over `serde_json`. Walking skeleton is unidirectional unary; framing is Content-Length + `\r\n\r\n` + body; one request → one response. **Resolved by Task 2.**
- **UQ-WP2-03** — Path jail symlink semantics: canonicalise via `std::fs::canonicalize` which follows symlinks. A symlink inside the root that resolves outside is rejected. **Resolved by Task 4.**
- **UQ-WP2-04** — Content-Length ceiling: **8 MiB** per frame (ADR-021 §2b default; floor 1 MiB). Transport parser refuses the frame before body deserialisation; host kills plugin with SIGTERM → SIGKILL; emits `CLA-INFRA-PLUGIN-FRAME-OVERSIZE`. **Resolved by Task 4.**
- **UQ-WP2-05** — Entity-count cap: **500,000** combined `entity + edge + finding` records per run (ADR-021 §2c default; floor 10,000). On trip: in-flight batch flushed, plugin killed, run enters partial-results; `CLA-INFRA-PLUGIN-ENTITY-CAP` emitted. Sprint 1 emits only entity notifications; the cap counts them and leaves the edge/finding path ready for WP3+. **Resolved by Task 4.**
- **UQ-WP2-06** — prlimit on non-Linux: `#[cfg(target_os = "linux")]`-gate the `setrlimit` pre_exec; on other targets, log a one-shot tracing warning and proceed without the limit. Sprint 1 scope is Linux per WP1 §1; macOS path (ADR-021 §2d names `setrlimit(RLIMIT_AS)` on POSIX) lands with whichever sprint first adds macOS CI. **Resolved by Task 4.**
- **UQ-WP2-07** — Plugin non-entity output: stderr is free-form and forwarded line-by-line to `tracing::info!` target `plugin::<plugin_id>`. Stdout is JSON-RPC only. Progress notifications are deferred. **Resolved by Task 3.**
- **UQ-WP2-08** — Plugin stdout discipline: documented in the plugin-author guide (WP3 scope); host does not enforce. A stray `print()` in a Python plugin corrupts framing → transport parse error → plugin killed → crash-loop counter ticks. **Resolved by Task 3** (doc note in module rustdoc).
- **UQ-WP2-09** — Manifest hot-reload: Sprint 1 always re-reads on `clarion analyze`. Caching belongs to WP8 `serve`. **Resolved by Task 2** (manifest is re-parsed on every `PluginHost::spawn`).
- **UQ-WP2-10** — Crash-loop breaker parameters: **>3 crashes in 60s** → plugin disabled for the run (`CLA-INFRA-PLUGIN-DISABLED-CRASH-LOOP`, per ADR-002 + ADR-021 Layer 3). Path-escape sub-breaker: **>10 escapes in 60s** → plugin killed (`CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE`, per ADR-021 §2a). Both hard-coded in Sprint 1; config surface deferred to WP6. **Resolved by Task 7.**
- **UQ-WP2-11** — Identity-mismatch rejection: host reconstructs `entity_id(plugin_id, kind, qualified_name)` and compares byte-for-byte against the plugin's returned `id`. Mismatch → drop entity, emit `CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH`, plugin stays alive. **Resolved by Task 6.**

**Scope note on "unsafe_code = forbid":** WP1's workspace `[lints.rust]` sets `unsafe_code = "forbid"`. ADR-021 §2d's enforcement point is `CommandExt::pre_exec` — an unsafe API because the closure runs in the fork'd child before exec. Task 4 relaxes the workspace-level lint from `"forbid"` to `"deny"` and places a narrow `#[allow(unsafe_code)]` at the single pre_exec call site in `plugin/limits.rs` with a safety-justifying comment. No other unsafe is introduced. The workspace guarantee becomes: "unsafe is denied except at one audited call site that is the only way to express the ADR-021 §2d enforcement."

**Findings catalogue (WP2-introduced rule IDs):**

| Rule ID | Trigger |
|---|---|
| `CLA-INFRA-PLUGIN-PATH-ESCAPE` | Plugin-returned path canonicalises outside `project_root`; entity dropped, plugin alive. |
| `CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE` | >10 path-escapes in 60s; plugin killed. |
| `CLA-INFRA-PLUGIN-FRAME-OVERSIZE` | Inbound Content-Length header above the 8 MiB ceiling; plugin killed. |
| `CLA-INFRA-PLUGIN-ENTITY-CAP` | Per-run cumulative record count exceeds 500k; plugin killed, run partial. |
| `CLA-INFRA-PLUGIN-OOM-KILLED` | Plugin exit was `WIFSIGNALED && WTERMSIG == 9` after `RLIMIT_AS` hit. |
| `CLA-INFRA-PLUGIN-DISABLED-CRASH-LOOP` | >3 crashes in 60s; plugin disabled for the run. |
| `CLA-INFRA-PLUGIN-UNDECLARED-KIND` | Entity emitted with `kind` not in manifest's `[ontology].entity_kinds`. |
| `CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH` | Entity's `id` does not match `entity_id(plugin_id, kind, qualified_name)`. |
| `CLA-INFRA-MANIFEST-MALFORMED` | Manifest fails grammar or required-field validation at `initialize`. |
| `CLA-INFRA-MANIFEST-RESERVED-KIND` | Manifest declares a core-reserved kind (`file`, `subsystem`, `guidance`). |

**Sprint 1 scope on findings emission:** Sprint 1 does NOT create a `findings` table row for these rule IDs — that path is ADR-013's scanner-ingest API and arrives in WP6. For Sprint 1, each trigger logs via `tracing::warn!(rule_id = "CLA-INFRA-...", ...)` with the offending context as fields. The strings are still authoritative — WP6 reads them when wiring the `Finding` record emission. Tests assert on the log lines via `tracing-test` or captured output, not on DB rows.

---

## Task 1: Workspace prep + L5 manifest parser

**Files:**
- Modify: `/home/john/clarion/Cargo.toml` (workspace deps: add `toml`, extend `tokio` features)
- Modify: `/home/john/clarion/crates/clarion-core/Cargo.toml` (add `toml`, `serde`, `thiserror`)
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs`
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/manifest.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/lib.rs` (add `pub mod plugin;` re-exports)
- Create: `/home/john/clarion/crates/clarion-core/tests/fixtures/manifest_valid.toml`
- Create: `/home/john/clarion/crates/clarion-core/tests/fixtures/manifest_missing_name.toml`

- [ ] **Step 1: Extend workspace dependencies**

Modify the top `[workspace.dependencies]` block in `/home/john/clarion/Cargo.toml` — **add** two entries, **modify** the `tokio` line to include `"process"` and `"io-util"` features:

```toml
# Replace the existing tokio line with:
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "process", "io-util"] }

# Add these new workspace deps (keep alphabetical order):
toml = "0.8"
```

Do NOT add `nix` here — that arrives in Task 4 where it's first used.

- [ ] **Step 2: Extend `clarion-core`'s Cargo.toml**

Modify `/home/john/clarion/crates/clarion-core/Cargo.toml` — add `toml`, `serde`, and `thiserror` to `[dependencies]`. They may already be present from WP1 (entity_id uses serde + thiserror); add only what's missing. The end state is:

```toml
[dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
thiserror.workspace = true
toml.workspace = true
```

(Keep any pre-existing WP1 entries; append `toml.workspace = true` if not already there.)

- [ ] **Step 3: Write the module skeleton**

Create `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs`:

```rust
//! Plugin host — subprocess supervision, JSON-RPC transport, manifest
//! parsing, and ADR-021 core-enforced minimums.
//!
//! # Scope
//!
//! This module is the Clarion-side end of the plugin transport defined in
//! [ADR-002](../../clarion/adr/ADR-002-plugin-transport-json-rpc.md)
//! and the enforcement surface for
//! [ADR-021](../../clarion/adr/ADR-021-plugin-authority-hybrid.md)
//! §2a-d. The ontology boundary rules from
//! [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md)
//! are enforced by [`host`] against the manifest parsed by [`manifest`].
//!
//! # Sub-modules
//!
//! - [`manifest`] — `plugin.toml` parser + validator (L5).
//! - Subsequent WP2 tasks add `transport`, `protocol`, `mock`, `jail`,
//!   `limits`, `discovery`, `host`, `breaker`.

pub mod manifest;

pub use manifest::{
    Capabilities, Manifest, ManifestError, Ontology, PluginHeader, parse_manifest,
};
```

- [ ] **Step 4: Export from `lib.rs`**

Modify `/home/john/clarion/crates/clarion-core/src/lib.rs` to declare the new module. The final file body:

```rust
//! clarion-core — domain types, identifiers, provider traits, and plugin host.
//!
//! WP2 introduces the `plugin` module which drives subprocess plugins
//! via `JSON-RPC` over Content-Length framed stdio. The module's public
//! surface is re-exported at the crate root for short import paths.

pub mod entity_id;
pub mod llm_provider;
pub mod plugin;

pub use entity_id::{EntityId, EntityIdError, entity_id};
pub use llm_provider::{LlmProvider, NoopProvider};
```

- [ ] **Step 5: Add the valid-manifest fixture**

Create `/home/john/clarion/crates/clarion-core/tests/fixtures/manifest_valid.toml`:

```toml
[plugin]
name = "clarion-plugin-python"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-python"
language = "python"
extensions = ["py"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function", "class", "module", "decorator"]
edge_kinds = ["imports", "calls", "decorates", "contains"]
rule_id_prefix = "CLA-PY-"
ontology_version = "0.1.0"
```

- [ ] **Step 6: Add the missing-name fixture**

Create `/home/john/clarion/crates/clarion-core/tests/fixtures/manifest_missing_name.toml`:

```toml
[plugin]
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-python"
language = "python"
extensions = ["py"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-PY-"
ontology_version = "0.1.0"
```

- [ ] **Step 7: Write the failing tests**

Create `/home/john/clarion/crates/clarion-core/src/plugin/manifest.rs` with the test skeleton first (TDD — tests before implementation):

```rust
//! `plugin.toml` parser + validator (L5).
//!
//! Parses the manifest shape locked by WP2 §L5 and validates against
//! [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md):
//! plugin `name` must match the identifier grammar; entity kinds cannot
//! shadow the core-reserved set (`file`, `subsystem`, `guidance`); rule-ID
//! prefix must be `CLA-<PLUGIN_ID_UPPERCASE>-` and end with `-`.
//!
//! # Sprint 1 scope note
//!
//! The manifest is re-parsed on every `PluginHost::spawn` (UQ-WP2-09).
//! Caching belongs to the `serve` path in WP8.

use serde::Deserialize;
use thiserror::Error;

// ===== types defined in Step 8 below =====

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &[u8] = include_bytes!("../../tests/fixtures/manifest_valid.toml");
    const MISSING_NAME: &[u8] =
        include_bytes!("../../tests/fixtures/manifest_missing_name.toml");

    #[test]
    fn parses_valid_manifest() {
        let m = parse_manifest(VALID).expect("valid fixture must parse");
        assert_eq!(m.plugin.name, "clarion-plugin-python");
        assert_eq!(m.plugin.version, "0.1.0");
        assert_eq!(m.plugin.protocol_version, "1.0");
        assert_eq!(m.plugin.executable, "clarion-plugin-python");
        assert_eq!(m.plugin.language, "python");
        assert_eq!(m.plugin.extensions, vec!["py".to_string()]);
        assert_eq!(m.capabilities.max_rss_mb, 512);
        assert_eq!(m.capabilities.max_runtime_seconds, 300);
        assert_eq!(m.capabilities.max_content_length_bytes, 10_485_760);
        assert_eq!(m.capabilities.max_entities_per_run, 100_000);
        assert_eq!(
            m.ontology.entity_kinds,
            vec!["function", "class", "module", "decorator"]
        );
        assert_eq!(
            m.ontology.edge_kinds,
            vec!["imports", "calls", "decorates", "contains"]
        );
        assert_eq!(m.ontology.rule_id_prefix, "CLA-PY-");
        assert_eq!(m.ontology.ontology_version, "0.1.0");
    }

    #[test]
    fn rejects_missing_name() {
        let err = parse_manifest(MISSING_NAME).expect_err("missing name must fail");
        match err {
            ManifestError::Toml(_) => {}
            other => panic!("expected Toml deserialize error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_zero_rss() {
        let input = br#"
[plugin]
name = "clarion-plugin-x"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = ["x"]

[capabilities]
max_rss_mb = 0
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-X-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(input).expect_err("zero RSS must fail");
        assert!(
            matches!(err, ManifestError::InvalidCapability { ref field, .. } if field == &"max_rss_mb"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn rejects_empty_entity_kinds() {
        let input = br#"
[plugin]
name = "clarion-plugin-x"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = ["x"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = []
edge_kinds = ["imports"]
rule_id_prefix = "CLA-X-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(input).expect_err("empty entity_kinds must fail");
        assert!(
            matches!(err, ManifestError::EmptyOntologyField { field: "entity_kinds" }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn rejects_rule_id_prefix_without_trailing_dash() {
        let input = br#"
[plugin]
name = "clarion-plugin-x"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = ["x"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-X"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(input).expect_err("prefix without '-' must fail");
        assert!(
            matches!(err, ManifestError::RuleIdPrefixFormat { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn rejects_reserved_entity_kind_file() {
        let input = br#"
[plugin]
name = "clarion-plugin-x"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = ["x"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function", "file"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-X-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(input).expect_err("reserved kind must fail");
        assert!(
            matches!(err, ManifestError::ReservedKind { ref kind } if kind == "file"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn rejects_plugin_name_uppercase() {
        let input = br#"
[plugin]
name = "Clarion-Plugin-X"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = ["x"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-X-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(input).expect_err("uppercase name must fail");
        assert!(
            matches!(err, ManifestError::InvalidPluginName { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn rejects_empty_extensions() {
        let input = br#"
[plugin]
name = "clarion-plugin-x"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-x"
language = "x"
extensions = []

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 300
max_content_length_bytes = 10485760
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function"]
edge_kinds = ["imports"]
rule_id_prefix = "CLA-X-"
ontology_version = "0.1.0"
"#;
        let err = parse_manifest(input).expect_err("empty extensions must fail");
        assert!(
            matches!(err, ManifestError::EmptyExtensions),
            "unexpected error: {err:?}"
        );
    }
}
```

- [ ] **Step 8: Run tests; expect failure (no types defined)**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core manifest --no-tests=pass 2>&1 | head -40
```

Expected: compile error `cannot find type 'Manifest'` (and several similar). That's the "red" state.

- [ ] **Step 9: Implement the types and `parse_manifest`**

Replace the `// ===== types defined in Step 8 below =====` marker in `plugin/manifest.rs` with:

```rust
/// The top-level `plugin.toml` structure.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub plugin: PluginHeader,
    pub capabilities: Capabilities,
    pub ontology: Ontology,
}

/// The `[plugin]` table.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginHeader {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
    pub executable: String,
    pub language: String,
    pub extensions: Vec<String>,
}

/// The `[capabilities]` table. Values are the plugin's own declared
/// envelope; the core's ADR-021 §2 ceilings apply independently — effective
/// limit is `min(manifest, core)`.
#[derive(Debug, Clone, Deserialize)]
pub struct Capabilities {
    pub max_rss_mb: u64,
    pub max_runtime_seconds: u64,
    pub max_content_length_bytes: u64,
    pub max_entities_per_run: u64,
}

/// The `[ontology]` table.
#[derive(Debug, Clone, Deserialize)]
pub struct Ontology {
    pub entity_kinds: Vec<String>,
    pub edge_kinds: Vec<String>,
    pub rule_id_prefix: String,
    pub ontology_version: String,
}

/// Errors returned by [`parse_manifest`].
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest is not valid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("manifest `TOML` parse/deserialize error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error(
        "[plugin].name {value:?} violates ADR-022 grammar \
         (identifier `[a-z][a-z0-9_-]*`, must start with `clarion-plugin-`)"
    )]
    InvalidPluginName { value: String },

    #[error("[plugin].extensions must declare at least one extension")]
    EmptyExtensions,

    #[error(
        "[capabilities].{field} invariant failed: value {value} outside \
         permitted range (must be > 0)"
    )]
    InvalidCapability { field: &'static str, value: u64 },

    #[error("[ontology].{field} must declare at least one entry")]
    EmptyOntologyField { field: &'static str },

    #[error(
        "[ontology].rule_id_prefix {value:?} must start with `CLA-` and end \
         with `-` (ADR-022 rule-ID namespace contract)"
    )]
    RuleIdPrefixFormat { value: String },

    #[error(
        "[ontology].entity_kinds contains core-reserved kind {kind:?} \
         (per ADR-022: `file`, `subsystem`, `guidance` are core-only)"
    )]
    ReservedKind { kind: String },
}

/// Core-reserved entity kinds per
/// [ADR-022](../../clarion/adr/ADR-022-core-plugin-ontology.md).
/// These are produced by core-owned algorithms (file-discovery, Leiden
/// clustering, guidance composition). A plugin declaring any of them in
/// its `entity_kinds` list is rejected at manifest parse.
const RESERVED_ENTITY_KINDS: &[&str] = &["file", "subsystem", "guidance"];

/// Parse a `plugin.toml` payload and validate it against ADR-022 + the L5
/// schema locked in WP2 §L5.
///
/// # Errors
///
/// Returns a [`ManifestError`] variant for any deserialization or
/// validation failure. The error message names the failing field so
/// plugin authors can fix the manifest without consulting the ADR.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, ManifestError> {
    let text = std::str::from_utf8(bytes)?;
    let manifest: Manifest = toml::from_str(text)?;
    validate(&manifest)?;
    Ok(manifest)
}

fn validate(m: &Manifest) -> Result<(), ManifestError> {
    validate_plugin_name(&m.plugin.name)?;
    if m.plugin.extensions.is_empty() {
        return Err(ManifestError::EmptyExtensions);
    }
    validate_capability("max_rss_mb", m.capabilities.max_rss_mb)?;
    validate_capability("max_runtime_seconds", m.capabilities.max_runtime_seconds)?;
    validate_capability(
        "max_content_length_bytes",
        m.capabilities.max_content_length_bytes,
    )?;
    validate_capability(
        "max_entities_per_run",
        m.capabilities.max_entities_per_run,
    )?;
    if m.ontology.entity_kinds.is_empty() {
        return Err(ManifestError::EmptyOntologyField {
            field: "entity_kinds",
        });
    }
    if m.ontology.edge_kinds.is_empty() {
        return Err(ManifestError::EmptyOntologyField {
            field: "edge_kinds",
        });
    }
    validate_rule_id_prefix(&m.ontology.rule_id_prefix)?;
    for k in &m.ontology.entity_kinds {
        if RESERVED_ENTITY_KINDS.contains(&k.as_str()) {
            return Err(ManifestError::ReservedKind { kind: k.clone() });
        }
    }
    Ok(())
}

fn validate_plugin_name(name: &str) -> Result<(), ManifestError> {
    // ADR-022 grammar for plugin identifiers is `[a-z][a-z0-9_]*`. WP2 §L9
    // requires PATH-installable binaries prefixed `clarion-plugin-`, which
    // broadens the grammar to include `-`. We accept the broader form for
    // the manifest `name` (which is also the binary name) and narrow it
    // back to `[a-z][a-z0-9_]*` when deriving `plugin_id` for `EntityId`
    // assembly. The narrowing is a Task 6 concern; Task 1 only validates
    // the broader binary-name grammar.
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(ManifestError::InvalidPluginName {
            value: name.to_owned(),
        });
    };
    if !first.is_ascii_lowercase() {
        return Err(ManifestError::InvalidPluginName {
            value: name.to_owned(),
        });
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(ManifestError::InvalidPluginName {
                value: name.to_owned(),
            });
        }
    }
    if !name.starts_with("clarion-plugin-") {
        return Err(ManifestError::InvalidPluginName {
            value: name.to_owned(),
        });
    }
    Ok(())
}

fn validate_capability(field: &'static str, value: u64) -> Result<(), ManifestError> {
    if value == 0 {
        return Err(ManifestError::InvalidCapability { field, value });
    }
    Ok(())
}

fn validate_rule_id_prefix(value: &str) -> Result<(), ManifestError> {
    if !value.starts_with("CLA-") || !value.ends_with('-') || value.len() < 6 {
        return Err(ManifestError::RuleIdPrefixFormat {
            value: value.to_owned(),
        });
    }
    Ok(())
}
```

- [ ] **Step 10: Run tests; expect pass**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core manifest --no-tests=pass
```

Expected: 8 tests pass (`parses_valid_manifest`, `rejects_missing_name`, `rejects_zero_rss`, `rejects_empty_entity_kinds`, `rejects_rule_id_prefix_without_trailing_dash`, `rejects_reserved_entity_kind_file`, `rejects_plugin_name_uppercase`, `rejects_empty_extensions`).

- [ ] **Step 11: Full ADR-023 gate sweep**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

All five must exit 0. Pedantic expectations: `doc_markdown` may flag bare `TOML` / `JSON-RPC` tokens — backtick them (`` `TOML` ``, `` `JSON-RPC` ``) in the doc comments above before re-running clippy.

- [ ] **Step 12: Commit**

```bash
cd /home/john/clarion && git add Cargo.toml crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp2): L5 plugin.toml manifest parser and validator

Adds clarion-core::plugin::{manifest,mod} with parse_manifest(&[u8]) ->
Result<Manifest, ManifestError>. Validates against ADR-022: plugin name
grammar [a-z][a-z0-9_-]* with required `clarion-plugin-` prefix; rule-ID
prefix must be CLA-<UPPER>- and end with `-`; entity_kinds cannot shadow
the core-reserved set (file, subsystem, guidance); required fields present;
capabilities strictly positive.

Workspace: toml = "0.8" added; tokio features extended with "process" +
"io-util" (no runtime effect until Task 2). Fixtures live under
crates/clarion-core/tests/fixtures/ and are include_bytes!-loaded into the
unit tests to keep the positive-path assertion a single source of truth.

8 tests: positive, 7 negatives covering every ManifestError variant.
EOF
)"
```

---

## Task 2: L4 JSON-RPC Content-Length transport

**Files:**
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/transport.rs`
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/protocol.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs` (add module decls + re-exports)

- [ ] **Step 1: Extend `plugin/mod.rs`**

Replace the previous `plugin/mod.rs` body with:

```rust
//! Plugin host — subprocess supervision, JSON-RPC transport, manifest
//! parsing, and ADR-021 core-enforced minimums.
//!
//! # Sub-modules
//!
//! - [`manifest`] — `plugin.toml` parser + validator (L5).
//! - [`transport`] — Content-Length framed `JSON-RPC` codec (L4).
//! - [`protocol`] — typed request/response shapes for every L4 method.

pub mod manifest;
pub mod protocol;
pub mod transport;

pub use manifest::{
    Capabilities, Manifest, ManifestError, Ontology, PluginHeader, parse_manifest,
};
pub use protocol::{
    AnalyzeFileParams, AnalyzeFileResult, InitializeParams, InitializeResult, JsonRpcError,
    JsonRpcErrorCode, JsonRpcRequest, JsonRpcResponse, Method, PluginEntity, PluginSource,
    ShutdownParams,
};
pub use transport::{Frame, TransportError, read_frame, write_frame};
```

- [ ] **Step 2: Write `protocol.rs`**

Create `/home/john/clarion/crates/clarion-core/src/plugin/protocol.rs`:

```rust
//! Typed `JSON-RPC` 2.0 shapes for the L4 method set.
//!
//! The walking-skeleton method set (per WP2 §L4 + ADR-002):
//!
//! | Method         | Direction            | Purpose                                  |
//! |----------------|----------------------|------------------------------------------|
//! | `initialize`   | Core → plugin        | Handshake; exchange protocol + manifest  |
//! | `initialized`  | Core → plugin (note) | Start signal                             |
//! | `analyze_file` | Core → plugin        | Per-file entity extraction               |
//! | `shutdown`     | Core → plugin        | Graceful stop                            |
//! | `exit`         | Core → plugin (note) | Forceful termination notification        |
//!
//! Error codes follow the JSON-RPC 2.0 spec plus Clarion-reserved
//! [-32000, -32099] range for transport/host-side violations.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `JSON-RPC` 2.0 request envelope. `id` is `Option<Value>` because
/// notifications (`initialized`, `exit`) omit it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
}

impl JsonRpcRequest {
    /// Build a request carrying the given id, method, and typed params.
    pub fn new<P: Serialize>(id: u64, method: Method, params: &P) -> Result<Self, serde_json::Error> {
        Ok(Self {
            jsonrpc: "2.0".to_owned(),
            method: method.as_str().to_owned(),
            params: Some(serde_json::to_value(params)?),
            id: Some(Value::from(id)),
        })
    }

    /// Build a notification (no id, no response expected).
    pub fn notification<P: Serialize>(method: Method, params: &P) -> Result<Self, serde_json::Error> {
        Ok(Self {
            jsonrpc: "2.0".to_owned(),
            method: method.as_str().to_owned(),
            params: Some(serde_json::to_value(params)?),
            id: None,
        })
    }
}

/// `JSON-RPC` 2.0 response envelope. Exactly one of `result`/`error` is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Value,
}

/// `JSON-RPC` 2.0 error shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Error codes used by the Clarion host/plugin protocol. The standard
/// `JSON-RPC` 2.0 codes are included for completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JsonRpcErrorCode {
    ParseError = -32_700,
    InvalidRequest = -32_600,
    MethodNotFound = -32_601,
    InvalidParams = -32_602,
    InternalError = -32_603,
    /// Clarion-reserved: plugin refused the manifest handshake.
    ManifestRejected = -32_000,
    /// Clarion-reserved: plugin reports an analysis-level error.
    AnalyzeFailed = -32_001,
}

/// Canonical method name enum. Using `Method::AnalyzeFile.as_str()` at
/// the call site keeps the wire literal in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Initialize,
    Initialized,
    AnalyzeFile,
    Shutdown,
    Exit,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Initialized => "initialized",
            Self::AnalyzeFile => "analyze_file",
            Self::Shutdown => "shutdown",
            Self::Exit => "exit",
        }
    }
}

/// `initialize` request params — the core tells the plugin the protocol
/// version it expects and echoes the manifest name/version the core parsed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol_version: String,
    pub plugin_name: String,
    pub plugin_version: String,
    pub project_root: String,
}

/// `initialize` result — the plugin confirms the protocol version and
/// reports any handshake-time capabilities. Sprint 1 only carries the
/// echo fields; WP3+ extends this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol_version: String,
    pub plugin_name: String,
    pub plugin_version: String,
}

/// `analyze_file` request params — the core passes one file per request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeFileParams {
    pub path: String,
}

/// `analyze_file` result — the plugin returns a flat list of entities.
/// Edges and findings arrive via separate notifications in WP3+; Sprint 1
/// only deals with entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeFileResult {
    pub entities: Vec<PluginEntity>,
}

/// Shutdown params — empty. Kept as a struct so the typed-params call
/// sites don't have to special-case `()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShutdownParams {}

/// Entity as emitted by the plugin. The host validates this shape before
/// translating it into [`clarion_storage::EntityRecord`]:
///
/// - `id` must equal `entity_id(plugin_id, kind, qualified_name)` (ADR-003
///   + ADR-022; enforced by [`super::host`] per UQ-WP2-11).
/// - `kind` must appear in the manifest's `[ontology].entity_kinds`
///   (ADR-022; enforced by [`super::host`]).
/// - `source.file_path` must canonicalise inside `project_root`
///   (ADR-021 §2a; enforced by [`super::host`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntity {
    pub id: String,
    pub plugin_id: String,
    pub kind: String,
    pub qualified_name: String,
    pub short_name: String,
    pub source: PluginSource,
    #[serde(default)]
    pub properties: Value,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// Source range an entity came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSource {
    pub file_path: String,
    pub byte_start: Option<i64>,
    pub byte_end: Option<i64>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
}
```

- [ ] **Step 3: Write `transport.rs` — tests first**

Create `/home/john/clarion/crates/clarion-core/src/plugin/transport.rs`:

```rust
//! Content-Length framed `JSON-RPC` codec (L4).
//!
//! Exactly the LSP framing shape:
//!
//! ```text
//! Content-Length: <bytes>\r\n
//! \r\n
//! <body bytes>
//! ```
//!
//! Headers after `Content-Length` are tolerated and ignored (matches LSP
//! behaviour — future extensions may add `Content-Type`). Header lines end
//! with `\r\n`; the blank line `\r\n` separates headers from body.
//!
//! Per ADR-021 §2b, every inbound frame's Content-Length header is checked
//! against the ceiling **before** the body is consumed: a too-large frame
//! returns [`TransportError::FrameTooLarge`] without reading or buffering
//! the payload.

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A decoded frame. The body is raw JSON bytes — callers deserialise into
/// [`crate::plugin::protocol`] types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub body: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error in transport: {0}")]
    Io(#[from] std::io::Error),

    #[error("reached EOF while reading header")]
    UnexpectedEofInHeader,

    #[error("reached EOF while reading body (expected {expected} bytes, got {got})")]
    UnexpectedEofInBody { expected: usize, got: usize },

    #[error("header not valid UTF-8: {0}")]
    HeaderNotUtf8(#[from] std::str::Utf8Error),

    #[error("missing Content-Length header")]
    MissingContentLength,

    #[error("malformed Content-Length header: {raw:?}")]
    MalformedContentLength { raw: String },

    #[error(
        "frame size {observed} exceeds ADR-021 §2b ceiling {ceiling} \
         (rule-id CLA-INFRA-PLUGIN-FRAME-OVERSIZE)"
    )]
    FrameTooLarge { observed: usize, ceiling: usize },
}

/// Read exactly one frame from `reader`. Returns an error if the frame's
/// declared Content-Length exceeds `ceiling_bytes` — in that case, the
/// body is **not** consumed from the reader, so the caller may close the
/// subprocess without draining the oversize payload.
///
/// # Errors
///
/// Returns a [`TransportError`] variant for each failure mode documented
/// on the enum.
pub async fn read_frame<R>(reader: &mut R, ceiling_bytes: usize) -> Result<Frame, TransportError>
where
    R: AsyncRead + Unpin,
{
    let content_length = read_content_length(reader).await?;
    if content_length > ceiling_bytes {
        return Err(TransportError::FrameTooLarge {
            observed: content_length,
            ceiling: ceiling_bytes,
        });
    }
    let mut body = vec![0u8; content_length];
    let mut read = 0;
    while read < content_length {
        let n = reader.read(&mut body[read..]).await?;
        if n == 0 {
            return Err(TransportError::UnexpectedEofInBody {
                expected: content_length,
                got: read,
            });
        }
        read += n;
    }
    Ok(Frame { body })
}

/// Write a frame: `Content-Length: N\r\n\r\n<body>`.
///
/// # Errors
///
/// Returns [`TransportError::Io`] on any underlying write failure.
pub async fn write_frame<W>(writer: &mut W, body: &[u8]) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
{
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_content_length<R>(reader: &mut R) -> Result<usize, TransportError>
where
    R: AsyncRead + Unpin,
{
    let mut content_length: Option<usize> = None;
    loop {
        let line = read_line(reader).await?;
        if line.is_empty() {
            break;
        }
        let line_str = std::str::from_utf8(&line)?;
        if let Some(value) = line_str.strip_prefix("Content-Length:") {
            let trimmed = value.trim();
            content_length = Some(trimmed.parse::<usize>().map_err(|_| {
                TransportError::MalformedContentLength {
                    raw: trimmed.to_owned(),
                }
            })?);
        }
        // Other headers (Content-Type, future additions) are tolerated
        // and ignored per LSP behaviour.
    }
    content_length.ok_or(TransportError::MissingContentLength)
}

/// Read one `\r\n`-terminated line, returning the bytes **before** the
/// `\r\n`. Returns an empty `Vec` when the line is just `\r\n` (end of
/// headers).
async fn read_line<R>(reader: &mut R) -> Result<Vec<u8>, TransportError>
where
    R: AsyncRead + Unpin,
{
    let mut out: Vec<u8> = Vec::with_capacity(64);
    let mut prev: u8 = 0;
    loop {
        let mut b = [0u8; 1];
        let n = reader.read(&mut b).await?;
        if n == 0 {
            return Err(TransportError::UnexpectedEofInHeader);
        }
        if prev == b'\r' && b[0] == b'\n' {
            // Strip the trailing '\r' we wrote a step earlier.
            out.pop();
            return Ok(out);
        }
        out.push(b[0]);
        prev = b[0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    const CEILING_DEFAULT: usize = 8 * 1024 * 1024;

    #[tokio::test]
    async fn round_trip_empty_object() {
        let (mut a, mut b) = duplex(4096);
        let body = br#"{}"#;
        write_frame(&mut a, body).await.unwrap();
        drop(a);
        let frame = read_frame(&mut b, CEILING_DEFAULT).await.unwrap();
        assert_eq!(frame.body, body);
    }

    #[tokio::test]
    async fn round_trip_jsonrpc_request() {
        let (mut a, mut b) = duplex(4096);
        let body =
            br#"{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}"#;
        write_frame(&mut a, body).await.unwrap();
        drop(a);
        let frame = read_frame(&mut b, CEILING_DEFAULT).await.unwrap();
        assert_eq!(frame.body, body);
    }

    #[tokio::test]
    async fn two_frames_back_to_back() {
        let (mut a, mut b) = duplex(4096);
        write_frame(&mut a, br#"{"id":1}"#).await.unwrap();
        write_frame(&mut a, br#"{"id":2}"#).await.unwrap();
        drop(a);
        let f1 = read_frame(&mut b, CEILING_DEFAULT).await.unwrap();
        let f2 = read_frame(&mut b, CEILING_DEFAULT).await.unwrap();
        assert_eq!(f1.body, br#"{"id":1}"#);
        assert_eq!(f2.body, br#"{"id":2}"#);
    }

    #[tokio::test]
    async fn refuses_frame_above_ceiling_without_consuming_body() {
        let (mut a, mut b) = duplex(4096);
        // Declare 1024 bytes but the ceiling is 128 — should fail fast.
        let body = vec![b'x'; 1024];
        write_frame(&mut a, &body).await.unwrap();
        drop(a);
        let err = read_frame(&mut b, 128).await.unwrap_err();
        match err {
            TransportError::FrameTooLarge { observed, ceiling } => {
                assert_eq!(observed, 1024);
                assert_eq!(ceiling, 128);
            }
            other => panic!("expected FrameTooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_content_length_header_rejected() {
        let (mut a, mut b) = duplex(4096);
        a.write_all(b"Content-Type: application/json\r\n\r\n{}")
            .await
            .unwrap();
        drop(a);
        let err = read_frame(&mut b, CEILING_DEFAULT).await.unwrap_err();
        assert!(matches!(err, TransportError::MissingContentLength), "got {err:?}");
    }

    #[tokio::test]
    async fn malformed_content_length_rejected() {
        let (mut a, mut b) = duplex(4096);
        a.write_all(b"Content-Length: not-a-number\r\n\r\n{}")
            .await
            .unwrap();
        drop(a);
        let err = read_frame(&mut b, CEILING_DEFAULT).await.unwrap_err();
        assert!(
            matches!(err, TransportError::MalformedContentLength { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn eof_mid_body_returns_unexpected_eof_in_body() {
        let (mut a, mut b) = duplex(4096);
        // Declare 8 bytes then provide only 3.
        a.write_all(b"Content-Length: 8\r\n\r\nabc").await.unwrap();
        drop(a);
        let err = read_frame(&mut b, CEILING_DEFAULT).await.unwrap_err();
        match err {
            TransportError::UnexpectedEofInBody { expected, got } => {
                assert_eq!(expected, 8);
                assert_eq!(got, 3);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tolerates_unknown_headers_before_content_length() {
        let (mut a, mut b) = duplex(4096);
        a.write_all(b"X-Future-Header: whatever\r\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        drop(a);
        let frame = read_frame(&mut b, CEILING_DEFAULT).await.unwrap();
        assert_eq!(frame.body, b"{}");
    }
}
```

- [ ] **Step 4: Add `tokio/macros` + `tokio/io-util` to `clarion-core`'s dev-dependencies**

Modify `/home/john/clarion/crates/clarion-core/Cargo.toml` to ensure `tokio` (with `macros`, `io-util`, `rt`) is available under `[dev-dependencies]` for the `#[tokio::test]` attribute used above:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "io-util", "rt", "rt-multi-thread"] }
```

(The workspace-level `tokio` already enables these; dev-dependencies inherit features additively.)

- [ ] **Step 5: Run the transport tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core transport --no-tests=pass
```

Expected: 7 tests pass. If any fail with `doc_markdown`-style clippy complaints during compile, backtick `LSP` / `JSON-RPC` / `Content-Length` / `UTF-8` tokens in the module-level doc comments.

- [ ] **Step 6: Full ADR-023 gate sweep**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

- [ ] **Step 7: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp2): L4 JSON-RPC Content-Length transport + typed protocol shapes

plugin/transport.rs implements read_frame/write_frame over any
AsyncRead/AsyncWrite pair. Framing is LSP-identical: Content-Length
header + \r\n\r\n + body. Unknown headers tolerated. Ceiling enforced
BEFORE body read — a too-large frame returns FrameTooLarge without
consuming the payload (ADR-021 §2b enforcement point).

plugin/protocol.rs defines the JSON-RPC 2.0 envelope plus typed params
and results for every L4 method: initialize, initialized, analyze_file,
shutdown, exit. PluginEntity carries the on-wire shape the host
validates before translating to clarion_storage::EntityRecord in Task 6.

7 transport tests: round-trip empty object, round-trip real JSON-RPC
request, two frames back-to-back, ceiling refused before body, missing
Content-Length, malformed Content-Length, EOF mid-body, unknown
headers tolerated.
EOF
)"
```

---

## Task 3: In-process mock plugin test harness

**Files:**
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/mock.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs` (add `mock` module, re-export `MockPlugin` + variant enum under `#[cfg(any(test, feature = "mock"))]` — Sprint 1 uses plain `#[cfg(test)]`)

- [ ] **Step 1: Extend `plugin/mod.rs`**

Replace `plugin/mod.rs` with:

```rust
//! Plugin host — subprocess supervision, `JSON-RPC` transport, manifest
//! parsing, and ADR-021 core-enforced minimums.

pub mod manifest;
pub mod protocol;
pub mod transport;

#[cfg(test)]
pub(crate) mod mock;

pub use manifest::{
    Capabilities, Manifest, ManifestError, Ontology, PluginHeader, parse_manifest,
};
pub use protocol::{
    AnalyzeFileParams, AnalyzeFileResult, InitializeParams, InitializeResult, JsonRpcError,
    JsonRpcErrorCode, JsonRpcRequest, JsonRpcResponse, Method, PluginEntity, PluginSource,
    ShutdownParams,
};
pub use transport::{Frame, TransportError, read_frame, write_frame};
```

- [ ] **Step 2: Write the mock module**

Create `/home/john/clarion/crates/clarion-core/src/plugin/mock.rs`:

```rust
//! In-process mock plugin for unit tests.
//!
//! Provides a [`MockPlugin`] that owns one side of a `tokio::io::duplex`
//! pair and runs a small tokio task pretending to be a plugin. The host
//! drives the other side. This lets us unit-test the transport +
//! handshake logic without a real subprocess.
//!
//! Task 6's integration tests use a real subprocess fixture
//! (`clarion-mock-plugin` crate). This module is for unit-level coverage.
//!
//! # UQ-WP2-07 note
//!
//! Mock plugins do not emit stderr. Real plugins write free-form stderr
//! which the host forwards to `tracing::info!` — that forwarding path is
//! exercised by the subprocess integration tests.
//!
//! # UQ-WP2-08 note
//!
//! Stdout is `JSON-RPC` only. A plugin that prints to stdout corrupts
//! framing → transport parse error → plugin killed. This is plugin-author
//! discipline, not core enforcement; see the plugin-author guide.

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};
use tokio::task::JoinHandle;

use super::protocol::{
    AnalyzeFileParams, AnalyzeFileResult, InitializeParams, InitializeResult, JsonRpcRequest,
    JsonRpcResponse, Method, PluginEntity, PluginSource,
};
use super::transport::{read_frame, write_frame};

/// Which behaviour the mock adopts.
#[derive(Debug, Clone, Copy)]
pub enum MockVariant {
    /// Completes handshake and returns one valid entity per `analyze_file`.
    Compliant,
    /// Completes handshake then exits the task without responding further
    /// — simulates a plugin that dies mid-run.
    CrashingAfterHandshake,
    /// On first `analyze_file`, writes a body larger than 128 bytes —
    /// paired with a ceiling of 128 in the test so the host trips on it.
    Oversize,
}

/// Host-side handles returned when a mock is started.
pub struct MockPlugin {
    /// Host writes requests here.
    pub host_to_plugin: DuplexStream,
    /// Host reads responses from here.
    pub plugin_to_host: DuplexStream,
    /// Background task running the mock's protocol logic.
    pub task: JoinHandle<()>,
}

const CEILING_FOR_TESTS: usize = 8 * 1024 * 1024;

impl MockPlugin {
    /// Spawn a mock of the chosen variant. Returns the two `DuplexStream`
    /// ends for the host side plus a task handle.
    pub fn spawn(variant: MockVariant) -> Self {
        // Two duplex pairs: one carries host→plugin, the other plugin→host.
        let (host_write, mut plugin_read) = duplex(4096);
        let (mut plugin_write, host_read) = duplex(4096);
        let task = tokio::spawn(async move {
            run_mock(variant, &mut plugin_read, &mut plugin_write).await;
        });
        Self {
            host_to_plugin: host_write,
            plugin_to_host: host_read,
            task,
        }
    }
}

async fn run_mock(variant: MockVariant, rx: &mut DuplexStream, tx: &mut DuplexStream) {
    // ---- handshake: expect initialize request ----
    let Ok(frame) = read_frame(rx, CEILING_FOR_TESTS).await else {
        return;
    };
    let req: JsonRpcRequest = match serde_json::from_slice(&frame.body) {
        Ok(r) => r,
        Err(_) => return,
    };
    if req.method != Method::Initialize.as_str() {
        return;
    }
    let Some(id) = req.id.clone() else { return };
    let params: InitializeParams = match req.params {
        Some(ref v) => match serde_json::from_value(v.clone()) {
            Ok(p) => p,
            Err(_) => return,
        },
        None => return,
    };
    let init_result = InitializeResult {
        protocol_version: params.protocol_version,
        plugin_name: params.plugin_name,
        plugin_version: params.plugin_version,
    };
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::to_value(init_result).unwrap()),
        error: None,
        id,
    };
    let body = serde_json::to_vec(&resp).unwrap();
    if write_frame(tx, &body).await.is_err() {
        return;
    }

    // ---- expect `initialized` notification ----
    let Ok(frame) = read_frame(rx, CEILING_FOR_TESTS).await else {
        return;
    };
    let note: JsonRpcRequest = match serde_json::from_slice(&frame.body) {
        Ok(r) => r,
        Err(_) => return,
    };
    if note.method != Method::Initialized.as_str() {
        return;
    }

    if matches!(variant, MockVariant::CrashingAfterHandshake) {
        // Drop both ends to simulate a plugin exiting. The host's next
        // read_frame sees UnexpectedEofInHeader.
        return;
    }

    // ---- response loop ----
    loop {
        let frame = match read_frame(rx, CEILING_FOR_TESTS).await {
            Ok(f) => f,
            Err(_) => return,
        };
        let req: JsonRpcRequest = match serde_json::from_slice(&frame.body) {
            Ok(r) => r,
            Err(_) => return,
        };
        match req.method.as_str() {
            "analyze_file" => {
                let Some(id) = req.id.clone() else { return };
                let params: AnalyzeFileParams =
                    serde_json::from_value(req.params.unwrap_or_default()).unwrap_or(
                        AnalyzeFileParams {
                            path: String::new(),
                        },
                    );
                if matches!(variant, MockVariant::Oversize) {
                    // Write a frame whose Content-Length exceeds the
                    // test ceiling (paired 128 in read_frame).
                    let big = vec![b'x'; 1024];
                    let _ = write_frame(tx, &big).await;
                    return;
                }
                let result = AnalyzeFileResult {
                    entities: vec![PluginEntity {
                        id: format!("mock:function:{}", params.path.replace('/', ".")),
                        plugin_id: "mock".to_owned(),
                        kind: "function".to_owned(),
                        qualified_name: params.path.replace('/', "."),
                        short_name: params
                            .path
                            .rsplit_once('/')
                            .map(|(_, s)| s.to_owned())
                            .unwrap_or_else(|| params.path.clone()),
                        source: PluginSource {
                            file_path: params.path.clone(),
                            byte_start: Some(0),
                            byte_end: Some(10),
                            line_start: Some(1),
                            line_end: Some(1),
                        },
                        properties: serde_json::json!({}),
                        content_hash: None,
                        parent_id: None,
                    }],
                };
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_owned(),
                    result: Some(serde_json::to_value(result).unwrap()),
                    error: None,
                    id,
                };
                let body = serde_json::to_vec(&resp).unwrap();
                if write_frame(tx, &body).await.is_err() {
                    return;
                }
            }
            "shutdown" => {
                let Some(id) = req.id.clone() else { return };
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_owned(),
                    result: Some(serde_json::json!({})),
                    error: None,
                    id,
                };
                let body = serde_json::to_vec(&resp).unwrap();
                let _ = write_frame(tx, &body).await;
            }
            "exit" => return,
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::{InitializeParams, Method};

    #[tokio::test]
    async fn compliant_mock_completes_handshake() {
        let mut mock = MockPlugin::spawn(MockVariant::Compliant);

        let req = JsonRpcRequest::new(
            1,
            Method::Initialize,
            &InitializeParams {
                protocol_version: "1.0".to_owned(),
                plugin_name: "clarion-plugin-mock".to_owned(),
                plugin_version: "0.1.0".to_owned(),
                project_root: "/tmp".to_owned(),
            },
        )
        .unwrap();
        let body = serde_json::to_vec(&req).unwrap();
        write_frame(&mut mock.host_to_plugin, &body).await.unwrap();

        let frame = read_frame(&mut mock.plugin_to_host, CEILING_FOR_TESTS)
            .await
            .unwrap();
        let resp: JsonRpcResponse = serde_json::from_slice(&frame.body).unwrap();
        assert!(resp.error.is_none(), "handshake returned error: {resp:?}");
        assert_eq!(resp.id, serde_json::Value::from(1));

        let note = JsonRpcRequest::notification(Method::Initialized, &serde_json::json!({}))
            .unwrap();
        let body = serde_json::to_vec(&note).unwrap();
        write_frame(&mut mock.host_to_plugin, &body).await.unwrap();

        // Send an analyze_file request and read the one-entity response.
        let req = JsonRpcRequest::new(
            2,
            Method::AnalyzeFile,
            &AnalyzeFileParams {
                path: "demo/hello.py".to_owned(),
            },
        )
        .unwrap();
        let body = serde_json::to_vec(&req).unwrap();
        write_frame(&mut mock.host_to_plugin, &body).await.unwrap();

        let frame = read_frame(&mut mock.plugin_to_host, CEILING_FOR_TESTS)
            .await
            .unwrap();
        let resp: JsonRpcResponse = serde_json::from_slice(&frame.body).unwrap();
        let result: AnalyzeFileResult =
            serde_json::from_value(resp.result.expect("result present")).unwrap();
        assert_eq!(result.entities.len(), 1);
        assert_eq!(result.entities[0].kind, "function");
        assert_eq!(result.entities[0].source.file_path, "demo/hello.py");

        // Tell the mock to exit so the task joins cleanly.
        let note = JsonRpcRequest::notification(Method::Exit, &serde_json::json!({})).unwrap();
        let body = serde_json::to_vec(&note).unwrap();
        write_frame(&mut mock.host_to_plugin, &body).await.unwrap();
        mock.task.await.unwrap();
    }
}
```

Note the minor type-level annoyance: we `use super::super::protocol::{InitializeParams, Method};` inside the test module because the reach-through re-export from `plugin/mod.rs` doesn't include them when `mock` is a `pub(crate)` submodule. The fully qualified path is accurate.

- [ ] **Step 3: Run the mock test**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core mock --no-tests=pass
```

Expected: 1 test (`compliant_mock_completes_handshake`) passes.

- [ ] **Step 4: Full ADR-023 gate sweep + flake-check**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

Three nextest runs in a row verify the async mock doesn't flake (timing-dependent tests are a WP2 hotspot). If any run hangs beyond 30s, kill it and investigate — the duplex-drop ordering is the usual suspect.

- [ ] **Step 5: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp2): in-process mock plugin test harness

plugin/mock.rs provides MockPlugin::spawn(variant) over tokio::io::duplex
pairs. Variants: Compliant (handshake + one-entity analyze_file response),
CrashingAfterHandshake (drops pipes post-handshake), Oversize (writes a
1024-byte frame so tests with ceiling=128 trip FrameTooLarge).

Unit-level coverage: Task 6's real-subprocess integration tests use the
separate clarion-mock-plugin crate spawned via assert_cmd::cargo_bin.

1 test asserts the compliant variant's full handshake + analyze_file
round trip. Module is #[cfg(test)] pub(crate) — no runtime impact on
release builds.
EOF
)"
```

---

## Task 4: L6 core-enforced minimums — jail, limits, prlimit

**Files:**
- Modify: `/home/john/clarion/Cargo.toml` (add `nix` workspace dep; relax `unsafe_code` from `"forbid"` to `"deny"`)
- Modify: `/home/john/clarion/crates/clarion-core/Cargo.toml` (add `nix` as a `[target.'cfg(target_os = "linux")'.dependencies]` entry; add `tracing`)
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/jail.rs`
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/limits.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs` (declare `jail` + `limits`; add re-exports)

- [ ] **Step 1: Relax workspace `unsafe_code` lint**

In `/home/john/clarion/Cargo.toml`, change the `[workspace.lints.rust]` block from:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
```

to:

```toml
[workspace.lints.rust]
# ADR-021 §2d enforcement requires CommandExt::pre_exec (unsafe because the
# closure runs in the fork'd child before exec). Relaxed from "forbid" to
# "deny" so the single audited call site in
# crates/clarion-core/src/plugin/limits.rs can use `#[allow(unsafe_code)]`
# with a safety-justifying comment. No other unsafe is permitted.
unsafe_code = "deny"
```

- [ ] **Step 2: Add `nix` workspace dep**

Append to `[workspace.dependencies]` in `/home/john/clarion/Cargo.toml`:

```toml
nix = { version = "0.28", default-features = false, features = ["resource"] }
tracing-test = "0.2"
```

`tracing-test` is a dev-only helper that captures `tracing` events for assertion; we pin it at workspace level for reuse across crates.

- [ ] **Step 3: Extend `clarion-core/Cargo.toml`**

Modify `/home/john/clarion/crates/clarion-core/Cargo.toml` to add `tracing` + the target-scoped `nix`:

```toml
[dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["io-util", "process"] }
toml.workspace = true
tracing.workspace = true

[target.'cfg(target_os = "linux")'.dependencies]
nix.workspace = true

[dev-dependencies]
tempfile.workspace = true
tokio = { workspace = true, features = ["macros", "io-util", "rt", "rt-multi-thread"] }
tracing-test.workspace = true
```

(Keep any pre-existing entries; the snippet is the full `[dependencies]` / `[dev-dependencies]` / target-scoped section after this task.)

- [ ] **Step 4: Write `jail.rs` tests first**

Create `/home/john/clarion/crates/clarion-core/src/plugin/jail.rs`:

```rust
//! Path-jail helper — ADR-021 §2a enforcement primitive.
//!
//! `jail(root, candidate)` canonicalises both via `std::fs::canonicalize`
//! (follows symlinks per UQ-WP2-03) and returns the canonical candidate if
//! it lies under `root`. A canonical candidate outside `root` returns
//! [`JailError::EscapedRoot`]; the caller decides whether to drop the
//! offending record (ADR-021 §2a first-offense policy) or kill the plugin
//! (path-escape sub-breaker trip — handled in [`super::limits`]).

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum JailError {
    #[error("cannot canonicalise {role} path {path:?}: {source}")]
    Canonicalise {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "{candidate:?} canonicalises outside project root {root:?} \
         (rule-id CLA-INFRA-PLUGIN-PATH-ESCAPE)"
    )]
    EscapedRoot { candidate: PathBuf, root: PathBuf },
}

/// Return the canonical form of `candidate` if it lies under `root`.
///
/// # Errors
///
/// - [`JailError::Canonicalise`] if either path cannot be canonicalised
///   (non-existent, permission denied, etc.).
/// - [`JailError::EscapedRoot`] if the canonical candidate does not have
///   the canonical root as a prefix.
pub fn jail(root: &Path, candidate: &Path) -> Result<PathBuf, JailError> {
    let canon_root = root.canonicalize().map_err(|e| JailError::Canonicalise {
        role: "root",
        path: root.to_path_buf(),
        source: e,
    })?;
    let canon_candidate = candidate
        .canonicalize()
        .map_err(|e| JailError::Canonicalise {
            role: "candidate",
            path: candidate.to_path_buf(),
            source: e,
        })?;
    if canon_candidate.starts_with(&canon_root) {
        Ok(canon_candidate)
    } else {
        Err(JailError::EscapedRoot {
            candidate: canon_candidate,
            root: canon_root,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn admits_path_inside_root() {
        let tmp = tempfile::tempdir().unwrap();
        let inside = tmp.path().join("inside.txt");
        fs::write(&inside, b"").unwrap();
        let admitted = jail(tmp.path(), &inside).unwrap();
        assert!(admitted.starts_with(tmp.path().canonicalize().unwrap()));
    }

    #[test]
    fn rejects_parent_escape_via_dotdot() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"").unwrap();
        let escaping = sub.join("..").join("outside.txt");
        let err = jail(&sub, &escaping).unwrap_err();
        assert!(matches!(err, JailError::EscapedRoot { .. }), "got {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_pointing_outside_root() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, b"").unwrap();
        let link = root.join("link");
        symlink(&outside, &link).unwrap();
        let err = jail(&root, &link).unwrap_err();
        assert!(matches!(err, JailError::EscapedRoot { .. }), "got {err:?}");
    }

    #[test]
    fn rejects_nonexistent_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let ghost = tmp.path().join("ghost.txt");
        let err = jail(tmp.path(), &ghost).unwrap_err();
        assert!(matches!(err, JailError::Canonicalise { role: "candidate", .. }), "got {err:?}");
    }

    #[test]
    fn admits_nested_path() {
        let tmp = tempfile::tempdir().unwrap();
        let nested_dir = tmp.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested_file = nested_dir.join("deep.txt");
        fs::write(&nested_file, b"").unwrap();
        let admitted = jail(tmp.path(), &nested_file).unwrap();
        assert!(admitted.ends_with("deep.txt"));
    }
}
```

- [ ] **Step 5: Write `limits.rs` tests first**

Create `/home/john/clarion/crates/clarion-core/src/plugin/limits.rs`:

```rust
//! Core-enforced ceilings + rolling breakers — ADR-021 §2b/§2c/§2d +
//! path-escape sub-breaker (ADR-021 §2a).
//!
//! This module exposes four primitives:
//!
//! - [`ContentLengthCeiling`] — per-frame inbound byte ceiling. Consumed
//!   by [`super::transport::read_frame`] which already enforces the
//!   numerical comparison; this type exists so the default (8 MiB) and
//!   floor (1 MiB) are named once (ADR-021 §2b).
//! - [`EntityCountCap`] — per-run cumulative cap on `entity + edge +
//!   finding` notifications. Default 500,000, floor 10,000 (ADR-021 §2c).
//! - [`PathEscapeBreaker`] — rolling-window breaker on jail violations.
//!   Default >10 in 60s (ADR-021 §2a sub-breaker). Per Task 6, the host
//!   consults this after each [`super::jail::jail`] violation.
//! - [`apply_prlimit_as`] — installs `RLIMIT_AS` via `setrlimit` inside
//!   `CommandExt::pre_exec`. Default 2 GiB, floor 512 MiB (ADR-021 §2d).
//!   Linux-only; on other targets, a one-shot warning is logged.
//!
//! # Safety
//!
//! [`apply_prlimit_as`] contains the single audited `#[allow(unsafe_code)]`
//! call site in the workspace. The closure passed to `pre_exec` runs in the
//! fork'd child between `fork()` and `execvp()`; the closure body is
//! async-signal-safe (only `setrlimit`, an AS-safe syscall). See the
//! comment above the `unsafe` block for the full safety argument.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use thiserror::Error;

// ===== Content-Length ceiling =====

/// ADR-021 §2b default: 8 MiB per inbound frame.
pub const DEFAULT_CONTENT_LENGTH_CEILING: usize = 8 * 1024 * 1024;
/// ADR-021 §2b floor: 1 MiB.
pub const CONTENT_LENGTH_CEILING_FLOOR: usize = 1024 * 1024;

/// Content-Length ceiling with a floor-clamped constructor.
#[derive(Debug, Clone, Copy)]
pub struct ContentLengthCeiling {
    bytes: usize,
}

impl ContentLengthCeiling {
    pub fn new(bytes: usize) -> Self {
        Self {
            bytes: bytes.max(CONTENT_LENGTH_CEILING_FLOOR),
        }
    }

    pub fn bytes(self) -> usize {
        self.bytes
    }
}

impl Default for ContentLengthCeiling {
    fn default() -> Self {
        Self::new(DEFAULT_CONTENT_LENGTH_CEILING)
    }
}

// ===== Entity-count cap =====

/// ADR-021 §2c default: 500,000 combined records per run.
pub const DEFAULT_ENTITY_COUNT_CAP: u64 = 500_000;
/// ADR-021 §2c floor: 10,000 combined records per run.
pub const ENTITY_COUNT_CAP_FLOOR: u64 = 10_000;

#[derive(Debug, Error)]
#[error(
    "per-run entity-count cap exceeded: {observed} > {cap} \
     (rule-id CLA-INFRA-PLUGIN-ENTITY-CAP)"
)]
pub struct CapExceeded {
    pub observed: u64,
    pub cap: u64,
}

/// Rolling counter across one `analyze` run.
#[derive(Debug, Clone)]
pub struct EntityCountCap {
    cap: u64,
    observed: u64,
}

impl EntityCountCap {
    pub fn new(cap: u64) -> Self {
        Self {
            cap: cap.max(ENTITY_COUNT_CAP_FLOOR),
            observed: 0,
        }
    }

    /// Admit `delta` records. Returns `Ok(())` when the cumulative count
    /// stays at or below the cap; otherwise [`CapExceeded`] and the
    /// internal counter is left at the overflow value (so a follow-up
    /// call also fails — makes the error sticky until the run resets).
    ///
    /// # Errors
    ///
    /// Returns [`CapExceeded`] when admitting `delta` would push the
    /// cumulative count above the cap.
    pub fn try_admit(&mut self, delta: u64) -> Result<(), CapExceeded> {
        let proposed = self.observed.saturating_add(delta);
        if proposed > self.cap {
            self.observed = proposed;
            return Err(CapExceeded {
                observed: self.observed,
                cap: self.cap,
            });
        }
        self.observed = proposed;
        Ok(())
    }

    pub fn observed(&self) -> u64 {
        self.observed
    }
}

impl Default for EntityCountCap {
    fn default() -> Self {
        Self::new(DEFAULT_ENTITY_COUNT_CAP)
    }
}

// ===== Path-escape sub-breaker =====

/// ADR-021 §2a sub-breaker default: >10 escapes in 60s.
pub const DEFAULT_PATH_ESCAPE_LIMIT: usize = 10;
pub const DEFAULT_PATH_ESCAPE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Tripped,
}

#[derive(Debug, Clone)]
pub struct PathEscapeBreaker {
    limit: usize,
    window: Duration,
    events: VecDeque<Instant>,
}

impl PathEscapeBreaker {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            events: VecDeque::new(),
        }
    }

    /// Record one path-escape event at the given timestamp. Returns the
    /// breaker state AFTER recording. `Tripped` is returned once the count
    /// within the window crosses `limit` (i.e. the 11th event at default
    /// limit=10 trips).
    pub fn record_escape_at(&mut self, now: Instant) -> BreakerState {
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        while let Some(front) = self.events.front() {
            if *front < cutoff {
                self.events.pop_front();
            } else {
                break;
            }
        }
        self.events.push_back(now);
        if self.events.len() > self.limit {
            BreakerState::Tripped
        } else {
            BreakerState::Closed
        }
    }

    /// Convenience wrapper using `Instant::now()`.
    pub fn record_escape(&mut self) -> BreakerState {
        self.record_escape_at(Instant::now())
    }

    pub fn events_in_window(&self) -> usize {
        self.events.len()
    }
}

impl Default for PathEscapeBreaker {
    fn default() -> Self {
        Self::new(DEFAULT_PATH_ESCAPE_LIMIT, DEFAULT_PATH_ESCAPE_WINDOW)
    }
}

// ===== RLIMIT_AS via pre_exec =====

/// ADR-021 §2d default: 2 GiB virtual-memory ceiling.
pub const DEFAULT_RLIMIT_AS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// ADR-021 §2d floor: 512 MiB.
pub const RLIMIT_AS_FLOOR_BYTES: u64 = 512 * 1024 * 1024;

/// Install `RLIMIT_AS` on `cmd` so the spawned child inherits the cap.
///
/// `max_as_bytes` is clamped against [`RLIMIT_AS_FLOOR_BYTES`]. On Linux,
/// the limit is applied inside `CommandExt::pre_exec` — the closure runs
/// in the fork'd child between `fork()` and `execvp()`, so a failure there
/// returns an `io::Error` from the parent's `Command::spawn()`.
///
/// On non-Linux targets (Sprint 1 is Linux-only), this function logs a
/// one-shot `tracing::warn!` and returns without modifying the command.
/// See UQ-WP2-06 for the deferral rationale.
#[cfg(target_os = "linux")]
pub fn apply_prlimit_as(cmd: &mut std::process::Command, max_as_bytes: u64) {
    use std::os::unix::process::CommandExt;

    let clamped = max_as_bytes.max(RLIMIT_AS_FLOOR_BYTES);

    // SAFETY: ADR-021 §2d enforcement point. The closure passed to
    // pre_exec runs in the fork'd child between fork() and execvp().
    // At that point, only async-signal-safe calls are permitted. The
    // body calls `nix::sys::resource::setrlimit(Resource::RLIMIT_AS, _, _)`
    // which wraps the `setrlimit(2)` libc function — setrlimit is listed
    // in POSIX.1-2017 §2.4.3 as async-signal-safe. No allocation, no
    // locking, no Rust std calls that could be unsafe post-fork. A
    // failure from setrlimit propagates as an `io::Error` to the parent,
    // which then reports the spawn failure to the caller.
    #[allow(unsafe_code)]
    unsafe {
        cmd.pre_exec(move || {
            use nix::sys::resource::{Resource, setrlimit};
            setrlimit(Resource::RLIMIT_AS, clamped, clamped)
                .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))?;
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn apply_prlimit_as(_cmd: &mut std::process::Command, _max_as_bytes: u64) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "RLIMIT_AS enforcement not implemented on this target \
             (ADR-021 §2d — resolution per UQ-WP2-06 deferred to macOS \
             support sprint); proceeding without a memory ceiling"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_ceiling_defaults_to_8_mib() {
        let c = ContentLengthCeiling::default();
        assert_eq!(c.bytes(), 8 * 1024 * 1024);
    }

    #[test]
    fn content_length_ceiling_clamps_below_floor() {
        let c = ContentLengthCeiling::new(1024);
        assert_eq!(c.bytes(), 1024 * 1024);
    }

    #[test]
    fn content_length_ceiling_honours_above_floor() {
        let c = ContentLengthCeiling::new(16 * 1024 * 1024);
        assert_eq!(c.bytes(), 16 * 1024 * 1024);
    }

    #[test]
    fn entity_count_cap_admits_under_limit() {
        let mut cap = EntityCountCap::new(100);
        assert!(cap.try_admit(50).is_ok());
        assert!(cap.try_admit(50).is_ok());
        assert_eq!(cap.observed(), 100);
    }

    #[test]
    fn entity_count_cap_rejects_over_limit() {
        let mut cap = EntityCountCap::new(100);
        assert!(cap.try_admit(50).is_ok());
        let err = cap.try_admit(51).unwrap_err();
        assert_eq!(err.cap, 100);
        assert_eq!(err.observed, 101);
    }

    #[test]
    fn entity_count_cap_clamps_below_floor() {
        let cap = EntityCountCap::new(1);
        // Floor is 10,000; passed 1 → clamped up.
        let mut cap = cap;
        assert!(cap.try_admit(9_999).is_ok());
        assert!(cap.try_admit(1).is_ok());
        let err = cap.try_admit(1).unwrap_err();
        assert_eq!(err.cap, 10_000);
    }

    #[test]
    fn path_escape_breaker_eleventh_event_trips_at_default() {
        let mut b = PathEscapeBreaker::default();
        let base = Instant::now();
        for i in 0..10 {
            assert_eq!(
                b.record_escape_at(base + Duration::from_millis(i * 10)),
                BreakerState::Closed,
                "event {i} unexpectedly tripped"
            );
        }
        assert_eq!(
            b.record_escape_at(base + Duration::from_millis(200)),
            BreakerState::Tripped,
            "11th event did not trip"
        );
    }

    #[test]
    fn path_escape_breaker_drops_events_outside_window() {
        let mut b = PathEscapeBreaker::new(2, Duration::from_secs(1));
        let base = Instant::now();
        // Two events inside a 1s window — still Closed.
        assert_eq!(b.record_escape_at(base), BreakerState::Closed);
        assert_eq!(
            b.record_escape_at(base + Duration::from_millis(500)),
            BreakerState::Closed
        );
        // A third event > 1s later should find the first two expired, so
        // only the second + this one are "in-window" — 2 events, limit 2,
        // which is NOT strictly greater → still Closed.
        assert_eq!(
            b.record_escape_at(base + Duration::from_millis(1_600)),
            BreakerState::Closed,
            "sliding window failed to drop the first event"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apply_prlimit_as_clamps_below_floor_then_succeeds() {
        // Smoke test: install a floor-clamped limit on a sleep command and
        // make sure spawn returns Ok. We don't try to *trigger* the cap
        // (that would require a program that allocates >512 MiB, which is
        // flaky under CI resource constraints).
        let mut cmd = std::process::Command::new("true");
        apply_prlimit_as(&mut cmd, 1);  // clamps to 512 MiB
        let status = cmd.status().expect("spawn `true` with RLIMIT_AS");
        assert!(status.success());
    }
}
```

- [ ] **Step 6: Extend `plugin/mod.rs` to declare the new modules**

Replace `plugin/mod.rs`:

```rust
//! Plugin host — subprocess supervision, `JSON-RPC` transport, manifest
//! parsing, and ADR-021 core-enforced minimums.

pub mod jail;
pub mod limits;
pub mod manifest;
pub mod protocol;
pub mod transport;

#[cfg(test)]
pub(crate) mod mock;

pub use jail::{JailError, jail};
pub use limits::{
    BreakerState, CONTENT_LENGTH_CEILING_FLOOR, CapExceeded, ContentLengthCeiling,
    DEFAULT_CONTENT_LENGTH_CEILING, DEFAULT_ENTITY_COUNT_CAP, DEFAULT_PATH_ESCAPE_LIMIT,
    DEFAULT_PATH_ESCAPE_WINDOW, DEFAULT_RLIMIT_AS_BYTES, ENTITY_COUNT_CAP_FLOOR,
    EntityCountCap, PathEscapeBreaker, RLIMIT_AS_FLOOR_BYTES, apply_prlimit_as,
};
pub use manifest::{
    Capabilities, Manifest, ManifestError, Ontology, PluginHeader, parse_manifest,
};
pub use protocol::{
    AnalyzeFileParams, AnalyzeFileResult, InitializeParams, InitializeResult, JsonRpcError,
    JsonRpcErrorCode, JsonRpcRequest, JsonRpcResponse, Method, PluginEntity, PluginSource,
    ShutdownParams,
};
pub use transport::{Frame, TransportError, read_frame, write_frame};
```

- [ ] **Step 7: Run the jail + limits tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core jail --no-tests=pass
cd /home/john/clarion && cargo nextest run -p clarion-core limits --no-tests=pass
```

Expected: 5 jail tests pass, 8 limits tests pass (one of which is Linux-gated).

- [ ] **Step 8: Full ADR-023 gate sweep + breaker flake-check**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

Three nextest runs because the breaker tests are timing-adjacent (they use synthetic `Instant`s — should be deterministic — but the triple run catches any accidental `Instant::now()` slippage).

If `cargo deny check` complains about `nix`'s license or dependencies, add the specific license to `deny.toml`'s `[licenses].allow` list **with a commit-message note**; do not disable the gate.

- [ ] **Step 9: Commit**

```bash
cd /home/john/clarion && git add Cargo.toml crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp2): L6 core-enforced minimums — path jail, ceilings, prlimit (ADR-021 defaults)

plugin/jail.rs — `jail(root, candidate) -> Result<PathBuf, JailError>`
canonicalises via std::fs::canonicalize (follows symlinks per UQ-WP2-03)
and rejects anything whose canonical form doesn't start_with the canonical
root. Task 6's host drops offending records on first violation and ticks
the PathEscapeBreaker; the ADR-021 §2a drop-not-kill policy is the
caller's concern, not this helper's.

plugin/limits.rs — four primitives:
  * ContentLengthCeiling: 8 MiB default, 1 MiB floor (ADR-021 §2b).
  * EntityCountCap: 500k default, 10k floor, sticky-error try_admit
    (ADR-021 §2c).
  * PathEscapeBreaker: >10 escapes in 60s trips (ADR-021 §2a sub-breaker).
    Rolling window via VecDeque<Instant>; test uses synthetic timestamps.
  * apply_prlimit_as: sets RLIMIT_AS via CommandExt::pre_exec. 2 GiB
    default, 512 MiB floor. Linux-only; non-Linux targets log a one-shot
    tracing::warn! and skip enforcement (UQ-WP2-06).

Workspace: unsafe_code = "forbid" → "deny". Single audited
#[allow(unsafe_code)] call site at plugin/limits.rs with a fork-safety
comment. nix = "0.28" (features=["resource"]) added as a target-scoped
linux-only dependency. tracing-test = "0.2" added as a workspace
dev-dependency for later tests.

5 jail tests (admit-inside, reject-dotdot, reject-symlink, reject-absent,
admit-nested). 8 limits tests (ceiling default/floor, cap admit/reject/
floor, breaker 11th-event-trips, breaker window-drop, prlimit smoke).
EOF
)"
```

---

## Task 5: L9 plugin discovery

**Files:**
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/discovery.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs` (declare + re-export)

- [ ] **Step 1: Extend `plugin/mod.rs`**

Add to the module declarations in `plugin/mod.rs`:

```rust
pub mod discovery;
```

Add to the re-exports:

```rust
pub use discovery::{DiscoveredPlugin, DiscoveryError, discover, discover_with_path};
```

- [ ] **Step 2: Write `discovery.rs` with tests first**

Create `/home/john/clarion/crates/clarion-core/src/plugin/discovery.rs`:

```rust
//! Plugin discovery (L9).
//!
//! Convention: the core scans `$PATH` for executable files whose basename
//! matches `clarion-plugin-*`. For each candidate binary, it looks for a
//! `plugin.toml` alongside it; if absent, it falls back to
//! `<parent-dir>/../share/clarion/plugins/<binary-basename>/plugin.toml`.
//!
//! UQ-WP2-01 proposal: PATH-based with neighboring-manifest fallback.
//! Resolved here.
//!
//! # Sprint 1 scope
//!
//! - Linux-only path separator (`:`). Windows support is NG for Sprint 1.
//! - First-match-wins on duplicate basenames (stable PATH order).
//! - A candidate with no resolvable `plugin.toml` is silently skipped and
//!   logged at `tracing::debug!`. It is not an error — the user may have
//!   a `clarion-plugin-*` binary on PATH that isn't a Clarion plugin.

use std::path::{Path, PathBuf};

use thiserror::Error;

use super::manifest::{Manifest, ManifestError, parse_manifest};

/// A plugin found on `$PATH` whose neighboring (or share-dir) manifest
/// parsed successfully.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub executable: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: Manifest,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("PATH environment variable is not set")]
    NoPath,
}

/// Discover plugins using the process's `$PATH`.
///
/// # Errors
///
/// Returns [`DiscoveryError::NoPath`] if `$PATH` is unset.
pub fn discover() -> Result<Vec<DiscoveredPlugin>, DiscoveryError> {
    let path = std::env::var_os("PATH").ok_or(DiscoveryError::NoPath)?;
    Ok(discover_with_path(path.to_string_lossy().as_ref()))
}

/// Discover plugins in the colon-separated `path` list. Testable variant
/// of [`discover`] — tests assemble a synthetic `$PATH` pointing at
/// tempdirs.
pub fn discover_with_path(path: &str) -> Vec<DiscoveredPlugin> {
    let mut out: Vec<DiscoveredPlugin> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for dir in path.split(':').filter(|s| !s.is_empty()) {
        let dir = PathBuf::from(dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            if !name_str.starts_with("clarion-plugin-") {
                continue;
            }
            if seen.iter().any(|s| s == name_str) {
                continue;
            }
            let exec_path = entry.path();
            if !is_executable_file(&exec_path) {
                continue;
            }
            match locate_manifest(&exec_path, name_str) {
                Some((manifest_path, manifest)) => {
                    seen.push(name_str.to_owned());
                    out.push(DiscoveredPlugin {
                        executable: exec_path,
                        manifest_path,
                        manifest,
                    });
                }
                None => {
                    tracing::debug!(
                        executable = %exec_path.display(),
                        "clarion-plugin-* candidate has no resolvable plugin.toml; skipping"
                    );
                }
            }
        }
    }
    out
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    md.permissions().mode() & 0o111 != 0
}

fn locate_manifest(exec: &Path, binary_name: &str) -> Option<(PathBuf, Manifest)> {
    // Primary: plugin.toml beside the binary.
    if let Some(parent) = exec.parent() {
        let candidate = parent.join("plugin.toml");
        if let Some(manifest) = read_and_parse(&candidate) {
            return Some((candidate, manifest));
        }
        // Fallback: <parent>/../share/clarion/plugins/<name>/plugin.toml
        let candidate = parent
            .join("..")
            .join("share")
            .join("clarion")
            .join("plugins")
            .join(binary_name)
            .join("plugin.toml");
        if let Some(manifest) = read_and_parse(&candidate) {
            return Some((candidate, manifest));
        }
    }
    None
}

fn read_and_parse(path: &Path) -> Option<Manifest> {
    let bytes = std::fs::read(path).ok()?;
    match parse_manifest(&bytes) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "plugin.toml exists but failed to parse — skipping"
            );
            None
        }
    }
}

// Exhaustive-match stub so clippy doesn't flag the ManifestError re-export
// as dead code under some feature gates.
#[doc(hidden)]
pub fn __manifest_error_fan_out(_: ManifestError) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const VALID: &[u8] = include_bytes!("../../tests/fixtures/manifest_valid.toml");

    fn mk_exec(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&p, perm).unwrap();
        p
    }

    #[test]
    fn finds_plugin_with_neighboring_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = mk_exec(tmp.path(), "clarion-plugin-python");
        fs::write(tmp.path().join("plugin.toml"), VALID).unwrap();
        let path = tmp.path().to_string_lossy().into_owned();

        let plugins = discover_with_path(&path);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].executable, bin);
        assert_eq!(plugins[0].manifest.plugin.name, "clarion-plugin-python");
    }

    #[test]
    fn finds_plugin_via_share_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        // Simulated install layout:
        //   <tmp>/bin/clarion-plugin-python
        //   <tmp>/share/clarion/plugins/clarion-plugin-python/plugin.toml
        let bin_dir = tmp.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let share_dir = tmp
            .path()
            .join("share")
            .join("clarion")
            .join("plugins")
            .join("clarion-plugin-python");
        fs::create_dir_all(&share_dir).unwrap();
        fs::write(share_dir.join("plugin.toml"), VALID).unwrap();
        let bin = mk_exec(&bin_dir, "clarion-plugin-python");
        let path = bin_dir.to_string_lossy().into_owned();

        let plugins = discover_with_path(&path);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].executable, bin);
    }

    #[test]
    fn skips_non_clarion_prefixed_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        mk_exec(tmp.path(), "some-other-binary");
        fs::write(tmp.path().join("plugin.toml"), VALID).unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        assert!(discover_with_path(&path).is_empty());
    }

    #[test]
    fn skips_clarion_candidate_without_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        mk_exec(tmp.path(), "clarion-plugin-python");
        let path = tmp.path().to_string_lossy().into_owned();
        assert!(discover_with_path(&path).is_empty());
    }

    #[test]
    fn skips_non_executable_clarion_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("clarion-plugin-python");
        fs::write(&p, b"not executable").unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o644);
        fs::set_permissions(&p, perm).unwrap();
        fs::write(tmp.path().join("plugin.toml"), VALID).unwrap();
        let path = tmp.path().to_string_lossy().into_owned();
        assert!(discover_with_path(&path).is_empty());
    }

    #[test]
    fn duplicate_basename_first_wins() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        mk_exec(tmp1.path(), "clarion-plugin-python");
        fs::write(tmp1.path().join("plugin.toml"), VALID).unwrap();
        mk_exec(tmp2.path(), "clarion-plugin-python");
        fs::write(tmp2.path().join("plugin.toml"), VALID).unwrap();
        let path = format!(
            "{}:{}",
            tmp1.path().to_string_lossy(),
            tmp2.path().to_string_lossy()
        );
        let plugins = discover_with_path(&path);
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].executable.starts_with(tmp1.path()));
    }
}
```

- [ ] **Step 3: Run discovery tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core discovery --no-tests=pass
```

Expected: 6 tests pass.

- [ ] **Step 4: Full ADR-023 gate sweep**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

- [ ] **Step 5: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp2): L9 plugin discovery convention (PATH + neighboring manifest)

plugin/discovery.rs scans $PATH for executable files prefixed
`clarion-plugin-`. For each, it looks for plugin.toml beside the binary
first, then falls back to <parent>/../share/clarion/plugins/<name>/
plugin.toml. Non-matching names are skipped; candidates without a
resolvable manifest are skipped with a debug trace; duplicate basenames
across PATH entries keep the first-found wins.

6 tests: neighboring manifest, share-fallback manifest, non-clarion prefix
skipped, missing-manifest skipped, non-executable file skipped, PATH-order
deduplication.
EOF
)"
```

---

## Task 6: Plugin-host supervisor + `clarion-mock-plugin` fixture

**Files:**
- Create: `/home/john/clarion/crates/clarion-mock-plugin/` (new workspace crate)
- Create: `/home/john/clarion/crates/clarion-mock-plugin/Cargo.toml`
- Create: `/home/john/clarion/crates/clarion-mock-plugin/src/main.rs`
- Create: `/home/john/clarion/crates/clarion-mock-plugin/fixtures/plugin.toml`
- Modify: `/home/john/clarion/Cargo.toml` (add `clarion-mock-plugin` to `members`)
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/host.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs` (declare + re-export `host`)
- Create: `/home/john/clarion/crates/clarion-core/tests/host_integration.rs`

- [ ] **Step 1: Create the fixture crate's manifest**

Add `"crates/clarion-mock-plugin"` to the `members` array in the root `Cargo.toml`.

Create `/home/john/clarion/crates/clarion-mock-plugin/Cargo.toml`:

```toml
[package]
name = "clarion-mock-plugin"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[[bin]]
name = "clarion-mock-plugin"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
clarion-core = { path = "../clarion-core", version = "0.1.0-dev" }
serde_json.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "io-util", "io-std"] }
```

Note: we need tokio's `io-std` feature for `tokio::io::{stdin, stdout}`. The feature has to be enabled additively — add it to the workspace `tokio` definition:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "process", "io-util", "io-std"] }
```

(Update the workspace `tokio` line in `/home/john/clarion/Cargo.toml` accordingly.)

- [ ] **Step 2: Write the fixture plugin binary**

Create `/home/john/clarion/crates/clarion-mock-plugin/src/main.rs`:

```rust
//! Fixture binary for WP2 host integration tests.
//!
//! Behaviour is driven by a single CLI mode argument:
//!
//! ```text
//! clarion-mock-plugin compliant         # handshake + 1 valid entity per analyze_file
//! clarion-mock-plugin undeclared-kind   # emits an entity with kind="widget"
//! clarion-mock-plugin id-mismatch       # emits an entity whose `id` != entity_id(...)
//! clarion-mock-plugin path-escape       # emits an entity with file_path outside project_root
//! clarion-mock-plugin repeat-path-escape <n>  # emits n escape entities across one analyze_file
//! clarion-mock-plugin crash             # crashes immediately after handshake
//! ```
//!
//! Reads `JSON-RPC` frames from stdin, writes frames to stdout, logs
//! free-form text to stderr (host forwards to `tracing::info!`).

use anyhow::{Context, Result};
use clarion_core::plugin::protocol::{
    AnalyzeFileParams, AnalyzeFileResult, InitializeParams, InitializeResult, JsonRpcRequest,
    JsonRpcResponse, Method, PluginEntity, PluginSource,
};
use clarion_core::plugin::transport::{read_frame, write_frame};
use tokio::io::{stdin, stdout};

const CEILING: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
enum Mode {
    Compliant,
    UndeclaredKind,
    IdMismatch,
    PathEscape,
    RepeatPathEscape(u32),
    Crash,
}

fn parse_mode(args: &[String]) -> Result<Mode> {
    let mode = args.get(1).map(String::as_str).unwrap_or("compliant");
    match mode {
        "compliant" => Ok(Mode::Compliant),
        "undeclared-kind" => Ok(Mode::UndeclaredKind),
        "id-mismatch" => Ok(Mode::IdMismatch),
        "path-escape" => Ok(Mode::PathEscape),
        "repeat-path-escape" => {
            let n: u32 = args
                .get(2)
                .context("repeat-path-escape requires <n>")?
                .parse()
                .context("parse n")?;
            Ok(Mode::RepeatPathEscape(n))
        }
        "crash" => Ok(Mode::Crash),
        other => anyhow::bail!("unknown mode: {other}"),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = parse_mode(&args)?;

    let mut stdin = stdin();
    let mut stdout = stdout();
    eprintln!("clarion-mock-plugin: started in mode {mode:?}");

    // initialize
    let frame = read_frame(&mut stdin, CEILING).await?;
    let req: JsonRpcRequest = serde_json::from_slice(&frame.body)?;
    let params: InitializeParams = serde_json::from_value(req.params.unwrap_or_default())?;
    let result = InitializeResult {
        protocol_version: params.protocol_version,
        plugin_name: params.plugin_name,
        plugin_version: params.plugin_version,
    };
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::to_value(result)?),
        error: None,
        id: req.id.unwrap_or(serde_json::Value::Null),
    };
    write_frame(&mut stdout, &serde_json::to_vec(&resp)?).await?;

    // initialized notification
    let _ = read_frame(&mut stdin, CEILING).await?;

    if matches!(mode, Mode::Crash) {
        eprintln!("clarion-mock-plugin: crash mode — exit 1");
        std::process::exit(1);
    }

    // request loop
    loop {
        let frame = match read_frame(&mut stdin, CEILING).await {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };
        let req: JsonRpcRequest = serde_json::from_slice(&frame.body)?;
        match req.method.as_str() {
            m if m == Method::AnalyzeFile.as_str() => {
                let params: AnalyzeFileParams = serde_json::from_value(
                    req.params.unwrap_or_default(),
                )?;
                let result = build_response(mode, &params.path);
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_owned(),
                    result: Some(serde_json::to_value(result)?),
                    error: None,
                    id: req.id.unwrap_or(serde_json::Value::Null),
                };
                write_frame(&mut stdout, &serde_json::to_vec(&resp)?).await?;
            }
            m if m == Method::Shutdown.as_str() => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_owned(),
                    result: Some(serde_json::json!({})),
                    error: None,
                    id: req.id.unwrap_or(serde_json::Value::Null),
                };
                write_frame(&mut stdout, &serde_json::to_vec(&resp)?).await?;
            }
            m if m == Method::Exit.as_str() => return Ok(()),
            _ => {} // ignore unknown
        }
    }
}

fn build_response(mode: Mode, path: &str) -> AnalyzeFileResult {
    fn valid_entity(plugin_id: &str, qualified: &str, file: &str) -> PluginEntity {
        let short = qualified.rsplit('.').next().unwrap_or(qualified).to_owned();
        PluginEntity {
            id: format!("{plugin_id}:function:{qualified}"),
            plugin_id: plugin_id.to_owned(),
            kind: "function".to_owned(),
            qualified_name: qualified.to_owned(),
            short_name: short,
            source: PluginSource {
                file_path: file.to_owned(),
                byte_start: Some(0),
                byte_end: Some(10),
                line_start: Some(1),
                line_end: Some(1),
            },
            properties: serde_json::json!({}),
            content_hash: None,
            parent_id: None,
        }
    }

    match mode {
        Mode::Compliant => AnalyzeFileResult {
            entities: vec![valid_entity("mock", "demo.hello", path)],
        },
        Mode::UndeclaredKind => {
            let mut e = valid_entity("mock", "demo.widget", path);
            e.kind = "widget".to_owned();
            e.id = "mock:widget:demo.widget".to_owned();
            AnalyzeFileResult { entities: vec![e] }
        }
        Mode::IdMismatch => {
            let mut e = valid_entity("mock", "demo.hello", path);
            e.id = "mock:function:not.matching".to_owned();
            AnalyzeFileResult { entities: vec![e] }
        }
        Mode::PathEscape => AnalyzeFileResult {
            entities: vec![valid_entity("mock", "demo.hello", "/tmp/outside-root.py")],
        },
        Mode::RepeatPathEscape(n) => AnalyzeFileResult {
            entities: (0..n)
                .map(|i| {
                    valid_entity("mock", &format!("demo.esc{i}"), "/tmp/outside-root.py")
                })
                .collect(),
        },
        Mode::Crash => unreachable!(),
    }
}
```

- [ ] **Step 3: Create the fixture manifest**

Create `/home/john/clarion/crates/clarion-mock-plugin/fixtures/plugin.toml`:

```toml
[plugin]
name = "clarion-plugin-mock"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-mock-plugin"
language = "mock"
extensions = ["mock"]

[capabilities]
max_rss_mb = 512
max_runtime_seconds = 60
max_content_length_bytes = 8388608
max_entities_per_run = 100000

[ontology]
entity_kinds = ["function"]
edge_kinds = ["calls"]
rule_id_prefix = "CLA-MOCK-"
ontology_version = "0.1.0"
```

- [ ] **Step 4: Write the host module with integration tests first (TDD)**

Create `/home/john/clarion/crates/clarion-core/tests/host_integration.rs`. Tests here are red until Step 5 lands the implementation.

```rust
//! WP2 Task 6 host-supervisor integration tests.
//!
//! Uses the `clarion-mock-plugin` fixture binary spawned via cargo-bin.
//!
//! Asserts:
//! - handshake + analyze_file round-trip + clean shutdown (happy path)
//! - ADR-022 ontology enforcement: kind not in manifest → entity dropped
//! - UQ-WP2-11 identity mismatch → entity dropped
//! - ADR-021 §2a drop-not-kill: escape path → entity dropped, plugin alive
//! - ADR-021 §2a sub-breaker: 11 escapes → plugin killed

use std::path::PathBuf;

use clarion_core::plugin::host::{HostError, PluginHost};
use clarion_core::plugin::manifest::parse_manifest;

fn mock_plugin_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clarion-mock-plugin"))
}

fn mock_manifest() -> clarion_core::plugin::Manifest {
    let bytes = std::fs::read(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../clarion-mock-plugin/fixtures/plugin.toml"),
    )
    .expect("read fixture manifest");
    parse_manifest(&bytes).expect("fixture manifest must parse")
}

async fn spawn_with_mode(mode: &str, project_root: &std::path::Path) -> PluginHost {
    PluginHost::spawn(
        mock_plugin_binary(),
        vec![mode.to_owned()],
        mock_manifest(),
        project_root.to_path_buf(),
    )
    .await
    .expect("spawn mock plugin")
}

#[tokio::test]
async fn happy_path_handshake_and_analyze_file() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("demo.py");
    std::fs::write(&target, b"x = 1\n").unwrap();

    let mut host = spawn_with_mode("compliant", tmp.path()).await;
    let entities = host.analyze_file(&target).await.expect("analyze_file");
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].kind, "function");
    assert_eq!(entities[0].id, "mock:function:demo.hello");
    host.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn undeclared_kind_entity_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("demo.py");
    std::fs::write(&target, b"x = 1\n").unwrap();

    let mut host = spawn_with_mode("undeclared-kind", tmp.path()).await;
    let entities = host.analyze_file(&target).await.expect("analyze_file");
    assert!(
        entities.is_empty(),
        "widget kind should have been dropped; got {entities:?}"
    );
    host.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn id_mismatch_entity_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("demo.py");
    std::fs::write(&target, b"x = 1\n").unwrap();

    let mut host = spawn_with_mode("id-mismatch", tmp.path()).await;
    let entities = host.analyze_file(&target).await.expect("analyze_file");
    assert!(entities.is_empty(), "id-mismatch must drop: {entities:?}");
    host.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn path_escape_drops_entity_plugin_stays_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("demo.py");
    std::fs::write(&target, b"x = 1\n").unwrap();

    let mut host = spawn_with_mode("path-escape", tmp.path()).await;
    let entities = host.analyze_file(&target).await.expect("analyze_file");
    assert!(entities.is_empty(), "escape must drop: {entities:?}");

    // Plugin must still be alive — issue a second analyze_file.
    let entities = host.analyze_file(&target).await.expect("second analyze_file");
    assert!(entities.is_empty());
    host.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn eleven_escapes_trip_sub_breaker_and_kill() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("demo.py");
    std::fs::write(&target, b"x = 1\n").unwrap();

    let mut host = spawn_with_mode("repeat-path-escape 11", tmp.path()).await;
    let err = host.analyze_file(&target).await.unwrap_err();
    assert!(
        matches!(err, HostError::PathEscapeBreakerTripped { .. }),
        "expected breaker trip, got {err:?}"
    );
}
```

Note on fixture spawn args: `PluginHost::spawn` takes `Vec<String>` extra args. The test passes `"repeat-path-escape 11"` as a single string; split in the implementation via `split_whitespace`. That's fine for fixture use.

- [ ] **Step 5: Write `plugin/host.rs`**

Create `/home/john/clarion/crates/clarion-core/src/plugin/host.rs`:

```rust
//! Plugin-host supervisor (WP2 Task 6).
//!
//! Spawns a plugin subprocess, performs the L4 handshake, drives
//! `analyze_file` requests, and validates responses against ADR-022
//! (ontology boundary + identity reconstruction) and ADR-021 §2a (path
//! jail drop-not-kill + sub-breaker). Applies ADR-021 §2d `RLIMIT_AS` on
//! spawn.
//!
//! Shutdown discipline: `shutdown` consumes `self`, sends `shutdown` then
//! the `exit` notification, and waits for the child with a 5-second
//! timeout before SIGKILL.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

use crate::entity_id::{EntityIdError, entity_id};

use super::jail::{JailError, jail};
use super::limits::{
    BreakerState, CapExceeded, ContentLengthCeiling, DEFAULT_CONTENT_LENGTH_CEILING,
    DEFAULT_ENTITY_COUNT_CAP, DEFAULT_RLIMIT_AS_BYTES, EntityCountCap, PathEscapeBreaker,
    apply_prlimit_as,
};
use super::manifest::Manifest;
use super::protocol::{
    AnalyzeFileParams, AnalyzeFileResult, InitializeParams, InitializeResult, JsonRpcRequest,
    JsonRpcResponse, Method, PluginEntity, ShutdownParams,
};
use super::transport::{TransportError, read_frame, write_frame};

#[derive(Debug, Error)]
pub enum HostError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("transport: {0}")]
    Transport(#[from] TransportError),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("plugin returned JSON-RPC error: code={code} message={message}")]
    RpcError { code: i32, message: String },

    #[error("plugin shutdown did not complete within {0:?}")]
    ShutdownTimeout(Duration),

    #[error(
        "plugin path-escape sub-breaker tripped after {count} escapes — \
         plugin killed (rule-id CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE)"
    )]
    PathEscapeBreakerTripped { count: usize },

    #[error("per-run entity-count cap exceeded: {0}")]
    EntityCap(#[from] CapExceeded),

    #[error("plugin exited unexpectedly with status {status:?}")]
    UnexpectedExit { status: Option<i32> },
}

/// The spawned plugin and its state.
pub struct PluginHost {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    manifest: Manifest,
    project_root: PathBuf,
    ceiling: ContentLengthCeiling,
    escape_breaker: PathEscapeBreaker,
    entity_cap: EntityCountCap,
    next_id: AtomicU64,
}

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A validated entity ready for persistence. `source_file_id` /
/// `content_hash` carry through from the plugin; translating to
/// `clarion_storage::EntityRecord` is the caller's concern (Task 8) —
/// this struct deliberately mirrors the plugin shape so the host
/// doesn't need a direct dependency on `clarion-storage`.
#[derive(Debug, Clone)]
pub struct ValidatedEntity {
    pub id: String,
    pub plugin_id: String,
    pub kind: String,
    pub name: String,
    pub short_name: String,
    pub parent_id: Option<String>,
    pub source_file: PathBuf,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    pub source_line_start: Option<i64>,
    pub source_line_end: Option<i64>,
    pub properties_json: String,
    pub content_hash: Option<String>,
}

impl PluginHost {
    /// Spawn the plugin binary and complete the handshake.
    ///
    /// # Errors
    ///
    /// Returns a [`HostError`] variant on spawn failure, transport error,
    /// serde error, or if the plugin returns a `JSON-RPC` error during
    /// `initialize`.
    pub async fn spawn(
        executable: PathBuf,
        extra_args: Vec<String>,
        manifest: Manifest,
        project_root: PathBuf,
    ) -> Result<Self, HostError> {
        // Split tokens so tests can pass "repeat-path-escape 11" as one
        // logical arg and we forward the shell-split form.
        let args: Vec<String> = extra_args
            .iter()
            .flat_map(|s| s.split_whitespace().map(str::to_owned))
            .collect();

        let effective_as = manifest
            .capabilities
            .max_rss_mb
            .saturating_mul(1024 * 1024)
            .min(DEFAULT_RLIMIT_AS_BYTES);

        let mut std_cmd = std::process::Command::new(&executable);
        std_cmd
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_prlimit_as(&mut std_cmd, effective_as);

        let mut cmd = tokio::process::Command::from(std_cmd);
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .expect("piped stdin should be available post-spawn");
        let stdout = child
            .stdout
            .take()
            .expect("piped stdout should be available post-spawn");
        if let Some(stderr) = child.stderr.take() {
            let plugin_name = manifest.plugin.name.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    tracing::info!(target: "plugin::stderr", plugin = %plugin_name, "{line}");
                }
            });
        }

        let mut host = Self {
            child,
            stdin,
            stdout,
            manifest,
            project_root,
            ceiling: ContentLengthCeiling::default(),
            escape_breaker: PathEscapeBreaker::default(),
            entity_cap: EntityCountCap::new(DEFAULT_ENTITY_COUNT_CAP),
            next_id: AtomicU64::new(1),
        };
        host.handshake().await?;
        Ok(host)
    }

    async fn handshake(&mut self) -> Result<(), HostError> {
        let id = self.next_request_id();
        let req = JsonRpcRequest::new(
            id,
            Method::Initialize,
            &InitializeParams {
                protocol_version: self.manifest.plugin.protocol_version.clone(),
                plugin_name: self.manifest.plugin.name.clone(),
                plugin_version: self.manifest.plugin.version.clone(),
                project_root: self.project_root.to_string_lossy().into_owned(),
            },
        )?;
        write_frame(&mut self.stdin, &serde_json::to_vec(&req)?).await?;
        let frame = read_frame(&mut self.stdout, self.ceiling.bytes()).await?;
        let resp: JsonRpcResponse = serde_json::from_slice(&frame.body)?;
        if let Some(err) = resp.error {
            return Err(HostError::RpcError {
                code: err.code,
                message: err.message,
            });
        }
        let _result: InitializeResult = serde_json::from_value(
            resp.result.unwrap_or_default(),
        )?;

        let note = JsonRpcRequest::notification(Method::Initialized, &serde_json::json!({}))?;
        write_frame(&mut self.stdin, &serde_json::to_vec(&note)?).await?;
        Ok(())
    }

    /// Analyze one file, returning the list of entities that survive all
    /// validations.
    pub async fn analyze_file(
        &mut self,
        path: &Path,
    ) -> Result<Vec<ValidatedEntity>, HostError> {
        let _jailed = jail(&self.project_root, path).map_err(|e| match e {
            JailError::EscapedRoot { .. } => HostError::PathEscapeBreakerTripped { count: 1 },
            JailError::Canonicalise { source, .. } => HostError::Io(source),
        })?;
        let id = self.next_request_id();
        let req = JsonRpcRequest::new(
            id,
            Method::AnalyzeFile,
            &AnalyzeFileParams {
                path: path.to_string_lossy().into_owned(),
            },
        )?;
        write_frame(&mut self.stdin, &serde_json::to_vec(&req)?).await?;
        let frame = read_frame(&mut self.stdout, self.ceiling.bytes()).await?;
        let resp: JsonRpcResponse = serde_json::from_slice(&frame.body)?;
        if let Some(err) = resp.error {
            return Err(HostError::RpcError {
                code: err.code,
                message: err.message,
            });
        }
        let result: AnalyzeFileResult = serde_json::from_value(
            resp.result.unwrap_or_default(),
        )?;

        let mut accepted: Vec<ValidatedEntity> = Vec::with_capacity(result.entities.len());
        for entity in result.entities {
            match self.validate_entity(entity) {
                Ok(Some(v)) => accepted.push(v),
                Ok(None) => {} // dropped with logged finding
                Err(e) => return Err(e),
            }
        }
        self.entity_cap
            .try_admit(accepted.len() as u64)
            .map_err(HostError::from)?;
        Ok(accepted)
    }

    fn validate_entity(
        &mut self,
        e: PluginEntity,
    ) -> Result<Option<ValidatedEntity>, HostError> {
        // ADR-022: kind must be declared in the manifest's entity_kinds.
        if !self
            .manifest
            .ontology
            .entity_kinds
            .iter()
            .any(|k| k == &e.kind)
        {
            tracing::warn!(
                rule_id = "CLA-INFRA-PLUGIN-UNDECLARED-KIND",
                plugin = %self.manifest.plugin.name,
                kind = %e.kind,
                entity_id = %e.id,
                "dropping entity with kind not declared in manifest"
            );
            return Ok(None);
        }

        // ADR-003 + UQ-WP2-11: id must match entity_id(plugin_id, kind, qualified_name).
        // plugin_id narrows `clarion-plugin-<suffix>` to `<suffix>` with dashes
        // replaced by underscores — the grammar in ADR-022 forbids dashes in
        // plugin_id.
        let derived_plugin_id = derive_plugin_id(&self.manifest.plugin.name);
        let expected_id = match entity_id(&derived_plugin_id, &e.kind, &e.qualified_name) {
            Ok(id) => id,
            Err(err) => {
                tracing::warn!(
                    rule_id = "CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH",
                    plugin = %self.manifest.plugin.name,
                    entity_id = %e.id,
                    error = %err,
                    "dropping entity whose reconstruction failed"
                );
                return Ok(None);
            }
        };
        if expected_id.as_str() != e.id || e.plugin_id != derived_plugin_id {
            tracing::warn!(
                rule_id = "CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH",
                plugin = %self.manifest.plugin.name,
                expected = %expected_id,
                observed = %e.id,
                "dropping entity with mismatched id"
            );
            return Ok(None);
        }

        // ADR-021 §2a: jail the source path.
        let source_path = PathBuf::from(&e.source.file_path);
        match jail(&self.project_root, &source_path) {
            Ok(canonical) => {
                let properties_json = if e.properties.is_null() {
                    "{}".to_owned()
                } else {
                    serde_json::to_string(&e.properties)?
                };
                Ok(Some(ValidatedEntity {
                    id: e.id,
                    plugin_id: e.plugin_id,
                    kind: e.kind,
                    name: e.qualified_name.clone(),
                    short_name: e.short_name,
                    parent_id: e.parent_id,
                    source_file: canonical,
                    source_byte_start: e.source.byte_start,
                    source_byte_end: e.source.byte_end,
                    source_line_start: e.source.line_start,
                    source_line_end: e.source.line_end,
                    properties_json,
                    content_hash: e.content_hash,
                }))
            }
            Err(JailError::EscapedRoot { candidate, .. }) => {
                tracing::warn!(
                    rule_id = "CLA-INFRA-PLUGIN-PATH-ESCAPE",
                    plugin = %self.manifest.plugin.name,
                    offending_path = %candidate.display(),
                    "dropping entity whose source path escapes project_root"
                );
                if self.escape_breaker.record_escape() == BreakerState::Tripped {
                    let count = self.escape_breaker.events_in_window();
                    tracing::warn!(
                        rule_id = "CLA-INFRA-PLUGIN-DISABLED-PATH-ESCAPE",
                        plugin = %self.manifest.plugin.name,
                        count = count,
                        "path-escape sub-breaker tripped; killing plugin"
                    );
                    let _ = self.child.start_kill();
                    return Err(HostError::PathEscapeBreakerTripped { count });
                }
                Ok(None)
            }
            Err(JailError::Canonicalise { source, .. }) => Err(HostError::Io(source)),
        }
    }

    /// Shut down the plugin cleanly: send `shutdown`, await the reply,
    /// send `exit`, then wait for the child to exit with a 5s timeout.
    pub async fn shutdown(mut self) -> Result<(), HostError> {
        let id = self.next_request_id();
        let req = JsonRpcRequest::new(id, Method::Shutdown, &ShutdownParams::default())?;
        write_frame(&mut self.stdin, &serde_json::to_vec(&req)?).await?;
        let _frame = read_frame(&mut self.stdout, self.ceiling.bytes()).await?;
        let note = JsonRpcRequest::notification(Method::Exit, &serde_json::json!({}))?;
        write_frame(&mut self.stdin, &serde_json::to_vec(&note)?).await?;
        drop(self.stdin);
        match timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(HostError::Io(e)),
            Err(_) => {
                let _ = self.child.start_kill();
                Err(HostError::ShutdownTimeout(SHUTDOWN_TIMEOUT))
            }
        }
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Derive the ADR-022-compliant plugin_id from the manifest name.
/// `clarion-plugin-python` → `python`. Dashes are replaced with
/// underscores so the grammar `[a-z][a-z0-9_]*` is satisfied.
fn derive_plugin_id(manifest_name: &str) -> String {
    manifest_name
        .strip_prefix("clarion-plugin-")
        .unwrap_or(manifest_name)
        .replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_plugin_id_strips_prefix() {
        assert_eq!(derive_plugin_id("clarion-plugin-python"), "python");
        assert_eq!(derive_plugin_id("clarion-plugin-foo-bar"), "foo_bar");
    }
}

// EntityIdError is part of this module's public error surface reach —
// keep the import live even if no variant is currently constructed here.
#[doc(hidden)]
fn _entity_id_error_type_reach(e: EntityIdError) -> EntityIdError {
    e
}
```

- [ ] **Step 6: Extend `plugin/mod.rs` to declare host**

Add to `plugin/mod.rs`:

```rust
pub mod host;
pub use host::{HostError, PluginHost, ValidatedEntity};
```

- [ ] **Step 7: Run host integration tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core --test host_integration --no-tests=pass
```

Expected: 5 tests pass (`happy_path_handshake_and_analyze_file`, `undeclared_kind_entity_dropped`, `id_mismatch_entity_dropped`, `path_escape_drops_entity_plugin_stays_alive`, `eleven_escapes_trip_sub_breaker_and_kill`).

If the last test flakes, it's usually one of:
- The mock emits the 11 entities in one response; the host processes them in-order and trips on the 11th. If the test expects `err` but the host returned `Ok(vec![])` with all 11 dropped, the breaker `>` vs `>=` threshold is the bug.
- The fixture's `repeat-path-escape 11` spawn arg didn't split properly. Check `PluginHost::spawn`'s `split_whitespace` flatten.

- [ ] **Step 8: Full ADR-023 gate sweep + flake-check x3**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

- [ ] **Step 9: Commit**

```bash
cd /home/john/clarion && git add Cargo.toml crates/ && git commit -m "$(cat <<'EOF'
feat(wp2): plugin-host supervisor with ADR-021 enforcement + ADR-022 ontology

plugin/host.rs — PluginHost::spawn(executable, args, manifest, project_root)
applies RLIMIT_AS on spawn, pipes stdio, performs the L4 handshake, and
exposes analyze_file + shutdown.

Per-entity validation on each analyze_file response:
  * ADR-022: kind must be in manifest.ontology.entity_kinds (drop + log
    CLA-INFRA-PLUGIN-UNDECLARED-KIND).
  * UQ-WP2-11: id must equal entity_id(plugin_id, kind, qualified_name)
    (drop + log CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH).
  * ADR-021 §2a: source.file_path must canonicalise inside project_root
    (drop + log CLA-INFRA-PLUGIN-PATH-ESCAPE + tick sub-breaker; trip on
    11th escape kills the plugin + returns HostError).
  * ADR-021 §2c: per-run entity-count cap consulted after each batch.

stderr is forwarded line-by-line to tracing::info! target plugin::stderr
(UQ-WP2-07 resolution). shutdown consumes self, sends shutdown+exit, waits
with a 5s timeout, SIGKILLs on timeout.

crates/clarion-mock-plugin — new workspace fixture binary. Modes: compliant,
undeclared-kind, id-mismatch, path-escape, repeat-path-escape <n>, crash.
Each writes JSON-RPC frames on stdout, reads from stdin, logs free-form to
stderr. Host integration tests drive it via CARGO_BIN_EXE_clarion-mock-plugin.

5 host integration tests: happy path, undeclared kind dropped, id mismatch
dropped, single escape dropped + plugin alive, 11 escapes trip the
sub-breaker and kill. 1 unit test for derive_plugin_id.
EOF
)"
```

---

## Task 7: Crash-loop breaker

**Files:**
- Create: `/home/john/clarion/crates/clarion-core/src/plugin/breaker.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/plugin/mod.rs` (declare + re-export)

- [ ] **Step 1: Extend `plugin/mod.rs`**

Add:

```rust
pub mod breaker;
pub use breaker::{CrashLoopBreaker, CrashLoopState, DEFAULT_CRASH_LIMIT, DEFAULT_CRASH_WINDOW};
```

- [ ] **Step 2: Write `breaker.rs` tests first**

Create `/home/john/clarion/crates/clarion-core/src/plugin/breaker.rs`:

```rust
//! Per-plugin crash-loop breaker (ADR-002 + ADR-021 Layer 3).
//!
//! Default: >3 crashes in 60s trips the breaker and disables the plugin
//! for the run. Sprint 1 hard-codes these values (UQ-WP2-10); config
//! surface lives in `clarion.yaml:plugin_limits.*` from WP6 onwards.
//!
//! The breaker's only job is to answer "can I spawn this plugin right
//! now?" and "record that this plugin just crashed". Actual spawn,
//! kill, and finding emission are [`super::host`]'s concerns.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const DEFAULT_CRASH_LIMIT: usize = 3;
pub const DEFAULT_CRASH_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashLoopState {
    /// Spawn is permitted.
    Closed,
    /// Breaker tripped — spawn refused until the window elapses.
    Tripped,
}

#[derive(Debug, Clone)]
pub struct CrashLoopBreaker {
    limit: usize,
    window: Duration,
    events: VecDeque<Instant>,
}

impl CrashLoopBreaker {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            events: VecDeque::new(),
        }
    }

    /// Record a crash event at `now`. Returns the breaker state AFTER
    /// recording: `Tripped` once `limit` is exceeded within `window`.
    pub fn record_crash_at(&mut self, now: Instant) -> CrashLoopState {
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        while let Some(front) = self.events.front() {
            if *front < cutoff {
                self.events.pop_front();
            } else {
                break;
            }
        }
        self.events.push_back(now);
        self.state_at(now)
    }

    pub fn record_crash(&mut self) -> CrashLoopState {
        self.record_crash_at(Instant::now())
    }

    /// Query current state without recording a new event.
    pub fn state_at(&self, now: Instant) -> CrashLoopState {
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        let in_window = self.events.iter().filter(|&&t| t >= cutoff).count();
        if in_window > self.limit {
            CrashLoopState::Tripped
        } else {
            CrashLoopState::Closed
        }
    }

    pub fn state(&self) -> CrashLoopState {
        self.state_at(Instant::now())
    }
}

impl Default for CrashLoopBreaker {
    fn default() -> Self {
        Self::new(DEFAULT_CRASH_LIMIT, DEFAULT_CRASH_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourth_crash_within_window_trips_at_default() {
        let mut b = CrashLoopBreaker::default();
        let t0 = Instant::now();
        for i in 0..3 {
            assert_eq!(
                b.record_crash_at(t0 + Duration::from_millis(i * 10)),
                CrashLoopState::Closed,
                "crash {i} unexpectedly tripped"
            );
        }
        assert_eq!(
            b.record_crash_at(t0 + Duration::from_millis(100)),
            CrashLoopState::Tripped,
            "4th crash did not trip"
        );
    }

    #[test]
    fn crashes_outside_window_dont_count() {
        let mut b = CrashLoopBreaker::new(2, Duration::from_secs(1));
        let t0 = Instant::now();
        assert_eq!(b.record_crash_at(t0), CrashLoopState::Closed);
        assert_eq!(
            b.record_crash_at(t0 + Duration::from_millis(500)),
            CrashLoopState::Closed
        );
        // 1.6s later — first two expired; this is a single in-window event.
        assert_eq!(
            b.record_crash_at(t0 + Duration::from_millis(1_600)),
            CrashLoopState::Closed
        );
    }

    #[test]
    fn query_state_without_recording() {
        let mut b = CrashLoopBreaker::new(1, Duration::from_secs(60));
        let t0 = Instant::now();
        b.record_crash_at(t0);
        b.record_crash_at(t0);
        assert_eq!(b.state_at(t0), CrashLoopState::Tripped);
    }
}
```

- [ ] **Step 3: Run breaker tests; expect pass**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core breaker --no-tests=pass
```

Expected: 3 tests pass.

- [ ] **Step 4: Add an integration test using the crashing mock fixture**

Append to `/home/john/clarion/crates/clarion-core/tests/host_integration.rs`:

```rust
#[tokio::test]
async fn crashing_plugin_trips_breaker_across_spawns() {
    use clarion_core::plugin::breaker::{CrashLoopBreaker, CrashLoopState};
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let mut breaker = CrashLoopBreaker::new(3, Duration::from_secs(60));

    for i in 0..4 {
        // Spawning the crash mock succeeds (handshake completes) but the
        // plugin exits 1 immediately after. The host's child handle goes
        // to "exited" next poll.
        let host_result = spawn_with_mode("crash", tmp.path()).await;
        drop(host_result); // plugin has already exited by now
        let state = breaker.record_crash();
        if i < 3 {
            assert_eq!(state, CrashLoopState::Closed);
        } else {
            assert_eq!(state, CrashLoopState::Tripped);
        }
    }
}
```

Note: the `crash` mode in the fixture exits 1 immediately after handshake, so `spawn_with_mode("crash", ...)` returns a `PluginHost` whose child has already exited. The test doesn't exercise the host's kill path — it just verifies the breaker's arithmetic across repeated spawn attempts. This matches the spec: Sprint 1's breaker is scaffolding; "a unit test proves the breaker trips".

- [ ] **Step 5: Run the updated integration test**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core --test host_integration --no-tests=pass
cd /home/john/clarion && cargo nextest run -p clarion-core --test host_integration --no-tests=pass
cd /home/john/clarion && cargo nextest run -p clarion-core --test host_integration --no-tests=pass
```

Three runs for timing flake check. All 6 integration tests should pass each run.

- [ ] **Step 6: Full ADR-023 gate sweep**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

- [ ] **Step 7: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp2): crash-loop breaker

plugin/breaker.rs — CrashLoopBreaker with ADR-002 / ADR-021 Layer 3
defaults: >3 crashes in 60s trips. Rolling window via VecDeque<Instant>;
synthetic timestamps in unit tests keep the arithmetic deterministic.

3 unit tests: 4th-crash-trips, outside-window-resets, state-query-no-record.
1 added integration test: crashing mock binary spawned 4 times; breaker
records each crash; 4th record returns Tripped.

Sprint 1 scope per UQ-WP2-10: the breaker is a pure counter today;
spawn-refusal wiring is Task 8/WP6. The data structure and thresholds
are locked; wiring is the next layer up.
EOF
)"
```

---

## Task 8: Wire `clarion analyze` to use the plugin host

**Files:**
- Modify: `/home/john/clarion/crates/clarion-cli/Cargo.toml` (add `clarion-core` plugin surface, `walkdir`)
- Modify: `/home/john/clarion/crates/clarion-cli/src/analyze.rs` (replace Sprint-1 stub)
- Modify: `/home/john/clarion/Cargo.toml` (add `walkdir` workspace dep)
- Create: `/home/john/clarion/crates/clarion-cli/tests/analyze_with_plugin.rs`

- [ ] **Step 1: Add `walkdir` workspace dep**

Append to `[workspace.dependencies]`:

```toml
walkdir = "2"
```

- [ ] **Step 2: Extend `clarion-cli/Cargo.toml`**

Add to `[dependencies]`:

```toml
walkdir.workspace = true
```

Nothing else needs adding — `clarion-core` is already a path dependency and the plugin module is exposed at the crate root through the re-exports in `plugin/mod.rs`.

- [ ] **Step 3: Rewrite `analyze.rs` to use the plugin host**

Replace `/home/john/clarion/crates/clarion-cli/src/analyze.rs` (keep the helpers `iso8601_now` + `civil_from_unix_secs` from Sprint 1 — they're still needed for timestamps):

```rust
//! `clarion analyze` — WP2-wired walking skeleton.
//!
//! Discovers plugins via [`clarion_core::plugin::discover`], spawns each,
//! walks the project tree, calls `analyze_file` per matching file, and
//! persists returned entities through the WP1 writer-actor.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use uuid::Uuid;
use walkdir::WalkDir;

use clarion_core::plugin::host::{PluginHost, ValidatedEntity};
use clarion_core::plugin::{discovery, manifest::Manifest};
use clarion_storage::{
    DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY, EntityRecord, RunStatus, Writer,
    commands::WriterCmd,
};

/// Run the analyze command against `project_path`.
///
/// # Errors
///
/// Returns an error if the target directory does not exist, has no
/// `.clarion/` directory, or if any subsystem (discovery, spawn,
/// writer-actor) fails fatally.
pub async fn run(project_path: PathBuf) -> Result<()> {
    if !project_path.exists() {
        bail!(
            "target directory does not exist: {}. Pass a valid path or cd to it first.",
            project_path.display()
        );
    }
    let project_root = project_path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise path {}", project_path.display()))?;
    let clarion_dir = project_root.join(".clarion");
    if !clarion_dir.exists() {
        bail!(
            "{} has no .clarion/ directory. Run `clarion install` first.",
            project_root.display()
        );
    }
    let db_path = clarion_dir.join("clarion.db");

    let plugins = discovery::discover().context("plugin discovery")?;

    let (writer, handle) = Writer::spawn(db_path, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("spawn writer actor")?;
    let run_id = Uuid::new_v4().to_string();
    let started_at = iso8601_now();

    writer
        .send_wait(|ack| WriterCmd::BeginRun {
            run_id: run_id.clone(),
            config_json: "{}".into(),
            started_at: started_at.clone(),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("BeginRun")?;

    let mut total_entities: u64 = 0;
    let mut any_failure = false;

    if plugins.is_empty() {
        tracing::warn!(run_id = %run_id, "no plugins discovered on PATH");
    }

    for plugin in plugins {
        match drive_plugin(&plugin, &project_root, &writer, &run_id).await {
            Ok(count) => {
                total_entities += count;
                tracing::info!(
                    plugin = %plugin.manifest.plugin.name,
                    entities = count,
                    "plugin finished"
                );
            }
            Err(e) => {
                any_failure = true;
                tracing::error!(
                    plugin = %plugin.manifest.plugin.name,
                    error = %e,
                    "plugin failed; continuing with remaining plugins"
                );
            }
        }
    }

    let completed_at = iso8601_now();
    let status = if any_failure {
        RunStatus::Failed
    } else if total_entities == 0 {
        RunStatus::SkippedNoPlugins
    } else {
        RunStatus::Completed
    };
    let stats = format!(r#"{{"entities_inserted":{total_entities}}}"#);
    let cmd_status = status;
    writer
        .send_wait(move |ack| WriterCmd::CommitRun {
            run_id: run_id.clone(),
            status: cmd_status,
            completed_at: completed_at.clone(),
            stats_json: stats.clone(),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("CommitRun")?;

    drop(writer);
    handle
        .await
        .map_err(|e| anyhow::anyhow!("writer actor panic: {e}"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("analyze complete: status {}", status.as_str());
    Ok(())
}

async fn drive_plugin(
    plugin: &discovery::DiscoveredPlugin,
    project_root: &Path,
    writer: &Writer,
    _run_id: &str,
) -> Result<u64> {
    let mut host = PluginHost::spawn(
        plugin.executable.clone(),
        vec![],
        plugin.manifest.clone(),
        project_root.to_path_buf(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("plugin spawn")?;

    let mut count: u64 = 0;
    for file in walk_files(project_root, &plugin.manifest) {
        match host.analyze_file(&file).await {
            Ok(entities) => {
                for v in entities {
                    persist_entity(writer, v).await?;
                    count += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    plugin = %plugin.manifest.plugin.name,
                    file = %file.display(),
                    error = %e,
                    "analyze_file failed; skipping file"
                );
            }
        }
    }

    host.shutdown()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("plugin shutdown")?;
    Ok(count)
}

fn walk_files(project_root: &Path, manifest: &Manifest) -> Vec<PathBuf> {
    let exts: Vec<&str> = manifest
        .plugin
        .extensions
        .iter()
        .map(String::as_str)
        .collect();
    WalkDir::new(project_root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let path = entry.path();
            let ext = path.extension()?.to_str()?;
            if exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                Some(path.to_path_buf())
            } else {
                None
            }
        })
        // Skip anything inside the .clarion/ state directory.
        .filter(|p| !p.components().any(|c| c.as_os_str() == ".clarion"))
        .collect()
}

async fn persist_entity(writer: &Writer, v: ValidatedEntity) -> Result<()> {
    let now = iso8601_now();
    let record = EntityRecord {
        id: v.id,
        plugin_id: v.plugin_id,
        kind: v.kind,
        name: v.name,
        short_name: v.short_name,
        parent_id: v.parent_id,
        source_file_id: Some(v.source_file.to_string_lossy().into_owned()),
        source_byte_start: v.source_byte_start,
        source_byte_end: v.source_byte_end,
        source_line_start: v.source_line_start,
        source_line_end: v.source_line_end,
        properties_json: v.properties_json,
        content_hash: v.content_hash,
        summary_json: None,
        wardline_json: None,
        first_seen_commit: None,
        last_seen_commit: None,
        created_at: now.clone(),
        updated_at: now,
    };
    writer
        .send_wait(|ack| WriterCmd::InsertEntity {
            entity: Box::new(record),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("InsertEntity")
}

fn iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("SystemTime before UNIX epoch");
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let (y, mo, da, h, mi, se) = civil_from_unix_secs(secs);
    format!("{y:04}-{mo:02}-{da:02}T{h:02}:{mi:02}:{se:02}.{millis:03}Z")
}

fn civil_from_unix_secs(mut secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let se = u32::try_from(secs % 60).expect("modulo 60 fits in u32");
    secs /= 60;
    let mi = u32::try_from(secs % 60).expect("modulo 60 fits in u32");
    secs /= 60;
    let h = u32::try_from(secs % 24).expect("modulo 24 fits in u32");
    secs /= 24;
    let days = i64::try_from(secs).expect("days since epoch fits in i64");
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u64::try_from(z - era * 146_097).expect("day-of-era is non-negative");
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y_shifted = i64::try_from(yoe).expect("year-of-era fits in i64") + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let da = u32::try_from(doy - (153 * mp + 2) / 5 + 1).expect("day-of-month fits in u32");
    let mo = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).expect("month fits in u32");
    let y_i64 = if mo <= 2 { y_shifted + 1 } else { y_shifted };
    let y = u32::try_from(y_i64).expect("year fits in u32 (post-1970)");
    (y, mo, da, h, mi, se)
}
```

- [ ] **Step 4: Write the plugin-wired integration test**

Create `/home/john/clarion/crates/clarion-cli/tests/analyze_with_plugin.rs`:

```rust
//! `clarion analyze` with a mock plugin on PATH produces persisted entities.
//!
//! Assembles a temporary `$PATH` containing the `clarion-mock-plugin`
//! fixture binary and a neighboring `plugin.toml`. Runs `clarion install`
//! then `clarion analyze` and asserts the DB row shape.

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

fn mock_plugin_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clarion-mock-plugin"))
}

fn mock_manifest_bytes() -> Vec<u8> {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../clarion-mock-plugin/fixtures/plugin.toml"
    );
    std::fs::read(fixture).expect("read mock manifest fixture")
}

#[test]
fn analyze_with_mock_plugin_persists_entities() {
    let project_dir = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();

    // Symlink the real mock-plugin binary into bin_dir under the expected name.
    let symlinked = bin_dir.path().join("clarion-plugin-mock");
    #[cfg(unix)]
    std::os::unix::fs::symlink(mock_plugin_bin(), &symlinked).unwrap();
    #[cfg(not(unix))]
    compile_error!("WP2 Sprint 1 is Linux-only; this test requires symlinks");

    // Neighboring manifest.
    fs::write(bin_dir.path().join("plugin.toml"), mock_manifest_bytes()).unwrap();

    // One `.mock` file for the plugin to pick up.
    fs::write(project_dir.path().join("demo.mock"), b"sample\n").unwrap();

    // clarion install
    clarion_bin()
        .args(["install", "--path"])
        .arg(project_dir.path())
        .assert()
        .success();

    // clarion analyze with our custom PATH
    clarion_bin()
        .env("PATH", bin_dir.path())
        .args(["analyze"])
        .arg(project_dir.path())
        .assert()
        .success();

    // Assert the DB row shape.
    let db = project_dir.path().join(".clarion").join("clarion.db");
    let conn = Connection::open(&db).unwrap();
    let (count, status): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(status), '') FROM runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(status, "completed");

    let entity_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert!(entity_count >= 1, "expected at least one entity; got {entity_count}");

    let id: String = conn
        .query_row("SELECT id FROM entities LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(
        id.starts_with("mock:function:"),
        "entity id does not match the fixture mock's emission: {id}"
    );
}
```

**Integration-test machinery caveat:** `Command::cargo_bin("clarion")` requires the `clarion` binary to exist in a known location. `CARGO_BIN_EXE_clarion-mock-plugin` is set automatically because `clarion-cli` depends on `clarion-mock-plugin` via its dev-dependencies only if declared. Add the fixture as a dev-dependency of `clarion-cli` so the env var is populated:

Modify `/home/john/clarion/crates/clarion-cli/Cargo.toml` `[dev-dependencies]`:

```toml
[dev-dependencies]
assert_cmd.workspace = true
clarion-mock-plugin = { path = "../clarion-mock-plugin", version = "0.1.0-dev" }
rusqlite.workspace = true
serde_json.workspace = true
tempfile.workspace = true
```

- [ ] **Step 5: Run the analyze tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-cli --no-tests=pass
```

Expected: all `clarion-cli` tests pass, including the new `analyze_with_mock_plugin_persists_entities` (plus WP1's pre-existing install + analyze tests).

- [ ] **Step 6: Full ADR-023 gate sweep**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

- [ ] **Step 7: Commit**

```bash
cd /home/john/clarion && git add Cargo.toml crates/ && git commit -m "$(cat <<'EOF'
feat(wp2): wire clarion analyze to plugin host

analyze.rs — replaces the Sprint-1 skipped_no_plugins stub. Discovers
plugins via clarion_core::plugin::discover, spawns each PluginHost, walks
the project tree filtering by the manifest's [plugin].extensions, calls
analyze_file per file, and persists each ValidatedEntity via the WP1
writer-actor InsertEntity path. Per-plugin shutdown is clean (shutdown +
exit + 5s wait).

Run status wiring:
  * Completed when at least one entity persisted and no plugin failed.
  * SkippedNoPlugins when discovery returned empty or no entities emerged.
  * Failed when any plugin errored fatally (the other plugins still run;
    this matches "partial-results" framing from ADR-021 §2c).

walkdir = "2" added as a workspace dep. clarion-mock-plugin added as a
dev-dependency of clarion-cli so integration tests get the fixture's
CARGO_BIN_EXE_ env var.

1 new integration test assembles a temp PATH dir with a symlinked
clarion-mock-plugin + neighboring plugin.toml, runs clarion install +
analyze, and asserts status=completed + entities >= 1 + id prefix
"mock:function:".
EOF
)"
```

---

## Task 9: WP2 end-to-end smoke test

**Files:**
- Create: `/home/john/clarion/crates/clarion-cli/tests/wp2_e2e.rs`

- [ ] **Step 1: Write the E2E smoke test**

Create `/home/john/clarion/crates/clarion-cli/tests/wp2_e2e.rs`:

```rust
//! WP2 end-to-end smoke test — mirrors the README §3 demo script at WP2
//! scope (real plugin host + mock plugin, entities persisted end-to-end).

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

fn mock_plugin_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_clarion-mock-plugin"))
}

fn mock_manifest_bytes() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../clarion-mock-plugin/fixtures/plugin.toml"
    ))
    .expect("read mock manifest")
}

#[test]
fn wp2_walking_skeleton_end_to_end() {
    let project = tempfile::tempdir().unwrap();
    let path_dir = tempfile::tempdir().unwrap();

    // Seed the project with two .mock files so we exercise the per-file loop.
    fs::write(project.path().join("a.mock"), b"a\n").unwrap();
    fs::write(project.path().join("b.mock"), b"b\n").unwrap();
    // And one non-matching file the plugin should skip.
    fs::write(project.path().join("README.txt"), b"readme\n").unwrap();

    // Install the plugin into path_dir + neighboring manifest.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        mock_plugin_bin(),
        path_dir.path().join("clarion-plugin-mock"),
    )
    .unwrap();
    fs::write(path_dir.path().join("plugin.toml"), mock_manifest_bytes()).unwrap();

    // clarion install
    clarion_bin()
        .args(["install", "--path"])
        .arg(project.path())
        .assert()
        .success();

    let clarion_dir = project.path().join(".clarion");
    assert!(clarion_dir.join("clarion.db").exists());
    assert!(clarion_dir.join("config.json").exists());
    assert!(clarion_dir.join(".gitignore").exists());
    assert!(project.path().join("clarion.yaml").exists());

    // clarion analyze with the mock plugin on PATH
    clarion_bin()
        .env("PATH", path_dir.path())
        .args(["analyze"])
        .arg(project.path())
        .assert()
        .success();

    let conn = Connection::open(clarion_dir.join("clarion.db")).unwrap();

    let migration_version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(migration_version, 1);

    let runs_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs_count, 1);

    let run_status: String = conn
        .query_row("SELECT status FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_status, "completed");

    let entity_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entity_count, 2, "two .mock files should produce 2 entities");

    // Assert the 3-segment ID shape matches L2 + the fixture emission.
    let kinds: Vec<(String, String)> = conn
        .prepare("SELECT id, kind FROM entities")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (id, kind) in kinds {
        assert!(id.starts_with("mock:function:"), "id shape: {id}");
        assert_eq!(kind, "function");
    }
}
```

- [ ] **Step 2: Run the E2E test three times (flake check)**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-cli --test wp2_e2e --no-tests=pass
cd /home/john/clarion && cargo nextest run -p clarion-cli --test wp2_e2e --no-tests=pass
cd /home/john/clarion && cargo nextest run -p clarion-cli --test wp2_e2e --no-tests=pass
```

- [ ] **Step 3: Release-profile build**

```bash
cd /home/john/clarion && cargo build --workspace --release
```

Any warning-as-error surfacing here must be fixed in-code, not by loosening lints.

- [ ] **Step 4: Final ADR-023 gate sweep (all 7 gates)**

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features --no-tests=pass
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
cd /home/john/clarion && cargo build --workspace
cd /home/john/clarion && cargo build --workspace --release
```

All 7 exit 0. This is the WP2 closing-commit gate; CI runs the same set.

- [ ] **Step 5: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-cli/ && git commit -m "$(cat <<'EOF'
test(wp2): end-to-end smoke with mock plugin

wp2_e2e.rs runs the README §3 demo script at WP2 scope: clarion install
+ clarion analyze against a project containing 2 .mock files, with the
clarion-plugin-mock fixture on a synthetic PATH + neighboring plugin.toml.
Asserts runs.status = 'completed', entity_count = 2, all IDs are the
3-segment form `mock:function:<qualified>` per L2.

WP3 extends this test with the Python plugin's real emission; the
assertions here carry forward unchanged (status + id shape + count).
EOF
)"
```

---

## Task 10: Sign-off ladder + lock-in stamps + UQ resolutions

**Files:**
- Modify: `/home/john/clarion/docs/implementation/sprint-1/signoffs.md`
- Modify: `/home/john/clarion/docs/implementation/sprint-1/README.md`
- Modify: `/home/john/clarion/docs/implementation/sprint-1/wp2-plugin-host.md`

- [ ] **Step 1: Tick Tier A.2 boxes in `signoffs.md`**

For each checkbox A.2.1 through A.2.9, change `- [ ]` to `- [x]` after verifying the cited proof. For lock-in rows A.2.1 (L4), A.2.2 (L5), A.2.3 (L6), A.2.4 (L9), fill in `locked on <YYYY-MM-DD>` with the closing-commit date (`git log -1 --format=%as HEAD`).

Do NOT tick A.3 / A.4 / A.5 / A.6 — those belong to WP3 / sprint-close.

- [ ] **Step 2: Stamp lock-in dates in `README.md` §4**

In `/home/john/clarion/docs/implementation/sprint-1/README.md` §4 "Lock-in summary", annotate L4, L5, L6, L9 with the same `locked on <date>` stamp used in signoffs.md.

- [ ] **Step 3: Mark UQ-WP2-* resolved in `wp2-plugin-host.md §5`**

For each UQ-WP2-01 through UQ-WP2-11, append a `**Resolved**: <task + outcome>` line:

- UQ-WP2-01 — resolved in Task 5: PATH + neighboring `plugin.toml` (share-dir fallback). First-match-wins on duplicates.
- UQ-WP2-02 — resolved in Task 2: hand-rolled framing over `serde_json`.
- UQ-WP2-03 — resolved in Task 4: canonicalise via `std::fs::canonicalize` (follows symlinks); symlinks inside the root resolving outside are rejected.
- UQ-WP2-04 — resolved in Task 4: 8 MiB default, 1 MiB floor.
- UQ-WP2-05 — resolved in Task 4: per-run combined `entity + edge + finding`, 500k default, 10k floor.
- UQ-WP2-06 — resolved in Task 4: `#[cfg(target_os = "linux")]`-gated prlimit; non-Linux logs a one-shot warning. The macOS `setrlimit(RLIMIT_AS)` path lands with whichever sprint first adds macOS CI.
- UQ-WP2-07 — resolved in Task 3/Task 6: stderr forwarded line-by-line to `tracing::info!` target `plugin::stderr`; progress notifications deferred.
- UQ-WP2-08 — resolved in Task 3 (docs): plugin-author discipline; documented in the mock plugin's module rustdoc and inherited by WP3's plugin-author guide.
- UQ-WP2-09 — resolved in Task 6: manifest is re-parsed on every `PluginHost::spawn`. Caching is a `serve` concern (WP8).
- UQ-WP2-10 — resolved in Task 7: >3 crashes/60s crash-loop breaker + >10 escapes/60s path-escape sub-breaker hard-coded; config surface deferred to WP6.
- UQ-WP2-11 — resolved in Task 6: host reconstructs `entity_id(derived_plugin_id, kind, qualified_name)` and compares against the returned `id`; mismatch drops the entity, logs `CLA-INFRA-PLUGIN-ENTITY-ID-MISMATCH`, plugin stays alive.

- [ ] **Step 4: Commit**

```bash
cd /home/john/clarion && git add docs/implementation/sprint-1/ && git commit -m "$(cat <<'EOF'
docs(sprint-1): tick WP2 sign-off and stamp L4/L5/L6/L9 lock-ins

Tier A.2 boxes ticked in signoffs.md with the WP2 closing-commit date.
README.md §4 lock-in table stamped for L4/L5/L6/L9. wp2-plugin-host.md §5
UQ resolutions recorded inline with the resolving task and outcome.

WP2 complete; WP3 (Python plugin) is now unblocked.
EOF
)"
```

---

## Self-review summary

**Spec coverage vs `wp2-plugin-host.md`:**

| Spec task | Plan task | Status |
|---|---|---|
| §6.Task 1 Manifest parser (L5) | Task 1 | ✓ |
| §6.Task 2 JSON-RPC transport (L4) | Task 2 | ✓ |
| §6.Task 3 In-process mock plugin | Task 3 | ✓ |
| §6.Task 4 Core-enforced minimums (L6) | Task 4 | ✓ |
| §6.Task 5 Plugin discovery (L9) | Task 5 | ✓ |
| §6.Task 6 Plugin-host supervisor | Task 6 | ✓ |
| §6.Task 7 Crash-loop breaker | Task 7 | ✓ |
| §6.Task 8 Wire `clarion analyze` | Task 8 | ✓ |
| §6.Task 9 E2E smoke | Task 9 | ✓ |
| §8 Exit criteria sign-off | Task 10 | ✓ |

Every lock-in (L4/L5/L6/L9) has a dedicated Task that lands it. Every UQ-WP2-* has a designated resolving Task. Every exit-criteria bullet has a verification step.

**Type-consistency spot-check:**

- `PluginHost::spawn(executable, extra_args, manifest, project_root)` signature identical between Task 6 definition and Task 8 call site.
- `ValidatedEntity` shape identical between Task 6 emission and Task 8 `EntityRecord` translation (Task 8's `persist_entity` maps field-for-field).
- `ContentLengthCeiling::default()` = 8 MiB; `read_frame(reader, ceiling.bytes())` called with the struct's `bytes()` accessor — no `usize` literal drift.
- `EntityCountCap::new(DEFAULT_ENTITY_COUNT_CAP)` uses the same const in Task 4 and Task 6.
- `entity_id()` (WP1 Task 2) and Task 6's `derive_plugin_id` produce a plugin_id satisfying ADR-022 grammar; Task 6 tests explicitly exercise `clarion-plugin-foo-bar` → `foo_bar`.
- `Method::{Initialize, Initialized, AnalyzeFile, Shutdown, Exit}` — same five variants in `protocol.rs` (Task 2), `mock.rs` (Task 3), `host.rs` (Task 6), and the fixture binary's `main.rs` (Task 6).
- `WriterCmd::InsertEntity { entity: Box<EntityRecord>, ack }` — Task 8's `persist_entity` wraps the record in `Box::new(record)` per WP1's L3 locked shape.

**Placeholder scan:** no `TODO` / `TBD` / `implement later` / "Similar to Task N" in the plan body. Every code step has a complete code block; every command step has a literal command + expected output. Every task ends with an explicit commit command using HEREDOC formatting.

**Divergences the executor should know about:**

1. **`unsafe_code` workspace lint relaxed from `"forbid"` to `"deny"`**. Task 4 introduces a single audited `#[allow(unsafe_code)]` at `plugin/limits.rs::apply_prlimit_as` with a safety-justifying comment. This is the ONLY unsafe call site in the workspace. A code reviewer on Task 4 should verify:
   - No other module uses `#[allow(unsafe_code)]`.
   - The safety comment addresses fork-safety (post-fork, pre-exec, async-signal-safe only).
   - `setrlimit` is on the POSIX.1-2017 §2.4.3 AS-safe list.

2. **`plugin_id` derivation narrowing**. Manifest `plugin.name` uses grammar `[a-z][a-z0-9_-]*` (dashes permitted) because it doubles as the PATH binary name. ADR-022's EntityId grammar for the `plugin_id` segment is stricter: `[a-z][a-z0-9_]*`. The host derives `plugin_id = manifest.name.strip_prefix("clarion-plugin-").replace('-', '_')`. This is a WP2 convention not stated verbatim in ADR-022; Task 6's commit message cites the derivation, and Task 10 should mention it in the UQ-WP2-11 resolution line.

3. **`source_file_id` carries the canonical file path string, not a file-entity ID**. WP1's `EntityRecord.source_file_id: Option<String>` is a foreign-key placeholder. Task 8's `persist_entity` puts the canonical file path string into it. WP4/WP5's file-discovery pass will replace that placeholder with the real core-minted file-entity ID; the schema column is permissive (TEXT). Flag to the design-doc author post-Sprint-1 if a stricter foreign-key constraint is wanted.

4. **Sprint 1 findings are log-only**. The `CLA-INFRA-PLUGIN-*` rule IDs are emitted via `tracing::warn!(rule_id = "...", ...)` rather than as DB `findings` rows. WP6's scanner-ingest API (ADR-013) and the `POST /api/v1/scan-results` Filigree endpoint are where these logs become persisted findings. The rule IDs and field names locked in Task 6 are the forward-compatible contract.

5. **`nix = "0.28"` with `features = ["resource"]` + `default-features = false`** — minimises the dependency surface. Older nix versions had a different `setrlimit` signature; the plan pins 0.28 exactly. If a reviewer bumps this, re-verify the pre_exec closure compiles.

6. **Fixture binary lives under `crates/clarion-mock-plugin/`** as a full workspace member, not an example. Reason: `assert_cmd::Command::cargo_bin` requires a real bin target, and `CARGO_BIN_EXE_clarion-mock-plugin` is only set when the consuming crate declares it as a dev-dependency. Tasks 6 + 8 both declare it.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-18-wp2-plugin-host.md`.**

Two execution options:

1. **Subagent-Driven (recommended)** — Fresh implementer subagent per task, two-stage review (spec compliance first, then code quality), fix subagents stacked without amending. Matches WP1's review-looped 18-commit history.
2. **Inline Execution** — Batch execution in the current session with checkpoints.

**Recommendation:** Subagent-Driven. Task 4 (unsafe relaxation + fork-safety) and Task 6 (host supervisor with five enforcement concerns) benefit most from independent code-quality review.



