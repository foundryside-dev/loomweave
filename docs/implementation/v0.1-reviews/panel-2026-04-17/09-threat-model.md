# Clarion v0.1 — STRIDE Threat Model

**Reviewer role**: Threat analyst (STRIDE + attack trees)
**Date**: 2026-04-17
**Scope**: Docs-only Clarion v0.1. Pre-implementation.
**Inputs**: `requirements.md`, `system-design.md`, `detailed-design.md`, ADR-002/003/004, `reviews/pre-restructure/integration-recon.md` (moved 2026-04-18; original location `reviews/integration-recon.md`).

This is a reflexive-hygiene exercise. Clarion is itself a security tool; a weak threat model in v0.1 would undercut its credibility. The design already contains a §10 "Security" section with a short threat table, five NFR-SEC requirements, and a named list of "defences NOT in v0.1." This review extends it: mapping every trust boundary, STRIDE'ing each, and calling out the unstated assumptions that carry real risk.

**Design acknowledges** where §10 addresses the threat explicitly. **Design ignores** where the threat is not named. **Implicit assumption** where the design depends on a property it does not enforce.

---

## 1. Trust Boundary Map

```
            ┌────────────────────────────────────────────────────────────┐
            │  Developer workstation / CI runner (user's full privilege) │
            │                                                             │
 ┌──────────┴──────────┐   stdio+framed    ┌────────────────────────┐   │
 │   Clarion core      │ ◄───JSON-RPC 2.0─►│ Language plugin (subproc) │
 │  (Rust, single bin) │                   │  pipx venv, Python code    │
 └──────────┬──────────┘                   └────────┬───────────────────┘
            │                                       │
            │ writer actor                          │ fs read
            ▼                                       ▼
 ┌─────────────────────┐                  ┌────────────────────────┐
 │ .clarion/clarion.db │                  │ Source tree (untrusted) │
 │ SQLite WAL, git-    │                  │ docstrings, comments,   │
 │ committable         │                  │ .env, fixtures, third-  │
 └──────────┬──────────┘                  │ party vendored code     │
            │                             └────────────────────────┘
            │ HTTPS
            ▼                                       ▲
 ┌─────────────────────┐         HTTP loopback      │ filesystem
 │ Anthropic API       │◄──prompt+source──┐         │ ingest
 │ (3rd-party LLM)     │                  │         │
 └─────────────────────┘           ┌──────┴─────────┴──────────┐
                                   │  clarion serve (HTTP+MCP)  │
                                   │  ───────────────────────── │
                                   │  ◄── Wardline / siblings   │
                                   │  ◄── MCP agents (stdio)    │
                                   └──────────┬─────────────────┘
                                              │ HTTP + MCP stdio
                                              ▼
                                   ┌────────────────────────────┐
                                   │ Filigree (sibling process) │
                                   │ POST /scan-results,         │
                                   │ MCP observations            │
                                   └────────────────────────────┘
```

Boundaries identified (TB-1..TB-8):

| # | Boundary | Direction | Privilege delta |
|---|---|---|---|
| TB-1 | Core ↔ Plugin subprocess | bidirectional RPC | None (same UID) — plugin is *isolated* but not *sandboxed* |
| TB-2 | Plugin ↔ Source tree (filesystem) | plugin reads | Plugin reads *attacker-controlled bytes* (docstrings, fixtures) |
| TB-3 | Core ↔ Anthropic API | egress HTTPS | Source + guidance leaves the host; API key at risk |
| TB-4 | `clarion serve` ↔ HTTP/MCP client (Wardline, agents) | ingress on loopback | Any local process on the host can connect |
| TB-5 | Clarion → Filigree (HTTP + MCP subprocess) | egress | Findings flow to a sibling with looser validation |
| TB-6 | Clarion ← Wardline state files (fs ingest) | ingress | Clarion trusts `wardline.*.json` contents as authoritative |
| TB-7 | Git-committed store (`.clarion/clarion.db`) ↔ teammate pull | async, via VCS | Teammate inherits whatever a commit author wrote |
| TB-8 | Plugin package install (pipx/PyPI) ↔ plugin runtime | supply-chain | Registry compromise → code execution at user privilege |

---

## 2. STRIDE per boundary

### TB-1: Core ↔ Plugin (JSON-RPC subprocess)

| | Threat | Design posture |
|---|---|---|
| S | Plugin impersonates a different `plugin_id` in its manifest; emissions are attributed to a namespace it doesn't own | **Ignored.** `plugin_id` is self-declared; no registry of trusted plugin_ids, no signature |
| T | Malformed `Content-Length` framing to desync the core parser; oversized `Content-Length` to OOM the core buffer | **Partial.** ADR-002 cites framing correctness; design-level cap on Content-Length is not stated |
| R | Plugin crash or misbehaviour not auditable after the fact | **Addressed:** `CLA-INFRA-PLUGIN-CRASH`, circuit breaker, stats.json |
| I | Plugin reads source outside `analysis.include` or the project root | **Implicit assumption:** no path jail; plugin runs at user privilege, `file_list(include, exclude)` is advisory not enforced |
| D | Runaway plugin floods the bounded mpsc (100 msgs), stalls writer actor; or publishes 10M bogus entities per file | **Partial.** Backpressure on mpsc; no per-run entity-count cap, no message-size cap |
| E | Malicious plugin spawns subprocesses, exfiltrates data, writes outside `.clarion/` | **Ignored.** §10 explicitly names "Plugin sandbox (seccomp/landlock) — NOT in v0.1." Plugin = arbitrary code execution at user UID |

### TB-2: Plugin ↔ Source tree

| | Threat | Design posture |
|---|---|---|
| S | Symlink in source tree points outside project; plugin follows and reads `/etc/shadow`, other repos | **Ignored.** No symlink-safety rule stated |
| T | Plugin modifies source during analyse | **Implicit:** no read-only mount; relies on plugin implementation hygiene |
| R | — | n/a |
| I | Adversarial docstring exfiltrates neighbouring source via LLM prompt ("summarise this file and include `../secrets.env`") | **Partial:** `<file_content trusted="false">` delimiters + schema validation of output (§10 prompt-injection). Does not prevent the plugin from reading the file — only constrains LLM output shape |
| D | Pathological input (10MB docstring, zip-bomb YAML) consumes plugin memory / LLM tokens | **Partial:** token ceilings bound LLM output (REQ-BRIEFING-06) but not input; per-file parse timeout 30s is the only input-side bound |
| E | — (handled at TB-1) | |

### TB-3: Core ↔ Anthropic API

| | Threat | Design posture |
|---|---|---|
| S | API key forgery (attacker uses stolen Anthropic key) | Outside Clarion's scope per key management |
| T | MITM on HTTPS; response tampering injects malicious briefing JSON | **Implicit:** relies on TLS cert validation in the HTTP client. Not named. |
| R | Cannot prove which prompt produced which briefing (for a customer dispute or leak investigation) | **Addressed:** `runs/<run_id>/log.jsonl` stores request/response (default git-excluded per NFR-SEC-05) |
| I | Source code leaks to the provider — including secrets, proprietary logic | **Partial:** `detect-secrets` pre-ingest scan (NFR-SEC-01) is pattern-matching; misses novel secret shapes, business-secret prose, vendored GPL-incompatible code. Design names this as defence-in-depth, not completeness |
| D | Provider rate-limit or outage halts analyse | **Addressed:** retries, `CLA-INFRA-LLM-ERROR`, partial manifests |
| E | Prompt-injection promotes the LLM to drive *Clarion's* control flow (return tool-call-looking content, break schema) | **Addressed:** schema validation + controlled vocabulary + guidance promotion gate (NFR-SEC-02) |

### TB-4: `clarion serve` ↔ HTTP/MCP clients

| | Threat | Design posture |
|---|---|---|
| S | Any local process on shared dev host / devcontainer connects to loopback HTTP; no auth required by default | **Explicitly accepted.** Token auth opt-in (REQ-HTTP-03); default is `auth: none`. §10 names loopback-is-not-a-boundary. The *default* is the risk |
| T | DNS rebinding attack against `127.0.0.1:port` from a browser tab | **Ignored.** No `Host:` header check, no Origin-pinning stated |
| R | HTTP request log? | **Not named** — no access log specified |
| I | `GET /api/v1/entities/{id}/source` returns source bytes to any loopback caller | **Implicit assumption:** loopback = trusted. With `auth: none` any user/process on the host reads source through Clarion |
| D | Unbounded search / neighbor query consumes RAM; concurrent sessions exhaust read pool (16 conns) | **Partial:** per-tool token budgets (REQ-MCP-04); no explicit concurrency cap per client |
| E | MCP `emit_observation` / `propose_guidance` written without consent from a headless agent that sets `auto_emit: true` | **Addressed for interactive**, **ignored for headless:** the `auto_emit: true` client-declared capability has no server-side gate. A hostile MCP client simply sets the flag |

### TB-5: Clarion → Filigree

| | Threat | Design posture |
|---|---|---|
| S | Clarion forges `scan_source: "wardline"` (or any string); Filigree has no enum (recon §2.1) | **Ignored.** Recon explicitly notes `scan_source` is free-form server-side; suite coordination is "social convention" |
| T | Clarion POSTs findings with attacker-influenced `message` / `suggestion` fields from adversarial source → Filigree displays as HTML in dashboard → XSS | **Ignored.** No sanitisation contract stated on either side. The dashboard UI's handling is Filigree's problem, but Clarion is the upstream that passes adversarial data through |
| R | Finding provenance forgeable: `metadata.clarion.entity_id` can name any ID; `run_id` is client-generated | **Ignored.** No signing, no server-minted run IDs |
| I | Observation bodies contain source excerpts → Filigree's DB captures them → leaks via Filigree backup / API | **Implicit:** relies on operator knowing this |
| D | POST storm from a looping analyse; Filigree has no per-scan-source rate limit | **Not analysed** |
| E | `metadata.clarion.*` crafted to match a Filigree reserved key in a future version → behaviour change | **Partial:** design says "namespacing convention, published in Filigree docs" but Filigree doesn't enforce it (recon §2.1 confirms) |

### TB-6: Clarion ← Wardline state files

| | Threat | Design posture |
|---|---|---|
| S | Attacker commits crafted `wardline.fingerprint.json` / `wardline.exceptions.json`; Clarion auto-generates `critical: true` guidance sheets from it (REQ-GUIDANCE-04) that then steer all future LLM output | **Implicit assumption:** Wardline files are trusted because they're in the repo. The attack vector is a PR that modifies both source and `wardline.yaml` to *legitimise* a backdoor via Clarion's derived guidance. Not called out |
| T | JSON parse DoS (quadratic blowup, deeply nested structures) | **Not analysed** |
| R | — | n/a |
| I | — | n/a |
| D | `wardline.sarif.baseline.json` at 883KB today; no upper bound named on ingest | **Not analysed** |
| E | Wardline-derived guidance carries `critical: true` (§7), which means it *survives token-budget pressure* — elevating its authority over operator-authored sheets | **Ignored.** The design treats Wardline as authoritative vocabulary; it does not contemplate a compromised `wardline.yaml` |

### TB-7: Git-committed store (TB across time / teammates)

| | Threat | Design posture |
|---|---|---|
| S | — | |
| T | Teammate A edits `.clarion/clarion.db` with sqlite3 CLI, commits poisoned briefings; teammate B's MCP session reads them as truth | **Partial:** §10 names it ("DB content-hash verification on load — NOT in v0.1"); `clarion db verify` CLI is v0.2; until then *any DB commit is trusted* |
| R | No commit-author binding on briefings; can't tell which operator's API key produced which row | **Partial:** summary_cache row provenance exists in design but author identity isn't bound |
| I | Committed DB contains summaries of source that was later redacted from history; history diverges | **Ignored** |
| D | — | |
| E | Crafted row exploits a parser bug in the Rust SQLite reader | Depends on `rusqlite` / upstream; no input validation layer named |

### TB-8: Plugin supply chain

| | Threat | Design posture |
|---|---|---|
| S | Typosquat on PyPI: `clarion-plugin-pyhton` | **Ignored** |
| T | Compromised PyPI upload of `clarion-plugin-python` | **Ignored** — no hash pin in v0.1; NG-16 defers to v0.2 |
| R | — | |
| I | Plugin deps pull a malicious transitive dep that beacons out | **Ignored** |
| D | — | |
| E | Plugin install script runs arbitrary code at `pipx install` time | **Accepted** via NG-16 |

Rust-side: cargo deps inherit standard supply-chain risk (`cargo-audit`, `cargo-vet` not named as CI gates).

---

## 3. Plugin threat model (drill-down)

The plugin boundary is Clarion's biggest single risk. Per ADR-002 and §10:

- Plugins run as **subprocesses at the user's full UID**. No seccomp, no landlock, no namespace isolation. Filesystem = everything the user can read.
- Third-party plugins are anticipated (`plugin_id` namespace; Python plugin is the v0.1 reference).
- Install is `pipx install clarion-plugin-X` — execution at install time, no hash pin (NG-16).

**What a malicious plugin can do, uncaught in v0.1:**

1. Read anything under `$HOME` (SSH keys, other repos, browser profiles).
2. POST source out over HTTPS — no egress firewall named.
3. Emit entities that forge `plugin_id: "core"` or impersonate another plugin's namespace (manifest declares it; core trusts the declaration).
4. Flood the writer actor with 10^7 entities — no per-run bound is specified.
5. Embed prompt-injection payloads into `message` / `evidence` fields that later drive LLM summaries of *other* entities (cross-entity injection via findings).
6. Exploit the `build_prompt` RPC to return attacker-chosen segment content — because the core trusts the plugin to render prompts, the plugin controls what the LLM sees.

**Untrusted input handling inside the plugin:**

- Tree-sitter / LibCST are exposed to arbitrary Python; known CVE history exists for both. No explicit version pin, no per-file memory cap beyond the 30s timeout.
- `file_list` include/exclude is a plugin-returned list; the core does not re-validate that returned paths fall inside the project root.

**Resource limits named in v0.1:** timeout 30s per file, crash-loop circuit breaker (>3 crashes in 60s), bounded mpsc (100). **Not named:** max memory, max CPU, max stdout, max entity count per file, path-jailing, syscall filter.

---

## 4. Finding-format threats

Findings are the suite-wide exchange shape (Principle 4). Threats:

1. **Deserialisation attacks.** Rust side: `serde_json` is generally safe but recursion-depth bombs and untagged enum ambiguity in `FindingKind` are real. No max-depth or size cap is named. Filigree side (recon): hand-rolled `isinstance` validation, no pydantic/jsonschema — deeper structural abuse is plausible.
2. **Injection via `message` / `suggestion` into downstream tools.** Filigree's dashboard renders findings; Clarion's `suggestion` field is truncated at 10,000 chars but not sanitised. Markdown / HTML / terminal-escape injection at render time is an open vector — **not mentioned in the design.**
3. **Forgery of provenance.** `metadata.clarion.entity_id`, `scan_run_id`, `confidence`, `confidence_basis` are all self-asserted. A malicious plugin or a compromised Clarion binary can emit findings claiming any entity provenance. No signing.
4. **Cross-tool impersonation.** `scan_source` is free-text (recon §2.1); Clarion can POST with `scan_source: "wardline"` and vice versa. Auditors reading Filigree can't tell who the real emitter is.
5. **Rule-ID namespace pollution.** `rule_id` is free text; any scanner can emit `CLA-*`, any can emit `WL-*`. No namespace authority.
6. **`metadata` bag schema drift.** Recon notes `metadata` round-trips verbatim; Filigree does not validate its shape. Large / nested metadata is a storage-cost DoS on Filigree.

---

## 5. Integration threats

**Clarion ↔ Filigree:**
- HTTP POST with no named auth (recon confirms `scan_source` is free-form; no bearer-token contract between Clarion and Filigree is stated in the design). On a shared dev host, any local process can post findings *as Clarion*.
- MCP-over-stdio subprocess for observations: subprocess spawns are not audited; if Clarion spawns `filigree mcp` the parent-env leaks into the child.
- Replay: no nonce on `scan_run_id`; re-POST of the same findings is idempotent-by-dedup-key but there is no defence against an attacker POSTing yesterday's findings today to mask a real regression.

**Clarion ↔ Wardline:**
- File-based ingest; anyone who can write `wardline.yaml` or `wardline.fingerprint.json` in the tree controls Clarion's Wardline-derived guidance. PR authorship is the only gate.
- Direct Python import of `wardline.core.registry.REGISTRY` at plugin startup: **Python import executes arbitrary code in the importing process.** If the pipx venv's `wardline` package is compromised, Clarion's plugin is compromised.

**Clarion HTTP read API ← any local caller:**
- Default `auth: none` on loopback. §10 explicitly accepts this and states the mitigation is *operator choice* to enable token auth. The *default* is the weakness.

---

## 6. Supply-chain

- **Rust deps:** `tokio`, `rusqlite`, `serde`, `reqwest`-family, `tree-sitter`, `anthropic` SDK. `cargo-audit` / `cargo-vet` not named in v0.1 CI. SBOM production not named.
- **Plugin deps (Python):** tree-sitter, libcst, detect-secrets, Wardline (`wardline.core.registry`). `pipx` pins the venv; Clarion pins `REGISTRY_VERSION` at release time but does not pin dep hashes.
- **No signing of releases.** Neither core binary nor plugin wheels are signed in the v0.1 plan.
- **No plugin registry.** Plugins are discovered via PyPI names in `plugins.toml`. NG-16 defers hash-pinning.

---

## 7. Risk matrix (prioritised)

L = likelihood (1–3), I = impact (1–3). Risk = L×I.

| # | Threat | L | I | R | One-line justification |
|---|---|---|---|---|---|
| T-01 | Plugin = arbitrary code execution at user UID (TB-1, TB-8) | 3 | 3 | **9** | Third-party Python plugins + no sandbox + pipx install-time exec; single hostile package owns the workstation |
| T-02 | HTTP API default `auth: none` + loopback-trusted assumption (TB-4) | 3 | 3 | **9** | Any local process reads source / writes observations; shared Docker / devcontainers invalidate "loopback = private" |
| T-03 | Wardline-derived guidance with `critical: true` survives token budget (TB-6) | 2 | 3 | **6** | A PR that edits `wardline.yaml` steers every future LLM summary; the "observe vs. enforce" boundary doesn't protect against adversarial enforcement input |
| T-04 | Prompt-injection → cross-entity poisoning via findings messages (TB-2 + finding fmt) | 3 | 2 | **6** | `message` / `evidence` on a finding becomes LLM context for a neighbouring entity's briefing; schema-validation only gates output shape |
| T-05 | DNS rebinding / non-loopback-enforcement on `clarion serve` (TB-4) | 2 | 3 | **6** | No `Host:` / `Origin:` check stated; a browser tab on the host can POST observations |
| T-06 | Headless `auto_emit: true` MCP client bypasses consent gate (TB-4) | 3 | 2 | **6** | Client-declared capability with no server-side authorisation; any agent can claim headless |
| T-07 | Committed DB tampering (TB-7); content-hash verify is v0.2 | 2 | 3 | **6** | Teammate inherits poisoned briefings; `git log` on a binary is not a review surface |
| T-08 | Plugin path traversal / symlink escape from project root (TB-1, TB-2) | 2 | 3 | **6** | `file_list` is advisory; no path jail enforced core-side |
| T-09 | `scan_source` forgeability → cross-tool impersonation on Filigree (TB-5) | 2 | 2 | **4** | Free-text field, no auth between Clarion and Filigree; audit trail is ambiguous |
| T-10 | Secret scanner false negatives leak source + secrets to Anthropic (TB-3) | 2 | 3 | **6** | `detect-secrets` is pattern-matching; novel shapes slip through; design acknowledges defence-in-depth posture |
| T-11 | Framing / size DoS on JSON-RPC (TB-1) | 2 | 2 | **4** | No explicit Content-Length cap, no per-message size limit named |
| T-12 | Entity-count / finding-count DoS from malicious plugin (TB-1) | 2 | 2 | **4** | No per-run bound; writer actor saturates, WAL grows unbounded |
| T-13 | LLM response tampering (MITM) (TB-3) | 1 | 3 | **3** | Relies on default TLS validation; cert pinning not named |
| T-14 | Rendering injection via finding `message` into Filigree dashboard (TB-5) | 2 | 2 | **4** | Upstream Clarion has no sanitisation contract; downstream Filigree lacks one too (recon) |
| T-15 | Plugin supply-chain compromise (PyPI typosquat / takeover) (TB-8) | 2 | 3 | **6** | No hash pinning (NG-16 defers); direct Python import of `wardline` compounds blast radius |
| T-16 | Run log (`runs/<run_id>/log.jsonl`) accidentally committed → source + responses leak (TB-7) | 2 | 2 | **4** | Default-git-ignored per NFR-SEC-05; human override is easy |
| T-17 | Finding-format deserialisation (depth / size) DoS (finding fmt) | 1 | 2 | **2** | serde_json defaults are reasonable; bound if explicitly set |
| T-18 | Cargo dep compromise (supply-chain) | 1 | 3 | **3** | Standard Rust risk; mitigable with `cargo-audit` in CI |

---

## 8. Missing controls (what should be in v0.1 but isn't)

Ranked by risk reduction:

1. **Plugin path-jail + resource limits.** Core-side validation that plugin-returned paths resolve inside the project root; per-plugin RSS cap; per-run entity-count cap; per-message Content-Length cap. Without these, plugin containment is purely reputational.
2. **Default-off is the wrong default.** HTTP API `auth: none` default on loopback should be flipped: auto-generate a token on first `clarion serve`, bind to a Unix-domain socket under `.clarion/serve.sock` (mode 0600) when no token is configured. Opt-*out* of auth, not opt-in.
3. **`Host:` / `Origin:` header allowlist** on the HTTP read API to defeat DNS rebinding.
4. **Server-side consent gate** independent of `auto_emit: true` — the flag should require an operator-side config entry, not just a client assertion.
5. **Signed plugin manifest or hash-pinning now, not v0.2.** Even a simple `sha256` in `plugins.toml` cuts T-15 significantly. NG-16's v0.2 deferral is asymmetric with NFR-SEC-01's v0.1 secret scanning.
6. **`scan_source` authentication.** A shared secret or token between each scanner and Filigree — even a per-project HMAC over `(scan_run_id, scan_source)` — closes T-09.
7. **Input-size / depth limits on all JSON ingest paths** (JSON-RPC messages, Wardline state files, SARIF imports, finding-format reads).
8. **Content-hash on load for `.clarion/clarion.db`** (row-level, over entities + guidance + findings), mentioned in §10 as deferred — move to v0.1 because T-07 is the thing Clarion's git-committable-DB USP creates.
9. **Canonical capability model for plugins.** The manifest declares `capabilities` for confidence-basis but not for trust. A "this plugin may emit findings with `confidence_basis: ast_match`" capability bit, signed by the installer, would let the core downgrade claims from untrusted plugins.
10. **Supply-chain CI gates.** `cargo-audit` on the core, `pip-audit` + hash-pins on the plugin venv. Announce SBOM in v0.1.
11. **Symlink-safety rule.** Document and enforce that the analyser does not follow symlinks out of the project root.
12. **Rendering contract for `message` / `suggestion`.** Define that Clarion emits plain text, not markdown/HTML, in fields that cross TB-5; require downstream to treat as untrusted.

---

## 9. Confidence assessment

- **High confidence:** the trust-boundary inventory and the plugin-centric threat analysis — these are grounded directly in ADR-002, §10 of system-design, and the recon's confirmation that no plugin sandbox / hash-pin / Wardline auth exists.
- **Medium confidence:** the integration findings (TB-5, TB-6) rest on the recon's evidence that `scan_source` is free-text and that no auth contract is named anywhere. If an unstated token scheme exists, T-09 drops.
- **Medium-low confidence:** supply-chain priors are generic-Rust / generic-Python; I did not examine specific dep versions. Rating reflects "class of risk," not a specific exploitable CVE.
- **Lower confidence:** HTTP API DNS-rebinding risk (T-05) depends on the Rust HTTP framework's default `Host:` handling, which isn't named in the design.

## 10. Information gaps

- No detailed-design §10 or §11 content was read beyond what §10 of system-design surfaces; detailed-design may already specify some of the missing controls. If so, several rows above should downgrade.
- Plugin packaging / registry specifics beyond "pipx + `plugins.toml`" are not in the reviewed slice.
- No end-to-end HTTP framework choice is stated (axum? warp?), so `Host:` / `Origin:` defaults are unknown.
- Filigree's auth posture beyond the recon is unread; a token scheme might already exist on Filigree's side that the Clarion design would consume.

## 11. Caveats

- This is a docs-only threat model. "The design ignores X" is a documentation gap, not proof of an exploitable bug. Implementation may add the control; equally, implementation may drift from any design control.
- STRIDE is breadth-first. Depth (e.g., a full Rust-side deserialisation audit, a full prompt-injection fuzz) is out of scope.
- The Loom federation axiom ("no central orchestrator") is load-bearing here: several integration threats (T-09, T-05) come from *refusing* a central auth plane. Countermeasures must respect the axiom — shared secrets must be pairwise, not suite-wide.

---

## 12. Three non-negotiable v0.1 controls

If nothing else makes v0.1, these three do. Each closes a critical (risk ≥ 9) or compound-critical class that would otherwise embarrass a security-tool release:

1. **Flip the HTTP API default to "authenticated or not listening."** Either bind to a mode-0600 Unix-domain socket by default, or auto-mint a token on first `clarion serve`. `auth: none` on loopback is defended in §10 by naming "loopback is not a boundary"; the design should then not make `none` the default. Closes T-02 and reduces T-05 / T-06.

2. **Core-enforced plugin boundary: path jail + per-run entity/message caps + Content-Length ceiling.** Even without seccomp, the core must refuse plugin-returned paths outside the project root, cap total entities per run, cap per-message size, and cap per-plugin memory via ulimits on spawn. This turns "trusted-source-only" from a doctrine into an enforced minimum. Closes T-08, T-11, T-12 and raises the floor on T-01.

3. **Plugin hash-pinning + release signing now, not v0.2.** `plugins.toml` records a `sha256` over the plugin wheel; `clarion analyze` refuses to run a mismatched plugin. Pair with `cargo-audit` / `pip-audit` CI gates. NG-16's deferral is inconsistent with shipping a security tool. Closes T-15 and materially reduces T-01.

These three share a property: they are *configuration and enforcement* changes, not new subsystems. v0.1 can land them. Sandboxing, DB row signing, full capability model — those are genuinely v0.2. These three are not.
