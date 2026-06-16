# Plan: inject Loomweave agent-orientation guidance into CLAUDE.md / AGENTS.md

Status: proposal / sketch (2026-06-06)
Scope: `loomweave install` (write) + `loomweave doctor [--fix]` (verify/repair)

## Problem

An agent that opens a Loomweave-indexed repo only learns how to use Loomweave
by *pulling* a surface: the MCP server's instructions preamble, or the
`loomweave-workflow` skill. Neither is in the always-loaded `CLAUDE.md` /
`AGENTS.md` context. Filigree already solves the same problem by *pushing* a
managed marker-block into those files (`inject_instructions`,
`src/filigree/install.py`). Loomweave does not — confirmed: a full grep of the
source for `CLAUDE.md`/`AGENTS.md` returns zero hits; the marker blocks in this
repo's own `CLAUDE.md`/`AGENTS.md` are Filigree's
(`<!-- filigree:instructions:… -->`).

This plan adds the equivalent for Loomweave, slotting into the existing
orientation-surface machinery rather than inventing new structure.

## Design (mirrors the established surface pattern)

Every orientation surface in `loomweave-cli` already follows one shape, and the
new surface adopts it verbatim:

| Surface | state query | idempotent installer (= `--fix` repair) | doctor checks |
|---|---|---|---|
| skill pack | `skill_pack::skill_pack_state` → `SkillPackState` | `install_skill_pack` | `check_skill` / `check_skill_json` |
| hook | `hooks_settings::…` → `HookState` | `install_session_start_hook` | `check_hook*` |
| MCP | `mcp_registration::…` → `McpState` | `install_mcp_entry` | `check_mcp*` |
| bindings | `integration_bindings::binding_state` → `BindingState` | `install_bindings` | `check_integration_bindings*` |
| **instructions (new)** | `instructions::instructions_state` → `InstructionsState` | `install_instructions` | `check_instructions*` |

### New module: `crates/loomweave-cli/src/instructions.rs`

Embeds one asset and manages a marker-block in two files at the project root:
`CLAUDE.md` and `AGENTS.md`.

```rust
// Embedded, cli-local (no MCP owner exists for this asset, unlike the skill).
const INSTRUCTIONS_BODY: &str =
    include_str!("../assets/instructions/loomweave.md");

// Loomweave's OWN marker namespace — must coexist with Filigree's block in the
// same file. Never collides with, reads, or edits `<!-- filigree:instructions -->`.
const START_PREFIX: &str = "<!-- loomweave:instructions";   // detection prefix
const END_MARKER:  &str  = "<!-- /loomweave:instructions -->";

// Provenance only (human-readable); NOT the drift signal. See "Drift" below.
fn start_marker() -> String {
    format!("<!-- loomweave:instructions:v{}:{} -->",
            env!("CARGO_PKG_VERSION"), body_hash_prefix())
}

const TARGET_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md"];
```

The rendered block is:

```
<!-- loomweave:instructions:v1.1.0-rc2:ab12cd34 -->
…INSTRUCTIONS_BODY…
<!-- /loomweave:instructions -->
```

## Five decisions baked in (do not regress these)

### 1. SAFE marker recovery — do NOT copy Filigree's truncate-to-EOF

Filigree's malformed-recovery (`install.py`: start marker present, end marker
missing → `content[:start] + INSTRUCTIONS`) **truncates from the start marker
to EOF**. That is only safe when the tool owns the tail of the file. Loomweave
*never* owns the tail: this repo's `AGENTS.md` already holds Filigree's block at
lines 1–119, so every file Loomweave writes is a **two-block file**. Copying
Filigree's recovery verbatim would let a dangling Loomweave start marker eat
Filigree's block (and vice-versa).

Rule for `install_instructions`:

- **Replace** only when BOTH `START_PREFIX` and a following `END_MARKER` are
  found and well-ordered (`end > start`). Replace exactly that span; touch no
  byte outside it.
- **Append** when neither marker is present.
- **Dangling start marker** (start present, no following end): do **not**
  truncate to EOF. Treat as malformed → `install` strips only the orphaned
  marker line and re-appends a fresh well-formed block; `doctor` without
  `--fix` reports it as a **problem** (see #2). Never delete bytes outside the
  Loomweave span.
- Atomic write (temp + rename, preserve mode) and symlink rejection, matching
  Filigree's `_atomic_write_text` / `reject_symlink` and Loomweave's existing
  atomic-write convention.

Guard test (mandatory): inject into a file that already contains a Filigree
block, and assert **both blocks survive** create / append / replace / malformed
round-trips.

### 2. Severity = `integration_bindings` model, not `skill_pack` model

The how-to-use guidance is already delivered twice (MCP preamble + skill); the
CLAUDE.md block is a redundant always-on *push*. A project that omits it is
still first-class. Therefore:

- `Missing` → **warning** (surfaced, suggests `--fix`; does NOT fail the
  `doctor` gate).
- `Unparseable` / malformed / dangling-marker → **problem** (fails the gate;
  this is the "genuinely broken, needs a human" case, and it composes with #1 —
  we never auto-truncate an ambiguous block).
- `Drifted` → **problem** when `--fix` is absent, auto-repaired with `--fix`
  (parity with skill pack's drift handling; safe because the repair only
  rewrites Loomweave's own span).

> **User veto point.** This is the one product-judgment call. If we'd rather
> treat the block as a first-class surface (Missing = problem, gate fails),
> flip `Missing` to problem. Recommended: warning.

### 3. Drift signal = block-body content hash, not the marker version

If the marker version string were the drift signal, every workspace version
bump (`v1.1.0-rc2` → next) would make `doctor` report "drifted" on byte-for-byte
identical content. `skill_pack` already avoids this: its blake3 fingerprint is
the drift signal and the version is "informational only." Mirror it — compare
the **extracted block-body bytes** against `INSTRUCTIONS_BODY`; keep
`v{version}` in the marker as provenance only.

### 4. Concurrent session-start refresh race — accepted, not engineered around

If the session-start hook re-injects on every start (as Filigree does), two
sessions race read-modify-write on the same files. Steady-state this is
harmless: each tool's refresh is deterministic, so a lost write reproduces
identical bytes next session. The only corruption risk was the truncation in
#1, already removed. Decision: **do not re-inject from the session-start hook**;
injection happens on `install` and `doctor --fix` only. Note the race as
accepted; no cross-tool lock.

### 5. One flag, both files

Add a single `--instructions` component (match `skill_pack`'s one-flag-both-roots
ergonomics, not Filigree's two-flag `--claude-md`/`--agents-md` split). It
writes both `CLAUDE.md` and `AGENTS.md`.

## Content of the embedded asset (keep it THIN)

`crates/loomweave-cli/assets/instructions/loomweave.md` — deliberately shorter
than Filigree's ~120 lines, because it is always-loaded context competing with
the skill that says the same thing. Target ~15–25 lines: a pointer, not a
manual. Sketch:

```markdown
## Loomweave (code archaeology)

This repo is indexed by Loomweave. Before grepping or re-reading the tree to
answer "what calls X", "where is X defined", "what subsystem owns X", or "find
the thing that does Y" — ask Loomweave's MCP tools (`mcp__loomweave__*`):
`entity_find`, `entity_at`, `entity_callers_list`, `entity_neighborhood_get`,
`project_status_get`. Entity IDs are `{plugin}:{kind}:{qualified_name}`.

Index freshness and counts: `project_status_get` (or the `loomweave://context`
resource). If stale, run `loomweave analyze <path>`.

Full workflow: the `loomweave-workflow` skill.
```

(Final wording to track the MCP server instructions preamble so the two don't
drift apart in tone.)

## Wiring changes (exact insertion points)

1. **`cli.rs`** — add `InstallComponent::Instructions` and an `--instructions`
   flag to the `Install` subcommand args (alongside `--skills`/`--hooks`).
2. **`install.rs`**
   - `InstallPlan::Components` — add `instructions: bool` field.
   - `from_components` — populate it from `InstallComponent::Instructions`.
   - add `InstallPlan::instructions(self) -> bool` (true for `All` and the
     component).
   - `validate_plan` — include `instructions()` in the do-nothing guard.
   - `run()` — `if plan.instructions() { install_instructions(&project_root)?; }`
     plus an `install_instructions` wrapper printing changed/up-to-date, in the
     same style as `install_claude_skills`.
   - Naked `install` (`InstallPlan::All`) therefore writes the blocks by default.
3. **`doctor.rs`**
   - `use crate::instructions::{self, InstructionsState};`
   - text path: `tally += check_instructions(&project_root, fix);` in `run()`,
     plus `fn check_instructions` mirroring `check_skill`.
   - json path: add `check_instructions_json(project_root, fix)` to the
     `json_report` `checks` vec.
   - `next_actions` map: add
     `"instructions.block" => "Run \`loomweave doctor --fix\` or \`loomweave install --instructions\`."`.
4. **`main.rs`** — route the new component flag into `from_components` (follows
   the existing component plumbing; no new branch logic).

## Tests

- `instructions.rs` unit tests:
  - create (no file) → file created with one well-formed block.
  - append (file without marker) → block appended, prior content intact.
  - replace (file with current marker) → idempotent no-op when body matches;
    rewrite when body differs (drift).
  - **coexistence**: file pre-seeded with a Filigree block → after every
    operation, BOTH blocks present and Filigree's bytes untouched.
  - dangling start marker → repaired without eating any other bytes; reported
    `problem` without `--fix`.
  - symlink target rejected.
- `doctor.rs`: `InstructionsState` → severity mapping (Missing=warning,
  Unparseable/dangling=problem, Drifted=problem-without-fix / fixed-with-fix);
  `--fix` converges to `UpToDate`; json `ok` flag reflects only problems.
- e2e: extend an install smoke script to assert the block lands in CLAUDE.md and
  `doctor` reports it healthy.

## Suite-level follow-up (out of band, worth flagging)

The "each Weft tool owns `<!-- {tool}:instructions -->` and edits only its own
span" rule is a **suite contract**, not a Loomweave-local detail — Filigree's
current truncate-to-EOF recovery violates it and can eat Loomweave's block.
Recommend a short ADR (or a line in `docs/suite/weft.md`, already cited by
`doctor.rs`) capturing the contract, and a matching fix to Filigree's
`inject_instructions` so both tools stop being able to corrupt each other.

## Effort

Small–medium. One new module (~150 lines + tests) modelled line-for-line on
`skill_pack.rs`, one embedded asset, and ~6 mechanical insertion points across
`cli.rs` / `install.rs` / `doctor.rs` / `main.rs`. No schema, no migration, no
new dependency (blake3 + tempfile already in the tree).
```
