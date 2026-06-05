# ADR-013: Pre-Ingest Secret Scanner with LLM-Dispatch Block

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: every file reaching the LLM is a potential secret-exfiltration event; v0.1 design review flagged this as CRITICAL

## Summary

Before any file content reaches the LLM provider (Phases 4, 5, 6), Loomweave runs a **core-owned** secret scanner over the file buffer. Detections on any file block LLM dispatch for that file specifically: structural extraction (Phase 1) still runs, entities still land in the store, but briefings marked `briefing_blocked: secret_present` are not dispatched to Anthropic. A `LMWV-SEC-SECRET-DETECTED` finding (severity ERROR) is emitted per detection. Operators mark known false positives in a committable `.loomweave/secrets-baseline.yaml` (detect-secrets baseline format). The `--allow-unredacted-secrets` override requires explicit TTY confirmation or an explicit `--confirm-allow-unredacted-secrets=yes-i-understand` non-TTY flag, and records `LMWV-SEC-UNREDACTED-SECRETS-ALLOWED` per affected file. The scanner implementation is a **Rust-native port of the detect-secrets rule set** — chosen over Python-embed / subprocess-invocation to preserve single-binary distribution (NFR-OPS-04).

## Context

The 2026-04-17 panel threat model (`09-threat-model.md` §7, T-10) scored "secret scanner false negatives leak source + secrets to Anthropic" at risk 6. The design review (`design-review.md:88-91`) escalated the v0.1 case to CRITICAL:

> The design sends file content to Anthropic for summarization (§5, §6) with no pre-ingest secret scanner, no redaction pass, no per-file allow/deny beyond include/exclude globs. Any `.env`, test fixture, or committed API key enters Anthropic's API and persists in the summary cache and `runs/<run_id>/log.jsonl` (which the design keeps "for audit"). The first real user running `loomweave analyze` on a repo with a committed `.env` leaks to Anthropic silently.

The mitigation the review prescribed — pre-ingest secret scanner with LLM-dispatch block — is a v0.1 non-negotiable. The scope-commitment memo confirmed P0 (`v0.1-scope-commitments.md:188`).

Open questions this ADR settles:

1. **Implementation path** — Python `detect-secrets` embedded/subprocessed, or Rust-native port?
2. **Block granularity** — file, entity, or briefing level?
3. **Baseline format + location** — where false-positives go; committable?
4. **Override semantics** — what prevents accidental CI bypass?
5. **Coverage list** — which secret kinds v0.1 commits to detecting?
6. **Plugin-boundary interaction** — scanner relative to ADR-021 path-jail and ADR-022 core/plugin ontology split.

## Decision

### Implementation — Rust-native port of detect-secrets rule set

The scanner is a Rust module in the Loomweave core. It implements the rule set of `detect-secrets` (v1.x baseline compatibility) natively:

- **High-entropy strings**: base64 (entropy ≥ 4.5 over ≥20 chars), hex (entropy ≥ 3.0 over ≥40 chars).
- **Named credential patterns**: AWS access keys (`AKIA[0-9A-Z]{16}`, `ASIA[0-9A-Z]{16}`), AWS secret-key adjacency, GitHub PATs (`ghp_[a-zA-Z0-9]{36}`, `github_pat_[a-zA-Z0-9_]{82}`), GitHub OAuth tokens (`gho_`, `ghu_`, `ghs_`, `ghr_`), Anthropic API keys (`sk-ant-[a-zA-Z0-9_-]{90,}`), OpenAI keys (`sk-[a-zA-Z0-9]{48}`), Stripe keys (`sk_live_`, `pk_live_`, `rk_live_`), Slack tokens, JWT tokens (`eyJ[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}\.[a-zA-Z0-9_-]{10,}`).
- **Private key headers**: `-----BEGIN (RSA|EC|DSA|OPENSSH|PGP) PRIVATE KEY-----`, `-----BEGIN ENCRYPTED PRIVATE KEY-----`.
- **Contextual credentials**: name-based patterns (`password`, `passwd`, `secret`, `token`, `api_key`, `api-key` followed by `=`, `:`, `:=`, or equivalent assignment + a quoted string).

The rule set matches `detect-secrets` v1.x so that operators familiar with that tool (and its baseline file) migrate without surprise. The implementation lives in `loomweave_scanner` crate inside the Loomweave workspace; ships as part of the core binary; zero runtime dependency on Python or external `detect-secrets` install.

### Block granularity — file-level; structural extraction preserved

When the scanner flags file `F`:

- `F`'s Phase-1 structural extraction runs normally. The plugin parses `F`, emits entities, edges, and structural findings (`LMWV-PY-STRUCTURE-*`, `LMWV-FACT-*`). These land in the store.
- `F`'s entities carry `briefing_blocked: secret_present` in their `properties` dict. Phase 4–6 dispatch skips them; no LLM call, no summary-cache write.
- `LMWV-SEC-SECRET-DETECTED` findings are emitted — one per (rule, file, line) tuple. Severity ERROR.
- Consult-mode queries on blocked entities return the entity with no `summary` field and an explicit `briefing_blocked` signal so agents know the absence is policy, not pipeline failure.

Why file-level, not entity-level: the scanner runs over file-buffers (byte streams), not entity slices. Secret detection operating on pre-parse bytes catches secrets outside the parseable structure (comments, top-of-file docstrings, binary-ish strings that still embed a secret). Entity-level would miss file-header secrets.

Why not refuse to parse: structural information is needed for the catalog even when briefings can't be written. Operators fixing a committed `.env` should get the full Phase-1 picture of what the codebase looks like — they just don't get summaries for the affected files until they fix the secret.

### Baseline — `.loomweave/secrets-baseline.yaml`

False positives are marked in `.loomweave/secrets-baseline.yaml`, using the exact format `detect-secrets`' `baseline` command produces:

```yaml
version: "1.0"
results:
  "src/auth/fixtures.py":
    - type: "Base64 High Entropy String"
      hashed_secret: "a1b2c3..."  # sha1 of the literal bytes
      line_number: 42
      is_secret: false
      justification: "Test fixture — Stripe sandbox key, documented public"
```

- Committable by default. Reviewing a `.loomweave/secrets-baseline.yaml` diff in PRs is an intentional audit surface: "operator marked this string as not-a-secret" is a review-worthy signal.
- `justification` field is required (schema-validated at load time). A baseline entry without justification is rejected with `LMWV-INFRA-SECRET-BASELINE-NO-JUSTIFICATION`.
- `hashed_secret` stored over the literal bytes; the actual string never lives in the baseline.

### Override — `--allow-unredacted-secrets`

The override exists for specific legitimate cases: analysing a repo that genuinely contains committed test-only credentials against a sandbox, or urgent debugging where the operator accepts the leak. It is explicit and audited:

- **TTY sessions**: `--allow-unredacted-secrets` prompts interactively with the list of detected secrets; operator types `yes-i-understand` to proceed.
- **Non-TTY sessions (CI)**: requires both `--allow-unredacted-secrets` AND `--confirm-allow-unredacted-secrets=yes-i-understand`. A CI pipeline accidentally adding `--allow-unredacted-secrets` alone fails with `LMWV-INFRA-SECRET-OVERRIDE-UNCONFIRMED` rather than bypassing silently.
- **Audit trail**: each affected file emits `LMWV-SEC-UNREDACTED-SECRETS-ALLOWED` (severity ERROR). `runs/<run_id>/stats.json` records `{override_used: true, files_affected: [...]}`.
- **Filigree surfacing**: the override finding reaches Filigree via the normal scan-results path. Security-focused operators running `filigree list --rule-id=LMWV-SEC-UNREDACTED-SECRETS-ALLOWED --since 30d` see every override in the audit window.

### Coverage v0.1 — committed list

The scanner commits to detecting, at minimum:

- AWS (access keys, secret-adjacency)
- GitHub (PATs, OAuth tokens)
- Anthropic API keys
- OpenAI API keys
- Stripe keys (live + test)
- Slack tokens
- Google Cloud service-account JSON fragments (detected via `"private_key"` + RSA header)
- RSA/EC/DSA/OpenSSH private keys (header-based)
- JWT tokens
- High-entropy base64 and hex (bounded by length threshold to cut false positives on UUIDs etc.)
- Contextual credentials (name-based patterns)

Novel secret shapes — single-use custom API keys without a named pattern — are caught by high-entropy detection only. This is acknowledged as a v0.1 floor, not a ceiling. v0.2 adds Wardline-sourced custom-rule integration and allowlist-based coverage extensions.

### Plugin-boundary interaction

- The scanner runs **core-side**, not plugin-side. Reading the file buffer and running the scanner happens before `analyze_file` RPC is issued to the plugin.
- ADR-021's path-jail applies upstream: the file list the scanner operates on is filtered by `project_root` containment already, so the scanner doesn't need its own path-jail.
- ADR-022's ontology boundary classifies secret detection as an **algorithm** (core-owned), not an ontology (plugin-owned). This is consistent — pattern matching is language-independent; every language plugin benefits from the same scanner.

## Alternatives Considered

### Alternative 1: Python `detect-secrets` embedded / subprocessed from the core

Run `detect-secrets` via its Python API, either embedding Python interpreter in the core binary or subprocessing out to a system Python.

**Pros**: rule set is canonical (upstream `detect-secrets` maintains it); no port work; behaviour parity with a tool many operators already know.

**Cons**: adds a Python runtime dependency to the core binary, breaking NFR-OPS-04 (single-binary distribution). Subprocessing to system Python is fragile across operator environments (Python version, virtualenv contamination, missing `detect-secrets` install). Embedding Python bloats the binary significantly (~20+ MB).

**Why rejected**: violates single-binary commitment. The Rust-native port is bounded engineering cost (a few hundred LoC for the pattern set plus entropy calculation); the long-term deployment simplicity is worth it.

### Alternative 2: Redact rather than block — replace secrets with `<REDACTED>` placeholders

Detected secrets are replaced in the file buffer with placeholders; redacted file is sent to the LLM.

**Pros**: LLM still gets file context; partial briefings possible; operator doesn't have to fix the secret immediately.

**Cons**: partial-file context often loses semantic meaning. A file whose purpose is "parse this API key from env" reads nonsensically after redaction (`api_key = "<REDACTED>"`); the LLM produces a briefing that misrepresents the file's function. False-negative risk is higher (a secret the scanner missed still reaches Anthropic). The blast radius of a leak (secret in logs, cache, briefing) is the same whether 1 secret or many slip through; blocking is safer.

**Why rejected for v0.1**: block is the conservative correct answer. Redaction is a v0.2+ consideration with per-rule allowlists and operator opt-in, not a v0.1 default.

### Alternative 3: Post-ingest scanning

Run the scanner on LLM responses or on briefings before they're cached.

**Pros**: catches secrets the LLM might hallucinate or re-emit; lightweight implementation.

**Cons**: secrets already went to Anthropic. The leak has occurred. Post-ingest scanning is secondary defence at best, not the primary control. T-10 is about source-to-Anthropic flow; post-ingest doesn't address it.

**Why rejected as primary**: too late. Post-ingest scanning on briefings is v0.2+ additive defence; v0.1 commits to the pre-ingest block as the primary line.

### Alternative 4: Scanner runs inside the language plugin, not the core

Each plugin (Python, future Java, future Rust) runs its own scanner as part of `analyze_file`.

**Pros**: plugins have file-buffer access already; scanner output joins the plugin's findings naturally.

**Cons**: every plugin reimplements the same rule set; vocabulary drift between plugins; scanner quality depends on plugin-author discipline. ADR-022's core/plugin ontology split puts algorithms in the core, ontology in the plugin — secret detection is an algorithm (pattern matching over bytes), not ontology (language-specific concepts). Putting it in the plugin is a category error.

**Why rejected**: violates ADR-022 categorisation and duplicates effort.

### Alternative 5: No pre-ingest scanning (accept the leak risk)

Rely on operator discipline — "don't commit `.env` files"; add documentation.

**Pros**: zero engineering cost.

**Cons**: design review called this CRITICAL. The first real user running `loomweave analyze` on a repo with a committed `.env` leaks to Anthropic silently. Marketing copy describes Loomweave as a security-analysis tool; shipping without the scanner is a credibility failure before v0.1 launches.

**Why rejected**: not an option for a security-tool release.

## Consequences

### Positive

- Closes the design-review CRITICAL flag on secret exfiltration. The first real user on a repo with a committed `.env` now sees a clear ERROR, not a silent leak.
- Reduces T-10 (false-negative source+secret leak) from risk 6 toward risk 3 — pattern-based detection has false negatives, but the floor is substantially raised.
- Audit trail is first-class: every block, every override is a finding in Filigree. Security-focused operators can produce compliance reports straight from `filigree list`.
- Core-owned scanner works for every current and future plugin. Adding a Java plugin doesn't require re-implementing the scanner.
- Single-binary distribution preserved (NFR-OPS-04).

### Negative

- Pattern-based scanning has false negatives. Novel secret shapes (custom internal API keys without a named pattern, stegano-encoded secrets, secrets in non-text formats) slip through. Mitigation: high-entropy detection catches many; operators running against truly high-risk repos should prefer `--no-llm` mode or air-gapped alternatives.
- Rust-native port is engineering cost. Core team owns maintenance; rule additions are Rust code changes. Mitigation: v0.2 adds Wardline-sourced custom rules so the core rule set doesn't need to absorb every project's unique patterns.
- False-positive pressure on operators. High-entropy detection triggers on UUIDs, base64-encoded checksums, test-fixture tokens. Operators maintain `.loomweave/secrets-baseline.yaml` to suppress; this is manual work. Mitigation: baseline-format compatibility with `detect-secrets` means `detect-secrets scan --baseline` from operator habits transfers directly.
- Override path exists and is used sometimes. `LMWV-SEC-UNREDACTED-SECRETS-ALLOWED` must be monitored; absence of monitoring turns the audit trail into theatre.

### Neutral

- The scanner's file-buffer input comes through core-side file I/O, upstream of plugin `file_list` returns. ADR-021's path-jail filters the file list before the scanner sees it.
- Entities in blocked files land without briefings; the summary cache (ADR-007) has no row for them until the secret is fixed. Next `loomweave analyze` post-fix cache-misses and computes normally.
- Baseline file committed alongside source means teammates pulling a repo inherit each other's false-positive markings. This is correct — security-review consensus is expressed in the baseline diff.

## Related Decisions

- [ADR-004](./ADR-004-finding-exchange-format.md) — `LMWV-SEC-SECRET-DETECTED`, `LMWV-SEC-UNREDACTED-SECRETS-ALLOWED`, `LMWV-INFRA-SECRET-*` findings use the Filigree-native exchange format this ADR defines.
- [ADR-007](./ADR-007-summary-cache-key.md) — `briefing_blocked: secret_present` entities produce no cache rows; the cache correctness argument relies on this ADR's block behaviour being deterministic.
- [ADR-017](./ADR-017-severity-and-dedup.md) — `LMWV-SEC-*` is one of the namespaced rule-ID prefixes; this ADR is its primary producer.
- [ADR-021](./ADR-021-plugin-authority-hybrid.md) — path-jail (Layer 2a) filters the file list this scanner operates on. The scanner does not need its own path validation.
- [ADR-022](./ADR-022-core-plugin-ontology.md) — classifies secret detection as a core-owned algorithm (not plugin-owned ontology). This ADR is the canonical instance of the "algorithm stays in the core" rule.

## References

- [Loomweave v0.1 design review §3 (Secret exfiltration)](../../implementation/v0.1-reviews/pre-restructure/design-review.md) (lines 88-91) — the CRITICAL flag this ADR retires.
- [Loomweave v0.1 panel threat model T-10](../../implementation/v0.1-reviews/panel-2026-04-17/09-threat-model.md) (line 241) — risk scoring and residual-risk framing.
- [Loomweave v0.1 system design §10 (Pre-ingest redaction)](../v0.1/system-design.md) (lines 1044-1056) — the behaviour this ADR formalises.
- [Loomweave v0.1 requirements — NFR-SEC-01, NFR-SEC-05](../v0.1/requirements.md) — requirement floor.
- [detect-secrets baseline format](https://github.com/Yelp/detect-secrets/blob/master/README.md#baseline-file) — the format this ADR's baseline layout matches.
