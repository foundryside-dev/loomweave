# Security-architecture view on the five §8 open questions

**Role**: Threat analyst (STRIDE + attack-surface focus).
**Question**: Which of the five §8 questions have *security* answers, and which are operational/architectural neutral?

## Verdict matrix

| Q | Topic | Security-relevant? | Dominant STRIDE category |
|---|-------|--------------------|--------------------------|
| Q1 | 25-file pyright restart | **Marginal** — DoS-adjacent, but the constant is a *mitigation*, not a threat surface | D |
| Q2 | Monolith refactor (host.rs 2935 LOC) | **Yes — high-stakes** | T, E, R |
| Q3 | `clarion-llm` split | **Yes — moderate** | T, I (supply-chain + outbound trust) |
| Q4 | `application_id` / `user_version` | **Yes — but mostly integrity, not confidentiality** | T (and a sliver of S in multi-tenant Loom) |
| Q5 | Hardcoded limits (11+) | **Yes — this is the question with the most teeth** | D, E |

## Per-question analysis

**Q1 — Pyright 25-file restart.** The constant is the *fix* for memory growth, not the surface. The real question is what happens if a pathological corpus drives Pyright RSS above the host's tolerance *within* a 25-file window. Today the answer is "the `RLIMIT_AS` 2 GiB ceiling on the plugin child kills it, the supervisor emits `FINDING_OOM_KILLED`, the run aborts cleanly." So Q1 has a security answer only in the sense that **the 25-file constant is itself a tuned defense parameter**, which folds into Q5. Standalone, neutral.

**Q2 — Monolith refactor.** This is the most security-load-bearing answer in the set. `host.rs` (2935 LOC) concentrates the four-stage pipeline, stderr drain, child reaping, breaker wiring, jail-check sequencing, and supervisor signal handling. The threat is **Tampering with security mechanisms via accidental refactor** — STRIDE-T against the supervisor itself, with Elevation as the eventual consequence (a plugin that should have been killed keeps running). Concrete failure modes a careless extract-method introduces:

- Breaker `record_escape` called on wrong code path → path-escape budget effectively infinite.
- Stderr drain detached from child-reap → zombie + log-channel DoS.
- `pre_exec` ordering broken (setrlimit applied after exec) → RLIMIT_AS no longer applies to the child.
- Jail-check moved *after* a downstream consumer that opens the path → revives the TOCTOU window (see below).

The risk is not that refactoring is impossible; it is that **the file has no test that says "the supervisor still kills the plugin under the breaker policy after this refactor."** Recommendation: before splitting `host.rs`, add a property-style integration test that asserts each ADR-021 §2a–§2d invariant survives module boundary changes. Then refactor.

**Q3 — `clarion-llm` split.** Currently `reqwest` (and TLS, and DNS) is reachable from the same crate that supervises plugins. **Outbound HTTP from inside the plugin-supervisor crate is a meaningful trust-surface widening** — a CVE in `reqwest`/`hyper`/`rustls` becomes a CVE in the process that holds the jail. Splitting `clarion-llm` into its own crate (and ideally its own process) is a defense-in-depth win: blast radius of an outbound-HTTP-stack exploit no longer reaches plugin supervision. STRIDE-T (compromised LLM client tampering with supervisor state) and STRIDE-I (LLM client exfiltrating jail-passed paths via DNS/SNI side channels) both shrink. **This question has a security answer: yes, split it, and the security argument is independent of the architectural one.**

**Q4 — `application_id` / `user_version`.** Mostly an integrity question (`user_version` for migrations) — but `application_id` has a **specific Loom-federation security value**: it lets a sibling tool refuse to operate on a database that isn't Clarion's. Without it, a misconfigured Filigree pointed at a stale or hostile `.clarion/state.db` cannot detect the substitution at the file-format layer. STRIDE-S (spoofing a Clarion DB at the federation read boundary). The cross-tool collision-detection benefit is real and cheap; set both, and have the storage layer refuse to open a DB whose `application_id` does not match. Not P0, but trivially worth doing.

**Q5 — Hardcoded limits.** This is the question with the most security weight. The 11+ values include the entity cap (500k), the Content-Length ceiling (8 MiB), the path-escape breaker threshold (10/60s), RLIMIT_AS (2 GiB), RLIMIT_NOFILE (256), RLIMIT_NPROC (32), HTTP body limit (16 KiB), concurrency limit (64), request timeout (10s), batch maxima (256/1000), and the pyright restart. **Recompile-to-tune is a security posture stance**: it means an operator under active adversarial-plugin pressure cannot tighten the breaker threshold from 10 to 3 without a rebuild. ADR-021 §2b already nods at this by mentioning a "configuration-surface" floor that isn't yet plumbed. The honest answer: **at v1.0 these are deliberately frozen so the security policy is uniform across deployments; post-1.0, the path-escape breaker threshold, the entity cap, and the RLIMIT_AS ceiling should become operator-tunable with hard floors enforced at config-load time.** The HTTP-body and concurrency limits can stay compiled-in (operational, not adversarial).

## TOCTOU claim

The catalog's "latent because current consumer doesn't open files" claim is **correct as of today** and **explicitly documented in `jail.rs` lines 67–72**. The function returns a `PathBuf` that is a "membership proof at canonicalization time, not a durable file handle." A future consumer that calls `jail(...)` then `std::fs::open(returned_path)` opens the canonical path — but between canonicalize and open, an attacker with write access to a directory on the canonical path (e.g. a plugin that can mutate its own workspace) can replace a path segment with a symlink to `/etc/shadow`. The next open follows the new symlink. **Concrete exploit**: plugin returns `<root>/staging/report.txt`, supervisor jail-checks it OK, plugin races a `rename` to swap `staging` for a symlink to `/etc`, supervisor opens `report.txt` and reads `/etc/passwd`. The mitigation is the `openat`-anchored-to-pinned-root strategy the docstring recommends; do this *before* any code path opens a jail-returned path.

## Confidence Assessment

**High** on Q2 (monolith risk is concrete and grounded in the source), Q3 (trust-surface argument is standard), and the TOCTOU exploit chain (the file documents the gap itself).
**Medium** on Q4 (the Loom federation S-axis is real but I haven't audited every sibling's DB-open path) and Q5 (specific recommendations on *which* limits to make tunable are judgment calls, not derivations).
**Low** on Q1 — I'm 60% confident it is "neutral folded into Q5"; could be argued the 25-file number is itself an adversary-tunable surface if a malicious corpus could be shaped to exhaust the host before the restart fires.

## Risk Assessment

If these answers are wrong: Q2 is the only one where being wrong is dangerous — recommending a refactor without first writing invariant-preserving tests could ship a broken jail. Q3/Q4/Q5 errors are forward-only (failure to *add* defense-in-depth, not removal of existing defense). Q1 has no downside either way.

## Information Gaps

- No audit of who opens jail-returned paths today; the claim "no current consumer" is from the docstring, not from a callsite sweep.
- Have not read every limit's actual call site to confirm "recompile-to-tune" applies uniformly. ADR-021 §2b suggests configuration-surface plumbing was contemplated but deferred.
- The `clarion-llm` outbound-HTTP threat model assumes the crate ships TLS — not verified.

## Caveats

- This is a STRIDE-shaped read, not a full threat model with attack trees. The Q2 monolith risk in particular deserves its own attack tree (root: "plugin escapes supervisor invariants via refactor regression").
- "Defense-in-depth" arguments are inherently judgment calls; a smaller team may reasonably prefer the operational simplicity of one HTTP stack over the security win of splitting `clarion-llm`.

## Relevant paths

- `/home/john/clarion/crates/clarion-core/src/plugin/jail.rs` (TOCTOU documented at lines 67–72)
- `/home/john/clarion/crates/clarion-core/src/plugin/limits.rs` (constants at lines 71, 142, 209–211, 261, 281, 289)
- `/home/john/clarion/crates/clarion-core/src/plugin/host.rs` (the 2935 LOC monolith — Q2)
- `/home/john/clarion/crates/clarion-cli/src/http_read.rs` (HMAC > bearer > loopback chain at lines 392–419; unauthenticated `_capabilities` at line 372; panic-on-unenumerated middleware error at lines 547–557; loopback-no-token warning at lines 199–252)
