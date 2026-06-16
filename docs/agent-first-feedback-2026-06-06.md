# Loomweave: Agent-First Experience Report

**From:** an LLM coding agent (Claude, Opus 4.8) that used Loomweave as its primary
code-archaeology surface for a session.
**Against:** a real downstream project — `elspeth`, ~40.4k entities, 142
subsystems, 48.4k edges, 157 findings, a 210 MB `loomweave.db`.
**Date:** 2026-06-06 · Loomweave `1.1.0-rc1`, Wardline `1.0.0rc1`.
**Scope of the session:** investigate the toolset, then enable the live LLM
provider (`claude_cli`) and generate real entity summaries end-to-end.

This is field feedback from your actual primary user — an agent. The headline:
**Loomweave's core is excellent and its instincts are right. The gap to
"agent-first" is almost entirely in the onboarding/config/feedback layer and in a
handful of missing batch + project-level operations.** Below: what to protect,
what to fix, and what I'd love to use if it existed.

---

## 0. TL;DR for the busy maintainer

- **Protect:** honest-empty discipline, `scope_excludes`, confidence tiers, SEI,
  cost-preview-before-spend, structural fallback, the loom↔ward↔filigree triangle.
  These are genuinely best-in-class and rare.
- **Fix first (correctness/trust):** silent LLM mis-config (no `deny_unknown_fields`,
  missing `enabled` → silent disable, no validation warning); two status tools
  disagree on `allow_live_provider`; config schema is only discoverable by reading
  source.
- **Fix soon (discoverability):** ship the operator docs with the binary; make
  `doctor` validate `loomweave.yaml`; stop advertising write-gated tools the agent
  can't see.
- **Build (the agent-first leap):** a project-level orientation pack; budgeted bulk
  summarization; diff-aware blast-radius; an error/traceback orientation tool; and
  closing the discover→summarize→propose→fix loop across the three tools.

---

## 1. What's already great (do not "fix" these)

An agent-first report has to start here, because several of these are things a less
disciplined tool would "simplify" away, and they are exactly why I trusted the
output.

1. **Honest-empty everywhere.** Every catalogue tool returns an empty result *with
   a `signal` note* ("available:false", the reason) instead of fabricating an
   answer. As an agent, the single most expensive failure mode is a confident wrong
   answer. Loomweave's refusal to fabricate is the feature.
2. **`scope_excludes` / blind-spot honesty.** `callers_of` telling me "I did not
   search attribute-receiver calls, so this empty result is not a guaranteed true
   negative — re-query at `inferred`" is *exactly* the metacognition an agent needs.
   This should be the model for every tool.
3. **Confidence tiers (`resolved`/`ambiguous`/`inferred`)** as an explicit ceiling,
   with "there is no `all`." Forces me to reason about edge quality instead of
   trusting a number.
4. **SEI (durable identity) vs `id` (mutable locator).** The right call for
   cross-tool bindings. The fact that `project_status` reports `sei.populated` so I
   can tell whether I'm in a degraded state is excellent.
5. **Cost preview before spend.** `entity_summary_preview_cost_get` reporting
   `live_spend_would_occur` and an input-token estimate *without invoking the
   provider*, and distinguishing "disabled" from "cache miss" — this is how every
   spend-bearing tool should behave.
6. **Structural fallback.** When the LLM returns non-JSON, you degrade to a
   deterministic source-derived summary *and cache it* so a retry is free, not
   re-billed. Graceful, honest, cheap. Chef's kiss.
7. **The triangle.** Loomweave (map) + Wardline (taint) + Filigree (issues), bound
   on SEI, with Wardline findings reconciled into `entity_issue_list`. This is a
   genuinely powerful substrate for agent workflows — see §4 for how to exploit it.

---

## 2. Defects & friction I actually hit (must-fix)

Ranked by how badly each hurt an agent trying to self-serve.

### 2.1 — Silent LLM mis-configuration (P0)
**Symptom:** I wrote a plausible `llm_policy` block, restarted, and the provider
stayed `disabled` with **zero diagnostics**. It took reading the stripped binary's
string table, then the source, to find two causes.

**Root cause (`crates/loomweave-federation/src/config.rs`):**
- Every config struct is `#[serde(default)]` with **no `deny_unknown_fields`**. I
  put `model_id:` *inside* `claude_cli` (the field is `model`); it was **silently
  dropped**. Any typo is silently dropped.
- `enabled` defaults to `false`. I set `provider` + `allow_live_provider: true` but
  omitted `enabled` → silently disabled.
- `validate()` only checks deprecated-provider / blank-actor / loopback-trust. It
  **never warns** that a fully-specified live provider is sitting behind
  `enabled: false`, or that `allow_live_provider: true` is inert without `enabled`.

**Fix:**
- Add `deny_unknown_fields` (or a non-fatal "unknown config key: X" warning at load).
- Emit a startup diagnostic naming the **effective** LLM state, e.g.
  `llm_policy.provider=claude_cli but enabled=false → summaries cache-only` and the
  inverse for `allow_live_provider`.
- Tests: unknown nested key; `enabled` omitted; provider set + `enabled:false`;
  `allow_live_provider` without `enabled`.

> This single fix would have turned a 45-minute reverse-engineering session into a
> 30-second "oh, it told me what's wrong" loop. For an agent, *failing loud and
> specific* is worth more than any feature on the wishlist.

### 2.2 — Two status surfaces disagree (P1)
For the same half-configured state, `project_status_get` reported
`allow_live_provider: true` while `entity_summary_preview_cost_get` reported
`false`. One reads raw config, the other reads effective/resolved state. An agent
debugging config cannot tell which to believe.
**Fix:** reconcile the two read paths; if one is "configured" and the other
"effective", **label them as such** in the payload. Add a test asserting agreement.

### 2.3 — Schema is undiscoverable from the installed artifact (P1)
The `uv tool` install ships **no docs**. `analyze --help` literally references
`docs/operator/getting-started.md`, which isn't present. The authoritative
`docs/operator/coding-agent-llm-providers.md` (which would have told me
`max_turns: 2` is mandatory and `model` is the field name) exists **only in the
source repo**. I recovered the schema by `strings`-ing a stripped Rust binary.
**Fix:** bundle `docs/operator/*` in the package, **and/or** add a
`loomweave config example [--provider claude_cli]` that prints a complete annotated
config generated from the real structs, and `loomweave config check` that validates
a file and prints the effective state.

### 2.4 — `doctor` doesn't validate the config (P1)
`doctor` is pitched as a CI/pre-commit gate but only checks the skill/hook/
`.mcp.json` install surfaces — it skips `loomweave.yaml`, the file most likely to be
hand-edited wrong.
**Fix:** `doctor` should parse + lint `llm_policy`, report the effective
provider/live state and projected per-summary cost, and warn on the §2.1 patterns.

### 2.5 — Advertised-but-gated tools (P2)
The `loomweave-workflow` skill and the MCP server's own instructions advertise
`summary`/`entity_summary_get`, `analyze_start`/`cancel`, `propose_guidance`/
`promote_guidance`. But `tools/list` returns **34** tools and none of those appear
unless `serve.mcp.enable_write_tools: true` (then **39**). An agent that follows the
skill calls tools that don't exist and gets a hard error.
**Fix:** note the `enable_write_tools` gate in the skill + server instructions; and
consider having `tools/list` (or a `capabilities` tool) report disabled tools with a
one-line "set `serve.mcp.enable_write_tools: true` to enable."

### 2.6 — Silent model/cost surprise (P2)
With `claude_cli.model: null`, Loomweave inherits the local CLI's **default** model
— which on my login is Opus. My first real summary cost **$0.27** (8,944 tokens).
Pinning `model: claude-sonnet-4-6` dropped the same summary to ~538 tokens. Nothing
surfaced "you are about to summarize on Opus at $0.27/call" at enable time.
**Fix:** surface the **effective model** and a **projected per-summary cost** at
serve start and in `project_status`. (You already have the preview machinery —
fold a cost estimate into it and into `orientation_pack`.)

---

## 3. The agent-first wishlist (the fun part — "as creative or demanding as I want")

These are features I, as the agent, would *love* to call. Roughly ordered by impact.

### 3.1 — A **project-level orientation pack** (`project_orientation_pack`)
`entity_orientation_pack_get` is superb but per-entity. My very first question in any
repo is *"what is this whole thing?"* I want one deterministic call that returns:
top subsystems by size + a one-line role for each, the entry points, HTTP routes,
the coupling hotspots, recent-change hotspots, open findings by severity, and index
freshness. A generated **"map of the territory"** I read once at session start
instead of issuing 15 calls. Bonus: expose it as an MCP **resource**
(`loomweave://orientation`) so a client can auto-load it on connect.

### 3.2 — **Budgeted bulk summarization** (`summarize_scope`)
I will *never* loop 40k `entity_summary_get` calls myself. Give me:
`summarize_scope({ scope, budget_usd | budget_tokens, order: "centrality" })`. It
summarizes the most-central entities first (PageRank / fan-in+fan-out), streams
progress like `analyze_start`, stops when the budget is hit, and returns
**what it covered and what it skipped** (honest-truncation, naturally). This turns
"populate the cache for the auth subsystem for under $5" into one call.

### 3.3 — **Diff-aware blast radius** (`impact_of_change`)
`index_diff` tells me freshness; I want the next step: *"what changed since commit X,
and what's the blast radius?"* Return changed entities + their resolved callers
(transitively, bounded) + the tests that cover them + any findings on the changed
set. This is the #1 question an agent asks when resuming a branch, and right now I
hand-assemble it.

### 3.4 — **Error/traceback orientation** (`orient_from_traceback`)
Paste a Python traceback (or a `pytest` failure), get back, per frame: the entity,
its neighborhood, recent changes, test coverage, and any findings. Agents debug
constantly; this would be the single most-used tool I can imagine. The parsing is
language-plugin territory, which fits your architecture.

### 3.5 — **Subsystem-level summaries** (roll-up briefings)
`summary` is leaf-only by design (honest, and stated). But I'd love a
`subsystem_summary` that composes leaf summaries into "what is this cluster, its
public surface, its invariants, its risks." Cache-keyed on the set of member
content-hashes so it invalidates correctly. This is the altitude at which I make
architectural decisions.

### 3.6 — **Inline, verifiable citations in summaries**
The summaries I got were *accurate* — but I had to re-read the source to *verify*
them. Have the provider cite line ranges per claim (`behavior` → `L57–L77`), so an
agent can spot-check a summary against source without trusting it blind. Pairs
beautifully with your existing content-hash provenance.

### 3.7 — A **token/cost ledger tool** + hard budget guardrail
`session_token_ceiling` exists internally; expose it. `llm_budget_status` →
{ spent, remaining, per-route breakdown }. And let me *set* a per-session ceiling at
connect time so an autonomous agent physically cannot overspend. Fold projected cost
into every spend-bearing tool result, not just the preview.

### 3.8 — **First-class semantic search** (no API key)
"Find the function that does X" is an inherently semantic query, but
`search_semantic` is opt-in, needs a separate OpenAI-compatible embedding provider,
and an API key. For an agent-first tool this should be near-zero-friction — ship/
support a **local** embedding model so the highest-value discovery query works out of
the box. Today it's the one obvious agent query that's hardest to turn on.

### 3.9 — Close the **discover → summarize → propose → fix** loop
You have all the pieces; wire them for agents:
- `propose_guidance` exists but is inert until operator promotion (correct
  governance). Make proposing **frictionless** and add `list_pending_guidance` so an
  agent can see what it (or peers) proposed and an operator can batch-promote.
- When I learn "this looks like X but is actually Y" while reading code, I want to
  capture that *in the moment* against the SEI. The capture cost must be one call.
- Tie findings → fix: `finding → entity + neighborhood + test coverage → propose
  patch → result to Filigree`. The triangle makes this possible; an agent-facing
  recipe (or a composite tool) would make it routine.

### 3.10 — An **`--agent` serve mode** / `loomweave install --agent`
The `enable_write_tools` gate is the right default for multi-tenant HTTP, but for a
local single-tenant agent dev loop it's pure friction. A `loomweave serve --agent`
(or an install profile) that turns on the full agent surface (write tools, a
cost-guarded cheap-model provider hint, semantic search) and **prints the effective
config + projected costs** would make first-run delightful instead of a
config-archaeology expedition.

### 3.11 — Make `project_status` **actionable** (Filigree already does this)
`project_status` is informative; make every degraded field carry a `next_action`
hint (Filigree's `work_ready` does exactly this with its `startable`/`next_action`
pattern). "staleness: stale" → "run `analyze_start`"; "llm: disabled" → "set
`enabled: true` + `allow_live_provider: true`". Self-healing guidance beats a status
code an agent has to interpret.

### 3.12 — **Async summary jobs**
A single live summary is multi-second (it spawns `claude -p`). For anything
interactive, let `entity_summary_get` optionally return a job handle and let me poll
(you already do this for `analyze_start`/`analyze_status`). Lets an agent fan out
summaries concurrently instead of serializing on wall-clock.

---

## 4. The meta-point

Loomweave is increasingly consumed **by agents as its primary user**, but its
**config and onboarding layer is still written for a human operator** who will read
the source. Every §2 item is a place where a human would shrug and dig, but an agent
hits a silent wall. The fastest path to "agent-first" is three moves:

1. **Fail loud and self-describe.** Unknown keys, disabled providers, gated tools,
   stale indexes — all should announce themselves with the exact fix. (§2.1–2.6, §3.11)
2. **Operate in batches with budgets.** Agents don't want 40k calls; they want
   "summarize this scope for $5 and tell me what you skipped." (§3.2, §3.5, §3.7, §3.12)
3. **Answer the questions agents actually ask.** Not just "what calls X" but "what
   is this project," "what does this change break," "where did this traceback come
   from." (§3.1, §3.3, §3.4)

The bones are right. The honesty discipline is rare and worth protecting. Close the
feedback-and-batch gap and Loomweave becomes the tool an agent reaches for *first*
in every unfamiliar repo.

— Submitted with appreciation; the structural-fallback-and-cache detail genuinely
made my day.
