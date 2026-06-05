# Phase B — Rename agent brief (Clarion → Loomweave, Loom → Weft)

> Standalone task brief for a fresh agent. Assumes zero context from prior sessions.
> Companion to the master plan `docs/implementation/2026-06-05-loomweave-1.0-rename-and-pypi-plan.md` (Phase B is the spec). Phases A/C/D are NOT in scope here.

---

You are executing a large, trap-laden product rename in a Rust + Python monorepo. Work from `/home/john/clarion`.

## Goal
Rename the product **Clarion → Loomweave** and the framework/suite **Loom → Weft**, and recut the version **1.3.0 → 1.0.0**, as ONE atomic change on a single branch. This is a triage problem (~403 working-set files mention `clarion`), NOT a blind sweep: most renames, a handful of buckets must NOT be touched, a few are cross-product.

## READ FIRST (authoritative detail)
- `docs/implementation/2026-06-05-loomweave-1.0-rename-and-pypi-plan.md` — **Phase B** is your spec (workstreams WS1–WS9, the reference-triage recipe, the do-not-rename list, the execution strategy, and the verification gate). Phases C/D are NOT yours (PyPI packaging + infra come later).
- Skim `docs/superpowers/specs/2026-06-05-loomweave-pypi-distribution-design.md` for context only.

## Naming hierarchy (the target end state)
**Weft** framework (was "Loom") › **Loomweave** (flagship, was "Clarion"), **Filigree**, **Wardline**, **Legis** (+ **Shuttle** planned).

## Exact transformations
- `clarion` → `loomweave`   (dirs, paths, crate names, binary, package names — bulk)
- `Clarion` → `Loomweave`   (prose, doc titles, `site_name` — bulk)
- `CLARION` → `LOOMWEAVE`    (env vars — EXCEPT the LOOM carve-out below)
- `clarion_` → `loomweave_`  (Rust identifiers — EXCEPT `clarion_entity_id`, see cross-product)
- `clarion-` → `loomweave-`  (crate names, plugin prefix `clarion-plugin-*` → `loomweave-plugin-*` — EXCEPT Filigree issue IDs)
- Framework/federation env vars: `CLARION_LOOM_*` → `WEFT_*`  (NOT `LOOMWEAVE_*`, and NOT `LOOMWEAVE_LOOM_*` — that's the double-loom trap)
- Python: package `clarion-plugin-python` → `loomweave-plugin-python`; module `clarion_plugin_python` → `loomweave_plugin_python`; shared-data path `share/clarion/plugins/` → `share/loomweave/plugins/`
- Persisted: `.clarion/` → `.loomweave/`; `clarion.db` → `loomweave.db`; `clarion.yaml` → `loomweave.yaml`
- Version: `1.3.0` → `1.0.0` in root `Cargo.toml` (workspace.package) and every `pyproject.toml`; add a CHANGELOG `## [1.0.0] — Loomweave` entry re-baselining history
- URLs: `github.com/tachyon-beep/clarion` → `github.com/foundryside-dev/loomweave`; docs domain `clarion.foundryside.dev` → `loomweave.foundryside.dev` (in `web/mkdocs.yml` etc.)
- SQLite `application_id` magic `0x434C524E ("CLRN")` → `0x4C4D5756 ("LMWV")` (owner-approved; updates the ForeignDatabase guard + its tests)
- ADR-021: add/keep the discovery-source amendment; its `clarion-plugin-*` refs become `loomweave-plugin-*`

## DO NOT RENAME (a naive find/replace corrupts these — exclude by path/regex)
1. **Filigree issue IDs** `clarion-[0-9a-f]{8,}` and `clarion-sf-*` anywhere (docs, CHANGELOG, commit messages, ADR filenames) — historical identifiers; the Filigree prefix stays `clarion` by owner decision.
2. **`.filigree.conf`** `project_name` / `prefix` — stay `clarion`.
3. **`/api/loom/...` wire paths and `api_version`** — a versioned federation contract, not the product. LEAVE until Wardline/Filigree move in lockstep.
4. **`docs/archive/`** dated reports — historical record; leave as-is (they also contain `clarion-XXXX` IDs).
5. Recorded **test corpus / golden-snapshot identifiers** that double as keys — verify before touching any fixture.

## CROSS-PRODUCT — flag to the owner, do NOT break unilaterally
- `--clarion-url` flag consumed by **Wardline** (see repo-root `.mcp.json`): renaming to `--loomweave-url` needs a coordinated change in the Wardline repo (same owner). Flag it; do not break it alone.
- `clarion_entity_id` federation field (`crates/clarion-federation/.../filigree.rs`) is the ADR-029 entity-association contract field read by Filigree. Before renaming to `loomweave_entity_id`, VERIFY Filigree's read path treats `entity_id` as opaque (it should). If unverifiable, leave it and flag.

## Hard constraints
- **ONE atomic branch** named `rename/clarion-to-loomweave`. Do NOT drip across small PRs — a ~400-file rename races the concurrent agent ("Antigravity") that commits to this repo in real time. Confirm a freeze window with the owner before starting.
- Base the branch so it INCLUDES the already-landed Phase A foundation commits `cecc134` (current_exe() plugin discovery level) and `7305af9` (doctor real-discovery) — they live on branch `docs/pypi-distribution-spec`. Either branch from there, or branch from latest `main` and cherry-pick those two. These commits contain literal `clarion-plugin-`/`share/clarion/` strings that your mechanical pass MUST sweep like everything else — the logic is final, only the names change.
- Use `git mv` for crate directories (`crates/clarion-*` → `crates/loomweave-*`) and `docs/clarion/` → `docs/loomweave/` to preserve history (the owner rejects stale-name "history preservation"; prefer real renames).
- **Do NOT push and do NOT open a PR** — stop and hand back to the owner when the verification gate is green.

## Execution order (per plan §B.6)
1. Create the branch (freeze window coordinated). Confirm Phase A commits are in its history.
2. Mechanical pass per case-variant (`clarion`/`Clarion`/`CLARION`/`clarion_`/`clarion-`), with the DO-NOT-RENAME buckets excluded by path/regex. Then the framework pass (`Loom`→`Weft` for the framework identity + `CLARION_LOOM_*`→`WEFT_*`).
3. `git mv` crate dirs + `docs/clarion/`.
4. Regenerate `Cargo.lock`.
5. MANUAL review of every carve-out: LOOM→WEFT env vars, `clarion_entity_id`, `--clarion-url`, Filigree issue IDs, `docs/archive/`, DB magic, golden snapshots.
6. Version recut 1.3.0→1.0.0 + CHANGELOG entry.

## Verification gate — ALL must pass before declaring done
```bash
cargo build --workspace
cargo nextest run
(cd plugins/python && pytest)
# residual audit — every remaining hit must be an INTENTIONAL carve-out:
grep -rniE 'clarion' . | grep -vE '/(target|\.git|.*cache|site-build)/' \
  | grep -vE 'clarion-[0-9a-f]{8}|clarion-sf-|/api/loom|\.filigree|docs/archive'
```
The residual audit returning ONLY known carve-outs is your completeness proof. Then wipe stale `.clarion/` and run `loomweave analyze /home/john/clarion` to rebuild the index under the new paths.

## Report back
- Branch name + base, and confirmation the Phase A commits are included.
- Counts: files changed, crates `git mv`'d.
- The residual-audit output (proving only carve-outs remain), with each remaining category named.
- Test/build results (exact pass counts).
- Every cross-product item you flagged rather than changed (Wardline `--clarion-url`, `clarion_entity_id` if unverified).
- Anything you were unsure about — STOP and ask rather than guessing on a destructive rename.
