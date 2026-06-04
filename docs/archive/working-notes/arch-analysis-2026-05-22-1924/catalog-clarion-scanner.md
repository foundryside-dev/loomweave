## 5. clarion-scanner

**Location:** `crates/clarion-scanner/`
**LOC:** 881 source (`lib.rs` 233 + `patterns.rs` 303 + `baseline.rs` 285 + `entropy.rs` 60) / 655 test (`tests/scanner.rs`)
**Crate type / role:** Library crate (`pub` API consumed by `clarion-cli`); pure CPU, no I/O except baseline YAML read.

### Responsibility

Owns Clarion's pre-ingest secret-detection pass: given an in-memory byte buffer, emit a deduplicated, byte-offset-anchored list of `Detection` records identifying secret-shaped substrings, plus a YAML-backed baseline mechanism that suppresses operator-acknowledged matches at the `(file, rule_type, hashed_secret, line_number)` granularity. The crate is deliberately scoped to detection + suppression — it does not walk the filesystem, decide which files to scan, emit findings, or report results; those concerns live in `clarion-cli/src/secret_scan/*`. Stores only positions, rule identifiers, and a detect-secrets-compatible SHA-1 digest of matched bytes — literal secret values do not leave the call (`lib.rs:1-5`).

### Key components

- `src/lib.rs:23-46` — `Detection` struct + `SecretCategory` enum: closed taxonomy with 9 categories (CloudCredential, VcsCredential, AiProviderCredential, PaymentsCredential, MessagingCredential, PrivateKey, JwtToken, HighEntropy, ContextualCredential).
- `src/lib.rs:49-99` — `HashedSecret([u8; 20])` newtype: SHA-1 digest with hex round-trip (`from_hex`, `Display`); decoupled from `sha1::Sha1` impl detail at the public surface.
- `src/lib.rs:102-200` — `DetectSecretsRule` enum: 14-variant closed vocabulary aligned to detect-secrets type names; `as_str()`/`rule_id()`/`FromStr` provide bidirectional mapping between human label (baseline YAML) and stable rule-id string (findings).
- `src/patterns.rs:25-71` — `Scanner` struct: pre-compiles a `RegexSet` (fast first-pass match) plus per-pattern `Regex` (for captures), holds entropy regexes and tunings; `Default`/`new()` build the ADR-013 v0.1 floor.
- `src/patterns.rs:79-162` — `scan_bytes()` + `scan_entropy()`: two-pass detection — named patterns first, entropy fallback over non-overlapping ranges; outputs sorted by `(byte_offset, rule_id)`.
- `src/patterns.rs:194-269` — `default_pattern_meta()`: the 12-entry rule floor (literal source of detection truth).
- `src/baseline.rs:11-44` — `Baseline`, `BaselineEntry`, `BaselineMatch`, `SuppressionResult` types: model the suppression file shape and the result envelope (allowed/suppressed/fired).
- `src/baseline.rs:104-144` — `Baseline::suppress()`: O(detections × entries_per_file) match using exact `(hashed_secret, line_number, rule_type)` triple as the suppression key.
- `src/baseline.rs:146-217` — `from_raw()` validation pipeline: version check (`"1.0"` only), path safety (`validate_baseline_path`), mandatory `justification`, hex-hash validity, closed rule-type vocabulary.

### Public interface (outbound)

Re-exported through `lib.rs:11-16`:

- `Scanner` (`patterns.rs:25`) — owns compiled regexes; cheap to construct once and reuse. Method: `scan_bytes(&self, buf: &[u8]) -> Vec<Detection>`.
- `Detection` (`lib.rs:23-32`) — one match: `rule_id`, `detect_secrets_type`, `category`, `byte_offset`, `line_number`, `matched_len`, `hashed_secret`.
- `DetectSecretsRule`, `SecretCategory` (`lib.rs:36-46`, `102-118`) — closed enums for downstream pattern-matching.
- `HashedSecret`, `HexDigestError` (`lib.rs:49-99`) — opaque hash newtype.
- `Baseline`, `BaselineEntry`, `BaselineEntryIssue`, `BaselineMatch`, `BaselineError`, `SuppressionResult` (`baseline.rs`) — operator-baseline model + error taxonomy with 7 variants (`UnsupportedVersion`, `MissingJustifications`, `InvalidPath`, `InvalidHash`, `UnsupportedRuleType`, `Parse`, `Io`).
- `load_baseline(&Path) -> Result<Baseline, BaselineError>` (`baseline.rs:71-77`) — accepts a missing file as `Baseline::empty()` (graceful absence).
- `EntropyTuning` (`entropy.rs:5-23`) — exposed but only `BASE64` and `HEX` consts are constructed internally; `min_len` + `min_entropy` fields.
- `PatternMeta` (`patterns.rs:10-15`) — exposed via `Scanner::pattern_meta()`, primarily for introspection.

### Dependencies

- **Inbound (who calls this):** Only `clarion-cli`:
  - `clarion-cli/src/secret_scan.rs` imports `Detection`, `Scanner`, `SuppressionResult`.
  - `clarion-cli/src/secret_scan/baseline.rs` imports `Baseline`, `BaselineError`.
  - `clarion-cli/src/secret_scan/findings.rs` imports `Detection`, `SecretCategory`, `DetectSecretsRule`, `HashedSecret`.
- **Outbound (what this calls):** No internal Clarion crates. External: `regex` (bytes flavour), `sha1`, `serde` + `serde_norway` (YAML), `thiserror`.
- **External services:** None directly. Reads the baseline file via `std::fs::read_to_string` (`baseline.rs:72`); no other I/O. Pure-function `scan_bytes` over an `&[u8]`.

### Internal architecture

Three modules + `lib.rs` umbrella. `lib.rs` owns the shared value types (`Detection`, `HashedSecret`, `DetectSecretsRule`, `SecretCategory`) and two tiny helpers — `sha1_digest` (`lib.rs:208-214`) and `line_number_for_offset` (`lib.rs:216-224`, a byte-by-byte newline count over the prefix). Concurrency is **none** — there is no shared mutable state; `Scanner` is `Send + Sync` by construction (only immutable compiled regexes), so callers parallelize at the file-level outside the crate (and `clarion-cli/src/secret_scan.rs:scan_source_files_parallel` does exactly that).

`patterns.rs` runs detection in two layers. Layer 1: `RegexSet` first-pass (`patterns.rs:80`) over the whole buffer — only patterns whose set-membership matched then have their full `Regex` run for capture extraction (`patterns.rs:83-87`). Layer 2: entropy fallback (`scan_entropy`, `patterns.rs:128-161`) runs the base64 candidate regex `[A-Za-z0-9+/]{20,}={0,2}` and hex candidate `\b[a-fA-F0-9]{40,}\b`, skipping candidates that **overlap any already-found named match** (`range_overlaps`, `patterns.rs:271-275`). The base64 fallback additionally requires non-base64 boundary bytes on each side (`base64_candidate_has_boundaries`, `patterns.rs:277-281`) — a hand-rolled `\b`-equivalent because `=` is not a word character.

Entropy is parameterized by two `EntropyTuning` constants:
- `BASE64`: `min_len = 20`, `min_entropy = 4.5` (`entropy.rs:11-14`).
- `HEX`: `min_len = 40`, `min_entropy = 3.0` (`entropy.rs:15-18`).

Entropy itself is Shannon entropy in bits/symbol over byte frequency (`entropy.rs:25-41`) using a `BTreeMap<u8, usize>` count table, computed as `-Σ p_i log2(p_i)`. These thresholds are deliberately wide: tests `entropy_minimum_lengths_are_pinned` (`tests/scanner.rs:171-174`) and the lockfile-SHA fixture (`tests/scanner.rs:568-638`) document that the hex threshold *intentionally* fires on git SHAs and npm lockfile integrity hashes — the v0.1 stance is to suppress via baseline rather than tighten the rule and risk missing real secrets.

The `KeywordDetector` rule (`patterns.rs:262-267`) is contextual: a Python/`.env`-shaped `name = "value"` assignment with name ∈ {`password`, `passwd`, `secret`, `token`, `api_key`, `secret_token`}, captured value ≥ 8 chars. To avoid false positives on commented-out examples, `scan_bytes` filters `ContextualCredential` matches whose line begins with `#` (`patterns.rs:91-94, 290-303`). The comment-handling is explicitly Python/shell-only — `//` and `/* */` comments are *not* skipped (tests at `tests/scanner.rs:162-167` lock this in deliberately).

`baseline.rs` is a value-typed YAML parser + suppression engine. The on-disk shape is a `version: "1.0"` envelope with `results: {path: [entry, ...]}` (`baseline.rs:237-253`). Parse-time validation rejects: non-1.0 versions, absolute or `..`-bearing paths, missing/blank `justification` (collected exhaustively — `from_raw` walks all entries before returning the error, see `baseline.rs:153-172` and the test at `tests/scanner.rs:408-433`), unknown detector-type strings, and invalid hex hashes. Suppression is deliberately exact-match: same path, same rule, same hash, same line number — *and* `is_secret: false` (a `true` entry, or omitted field defaulting to `true` per `default_is_secret` at `baseline.rs:255-257`, does **not** suppress; tests at `tests/scanner.rs:300-385` lock this in).

Error model is `thiserror`-derived (`baseline.rs:45-69`) with no panics in the parse path. The detection path has two `expect` calls in `Scanner::new` (`patterns.rs:51, 56-57, 66-69`) — all on compile-time constant regex literals, so they fire only on programmer error during edits, not on runtime input.

### Patterns observed

- **Two-phase regex matching** (`patterns.rs:80-87`) — `RegexSet` for cheap any-match prefilter, individual `Regex` only for matched indices. Trades memory for avoiding O(rules × text) capture work.
- **Closed enum at the interface, string only at the wire** — `DetectSecretsRule` (`lib.rs:102-118`) forces every consumer to handle the full vocabulary; conversions live at the YAML boundary (`baseline.rs:179-186`) so unknown types fail fast.
- **Capture-group routing via `Option<usize>`** (`patterns.rs:14, 96-103`) — `KeywordDetector` and `AwsSecretAccessKey` capture an inner group so the hash is over just the secret literal, not the `name="value"` framing.
- **Boundary-aware regex via helper functions** (`patterns.rs:277-303`) — Rust's `regex` crate's `\b` doesn't handle base64 padding `=` or `#`-comments, so the crate composes regex + post-filter.
- **Exhaustive error collection** for `MissingJustifications` (`baseline.rs:153-172`) — operator sees all missing entries at once, not one per re-run.
- **Path safety as a parse-time invariant** (`baseline.rs:219-235`) — absolute paths, `..`, drive prefixes all rejected at load; eliminates a class of suppression-escape attacks at the YAML boundary rather than at match time.
- **Cross-tool format compatibility by design** — the SHA-1 hash, the rule-name vocabulary, and the YAML schema all mirror Yelp's `detect-secrets` baseline format (cited explicitly in `lib.rs:1-5`), so operators familiar with that tool can reuse baselines.

### Concerns / Smells / Risks

- **`scan_bytes` is O(named_detections × entropy_candidates)** via `range_overlaps`'s linear scan (`patterns.rs:271-275`). For a file with hundreds of named matches and hundreds of entropy candidates this is quadratic. No interval tree, no sort-and-binary-search. For the target corpus (425k LOC Python, ADR-013 mentions) this is likely fine but worth flagging if scanner runtime ever becomes an issue.
- **`Baseline::suppress` is O(detections × entries_per_file)** with linear scan over the entries for that path (`baseline.rs:111-137`). Acceptable while baseline files are small (~tens of entries) but no index by `(rule, hash, line)`.
- **`usize_to_f64` truncates above 2³² bytes** (`entropy.rs:43-45`). For >4 GB files entropy will compute as if the buffer were 2³² bytes. Practically irrelevant — file-level scanning is bounded long before this — but the silent saturation deserves a comment.
- **Contextual-comment handling is Python/`.env`-only** (`patterns.rs:290-303`). A `// password = "..."` line in a JavaScript file *will* fire `ContextualCredential` (locked in by test at `tests/scanner.rs:163-167`). The crate's docstring acknowledges this; the cost is operator-baseline pollution in non-Python codebases until detector context is added.
- **`Scanner::new` panics on regex compile failure** (`patterns.rs:48, 51, 56, 66, 69`). These are compile-time literals so this is unreachable in shipping builds, but the public API has no fallible constructor — a future operator-provided pattern set would need a new constructor surface.
- **Entropy thresholds embed false-positive policy in code, not configuration** (`entropy.rs:11-18`). The test fixture explicitly documents that lockfile/git-SHA noise is *intentional* and the answer is operator baseline. This is a deliberate v0.1 trade-off but it pushes calibration burden to every operator until configurable thresholds or per-file-extension tuning lands.
- **`PatternMeta::capture_group` is private but the type is `pub`** (`patterns.rs:10-15`) — external consumers can read the struct via `Scanner::pattern_meta()` but can't construct one. This is fine as a closed API but the asymmetry is slightly awkward.
- **No fuzz or property tests** in `tests/scanner.rs` — only example-based assertions. Regex engines under pathological input (catastrophic backtracking) are a known foot-gun; the crate uses Rust's `regex` (which is linear-time by construction) so this is mitigated, but a quickcheck pass would be cheap insurance.

### Confidence: High

Read all 4 source files end-to-end (881 LOC), the full 655-line test file, the crate's `Cargo.toml`, and cross-checked inbound usage by greping the workspace for `clarion_scanner` imports (only `clarion-cli/src/secret_scan/*` and the scanner's own tests). Confirmed pre-ingest invocation timing at `clarion-cli/src/analyze.rs:237-243` (runs before plugin-host extraction, feeds `briefing_blocks` into the rest of the pipeline). Test file is unusually rich — it locks in not only positive/negative detection cases but also the *intentional* false positives (lockfile SHAs) with the documented operator-baseline workaround. No mystery code remains.
