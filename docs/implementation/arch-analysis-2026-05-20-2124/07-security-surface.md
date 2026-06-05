# RC1 Security Surface

## Security Posture Summary

Loomweave has a credible local-first security posture: it treats plugins as
untrusted subprocesses, scans source before LLM exposure, constrains HTTP read
access, and keeps sibling products optional. The main security risks are local
implementation choices that need discipline: hand-rolled HMAC, unauthenticated
`_capabilities`, Linux-only plugin memory ceilings, and the filesystem race
inherent in canonicalization-based path jails.

## Trust Boundaries

| Boundary | Control |
|---|---|
| Target repository to Loomweave | Secret scanning before ingest; path normalization; ignore handling; briefing-blocked file semantics. |
| Loomweave host to plugin subprocess | Content-Length JSON-RPC; manifest validation; executable basename validation; frame caps; path jail; field caps; Linux process limits. |
| Plugin output to storage | Entity/edge validation; reserved core ontology enforcement; malformed outputs become findings or breaker conditions. |
| Storage to MCP/HTTP | Typed query helpers; closed envelopes; traversal/path protections; briefing-blocked non-disclosure. |
| MCP to external LLM provider | Live-provider opt-in; source-hash checks; excerpt/range limiting; token budget; cache accounting. |
| HTTP API to sibling consumers | Loopback default; HMAC/bearer auth for non-loopback; body/concurrency/timeout limits; ETags. |
| Release workflow to public artifacts | Pinned action checks, governance guard, checksums, cosign, SLSA provenance. |

## Positive Controls

- Pre-ingest scanner blocks LLM summary paths before source bytes leave the
  local machine.
- Baselines require exact hash/rule/path/line matching and `is_secret = false`.
- Scanner findings do not export literal secrets.
- Python plugin stdout guard prevents accidental protocol corruption.
- Pyright is configured with low-indexing blast radius.
- HTTP path traversal and briefing-block behavior are test-pinned.
- Live LLM providers require explicit opt-in.

## Security Concerns

### Hand-Rolled HTTP HMAC

`crates/loomweave-cli/src/http_read.rs` implements HMAC behavior locally with
`sha2`. This increases review burden. The length-mismatch fast return in
constant-time comparison is probably not catastrophic if request parsing and
exact-length signatures are enforced, but the code should be treated as
cryptographic surface.

**Recommendation:** move to a vetted HMAC crate or document crypto-specific
review ownership.

### Unauthenticated `_capabilities`

The endpoint is intentionally unauthenticated for pre-auth discovery. That is
compatible with the current contract only if bind-address rules and
non-loopback auth remain strict.

**Recommendation:** keep tests around loopback/non-loopback policy mandatory.

### Linux-Only Plugin Memory Limits

`RLIMIT_AS` enforcement is Linux-specific. Non-Linux environments warn and
continue without equivalent memory ceilings.

**Recommendation:** document platform limits clearly in operator docs and avoid
overstating cross-platform resource isolation.

### Path Jail TOCTOU

The path jail relies on canonicalization. The source explicitly acknowledges a
race between proof and later open.

**Recommendation:** acceptable for local-first RC1 if documented, but revisit
if Loomweave is ever exposed to hostile multi-user repositories or remote read
surfaces.

### LLM Empty Excerpt Behavior

MCP LLM paths hash-check and range-limit source, but unreadable/non-UTF8 files
return an empty excerpt instead of a hard error.

**Recommendation:** decide whether empty excerpt is intentional safe
degradation or should be a hard refusal for summary/inference.

## Release Security Blockers

The code security posture is not the only release question. Live repository
governance remains a policy blocker until verified: protected `main`,
restricted Actions policy, required checks/rulesets, release workflow dry run,
and artifact smoke.

## Security Verdict

No systemic security design failure was found. The release should wait for live
governance verification and small contract/doc drift fixes, but the core
local-first security architecture is sound.
