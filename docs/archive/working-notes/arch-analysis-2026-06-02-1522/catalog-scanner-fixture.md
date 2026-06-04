## clarion-scanner

**Location:** `crates/clarion-scanner/`

**Responsibility:** Owns Clarion's pre-ingest secret-detection pass: given an in-memory byte buffer, emits a deduplicated, byte-offset-anchored list of `Detection` records identifying secret-shaped substrings, plus a YAML-backed baseline mechanism that suppresses operator-acknowledged matches at the `(file, rule_type, hashed_secret, line_number)` granularity — deliberately scoped to detection and suppression only; filesystem walk, finding emission, and LLM-dispatch blocking live in `clarion-cli`.

**Key Components:**
- `src/lib.rs:23-46` — `Detection` struct + `SecretCategory` enum (9 categories: CloudCredential, VcsCredential, AiProviderCredential, PaymentsCredential, MessagingCredential, PrivateKey, JwtToken, HighEntropy, ContextualCredential)
- `src/lib.rs:49-99` — `HashedSecret([u8; 20])` newtype: SHA-1 digest with hex round-trip (`from_hex`, `Display`); literal secret values do not leave the scanning call
- `src/lib.rs:102-118` — `DetectSecretsRule` enum: **14-variant** closed vocabulary (`AwsAccessKey`, `AwsSecretAccessKey`, `GitHubToken`, `GitHubFineGrainedToken`, `GitHubOAuthToken`, `AnthropicApiKey`, `OpenAiApiKey`, `StripeApiKey`, `SlackToken`, `JwtToken`, `PrivateKey`, `KeywordDetector`, `Base64HighEntropyString`, `HexHighEntropyString`); `as_str()`/`rule_id()`/`FromStr` provide bidirectional mapping
- `src/patterns.rs:25-71` — `Scanner` struct: pre-compiles `RegexSet` (fast first-pass) plus per-pattern `Regex` for captures; holds two `EntropyTuning` consts and two candidate regexes; `Default`/`new()` build the ADR-013 v0.1 floor
- `src/patterns.rs:79-161` — `scan_bytes()` (two-phase: named patterns first, entropy fallback over non-overlapping ranges) + `scan_entropy()` (base64 and hex candidate passes with boundary checks); public API entry point
- `src/patterns.rs:194-269` — `default_pattern_meta()`: **12-entry** named-rule floor (the literal source of detection truth; entropy rules are generated separately, bringing the total `DetectSecretsRule` variants to 14)
- `src/entropy.rs:10-23` — `EntropyTuning::BASE64` (`min_len=20`, `min_entropy=4.5`) and `EntropyTuning::HEX` (`min_len=40`, `min_entropy=3.0`); Shannon entropy in bits/symbol over byte frequency
- `src/baseline.rs:11-44` — `Baseline`, `BaselineEntry`, `BaselineMatch`, `SuppressionResult` types: YAML-backed suppression model; `is_secret: false` required for suppression; `default_is_secret()` returns `true` so omitted field does not suppress
- `src/baseline.rs:71-77` — `load_baseline(&Path)`: accepts missing file as `Baseline::empty()` (graceful absence)
- `src/baseline.rs:104-144` — `Baseline::suppress()`: exact `(hashed_secret, line_number, rule_type)` triple + `is_secret==false` as suppression key; O(detections × entries_per_file)
- `src/baseline.rs:146-209` — `from_raw()` validation pipeline: version check (`"1.0"` only), path safety, mandatory `justification` (exhaustive collection before returning error), hex-hash validity, closed rule-type vocabulary

**Dependencies:**
- Inbound: `clarion-cli` only — `clarion-cli/src/secret_scan.rs`, `clarion-cli/src/secret_scan/baseline.rs`, `clarion-cli/src/secret_scan/findings.rs`
- Outbound: no internal Clarion crates; external: `regex` (bytes flavour), `sha1`, `serde` + `serde_norway` (YAML), `thiserror`; no network or process I/O

**Patterns Observed:**
- **Two-phase regex matching** (`patterns.rs:80-87`): `RegexSet` any-match prefilter, individual `Regex` only for matched indices — avoids O(rules × text) capture work
- **Pure-detection library; caller drives FS walk** — `scan_bytes` takes `&[u8]`; `clarion-cli/src/secret_scan.rs:scan_source_files_parallel` owns parallelism and file I/O; `Scanner` is `Send + Sync` by construction
- **Closed rule enum at interface, string only at wire** — `DetectSecretsRule` forces exhaustive consumer handling; conversions at YAML boundary (`baseline.rs:179-186`) so unknown types fail fast
- **Baseline that won't mask drift** — suppression requires exact `(file, rule, hash, line)` match plus `is_secret: false`; a moved secret, a rotated key, or a line-number shift all re-fire (`baseline.rs:116-119`)
- **Comment-filter is Python/.env-only** (`patterns.rs:290-303`): only `#`-prefix lines suppressed for `ContextualCredential`; `//`/`/* */` intentionally not filtered (comment `patterns.rs:291-293` and test `tests/scanner.rs:162-167` lock this in)
- **Entropy thresholds encode false-positive policy in code** (`entropy.rs:11-18`): lockfile SHAs and git object hashes fire intentionally; operator baseline is the documented mitigation (`tests/scanner.rs:568-638`)
- **Cross-tool format compatibility by design** — SHA-1 hash, rule-name vocabulary, and YAML schema mirror Yelp `detect-secrets` baseline format (`lib.rs:1-5`)
- **Exhaustive error collection for `MissingJustifications`** (`baseline.rs:153-172`): operator sees all missing entries at once

**Drift: code vs. design docs**

1. **ADR-013 §Coverage omits `GitHubFineGrainedToken`** (`ADR-013-pre-ingest-secret-scanner.md:36`): the ADR coverage list names `github_pat_[a-zA-Z0-9_]{82}` (which is `GitHubFineGrainedToken`) but groups all GitHub variants under "GitHub (PATs, OAuth tokens)" without explicitly distinguishing fine-grained PATs as a separately named rule. The shipped code has 3 GitHub rules — `GitHubToken` (`ghp_`), `GitHubFineGrainedToken` (`github_pat_`), `GitHubOAuthToken` (`gho_`/`ghu_`/`ghs_`/`ghr_`) — which is consistent with the ADR's regex examples but not its prose coverage list (`ADR-013:87-88`). Minor: no behavioural gap, documentation is slightly under-specified.

2. **ADR-013 coverage list includes "Google Cloud service-account JSON fragments"** (`ADR-013:92`): `"Google Cloud service-account JSON fragments (detected via "private_key" + RSA header)"` appears in the ADR-013 committed coverage list but there is no dedicated `GoogleCloudServiceAccount` rule in `default_pattern_meta()` (`patterns.rs:194-269`). The `PrivateKey` rule (`-----BEGIN ... PRIVATE KEY-----` header) fires on RSA private key blocks in general, but the `"private_key"` field name in a GCP service-account JSON is not detected as a named pattern — only its embedded RSA header would fire. This is a **documentation gap**: the ADR claims coverage the scanner does not implement as a dedicated rule. The high-entropy fallback and `PrivateKey` header rule together cover most real-world GCP service-account keys, but the specific named coverage claim is unsupported. Severity: medium — real secrets in GCP service-account JSON without RSA headers embedded (e.g., with a different key format) would be missed as a named pattern.

3. **ADR-013 OpenAI pattern** (`ADR-013:36`): ADR states `sk-[a-zA-Z0-9]{48}` but the shipped pattern (`patterns.rs:235`) is `\bsk-(?:[A-Za-z0-9]{48}|(?:proj|svcacct)-[A-Za-z0-9_-]{20,})\b` — an extension that also covers `sk-proj-` and `sk-svcacct-` prefixes. This is a superset improvement not reflected in the ADR; ADR text is behind reality. Severity: low — the code is strictly better; the ADR is stale.

4. **ADR-013 Stripe pattern** (`ADR-013:36`): ADR states `sk_live_`, `pk_live_`, `rk_live_` (live only). Shipped pattern (`patterns.rs:241`) is `\b(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{16,}\b` — also covers `_test_` variants. Code is strictly more complete; ADR is stale. Severity: low.

5. **System-design §10 rule inventory omits named rules** (`system-design.md:1089`): §10 describes coverage as "high-entropy strings, common API key patterns (AWS, GitHub, Anthropic, Stripe, etc.), RSA private key headers, JWT-looking tokens" — prose only, no named enumeration. The detailed-design §10 (`detailed-design.md:1056-1064`) catalogs only the 5 Filigree finding IDs emitted by the scanner (`CLA-SEC-*`, `CLA-INFRA-SECRET-*`), not the 12 named detection rules. There is no design-doc table enumerating all 12 named rules with their patterns and categories. This is a **documentation gap**: a reader wanting to audit what the scanner detects must read `patterns.rs:194-269` directly; there is no doc-level inventory.

**Concerns:**
- (Medium) **GCP service-account named-rule gap** — ADR-013:92 claims coverage for "Google Cloud service-account JSON fragments" via `"private_key"` field detection, but no such rule exists in `default_pattern_meta()` (`patterns.rs:194-269`). The claim is partially met by the `PrivateKey` header rule + high-entropy fallback, but a dedicated `KeywordDetector`-style contextual rule matching `"private_key"\s*:\s*"..."` is absent. Fix: either add a named `GoogleServiceAccountKey` rule under `SecretCategory::CloudCredential` or update ADR-013 to remove the named-pattern claim and note reliance on `PrivateKey` + entropy. ADR-013 is Accepted and immutable; a new ADR or a supplementary note to detailed-design §10 is the correct path.
- (Low) **`scan_bytes` is O(named_detections × entropy_candidates)** via `range_overlaps`'s linear scan (`patterns.rs:271-275`). For a file with hundreds of named matches and hundreds of entropy candidates this is quadratic. No interval tree or sort-and-binary-search. Practically fine for target corpus but worth noting.
- (Low) **`Baseline::suppress` is O(detections × entries_per_file)** (`baseline.rs:111-137`); no index by `(rule, hash, line)`. Acceptable while baselines are small.
- (Low) **`usize_to_f64` saturates at 2³² bytes** (`entropy.rs:43-45`): entropy is computed incorrectly for buffers >4 GiB; silent saturation, no comment. Irrelevant in practice but deserves a doc comment.
- (Low) **Contextual comment handling is Python/.env-only** (`patterns.rs:290-303`): `//` and `/* */` comment lines are not skipped for `ContextualCredential`; fires on JavaScript/Rust commented-out examples. Documented intent; operator baseline is the workaround. A `//`-comment guard would reduce baseline pollution in multi-language codebases.
- (Low) **`Scanner::new` panics on regex compile failure** (`patterns.rs:48-69`): compile-time literals so unreachable in shipping builds, but a future operator-provided pattern set has no fallible constructor surface.
- (Low) **ADR-013 and `patterns.rs` coverage claims diverge on three rules** (OpenAI extended prefix, Stripe test keys, GCP service-account): ADR-013 is Accepted and immutable; the discrepancies should be noted in detailed-design §10 or a supplementary ADR addendum.

**Confidence:** High — read all 4 source files end-to-end (`lib.rs` 233 LOC, `patterns.rs` 303 LOC, `baseline.rs` 285 LOC, `entropy.rs` 60 LOC), cross-checked against ADR-013 (197 LOC), system-design §10, and detailed-design §10. Drift findings are primary-source (direct regex comparison `patterns.rs:194-269` vs. `ADR-013:36-38`). Prior catalog confirmed accurate on all counts except the Google Cloud coverage gap was not explicitly surfaced as a named drift item.

---

## clarion-plugin-fixture

**Location:** `crates/clarion-plugin-fixture/`

**Responsibility:** Provides the smallest valid implementation of the Clarion L4 JSON-RPC plugin protocol as a test-only binary that lets `clarion-core` and `clarion-cli` integration tests exercise the plugin host's subprocess code paths — including the OOM-kill and crash-loop-breaker paths — without requiring the Python plugin on `PATH`.

**Key Components:**
- `src/main.rs:23-123` — synchronous JSON-RPC dispatch loop: `read_frame` → parse `serde_json::Value` → branch on `(has_id, method)` → dispatch; no modules, threads, or async runtime
- `src/main.rs:67-76` — `initialize` handler: emits `InitializeResult { name, version: "0.1.0", ontology_version: "0.1.0", capabilities: {} }`
- `src/main.rs:51-53` — `initialized` notification handler: no-op, no reply
- `src/main.rs:77-115` — `analyze_file` handler: extracts `file_path` from params, returns one canned entity `fixture:widget:demo.sample` with `kind=widget`, `qualified_name=demo.sample`; no edges; default stats
- `src/main.rs:116-119` — `shutdown` handler: returns empty `ShutdownResult`
- `src/main.rs:54-56` — `exit` notification handler: `process::exit(0)`
- `src/main.rs:78-83, 137-184` — `CLARION_FIXTURE_EXCEED_RLIMIT_AS` fault-injection path (Unix only): repeatedly `mmap_anonymous(PROT_NONE, MAP_PRIVATE)` doubling-size regions to exhaust virtual address space; self-`SIGKILL` on failure so host observes signal-death not clean exit; pre-reserved `Vec<_>(1024)` to avoid allocation after memory pressure begins
- `src/main.rs:125-135` — `send_result` helper: wraps `Value` in `ResponseEnvelope`, serialises, frames via `write_frame`, flushes
- `src/lib.rs:1-3` — three-line doc-only stub (workspace-member compatibility trick)

**Dependencies:**
- Inbound: `crates/clarion-core/tests/host_subprocess.rs` (direct subprocess test via `CARGO_BIN_EXE_clarion-plugin-fixture`; asserts `fixture:widget:demo.sample`); `crates/clarion-cli/tests/wp2_e2e.rs` (4 test cases including OOM-kill and crash-loop-breaker; declared in `clarion-cli/Cargo.toml:43` as `[dev-dependencies]`)
- Outbound: `clarion-core::plugin::transport` (`read_frame`/`write_frame`/`Frame`); `clarion-core::plugin::limits::ContentLengthCeiling::DEFAULT` (8 MiB per ADR-021); `clarion-core::plugin` request/response DTOs (`AnalyzeFileParams`, `AnalyzeFileResult`, `AnalyzeFileStats`, `InitializeResult`, `ShutdownResult`, `JsonRpcVersion`, `ResponseEnvelope`, `ResponsePayload`); `serde_json`; `nix` (Unix-only, `mman`+`signal` features)

**Patterns Observed:**
- **Stub lib + real bin** (`lib.rs:1-3` + `Cargo.toml:12-14`): workspace-member compatibility trick; no executable code in lib target
- **Untyped envelope parse, typed payload re-parse** (`main.rs:37-44, 93-98`): reads whole frame as `serde_json::Value` to inspect `id`/`method` before committing to a typed struct — unknown/malformed messages rejected without spurious deserialisation errors
- **Crash-on-anomaly as a feature** (`main.rs:33-46, 57, 97, 120`): every error path is `process::exit(1)`; the host's crash-recovery and crash-loop-breaker code paths depend on this behaviour
- **Hard-coded fixture identity** (`main.rs:101-108`): `"fixture:widget:demo.sample"` is the ground-truth string tests assert verbatim; change requires updating both consumer test files
- **Environment-flag-driven fault injection** (`main.rs:78-83`): `CLARION_FIXTURE_EXCEED_RLIMIT_AS` toggles OOM path; default is benign
- **PROT_NONE address-space probe** (`main.rs:137-178`): exhausts virtual address space cheaply (no physical page commits) to trip `RLIMIT_AS` deterministically; `SIGKILL` self so parent sees signal-death
- **Pre-reserved Vec to avoid allocation under memory pressure** (`main.rs:142-145`): 1024 mapping handles reserved before the pressure loop starts

**Drift: code vs. design docs**

1. **Method set matches prior catalog** — 5 methods (`initialize`, `initialized`, `analyze_file`, `shutdown`, `exit`) confirmed in `main.rs:48-121`; no additions or removals since prior analysis.
2. **Hard-coded version strings** (`main.rs:71-72`): `version = "0.1.0"` and `ontology_version = "0.1.0"` do not reference `Cargo.toml`; no documented version-handshake requirement in ADR-013 or the plugin protocol spec, so this is not a normative gap, but the strings will silently drift if the workspace version is bumped and the handshake is ever enforced.
3. **Companion manifest location** (`clarion-core/tests/fixtures/plugin.toml`): the fixture's protocol identity (`plugin_id="fixture"`, `entity_kinds=["widget"]`, `rule_id_prefix="CLA-FIXTURE-"`) lives in `clarion-core`'s test tree, not co-located with this crate. Two callers (`host_subprocess.rs`, `wp2_e2e.rs`) each construct their own variant. No doc-level discrepancy, but the physical separation is an undocumented convention.

**Concerns:**
- (Low) **`process::exit(1)` everywhere with no diagnostic output** (`main.rs:33-46, 57, 97, 120`): if a future test asserts on stderr or a specific exit code other than 1, every error branch needs disambiguating; no `eprintln!` anywhere. Acceptable for the current fixture role.
- (Low) **No "initialize must come first" sequencing check**: a misbehaving host could call `analyze_file` before `initialize` and the fixture would respond correctly. The fixture is not a conformance checker; do not use it as one.
- (Low) **`unwrap()` on `serde_json::to_value(InitializeResult)`** (`main.rs:75`) and other result serialisations: defensible because types are `Serialize`-derived, but technically unchecked crash points.
- (Low) **Unix-gated OOM path** (`main.rs:78-83`): the `cfg(not(unix))` arm silently exits 1 on Windows, losing coverage of the RLIMIT_AS branch. Acceptable given OOM-kill semantics are Unix-specific; worth noting if Windows CI is ever added.
- (Low) **Manifest lives outside this crate's tree**: `clarion-core/tests/fixtures/plugin.toml` is the canonical fixture manifest; a co-located `tests/fixtures/plugin.toml` in this crate reused by both consumers would be cleaner — not urgent.

**Confidence:** High — read `src/main.rs` (184 LOC) and `src/lib.rs` (3 LOC) end-to-end; verified method set, fault-injection path, and dependency surface against prior catalog. Prior catalog confirmed fully accurate; this pass found no behavioural changes. Drift items are minor version-string and manifest-location observations, not functional gaps.
