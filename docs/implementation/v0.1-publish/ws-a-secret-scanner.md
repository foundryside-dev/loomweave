# Workstream A — WP5 Pre-Ingest Secret Scanner (detailed package)

**Status**: DRAFT — not yet seeded into Filigree; awaiting kickoff.
**Predecessor**: [`thread-1-pre-publish-blockers.md` §2](./thread-1-pre-publish-blockers.md#2-workstream-a--wp5-pre-ingest-secret-scanner)
**Successor**: Workstream D — external-operator smoke test (publish gate).
**Spec source**: [ADR-013 — Pre-Ingest Secret Scanner with LLM-Dispatch Block](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) (Accepted; do not re-litigate).
**Anchoring ADRs**: [ADR-007 (cache key)](../../clarion/adr/ADR-007-summary-cache-key.md), [ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md), [ADR-017 (severity + dedup)](../../clarion/adr/ADR-017-severity-and-dedup.md), [ADR-021 (plugin authority hybrid)](../../clarion/adr/ADR-021-plugin-authority-hybrid.md), [ADR-022 (core/plugin ontology)](../../clarion/adr/ADR-022-core-plugin-ontology.md), [ADR-023 (tooling baseline)](../../clarion/adr/ADR-023-tooling-baseline.md).
**Requirements floor**: `NFR-SEC-01` (pre-ingest scan + block + baseline), `NFR-SEC-04` (security events as findings), `NFR-OPS-01` / `NFR-OPS-04` (single-binary distribution).
**Effort estimate**: 6–9 working days at agentic velocity. Tasks 1 and 2 parallelisable; 3 → 4 → 5 sequential; 6 + 7 parallel with any.

**Implementation accuracy note**: code-path references and line numbers in
this planning package were last checked on 2026-05-19 against the `scanner`
branch. Treat symbol names as authoritative and line numbers as historical
orientation only.

---

## 1. Purpose

WP5 closes the design-review CRITICAL flag on secret exfiltration: every byte
reaching the LLM provider must first pass a Clarion-owned scanner. ADR-013
locked the behaviour; this document turns the ADR into seven implementation
tasks an agent (or two agents working in parallel) can execute, and pins the
file paths, public APIs, schema changes, rule-ID catalogue entries, and tests
each task is responsible for.

The workstream is the only one of Thread-1's four with significant
engineering weight; B and C are docs + packaging. WP5 ships green before the
publish-gate smoke test in Workstream D can attempt step 8 (the planted-`.env`
case).

## 2. Scope

In scope (this work package):

- New workspace crate `crates/clarion-scanner/` implementing the ADR-013
  rule set as Rust-native regex + Shannon entropy detection.
- Baseline parser for `.clarion/secrets-baseline.yaml` (`detect-secrets`
  v1.x format).
- CLI wiring: pre-ingest pass in `clarion-cli::analyze::run` between
  source-tree walk and per-plugin dispatch.
- `--allow-unredacted-secrets` override surface with TTY + non-TTY gates.
- MCP-side awareness of `briefing_blocked: secret_present` in
  `crates/clarion-mcp/`'s `summary` tool dispatch.
- Five new `CLA-SEC-*` / `CLA-INFRA-SECRET-*` rule-IDs in the
  `detailed-design.md` §5 catalogue.
- Operator documentation under `docs/operator/secret-scanning.md`.

Out of scope (deferred or owned elsewhere):

- Filigree emission of `CLA-SEC-*` findings — WP9-B, v0.2.
- Custom-rule integration from Wardline — v0.2.
- Post-ingest scanning on LLM responses — v0.2+ additive defence.
- Redaction-instead-of-block (ADR-013 Alternative 2) — v0.2+ with opt-in.
- The override-finding monitoring loop (filigree-side dashboard work) — not
  part of v0.1 publish.
- Windows support — `setrlimit` is Unix-only (`clarion-core::plugin::limits`);
  WP5 follows the same posture.

## 3. Locked surfaces and pre-existing context

WP5 reads from and writes to surfaces already present at `main`:

- Workspace at `Cargo.toml` (resolver 3, Rust 2024, MSRV 1.88). Current
  members: `clarion-core`, `clarion-storage`, `clarion-cli`, `clarion-mcp`,
  `clarion-plugin-fixture`. The scanner becomes the **sixth**.
- `crates/clarion-cli/src/analyze.rs` already contains:
  - `pub async fn run(project_path: PathBuf) -> Result<()>` at line 52.
  - `fn collect_source_files(root, wanted_extensions) -> Vec<PathBuf>` at
    line 1740 — the walk WP5's pre-ingest hook intercepts immediately after.
  - `RunOutcome::SoftFailed { reason }` at line 1038 — the partial-success
    path the override-unconfirmed exit must NOT take (override-unconfirmed
    runs never start a `runs` row at all).
- `crates/clarion-core/src/plugin/host.rs`:
  - `pub struct RawEntity { … extra … }` at line 105 — the `extra` field is
    the carrier for `briefing_blocked`. Per the host doc-comment at line 74
    and 222, `extra` flows into `properties_json` downstream.
  - `pub struct HostFinding` at `plugin/host_findings.rs:84` with subcode
    constructors (`HostFinding::malformed_entity`, etc.) — the pattern WP5's
    finding constructors follow.
- `crates/clarion-mcp/src/lib.rs`:
  - `fn entity_properties_json(entity: &EntityRow) -> Value` at line 2382 —
    central reader for `properties_json`; WP5's MCP awareness layer hooks
    above this, before LLM dispatch.
  - `fn summary_scope_deferred(entity: &EntityRow) -> Value` at line 2341 —
    precedent for the "absence is policy" envelope shape WP5 introduces
    (`briefing_blocked: secret_present`).
- `serde_norway` is already a workspace dep (replaced `serde_yaml` per
  commit `9ffc5c8`). WP5 reuses it for baseline YAML.

Surfaces this package does NOT touch:

- Plugin protocol (no `analyze_file` RPC change).
- Writer-actor command set in `crates/clarion-storage/` — the scanner
  produces `HostFinding`s and entity `extra` map entries; no new
  `WriterCmd` variants.
- SQLite schema — no new tables or columns. `entities.properties` and
  `runs.stats` are existing JSON-bearing columns; WP5 adds key/value pairs.

### 3.1 Finding-write channel (load-bearing distinction)

The codebase has two distinct "finding" concepts; WP5 emits into one of them.

- **`HostFinding`** (`crates/clarion-core/src/plugin/host_findings.rs:84`) is
  a plugin-host **integrity event**: subcode + message + structured metadata
  map. It carries no severity, no `rule_id`, no `(file, line)` slot. It is
  drained via `PluginHost::take_findings` and logged through
  `log_plugin_findings` (`analyze.rs:1042`). Per the doc-comment at line
  79–82, "for Sprint 1 they are collected only" — they are NOT persisted to
  the `findings` table.
- **`FindingRecord`** (`crates/clarion-storage/src/commands.rs:113`) is the
  ADR-004 finding shape that the writer-actor persists. It carries
  `rule_id`, `severity`, `entity_id`, `message`, `evidence_json`,
  `properties_json`, lifecycle status, etc. Write path: send
  `WriterCmd::InsertFinding { finding: Box<FindingRecord>, ack }` through
  the existing `send_wait` channel.

**WP5 emits `FindingRecord` via `WriterCmd::InsertFinding`.** Precedent:
`insert_weak_modularity_finding` at `analyze.rs:907–965` is the existing
pattern for a CLI-side, application-level finding emitted directly through
the writer. WP5's `secret_scan` module follows that pattern verbatim —
build a `FindingRecord`, send via the writer's command channel, await ack.

`HostFinding` is **not** the channel for `CLA-SEC-*` / `CLA-INFRA-SECRET-*`
because (a) it lacks the severity / rule-ID slots ADR-013 mandates and
(b) it is not persisted. The Thread-1 §2.A.1 reference to "the existing
`HostFinding` → `WriterCmd::InsertEntity` path" is imprecise; the
authoritative channel is `WriterCmd::InsertFinding`.

For each detection, the `FindingRecord` is populated:

| Field | Value |
|---|---|
| `id` | `uuid::Uuid::new_v4().to_string()` |
| `tool` | `"clarion"` |
| `tool_version` | `env!("CARGO_PKG_VERSION")` |
| `run_id` | current run's `run_id` |
| `rule_id` | `"CLA-SEC-SECRET-DETECTED"` (or other; see §6 catalogue) |
| `kind` | `"defect"` (per ADR-004; secrets are defects pending remediation) |
| `severity` | `"ERROR"` (or `"INFO"` for `CLA-INFRA-SECRET-BASELINE-MATCH`) |
| `confidence` | `Some(1.0)` for named-pattern matches; `Some(0.6)` for high-entropy detections (rough heuristic — refine in Task 1) |
| `confidence_basis` | `Some("pattern")` or `Some("entropy")` |
| `entity_id` | `core:file:{blake3(relative_path)}` synthetic file anchor for file-level findings; the file path is also retained in `evidence_json` |
| `related_entities_json` | `"[]"` |
| `message` | one-line human-readable, e.g. `"AWS access key detected in src/api/keys.py:42"` (the literal bytes NEVER appear in this string) |
| `evidence_json` | `{"file_path": "...", "line_number": 42, "rule": "AwsAccessKeyId", "hashed_secret_hex": "..."}` |
| `properties_json` | `"{}"` |
| `supports_json` / `supported_by_json` | `"[]"` |
| `created_at` / `updated_at` | run's `started_at` (deterministic) |

## 4. Architecture at a glance

```
clarion analyze <root>
   │
   ├─ collect_source_files(...)                                    [unchanged]
   │
   ├─ secret_scan::pre_ingest(source_files, baseline) ──► WP5 entry point
   │      │
   │      ├─ For each file:
   │      │     1. mmap or read buffer
   │      │     2. Scanner::scan_bytes(&buf) ─► Vec<Detection>
   │      │     3. baseline.suppress(detections, path)
   │      │     4. partition allowed/suppressed
   │      │
   │      ├─ If TTY and allowed.is_empty() == false:
   │      │     interactive prompt (per ADR-013 §Override)
   │      │
   │      ├─ Returns:
   │      │     - blocked: BTreeMap<PathBuf, Vec<Detection>>
   │      │     - findings: Vec<HostFinding>      (SEC-* + INFRA-SECRET-*)
   │      │     - override_state: OverrideState
   │      │
   │      └─ If override_unconfirmed → return Err → exit 78 (EX_CONFIG)
   │
   ├─ run_plugins(..., blocked) ─► plugins still analyse blocked files;
   │                                host stamps `briefing_blocked` into
   │                                RawEntity.extra for blocked files
   │
   ├─ writer persists entities (properties_json carries the flag)
   │
   └─ summary_pass / MCP serve / etc. → read briefing_blocked,
                                         skip LLM dispatch
```

Two invariants hold for any reader at any time:

- The scanner runs **before** any plugin process is spawned. ADR-013
  §"Plugin-boundary interaction" — file buffers reach Anthropic only via
  Phase 4–6 summarisation, which is gated by `briefing_blocked`.
- The override path **never** silently bypasses. Non-TTY without confirm
  flag exits 78 *before* a `runs` row is created.

## 5. Task breakdown

Each task fits one agent pass (≤ ~500 LOC plus tests). Tasks 1 and 2 are
independent and can parallelise; 3 depends on 1 + 2; 4 depends on 3; 5
depends on 3; 6 + 7 are documentation, parallel with any.

### Task 1 — Scanner crate, rule registry, entropy

**Owner**: `crates/clarion-scanner/`
**Estimated size**: 350–500 LOC including tests.

#### Files

| Action | Path |
|---|---|
| create | `crates/clarion-scanner/Cargo.toml` |
| create | `crates/clarion-scanner/src/lib.rs` |
| create | `crates/clarion-scanner/src/patterns.rs` |
| create | `crates/clarion-scanner/src/entropy.rs` |
| create | `crates/clarion-scanner/tests/fixtures/*.txt` (positive + negative per rule) |
| create | `crates/clarion-scanner/tests/scanner.rs` |
| modify | `Cargo.toml` (workspace root) — add `crates/clarion-scanner` member |

#### Scope

Implement the named-credential regex table from ADR-013 lines 35–38 (AWS
access keys, AWS secret adjacency, GitHub PATs / OAuth tokens, Anthropic
keys, OpenAI keys, Stripe keys, Slack tokens, JWT, RSA/EC/DSA/OpenSSH
private-key headers, contextual-credential `name=value` patterns).
Implement Shannon entropy detection over byte slices with the bounds
ADR-013 §Implementation specifies (base64 ≥ 4.5 entropy over ≥20 chars; hex
≥ 3.0 over ≥40 chars). UUIDs and short tokens must NOT trip the entropy
rule (positive/negative fixtures enforce this).

#### Public surface

```rust
pub struct Scanner {
    patterns: regex::RegexSet,
    pattern_meta: Vec<PatternMeta>,
    entropy_b64: EntropyTuning,
    entropy_hex: EntropyTuning,
}

pub struct Detection {
    pub rule_id: &'static str,         // e.g. "AwsAccessKeyId", "HighEntropyBase64"
    pub category: SecretCategory,
    pub byte_offset: usize,
    pub line_number: u32,
    pub matched_len: usize,            // never persist the literal bytes
    pub hashed_secret: [u8; 20],       // sha1, baseline-compat
}

pub enum SecretCategory {
    CloudCredential,                   // AWS, GCP, Azure
    VcsCredential,                     // GitHub PATs, OAuth
    AiProviderCredential,              // Anthropic, OpenAI
    PaymentsCredential,                // Stripe
    MessagingCredential,               // Slack
    PrivateKey,                        // RSA/EC/DSA/OpenSSH/PGP
    JwtToken,
    HighEntropy,
    ContextualCredential,              // password=, api_key=...
}

impl Scanner {
    pub fn new() -> Self;               // default thresholds per ADR-013
    pub fn scan_bytes(&self, buf: &[u8]) -> Vec<Detection>;
}
```

`Detection::hashed_secret` is sha1 over the literal matched bytes, computed
**at detection time**; the literal never lives anywhere downstream. This is
the same convention `detect-secrets` uses so baseline files round-trip.

#### Dependency budget

The scanner crate is intentionally a leaf:

- `regex` (workspace already pulls it for elsewhere)
- `sha1` (new — small, well-maintained)
- No `tokio`, no `rusqlite`, no `serde_norway`.

Verification at exit: `cargo tree -p clarion-scanner | head -30` shows no
`tokio` or `rusqlite` ancestors. If a downstream crate's `regex` features
leak something heavier, pin the feature set in `clarion-scanner`'s
`Cargo.toml`.

#### Tests

`crates/clarion-scanner/tests/scanner.rs`:

- One positive + one negative fixture per named pattern (AWS access key,
  GitHub PAT `ghp_…`, GitHub fine-grained `github_pat_…`, Anthropic
  `sk-ant-…`, OpenAI `sk-…`, Stripe `sk_live_…`, Slack `xoxb-…`, JWT
  `eyJ…`, RSA private-key header, OpenSSH private-key header).
- High-entropy positives: 32-char random base64, 64-char random hex.
- High-entropy negatives: UUIDs, base64-encoded SHA256 checksums (these
  are high-entropy but commonly safe — confirm whether to suppress; if
  not, document in the operator doc as a known-baseline candidate).
- Contextual-credential positives: `password = "..."`, `api_key: "…"`,
  `SECRET_TOKEN := "…"`.
- Contextual-credential negatives: `password_hash = "…"` (post-hash safe),
  `# password placeholder` (comments).

#### Exit criteria

- `cargo test -p clarion-scanner` green.
- `cargo clippy -p clarion-scanner -- -D warnings` clean.
- `cargo tree -p clarion-scanner` shows no `tokio` / `rusqlite` /
  `serde_norway` deps.
- `cargo build --workspace` green (new crate compiles into the workspace
  without changing existing build).

---

### Task 2 — Baseline parser (`.clarion/secrets-baseline.yaml`)

**Owner**: `crates/clarion-scanner/`
**Estimated size**: 200–300 LOC including tests.
**Parallelisable with Task 1**: yes — touches separate module.

#### Files

| Action | Path |
|---|---|
| create | `crates/clarion-scanner/src/baseline.rs` |
| modify | `crates/clarion-scanner/src/lib.rs` (re-export) |
| create | `crates/clarion-scanner/tests/fixtures/baselines/*.yaml` |
| modify | `crates/clarion-scanner/tests/scanner.rs` OR new `tests/baseline.rs` |

#### Scope

Parse the `detect-secrets` v1.x baseline schema (ADR-013 §Baseline). Schema
fields required:

```yaml
version: "1.0"                # must equal "1.0"
results:
  "<relative-path>":          # repository-relative path
    - type: "<detect-secrets pattern name>"
      hashed_secret: "<hex sha1>"
      line_number: 42
      is_secret: false        # operator declares not-a-secret
      justification: "..."    # REQUIRED (ADR-013 line 71)
```

`justification` missing → emit `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION`
(at load time, surfaces during CLI startup; one finding per offending
entry).

Match-then-suppress at `Detection` granularity, keyed on
`(file_path, hashed_secret, line_number)`. Both `allowed` and `suppressed`
partitions are retained so the CLI can emit one `CLA-SEC-SECRET-DETECTED`
per unsuppressed detection AND one `CLA-INFRA-SECRET-BASELINE-MATCH`
info-level finding per *actually-fired* baseline entry (the audit-surface
requirement of `NFR-SEC-04`).

`is_secret: true` in a baseline entry is treated as **not a suppression** —
the operator is declaring the entry IS a real secret they haven't fixed
yet; behaviour is identical to no baseline entry at all. (This mirrors
`detect-secrets`' convention.)

#### Public surface

```rust
pub struct Baseline {
    version: String,
    entries: BTreeMap<PathBuf, Vec<BaselineEntry>>,
}

pub struct BaselineEntry {
    pub rule_type: String,           // detect-secrets type name
    pub hashed_secret: [u8; 20],
    pub line_number: u32,
    pub is_secret: bool,
    pub justification: String,
}

pub struct SuppressionResult {
    pub allowed: Vec<Detection>,     // detections that survive suppression
    pub suppressed: Vec<Detection>,  // detections suppressed by a baseline entry
    pub fired_entries: Vec<BaselineMatch>,  // baseline entries that actually matched
}

pub fn load_baseline(path: &Path) -> Result<Baseline, BaselineError>;

impl Baseline {
    pub fn empty() -> Self;          // path-absent path (NOT an error)
    pub fn suppress(
        &self,
        detections: Vec<Detection>,
        file: &Path,
    ) -> SuppressionResult;
}

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    #[error("baseline version mismatch: expected 1.0, got {0}")]
    UnsupportedVersion(String),
    #[error("baseline entry missing required field 'justification' at {file}:{line}")]
    MissingJustification { file: PathBuf, line: u32 },
    #[error("baseline parse error: {0}")]
    Parse(#[from] serde_norway::Error),
    #[error("baseline I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

`load_baseline` returns `Ok(Baseline::empty())` if the file does not exist
(this is the common case — operators add a baseline only after the first
false positive). It errors only on parse failure, version mismatch, or
missing required fields.

#### Tests

- Fixture baseline + scanner detections → asserts `allowed` and
  `suppressed` partitions match expected.
- Missing `justification` → `BaselineError::MissingJustification` returned.
- Baseline file absent → `Baseline::empty()` returned, no error.
- Round-trip: parse → serialise → parse → byte-identical (subject to
  YAML serialisation determinism with `serde_norway`).
- Path-relativity test: baseline keys are repository-relative; scanner
  passes absolute paths. The suppression layer normalises before matching.
  Document the normalisation rule and assert it.

#### Exit criteria

- `cargo test -p clarion-scanner` (combined with Task 1) green.
- Round-trip baseline test passes.
- `BaselineError` is the only error type leaking out of the module; no
  bare `anyhow::Error` at the public surface.

---

### Task 3 — CLI wiring: pre-ingest hook in `analyze::run`

**Owner**: `crates/clarion-cli/`
**Estimated size**: 300–400 LOC including the new orchestration module + tests.
**Depends on**: Task 1, Task 2.

#### Files

| Action | Path |
|---|---|
| create | `crates/clarion-cli/src/secret_scan.rs` (orchestration module — keep `analyze.rs` from growing further per arch-analysis H-1) |
| modify | `crates/clarion-cli/src/analyze.rs` |
| modify | `crates/clarion-cli/Cargo.toml` (add `clarion-scanner` dep) |
| modify | `crates/clarion-core/src/plugin/host.rs` (stamp `briefing_blocked` into RawEntity.extra for blocked files — see "Host integration" below) |
| create | `crates/clarion-cli/tests/secret_scan.rs` (integration tests) |
| create | `crates/clarion-cli/tests/fixtures/secret-project/...` (fixture trees) |

#### Scope

Between `collect_source_files` (`analyze.rs:1740`) and the per-plugin
processing loop, insert a pre-ingest scan pass:

1. Load `.clarion/secrets-baseline.yaml` via Task 2's `load_baseline`.
2. For each file in `source_files`:
   a. Read the file buffer.
   b. `Scanner::scan_bytes(&buf)` → `Vec<Detection>`.
   c. `baseline.suppress(detections, path)` → `SuppressionResult`.
3. Build a `BlockSet: BTreeMap<PathBuf, BlockReason>` of files with
   non-empty `allowed`. `BlockReason` is an enum with a single variant at
   v0.1:

   ```rust
   pub enum BlockReason {
       SecretPresent,  // ADR-013: rendered as "secret_present" in properties_json
   }
   ```

   The string-on-the-wire form is `"secret_present"`. Future variants
   (e.g. `SizeCapExceeded`) would add new string values; readers treat
   `briefing_blocked` as open-vocabulary.

4. Emit findings via `WriterCmd::InsertFinding` (see §3.1):
   - One `CLA-SEC-SECRET-DETECTED` per `allowed` detection (severity
     `ERROR`).
   - One `CLA-INFRA-SECRET-BASELINE-MATCH` per `fired_entries` element
     (severity `INFO`).
   - One `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION` per
     `BaselineError::MissingJustification` surfaced at load time.
5. Pass the `BlockSet` through to the per-plugin processing loop so the
   plugin host can stamp `briefing_blocked: secret_present` into the
   `RawEntity.extra` for each entity coming out of those files.

#### Host integration

The plugin host (`crates/clarion-core/src/plugin/host.rs`) currently
accepts `RawEntity` from the plugin and translates `extra` into
`properties_json` downstream (host.rs:74, 222). WP5 adds one path:

- The host receives a `BTreeMap<PathBuf, BlockReason>` at spawn time (via
  a new field on the existing host configuration struct, or via a setter —
  pick the smaller diff). For each entity whose `source.path` is in the
  block map, the host inserts `"briefing_blocked": "secret_present"` into
  the entity's `extra` map **after** the plugin returns, **before**
  serialisation to `properties_json`.

This keeps the per-plugin code untouched. Plugins do not need to know
about secret scanning; the host stamps the flag on its way through.

Rationale: the alternative (passing block info to the plugin so the plugin
emits the flag) would require changing the plugin protocol — out of scope
for v0.1.

#### Tests

`crates/clarion-cli/tests/secret_scan.rs`:

1. **Happy path — clean project**: fixture with no secrets → `analyze`
   exits 0; no `CLA-SEC-*` findings persisted.
2. **One file with AWS key in `.env`**: fixture with a committed `.env`
   containing `AKIAIOSFODNN7EXAMPLE` →
   - `analyze` exits 0 with `RunOutcome::Completed`. Secret detection
     produces findings but does **not** affect the run outcome — the
     structural extraction succeeded, the security finding is the
     audit signal, and `SoftFailed` is reserved for actual plugin
     failures.
   - `entities` table has rows for the `.env` file's structural entities.
   - Those entities' `properties_json` contains
     `"briefing_blocked":"secret_present"`.
   - `findings` table has one `CLA-SEC-SECRET-DETECTED` row referencing
     the file.
3. **Baseline suppression**: same `.env` fixture + a `.clarion/secrets-baseline.yaml`
   with the matching entry → no `CLA-SEC-SECRET-DETECTED` finding; one
   `CLA-INFRA-SECRET-BASELINE-MATCH` info-level finding; no
   `briefing_blocked` flag on entities.
4. **Baseline missing justification**: malformed baseline →
   `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION` finding; analyze
   completes with `RunOutcome::Completed` (load errors degrade to
   "treat baseline as empty + surface finding", they do not abort the
   run and do not promote it to `SoftFailed`).
5. **Multi-file project**: secret in 1 of 10 files → 9 unblocked, 1
   blocked; entity counts match expectation.

#### Exit criteria

- All five integration tests green.
- `crates/clarion-cli/src/secret_scan.rs` ≤ 250 LOC (the orchestration
  module; arch-analysis H-1 ceiling).
- `analyze.rs` net growth ≤ 30 LOC (the new module owns the body; analyze
  delegates).
- The walking-skeleton E2E (`tests/e2e/sprint_1_walking_skeleton.sh`)
  still passes — clean fixture must not regress.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  clean.

---

### Task 4 — Override semantics: `--allow-unredacted-secrets`

**Owner**: `crates/clarion-cli/`
**Estimated size**: 250–350 LOC including tests.
**Depends on**: Task 3.

#### Files

| Action | Path |
|---|---|
| modify | `crates/clarion-cli/src/cli.rs` (clap definition) |
| modify | `crates/clarion-cli/src/secret_scan.rs` (override logic) |
| modify | `crates/clarion-cli/tests/secret_scan.rs` (override tests) |

#### Scope

Implement the override path per ADR-013 §Override.

CLI surface (additions to the `analyze` subcommand):

```rust
#[derive(Args)]
pub struct AnalyzeArgs {
    // ... existing fields ...
    /// Allow analysis of files containing unredacted secrets. REQUIRES a
    /// confirmation step (interactive prompt or --confirm-allow-unredacted-secrets).
    #[arg(long)]
    pub allow_unredacted_secrets: bool,

    /// Non-TTY confirmation token for --allow-unredacted-secrets.
    /// Must be the literal string "yes-i-understand".
    #[arg(long, value_name = "TOKEN", requires = "allow_unredacted_secrets")]
    pub confirm_allow_unredacted_secrets: Option<String>,
}
```

Behaviour:

- **No detections**: flag is a no-op (do NOT prompt; do NOT emit override
  findings). Document this in the operator doc so operators can safely
  leave the flag set in CI configurations.
- **Detections present, no override flag**: existing block behaviour
  (Task 3) — `briefing_blocked` stamped, `CLA-SEC-SECRET-DETECTED`
  emitted, analyze continues for structural pass.
- **Detections present, `--allow-unredacted-secrets` only, TTY**:
  - Print detection summary to stderr (file path, line, rule-ID, but
    NEVER the matched bytes).
  - Prompt: `Type 'yes-i-understand' to proceed: `.
  - Match → proceed without blocking; emit
    `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` per affected file; record
    `override_used: true, files_affected: [...]` in `runs.stats`.
  - Anything else (EOF, mismatched string, `^C`) → exit 78 (`EX_CONFIG`);
    no `runs` row started.
- **Detections present, `--allow-unredacted-secrets` only, non-TTY**:
  exit 78 immediately with
  `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED` rule-ID on stderr (the rule-ID
  is the operator's grep target; no `findings` row is persisted because
  the run never started).
- **Detections present, both flags, non-TTY, `--confirm-allow-unredacted-secrets=yes-i-understand`**:
  proceed; emit `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` findings; record
  `runs.stats` override entry.
- **Detections present, both flags, wrong confirm value**: exit 78;
  rule-ID on stderr; no run row.

`std::io::IsTerminal` on stdin is the TTY detector (stable since Rust
1.70; workspace MSRV 1.88).

#### Override state in `runs.stats`

The existing `runs.stats` column is a TEXT-affinity JSON blob. WP5 appends
two keys:

```json
{
  "secret_override_used": true,
  "secret_override_files_affected": [
    "src/api/keys.py",
    "tests/fixtures/secrets.yaml"
  ]
}
```

These keys are absent when no override fires. Existing readers ignore
unknown keys (verify by grepping for `runs.stats` JSON deserialisation
sites).

#### Tests

1. **Non-TTY override-confirmed**: fixture with one AWS key, both flags
   set, confirm value correct → exits 0, no `briefing_blocked`, one
   `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` per file, `runs.stats` carries
   the override keys.
2. **Non-TTY override-unconfirmed**: only `--allow-unredacted-secrets`,
   no confirm → exit 78, stderr contains
   `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED`, no `runs` row in the DB.
3. **Non-TTY override-wrong-confirm**: both flags but
   `--confirm-allow-unredacted-secrets=oops` → exit 78, stderr contains
   the rule-ID.
4. **Override + no detections**: flag set but no secrets present → no
   override findings, no `runs.stats` override keys (the operator's CI
   stays clean on clean repos).
5. **TTY path**: marked `#[ignore]` with a doc comment; manual
   verification belongs to Workstream D's smoke test step 8.

#### Exit criteria

- All non-TTY tests green.
- `cli.rs` exit codes documented in a module-level doc comment:
  - 0: success (with or without override).
  - 1: hard failure (existing behaviour).
  - 78 (`EX_CONFIG`): override misconfigured.
- Operator doc (Task 7) documents the exit-code contract and the
  `--confirm-allow-unredacted-secrets` syntax.

---

### Task 5 — MCP-side awareness of `briefing_blocked`

**Owner**: `crates/clarion-mcp/`
**Estimated size**: 150–250 LOC including tests.
**Depends on**: Task 3 (writes the flag); independent of Task 4.
**Parallelisable with Task 4**: yes.

#### Files

| Action | Path |
|---|---|
| modify | `crates/clarion-mcp/src/lib.rs` (summary tool dispatch) |
| modify | `crates/clarion-mcp/tests/storage_tools.rs` (or split into a new test file) |

#### Scope

When `summary(id)` is called on an entity whose
`properties.briefing_blocked == "secret_present"`, the MCP server must:

1. **Not** invoke the LLM provider.
2. **Not** consume any budget ledger.
3. **Not** write a row to `summary_cache`.
4. Return an envelope shape that tells the consult-mode agent the absence
   is policy, not pipeline failure.

#### Envelope shape

```json
{
  "entity_id": "python:function:demo.foo|function",
  "summary": null,
  "briefing_blocked": "secret_present",
  "remediation": "File flagged by pre-ingest secret scan. Fix the secret or whitelist via .clarion/secrets-baseline.yaml. See ADR-013."
}
```

This is parallel to the existing `summary_scope_deferred` envelope
(`crates/clarion-mcp/src/lib.rs:2341`) and the four `issues_unavailable`
envelopes in `clarion-mcp::filigree`. Add a `summary_briefing_blocked`
helper next to `summary_scope_deferred`.

#### Hook point

The summary tool dispatch reads `entity_properties_json` at line 2382 of
`lib.rs`. The branch on `briefing_blocked` happens **before** any cache
lookup or provider invocation. Recommended placement: right after the
existing `summary_scope_deferred` check, since both are
"return-immediately" policy branches.

#### Tests

1. **Storage-only**: fixture entity with
   `properties.briefing_blocked == "secret_present"` → `summary` tool
   returns the envelope; no row in `summary_cache`.
2. **Recording-provider isolation**: with `RecordingProvider` configured
   in recording mode and a counter wrapper around the inner provider,
   call `summary` on a blocked entity → the inner-provider call counter
   stays at zero. (The exact assertion form depends on the
   `RecordingProvider` fixture layout — either "no fixture file
   created" or "no new entry appended"; the load-bearing claim is "no
   outbound LLM call." Pick whichever assertion the existing
   `RecordingProvider` test harness already supports.)
3. **Budget untouched**: read the session's budget ledger before and
   after — bytes identical.
4. **Other tools unaffected**: `entity_at`, `find_entity`, `callers_of`,
   `execution_paths_from`, `neighborhood`, `issues_for` all return their
   normal envelopes for blocked entities. The block only affects LLM
   dispatch; structural navigation still works (this is the whole point
   of "block briefings, not analysis").

#### Exit criteria

- Storage-tools tests green.
- Manual log inspection on a fixture with one blocked entity confirms
  zero outbound LLM calls.

---

### Task 6 — Rule-ID catalogue in `detailed-design.md`

**Owner**: docs
**Estimated size**: ~100 lines of doc changes.
**Parallelisable with any task**.

#### Files

| Action | Path |
|---|---|
| modify | `docs/clarion/v0.1/detailed-design.md` (§5 rule catalogue) |

#### Scope

Append rule rows for each of the five new rule-IDs WP5 introduces. Use the
existing table shape in §5 (do not invent a new format).

| Rule-ID | Severity | Category | Description (one sentence) | Remediation (one sentence) | ADR |
|---|---|---|---|---|---|
| `CLA-SEC-SECRET-DETECTED` | error | security | Pre-ingest secret scanner detected a credential pattern in a file slated for LLM dispatch. | Remove the secret, rotate the credential, or whitelist via `.clarion/secrets-baseline.yaml` with a justification. | [ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-SEC-UNREDACTED-SECRETS-ALLOWED` | error | security | Operator invoked `--allow-unredacted-secrets`; file content reached the LLM provider with secrets intact. | Audit override usage via `filigree list --rule-id=CLA-SEC-UNREDACTED-SECRETS-ALLOWED --since 30d`. | [ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-INFRA-SECRET-BASELINE-NO-JUSTIFICATION` | error | infra | Baseline entry missing required `justification` field; entry not honoured. | Add a `justification` string explaining why the match is safe. | [ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-INFRA-SECRET-BASELINE-MATCH` | info | infra | Baseline entry suppressed a scanner detection (audit surface). | None — informational, retained for `NFR-SEC-04` audit. | [ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) |
| `CLA-INFRA-SECRET-OVERRIDE-UNCONFIRMED` | error | infra | `--allow-unredacted-secrets` supplied without confirmation; run aborted before start. | Supply `--confirm-allow-unredacted-secrets=yes-i-understand` in non-TTY contexts or run interactively. | [ADR-013](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) |

These IDs make the WP9-B Filigree-emission story (v0.2) able to
round-trip without a separate spec pass.

#### Exit criteria

- `detailed-design.md` lints clean (existing markdown / cross-reference
  checks in CI).
- ADR-013 retains canonical authority on *behaviour*; the design doc
  carries the *catalogue*. No behaviour is restated in the design doc
  beyond the one-sentence description and remediation.

---

### Task 7 — Operator documentation

**Owner**: docs
**Estimated size**: ≤ 250 lines.
**Parallelisable with any task**.

#### Files

| Action | Path |
|---|---|
| create | `docs/operator/secret-scanning.md` |
| modify | `docs/operator/README.md` (add link to the new page) |
| modify | `README.md` (Workstream B Task 1; cross-link) — coordinated with WS B, do not duplicate |

If `docs/operator/README.md` does not exist yet, create a minimal index
listing this and the (forthcoming) `getting-started.md` from WS-B Task 2.
Coordinate with WS-B to avoid duplicate index creation.

#### Scope

A single page that lets a non-engineer resolve a baseline false-positive
without reading ADR-013. Sections:

1. **What the scanner does** (3 sentences).
2. **What gets blocked** (file-level; structural extraction continues;
   summaries don't).
3. **How to whitelist a false positive** (edit
   `.clarion/secrets-baseline.yaml`; example entry; commit it).
4. **The override flag** (`--allow-unredacted-secrets` +
   `--confirm-allow-unredacted-secrets=yes-i-understand`; when to use it;
   what gets audited).
5. **Exit codes** (0 / 1 / 78).
6. **Finding the audit trail** (
   `select * from findings where rule_id like 'CLA-SEC-%'`;
   forward-pointer to Filigree integration in v0.2).
7. **Limitations** (pattern-based scanning has false negatives; novel
   secret shapes; air-gapped alternatives for truly high-risk repos —
   `--no-llm`).

#### Exit criteria

- A non-engineer can read the doc and resolve a baseline false-positive
  end-to-end (verified in WS-D smoke test).
- The doc is ≤ 250 lines.
- Cross-links to ADR-013 for the operator who wants the design rationale,
  but does not require reading the ADR to act.

---

## 6. Schema additions

WP5 does not create new tables or columns. It adds two well-known keys to
existing JSON-bearing columns:

### `entities.properties` (existing TEXT column, JSON object)

- `briefing_blocked: "secret_present"` — stamped by the host on entities
  whose source file was flagged by the pre-ingest scanner.

Other plausible `briefing_blocked` reasons in the future (e.g.
`"size_cap_exceeded"`, `"operator_excluded"`) are NOT introduced by WP5;
the schema is open-vocabulary on the value side. ADR-013 names
`secret_present` as the canonical first reason.

### `runs.stats` (existing TEXT column, JSON object)

- `secret_override_used: true` — present only when the override fired.
- `secret_override_files_affected: ["…", "…"]` — present only when
  `secret_override_used == true`.

Both keys absent on runs with no override → reader code defaults to "no
override happened."

### Rule-ID grammar

The five new rule-IDs all match the existing ADR-022 manifest rule-ID
grammar (see `plugin/manifest` correction in commit `0cb61b4`). No
manifest changes are needed because these are *core-emitted* findings,
not plugin-emitted.

---

## 7. Test strategy across layers

| Layer | Tests | Location |
|---|---|---|
| Unit | Scanner pattern fixtures (positive + negative per rule); entropy bounds; baseline parse / suppress / round-trip | `crates/clarion-scanner/tests/` |
| Unit | Override CLI exit codes; `runs.stats` JSON shape | `crates/clarion-cli/tests/secret_scan.rs` |
| Unit | MCP `summary` envelope on blocked entity | `crates/clarion-mcp/tests/storage_tools.rs` |
| Integration | `analyze` over fixture project with one secret-bearing file → entities + findings + properties_json shape | `crates/clarion-cli/tests/secret_scan.rs` |
| Integration | Baseline suppression; baseline missing-justification | same |
| Integration | Non-TTY override paths (3 cases) | same |
| E2E | `clarion install && clarion analyze` against a fixture project with one known secret; assert exit 0, entities present, briefing_blocked flagged, finding persisted; assert walking-skeleton fixture still green | `tests/e2e/wp5_secret_scan.sh` (new), parallels `tests/e2e/sprint_1_walking_skeleton.sh` |
| Manual | TTY interactive prompt path | WS-D smoke test step 8 |

### CI gates (ADR-023 floor, unchanged)

WP5 must keep all of these green:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace --bins`
- `cargo nextest run --workspace --all-features`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
- `cargo deny check`
- `plugins/python/.venv/bin/ruff check plugins/python`
- `plugins/python/.venv/bin/ruff format --check plugins/python`
- `plugins/python/.venv/bin/mypy --strict plugins/python`
- `plugins/python/.venv/bin/pytest plugins/python`
- `bash tests/e2e/sprint_1_walking_skeleton.sh`

A new E2E (`wp5_secret_scan.sh`) is added to the CI matrix's
`walking-skeleton` job.

---

## 8. Risks and open questions

| ID | Risk / question | Mitigation |
|---|---|---|
| R-1 | High-entropy detection on UUIDs and base64 checksums creates false-positive flood on real repos | Bound length thresholds per ADR-013 (≥20 chars base64, ≥40 chars hex); document baseline workflow prominently in Task 7; Workstream D smoke test on `requests` will surface real-world false-positive rate before publish |
| R-2 | sha1 hashing of literal secrets means a baseline rebuilt from a different `detect-secrets` version may mismatch | Document the exact hash function (sha1 over the literal matched bytes, no normalisation); migration story for `detect-secrets` v2.x is a v0.2 problem |
| R-3 | The override flag could become normalised in CI configurations ("we always pass it because…") | Audit-surface design: every override is a finding; v0.2 Filigree integration makes them visible; operator doc explicitly names this anti-pattern |
| R-4 | Plugin-host integration to stamp `briefing_blocked` adds a code path through a 3 126-LOC file flagged by arch-analysis §5.4 A-3 | Keep the host-side change small (≤ 30 LOC, a single map lookup); the orchestration lives in `clarion-cli/src/secret_scan.rs`, not in the host |
| R-5 | Baseline format compatibility with `detect-secrets` v1.x must be exact; an operator running `detect-secrets scan --baseline` and dropping the file in `.clarion/` must work | Round-trip test in Task 2; smoke-test the workflow with a real `detect-secrets`-generated baseline in WS-D |
| Q-1 | Should baseline entries carry an `expires_at` field so stale entries are surfaced? | Out of scope for v0.1; surface as a v0.2 enhancement if the override-monitoring loop also lands then |
| Q-2 | Does `briefing_blocked: secret_present` propagate up to module / subsystem summaries when those are aggregated (deferred to v0.2 per ADR-030)? | Defer to v0.2; the leaf summary skip is sufficient for v0.1; document in Task 7 that "summaries of containing modules may still be generated and may infer secret content indirectly — fix the underlying file" |
| Q-3 | Should the scanner also redact secrets from log lines emitted by Clarion itself (the `runs/<run_id>/log.jsonl` ADR-013 line 16 cites)? | **Resolved at planning time**: `runs/<run_id>/log.jsonl` is referenced only in `clarion-cli/src/install.rs:74` (the gitignore template) and `tests/install.rs:40` (the test asserting it's ignored). No writer code exists in `crates/` that emits to that path today. WP5 has nothing to redact. If a per-run JSONL log lands later (likely WP6 batched pipeline or WP9-B), that work must include a redaction pass for any file content emitted from `briefing_blocked` files; cite this row when the log writer is authored. |

All three questions resolved at planning time. None blocks task kickoff.

---

## 9. Workstream exit criteria (gate to Workstream D)

The workstream is signed-off when ALL of the following hold:

- [ ] All seven tasks closed.
- [ ] `cargo test --workspace --all-features` green.
- [ ] Existing CI gates (§7) unchanged in pass status.
- [ ] `tests/e2e/wp5_secret_scan.sh` added and green; included in the
      `walking-skeleton` CI job.
- [ ] The walking-skeleton E2E continues to pass on the clean-fixture
      path (no regression).
- [ ] `docs/operator/secret-scanning.md` reviewed by a non-author or
      verified during WS-D smoke test step 8.
- [ ] All five rule-IDs appear in `detailed-design.md` §5.
- [ ] No new clippy warnings; no `unsafe` blocks introduced.
- [ ] `cargo tree -p clarion-scanner` shows no `tokio` / `rusqlite` /
      `serde_norway` ancestors.

When this gate closes, Workstream D's smoke-test step 8 (planted-`.env`)
becomes the publish-gate proof point for WP5.

---

## 10. Filigree seeding

Issues to create at workstream kickoff (umbrella + seven tasks). Set
dependencies as shown.

```bash
# Umbrella
filigree create --type=work_package \
  --title="WP5 — Pre-ingest secret scanner (ADR-013)" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,tier:a" \
  --priority=1
# capture the returned id as $WP5_UMBRELLA

# Task 1
filigree create --type=task \
  --title="WP5 Task 1 — Scanner crate + rule registry + entropy" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,crate:scanner" \
  --priority=1
# capture as $T1

# Task 2
filigree create --type=task \
  --title="WP5 Task 2 — Baseline parser (.clarion/secrets-baseline.yaml)" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,crate:scanner" \
  --priority=1
# capture as $T2

# Task 3
filigree create --type=task \
  --title="WP5 Task 3 — CLI wiring: pre-ingest hook in analyze::run" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,crate:cli,crate:core" \
  --priority=1
# capture as $T3

# Task 4
filigree create --type=task \
  --title="WP5 Task 4 — Override semantics: --allow-unredacted-secrets" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,crate:cli" \
  --priority=1
# capture as $T4

# Task 5
filigree create --type=task \
  --title="WP5 Task 5 — MCP-side awareness of briefing_blocked" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,crate:mcp" \
  --priority=1
# capture as $T5

# Task 6
filigree create --type=task \
  --title="WP5 Task 6 — Rule-ID catalogue entries in detailed-design.md" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,docs" \
  --priority=2
# capture as $T6

# Task 7
filigree create --type=task \
  --title="WP5 Task 7 — Operator documentation: secret-scanning.md" \
  --labels="release:v0.1,sprint:3,wp:5,adr:013,docs" \
  --priority=2
# capture as $T7

# Dependencies
filigree add-dep $T3 $T1     # T3 depends on T1
filigree add-dep $T3 $T2     # T3 depends on T2
filigree add-dep $T4 $T3     # T4 depends on T3
filigree add-dep $T5 $T3     # T5 depends on T3 (writes the flag T5 reads)

# Umbrella rolls up
for t in $T1 $T2 $T3 $T4 $T5 $T6 $T7; do
  filigree add-dep $WP5_UMBRELLA $t
done

# Body for each issue should link back to the matching section of this doc:
#   docs/implementation/v0.1-publish/ws-a-secret-scanner.md#task-N-...
```

The umbrella is `done` when all seven tasks close; the workstream signs
off via the criteria in §9.

---

## 11. References

- [ADR-013 — Pre-Ingest Secret Scanner with LLM-Dispatch Block](../../clarion/adr/ADR-013-pre-ingest-secret-scanner.md) — canonical spec.
- [ADR-007 — Summary cache key](../../clarion/adr/ADR-007-summary-cache-key.md) — `briefing_blocked` interaction.
- [ADR-017 — Severity and dedup](../../clarion/adr/ADR-017-severity-and-dedup.md) — `CLA-SEC-*` namespace ownership.
- [ADR-021 — Plugin authority hybrid](../../clarion/adr/ADR-021-plugin-authority-hybrid.md) — path-jail upstream of scanner.
- [ADR-022 — Core/plugin ontology boundary](../../clarion/adr/ADR-022-core-plugin-ontology.md) — secret detection as a core-owned algorithm.
- [ADR-023 — Tooling baseline](../../clarion/adr/ADR-023-tooling-baseline.md) — CI floor every task in this workstream must clear.
- [Requirements — NFR-SEC-01, NFR-SEC-04, NFR-OPS-01, NFR-OPS-04](../../clarion/v0.1/requirements.md) — requirement floor.
- [Thread 1 — Pre-publish blockers (program of work)](./thread-1-pre-publish-blockers.md) — the umbrella program.
- [v0.1-plan.md — WP5 scope](../v0.1-plan.md#wp5--pre-ingest-secret-scanner) — original work-package definition.
- [Sprint 2 scope amendment — WP5 deferral rationale](../sprint-2/scope-amendment-2026-05.md) — "production deployment against unknown corpora gates on this returning."
- [Arch-analysis final report §7 follow-ups](../arch-analysis-2026-05-18-1244/04-final-report.md) — H-1 (`SoftFailed` coverage) coverage now closed via `clarion-141ca7de30`; relevant precondition for Task 3.
- [`detect-secrets` baseline format](https://github.com/Yelp/detect-secrets/blob/master/README.md#baseline-file) — the format Task 2 matches.
