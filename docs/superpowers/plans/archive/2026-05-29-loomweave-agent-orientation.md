# Loomweave Agent Orientation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give Loomweave "Filigree-parity" agent orientation — install a bundled `loomweave-workflow` skill into a project, merge a SessionStart hook, serve a fail-soft project snapshot via `loomweave hook session-start`, and add MCP `instructions` + `resources` (`loomweave://context`) + a `loomweave-workflow` prompt — so a consult-mode agent self-orients the moment it lands.

**Architecture:** A single embedded `SKILL.md` (already committed at `crates/loomweave-cli/assets/skills/loomweave-workflow/SKILL.md`) is compiled into the `loomweave` binary with `include_str!`. New `install` flags (`--skills`, `--hooks`, `--all`) copy that pack atomically (temp-stage→rename) and merge a SessionStart hook into `.claude/settings.json` without clobbering existing keys. A new `loomweave hook session-start` subcommand reads `.loomweave/loomweave.db` and prints a project snapshot; the snapshot logic is factored into one reusable function (`project_snapshot`) homed in `loomweave-mcp`, which the MCP `loomweave://context` resource also calls. The MCP `initialize` result gains static `instructions` and `prompts`+`resources` capabilities.

**Tech Stack:** Rust (clap, anyhow, serde_json, rusqlite, blake3, time), hand-rolled JSON-RPC MCP server (no rmcp), SQLite via `loomweave-storage` `ReaderPool`, bash e2e harness.

---

## Decision Points

**(a) `include_str!` vs `include_dir`.** Use **`include_str!`** (one `const &str` per file). The pack is a single `SKILL.md` today; `include_str!` matches the existing convention (`crates/loomweave-storage/src/schema.rs:20` embeds a migration `.sql` the same way), adds **zero new dependencies**, and stays clear of `cargo deny check`. Tradeoff: a future `references/` pack means adding one `const` per file plus one `(rel_path, contents)` tuple to a static slice — acceptable for a small, slowly-changing pack. We design the copier around a `&[(&str, &str)]` slice of `(relative_path, contents)` so growing the pack is a data change, not a logic change.

**(b) Snapshot staleness — one bounded algorithm.** Order: (1) db missing → `db_present: false`, nudge "run `loomweave analyze`"; (2) zero rows in `runs` with a non-null `completed_at` → `staleness: never_analyzed`, nudge; (3) otherwise take the latest run `completed_at`, parse it, and compare against the filesystem mtime of each **distinct `entities.source_file_path`** (the files Loomweave actually ingested), short-circuiting on the first source file whose mtime is newer than the run → `staleness: stale`; if none are newer → `staleness: fresh`. Any FS/parse/IO error inside the staleness check degrades to `staleness: unknown` (never an error — the hook is fail-soft, always exit 0). Bounded by indexed-file count; respects exactly what was ingested. Tradeoff: a *new* source file not yet ingested won't be detected as drift (it has no `source_file_path` row) — acceptable, and the "run analyze" nudge on `never_analyzed`/`stale` covers the common case.

**(c) Install flag semantics (resolved ambiguity).** Today **bare `loomweave install` refuses if `.loomweave/` exists**. So `--skills`/`--hooks` must NOT imply `.loomweave/` init — they'd fail on every already-installed project (the common case for adding skills later). Resolution:
- **bare `loomweave install`** = `.loomweave/` init only (today's behavior, refuses-if-exists). All existing `crates/loomweave-cli/tests/install.rs` tests pass unmodified.
- **`--skills`** = copy the skill pack (independent component; does NOT init `.loomweave/`).
- **`--hooks`** = merge the SessionStart hook (independent component; does NOT init `.loomweave/`).
- **`--all`** = init + `--skills` + `--hooks`.
- Flags compose: `--skills --hooks` does both, no init. When any component flag is present, the bare init runs only if `--all` is set.

**(d) Snapshot function signature.** `pub fn project_snapshot(conn: &rusqlite::Connection, project_root: &Path) -> ProjectSnapshot` (infallible — folds errors into the snapshot's `staleness`/counts). Homed in **`loomweave-mcp`** (`crates/loomweave-mcp/src/snapshot.rs`): the CLI already depends on `loomweave-mcp`, and the MCP `loomweave://context` resource lives right next to it. Both callers have a `&Connection` and a `project_root` (the hook opens a read-only `Connection`; MCP runs it inside `readers.with_reader(move |conn| ...)` with `project_root` cloned into the `'static` closure). `ProjectSnapshot` is owned `Send + 'static`.

**(e) Dogfood product gaps — DOCUMENT, do not fix here.** Two real gaps surfaced during dogfooding: `find_entity` has no `kind` filter, and there is no module→subsystem reverse lookup. The committed `SKILL.md` already documents both (lines 73–75). For this slice: **file each as a child of Filigree epic `clarion-8fe3060d4c`** (see "Filigree bookkeeping" task), no fix-tasks. Expanding the MCP tool surface is out of scope.

**(f) MCP prompt is the lowest-value piece (optional).** `prompts/list` + `prompts/get` for `loomweave-workflow` returns the *same* `SKILL.md` text the installed skill already provides, so it duplicates the on-disk skill. It is included for MCP-client completeness (clients that surface prompts but not skills) but is the first thing to cut under time pressure. Implement it last (Phase 5 Task 5.5) and treat it as droppable.

---

## File Structure

| File | Create/Modify | Single responsibility |
|------|---------------|-----------------------|
| `crates/loomweave-cli/assets/skills/loomweave-workflow/SKILL.md` | Exists (committed) | The bundled skill text. Source of truth for both the installed pack and the MCP prompt. |
| `crates/loomweave-cli/src/skill_pack.rs` | Create | Embeds the pack via `include_str!`, exposes `SKILL_PACK: &[(&str, &str)]`, `pack_fingerprint()`, and `install_skill_pack(dest_root)` (atomic, fingerprint-aware copy into both `.claude/skills/loomweave-workflow/` and `.agents/skills/loomweave-workflow/`). |
| `crates/loomweave-cli/src/install.rs` | Modify (`run`, add `InstallComponents`) | Orchestrates init + skills + hooks per the resolved flag semantics. |
| `crates/loomweave-cli/src/hooks_settings.rs` | Create | Pure `.claude/settings.json` SessionStart-hook merge logic (parse → add-if-absent → serialize) + the file-level `install_session_start_hook(dest_root)`. |
| `crates/loomweave-cli/src/hook.rs` | Create | `loomweave hook session-start` handler: re-sync skill on drift, open read-only db `Connection`, call `project_snapshot`, print a snapshot + nudge to stdout. Always returns `Ok(())` (fail-soft). |
| `crates/loomweave-cli/src/cli.rs` | Modify (`Install` variant ~14, add `Hook`) | CLI surface: new install flags + `Hook { SessionStart }` subcommand. |
| `crates/loomweave-cli/src/main.rs` | Modify (dispatch ~28) | Wire `Install` flags and `Hook` into the dispatch match. |
| `crates/loomweave-cli/src/serve.rs` | Modify (`run_mcp_stdio` ~107) | Pass `project_root` to MCP state (already passed) — no change needed beyond confirming; the resource reads via `readers`. |
| `crates/loomweave-mcp/src/snapshot.rs` | Create | `ProjectSnapshot` struct + `project_snapshot(conn, project_root)` shared function. |
| `crates/loomweave-mcp/src/lib.rs` | Modify (`initialize_result` ~2059, `ServerState::handle_json_rpc` ~255, add `mod snapshot`, embed `SKILL.md`) | `instructions` + `prompts`/`resources` capabilities; new RPC arms for `prompts/list|get`, `resources/list|read`. |
| `crates/loomweave-cli/tests/skills.rs` | Create | Integration tests for `install --skills`, `--hooks`, `--all` flag semantics and idempotency. |
| `crates/loomweave-cli/tests/hook.rs` | Create | Integration tests for `loomweave hook session-start` (fail-soft, snapshot output). |
| `tests/e2e/sprint_2_mcp_surface.sh` | Modify (assertions after line 379) | Assert `initialize` `instructions` field and a `resources/read` of `loomweave://context`. |

---

## Preflight — verify before Task 1.1 (fix inline if any differ)

The plan assumes the crate facts below. Confirm each against the real files first; if one differs, adjust the citing task before writing its code. Each is a hard compile blocker if wrong, so catch them up front rather than mid-task.

- **`loomweave-cli` deps:** `skill_pack.rs` (Task 1.1) uses `blake3`; `skill_pack.rs`/`install.rs` use `anyhow`. Confirm both are in `crates/loomweave-cli/Cargo.toml` `[dependencies]`. If `blake3` is absent, add `blake3.workspace = true` (the workspace pin already exists — it's a `loomweave-mcp` dep) and `git add` the Cargo.toml with Task 1.1.
- **`loomweave-cli` → `loomweave-mcp` dependency:** `hook.rs` (Task 3.2) imports `loomweave_mcp::snapshot::*`. Confirm `crates/loomweave-cli/Cargo.toml` depends on `loomweave-mcp` (it should — `serve.rs` uses `loomweave_mcp::ServerState`). If absent, add it with Task 3.2.
- **`loomweave-cli` dev-deps:** `tests/skills.rs` / `tests/hook.rs` use `assert_cmd` + `tempfile`. Confirm both are `[dev-dependencies]` (the existing `tests/install.rs` uses them; if it does, they're present).
- **`ServerState` shape (Phase 5):** confirm the constructor is `ServerState::new(project_root: PathBuf, readers: ReaderPool)` and the struct has fields `project_root` + `readers`, and that `self.readers.with_reader(move |conn| Ok(..)).await` returns `Result<T>`. If the names/signature differ, adjust Task 5.2's `context_snapshot_json` and the Phase-5 tests' `ServerState::new(...)` calls to match. (Recon: serve.rs builds `ServerState::new(...)` then `.with_summary_llm(..)`/`.with_filigree_client(..)`.)
- **`runs.completed_at` format:** `parse_iso8601_to_systemtime` (Task 2.1) parses with `Rfc3339`. Confirm the column is RFC3339 (e.g. `2026-05-29T12:00:00.000Z`). If it's a space-separated SQLite form (`2026-05-29 12:00:00`), swap to a matching `time` `format_description!` — and update the Phase-2 test fixtures to the real format.

---

## PHASE 1 — Embedding + `loomweave install --skills`

Atomic, fingerprint-aware copy of the bundled skill pack. Independently testable: produces the two on-disk skill copies.

### Task 1.1: Embed the skill pack and compute a fingerprint

**Files:**
- Create: `crates/loomweave-cli/src/skill_pack.rs`
- Modify: `crates/loomweave-cli/src/main.rs` (add `mod skill_pack;`)
- Test: inline `#[cfg(test)]` in `crates/loomweave-cli/src/skill_pack.rs`

- [ ] **Step 1: Write the failing test**

Add to the bottom of the new file `crates/loomweave-cli/src/skill_pack.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{SKILL_PACK, pack_fingerprint};

    #[test]
    fn pack_contains_skill_md_with_frontmatter() {
        let (rel, contents) = SKILL_PACK
            .iter()
            .find(|(rel, _)| *rel == "SKILL.md")
            .expect("SKILL.md present in pack");
        assert_eq!(*rel, "SKILL.md");
        assert!(
            contents.contains("name: loomweave-workflow"),
            "SKILL.md is missing its frontmatter name"
        );
    }

    #[test]
    fn fingerprint_is_stable_and_64_hex_chars() {
        let fp1 = pack_fingerprint();
        let fp2 = pack_fingerprint();
        assert_eq!(fp1, fp2, "fingerprint must be deterministic");
        assert_eq!(fp1.len(), 64, "blake3 hex digest is 64 chars");
        assert!(fp1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli skill_pack`
Expected: FAIL — `skill_pack.rs` does not yet define `SKILL_PACK`/`pack_fingerprint` (compile error: unresolved import / module not found until `mod skill_pack;` and the items exist).

- [ ] **Step 3: Write minimal implementation**

Top of `crates/loomweave-cli/src/skill_pack.rs`:

```rust
//! Embedded `loomweave-workflow` skill pack and its on-disk installer.
//!
//! The pack is compiled into the binary with `include_str!` (matching the
//! `include_str!` migration-embedding convention in
//! `loomweave-storage/src/schema.rs`). Each entry is `(relative_path, contents)`;
//! growing the pack with a `references/` directory is a data change here, not a
//! logic change. The fingerprint over the pack bytes drives drift-aware
//! re-copy (Phase 3 hook resync + `--skills` idempotency).

/// `(relative_path, contents)` for every file in the bundled skill pack.
pub const SKILL_PACK: &[(&str, &str)] = &[(
    "SKILL.md",
    include_str!("../assets/skills/loomweave-workflow/SKILL.md"),
)];

/// The on-disk subdirectory name the pack installs into.
pub const PACK_DIR_NAME: &str = "loomweave-workflow";

/// Deterministic blake3 hex digest over the pack's `(rel_path, contents)`
/// entries. Order-stable because `SKILL_PACK` is a fixed slice. Used as the
/// drift sentinel: a `.fingerprint` file written next to the installed pack
/// records the version on disk.
#[must_use]
pub fn pack_fingerprint() -> String {
    let mut hasher = blake3::Hasher::new();
    for (rel, contents) in SKILL_PACK {
        hasher.update(rel.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(contents.as_bytes());
        hasher.update(&[0u8]);
    }
    hasher.finalize().to_hex().to_string()
}
```

Add to `crates/loomweave-cli/src/main.rs` after `mod secret_scan;` (keep modules alphabetical-ish; placement is cosmetic):

```rust
mod skill_pack;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli skill_pack`
Expected: PASS (both tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/skill_pack.rs crates/loomweave-cli/src/main.rs
git commit -m "feat(cli): embed loomweave-workflow skill pack with blake3 fingerprint"
```

### Task 1.2: Atomic, fingerprint-aware pack installer

**Files:**
- Modify: `crates/loomweave-cli/src/skill_pack.rs` (add `install_skill_pack`)
- Test: inline `#[cfg(test)]` in `crates/loomweave-cli/src/skill_pack.rs`

- [ ] **Step 1: Write the failing test**

Add these tests inside the existing `mod tests` in `crates/loomweave-cli/src/skill_pack.rs`:

```rust
#[test]
fn install_writes_pack_into_both_skill_roots() {
    use super::install_skill_pack;
    let dir = tempfile::tempdir().unwrap();
    let report = install_skill_pack(dir.path()).expect("install ok");
    assert!(report.copied, "first install should copy");

    for root in [".claude/skills", ".agents/skills"] {
        let skill = dir
            .path()
            .join(root)
            .join("loomweave-workflow")
            .join("SKILL.md");
        assert!(skill.exists(), "missing {}", skill.display());
        let body = std::fs::read_to_string(&skill).unwrap();
        assert!(body.contains("name: loomweave-workflow"));
    }
}

#[test]
fn install_is_idempotent_when_fingerprint_matches() {
    use super::install_skill_pack;
    let dir = tempfile::tempdir().unwrap();
    let first = install_skill_pack(dir.path()).unwrap();
    assert!(first.copied);
    let second = install_skill_pack(dir.path()).unwrap();
    assert!(!second.copied, "second install should be a no-op on match");
}

#[test]
fn install_recopies_when_installed_pack_drifted() {
    use super::install_skill_pack;
    let dir = tempfile::tempdir().unwrap();
    install_skill_pack(dir.path()).unwrap();
    // Corrupt one installed copy + its fingerprint to simulate drift.
    let skill = dir
        .path()
        .join(".claude/skills/loomweave-workflow/SKILL.md");
    std::fs::write(&skill, "STALE").unwrap();
    let fp = dir
        .path()
        .join(".claude/skills/loomweave-workflow/.fingerprint");
    std::fs::write(&fp, "deadbeef").unwrap();

    let report = install_skill_pack(dir.path()).unwrap();
    assert!(report.copied, "drift should trigger re-copy");
    let body = std::fs::read_to_string(&skill).unwrap();
    assert!(body.contains("name: loomweave-workflow"), "drift not repaired");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli skill_pack`
Expected: FAIL — `install_skill_pack` and its `SkillInstallReport` are not defined (compile error).

- [ ] **Step 3: Write minimal implementation**

Add to `crates/loomweave-cli/src/skill_pack.rs` (above the `#[cfg(test)]` block):

```rust
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// The two skill roots Loomweave installs into, relative to the project root.
/// `.claude/skills/` is read by Claude Code; `.agents/skills/` is the
/// tool-agnostic convention so non-Claude agent harnesses find it too.
const SKILL_ROOTS: &[&str] = &[".claude/skills", ".agents/skills"];

const FINGERPRINT_FILE: &str = ".fingerprint";

/// Outcome of an [`install_skill_pack`] call.
#[derive(Debug, Clone, Copy)]
pub struct SkillInstallReport {
    /// True if the pack bytes were (re)written this call; false if every
    /// destination already matched the embedded fingerprint.
    pub copied: bool,
}

/// Install (or re-sync on drift) the embedded skill pack into both skill roots
/// under `project_root`, idempotently.
///
/// For each root, the pack lands at `<root>/loomweave-workflow/`. A
/// `.fingerprint` file recording [`pack_fingerprint`] is written alongside the
/// pack files. If every root's `.fingerprint` already equals the embedded
/// fingerprint, the call is a no-op (`copied: false`).
///
/// Writes are atomic: each pack is staged into a sibling temp directory in the
/// same skill root (so `rename` stays on one filesystem), then `rename`d over
/// the destination.
///
/// # Errors
///
/// Returns an error if any directory create, temp write, or rename fails.
pub fn install_skill_pack(project_root: &Path) -> Result<SkillInstallReport> {
    let fingerprint = pack_fingerprint();
    let mut copied = false;
    for root_rel in SKILL_ROOTS {
        let root = project_root.join(root_rel);
        let dest = root.join(PACK_DIR_NAME);
        if installed_fingerprint(&dest).as_deref() == Some(fingerprint.as_str()) {
            continue;
        }
        stage_and_swap(&root, &dest, &fingerprint)
            .with_context(|| format!("install skill pack into {}", dest.display()))?;
        copied = true;
    }
    Ok(SkillInstallReport { copied })
}

fn installed_fingerprint(dest: &Path) -> Option<String> {
    fs::read_to_string(dest.join(FINGERPRINT_FILE))
        .ok()
        .map(|s| s.trim().to_owned())
}

fn stage_and_swap(root: &Path, dest: &Path, fingerprint: &str) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("mkdir {}", root.display()))?;
    // Stage in a sibling temp dir so the final rename is same-filesystem.
    let staging = root.join(format!(".loomweave-workflow.tmp-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("clear stale staging {}", staging.display()))?;
    }
    fs::create_dir_all(&staging).with_context(|| format!("mkdir {}", staging.display()))?;
    for (rel, contents) in SKILL_PACK {
        let target = staging.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        fs::write(&target, contents).with_context(|| format!("write {}", target.display()))?;
    }
    fs::write(staging.join(FINGERPRINT_FILE), fingerprint)
        .with_context(|| format!("write fingerprint in {}", staging.display()))?;
    // Remove any prior install, then move the staged pack into place.
    if dest.exists() {
        fs::remove_dir_all(dest).with_context(|| format!("remove old {}", dest.display()))?;
    }
    fs::rename(&staging, dest)
        .with_context(|| format!("rename {} -> {}", staging.display(), dest.display()))?;
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli skill_pack`
Expected: PASS (all five tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/skill_pack.rs
git commit -m "feat(cli): atomic fingerprint-aware skill-pack installer"
```

### Task 1.3: `loomweave install --skills` CLI flag

**Files:**
- Modify: `crates/loomweave-cli/src/cli.rs` (`Install` variant ~14-23)
- Modify: `crates/loomweave-cli/src/install.rs` (`run` signature + `InstallComponents`)
- Modify: `crates/loomweave-cli/src/main.rs` (dispatch ~29)
- Test: `crates/loomweave-cli/tests/skills.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/loomweave-cli/tests/skills.rs`:

```rust
//! `loomweave install --skills/--hooks/--all` integration tests.

use std::fs;

use assert_cmd::Command;

fn loomweave_bin() -> Command {
    Command::cargo_bin("loomweave").expect("loomweave binary")
}

#[test]
fn install_skills_writes_pack_without_initialising_loomweave_dir() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(
        dir.path()
            .join(".claude/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "skill not installed under .claude"
    );
    assert!(
        dir.path()
            .join(".agents/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "skill not installed under .agents"
    );
    // --skills MUST NOT init .loomweave/.
    assert!(
        !dir.path().join(".loomweave").exists(),
        "--skills should not create .loomweave/"
    );
}

#[test]
fn install_skills_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        loomweave_bin()
            .args(["install", "--skills", "--path"])
            .arg(dir.path())
            .assert()
            .success();
    }
    let body = fs::read_to_string(
        dir.path().join(".claude/skills/loomweave-workflow/SKILL.md"),
    )
    .unwrap();
    assert!(body.contains("name: loomweave-workflow"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli --test skills`
Expected: FAIL — `--skills` is an unknown argument (clap errors with a nonzero exit; `assert().success()` fails).

- [ ] **Step 3: Write minimal implementation**

In `crates/loomweave-cli/src/cli.rs`, replace the `Install` variant (lines ~14-23) with:

```rust
    /// Initialise .loomweave/ and/or install agent-orientation assets.
    ///
    /// Bare `loomweave install` initialises .loomweave/ only (refuses if it
    /// already exists). `--skills` and `--hooks` install the orientation
    /// assets and do NOT initialise .loomweave/. `--all` does init + skills +
    /// hooks.
    Install {
        /// Overwrite an existing .loomweave/ directory.
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Install the bundled loomweave-workflow skill pack into
        /// .claude/skills/ and .agents/skills/.
        #[arg(long)]
        skills: bool,

        /// Merge a SessionStart hook into .claude/settings.json.
        #[arg(long)]
        hooks: bool,

        /// Do everything: .loomweave/ init + --skills + --hooks.
        #[arg(long)]
        all: bool,
    },
```

In `crates/loomweave-cli/src/install.rs`, replace the `pub fn run(path: &Path, force: bool)` signature and body opening. First add this struct + helper above `run` (after the `const` stubs, before the `/// Run the install subcommand` doc comment):

```rust
/// Which install components to perform. Resolved from CLI flags in
/// [`InstallComponents::from_flags`] per the flag semantics in the agent-
/// orientation plan: bare install = init only; `--skills`/`--hooks` are
/// independent and do NOT init; `--all` = init + skills + hooks.
#[derive(Debug, Clone, Copy)]
pub struct InstallComponents {
    pub init_loomweave: bool,
    pub skills: bool,
    pub hooks: bool,
}

impl InstallComponents {
    #[must_use]
    pub fn from_flags(skills: bool, hooks: bool, all: bool) -> Self {
        if all {
            return Self {
                init_loomweave: true,
                skills: true,
                hooks: true,
            };
        }
        let any_component = skills || hooks;
        Self {
            // Bare install (no component flags) keeps today's behavior: init.
            init_loomweave: !any_component,
            skills,
            hooks,
        }
    }
}
```

Now change `run` to take components. Replace:

```rust
pub fn run(path: &Path, force: bool) -> Result<()> {
    if !path.exists() {
```

with:

```rust
pub fn run(path: &Path, force: bool, components: InstallComponents) -> Result<()> {
    if !path.exists() {
```

Then, before the existing `let project_root = path.canonicalize()...` block, the canonicalization is still needed by all branches. Keep it. After the `let project_root = ...` line, wrap the `.loomweave/` work in the `init_loomweave` guard. Replace the block from `let loomweave_dir = project_root.join(".loomweave");` down through the final `println!("Initialised {}", loomweave_dir.display());\n    Ok(())\n}` with:

```rust
    if components.init_loomweave {
        let loomweave_dir = project_root.join(".loomweave");
        if loomweave_dir.exists() {
            if !force {
                bail!(
                    ".loomweave/ already exists at {}. Delete it or pass --force to overwrite it.",
                    loomweave_dir.display()
                );
            }
            if !loomweave_dir.is_dir() {
                bail!(
                    "--force can only overwrite an existing .loomweave/ directory; \
                     found non-directory at {}.",
                    loomweave_dir.display()
                );
            }
            fs::remove_dir_all(&loomweave_dir)
                .with_context(|| format!("remove existing {}", loomweave_dir.display()))?;
        }

        fs::create_dir_all(&loomweave_dir)
            .with_context(|| format!("mkdir {}", loomweave_dir.display()))?;

        if let Err(err) = populate_after_mkdir(&loomweave_dir, &project_root) {
            if let Err(cleanup_err) = fs::remove_dir_all(&loomweave_dir) {
                tracing::warn!(
                    loomweave_dir = %loomweave_dir.display(),
                    error = %cleanup_err,
                    "install failed and cleanup of partial .loomweave/ also failed; \
                     manual rm -rf may be required"
                );
            }
            return Err(err);
        }

        tracing::info!(
            loomweave_dir = %loomweave_dir.display(),
            "loomweave install complete"
        );
        println!("Initialised {}", loomweave_dir.display());
    }

    if components.skills {
        let report = crate::skill_pack::install_skill_pack(&project_root)
            .context("install loomweave-workflow skill pack")?;
        if report.copied {
            println!(
                "Installed loomweave-workflow skill into {}/.claude/skills and {}/.agents/skills",
                project_root.display(),
                project_root.display()
            );
        } else {
            println!("loomweave-workflow skill already up to date");
        }
    }

    // --hooks wired in Phase 4.

    Ok(())
}
```

In `crates/loomweave-cli/src/main.rs`, change the `Install` match arm (line ~29) from:

```rust
        cli::Command::Install { force, path } => install::run(&path, force),
```

to:

```rust
        cli::Command::Install {
            force,
            path,
            skills,
            hooks,
            all,
        } => install::run(
            &path,
            force,
            install::InstallComponents::from_flags(skills, hooks, all),
        ),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli --test skills`
Expected: PASS. Also run the existing install tests to prove bare-install behavior is unchanged:
Run: `cargo nextest run -p loomweave-cli --test install`
Expected: PASS (all existing tests green — `from_flags(false, false, false)` sets `init_loomweave: true`).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/cli.rs crates/loomweave-cli/src/install.rs crates/loomweave-cli/src/main.rs crates/loomweave-cli/tests/skills.rs
git commit -m "feat(cli): loomweave install --skills installs orientation skill pack"
```

### Task 1.4: Phase 1 gate

- [ ] **Step 1: Run the full Rust gate for the crate**

Run: `cargo fmt --all -- --check && cargo clippy -p loomweave-cli --all-targets --all-features -- -D warnings && cargo nextest run -p loomweave-cli`
Expected: PASS, no warnings.

- [ ] **Step 2: Commit any fmt/clippy fixes (only if the gate changed files)**

```bash
git add -A
git commit -m "style(cli): fmt/clippy after skill-pack phase"
```

---

## PHASE 2 — Shared snapshot module

A pure-ish function over a `&Connection` + `project_root`, unit-tested against a fixture db. Homed in `loomweave-mcp`.

### Task 2.1: `ProjectSnapshot` + counts (no staleness yet)

**Files:**
- Create: `crates/loomweave-mcp/src/snapshot.rs`
- Modify: `crates/loomweave-mcp/src/lib.rs` (add `pub mod snapshot;`)
- Test: inline `#[cfg(test)]` in `crates/loomweave-mcp/src/snapshot.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/loomweave-mcp/src/snapshot.rs` with this test block at the bottom:

```rust
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use loomweave_storage::{pragma, schema};

    use super::{Staleness, project_snapshot};

    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        schema::apply_migrations(&mut conn).unwrap();
        conn
    }

    fn insert_entity(conn: &Connection, id: &str, kind: &str, source_file_path: Option<&str>) {
        conn.execute(
            "INSERT INTO entities \
             (id, plugin_id, kind, name, short_name, properties, source_file_path, created_at, updated_at) \
             VALUES (?1, 'python', ?2, ?3, ?3, '{}', ?4, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            rusqlite::params![id, kind, id, source_file_path],
        )
        .unwrap();
    }

    #[test]
    fn counts_entities_subsystems_and_findings() {
        let conn = migrated_conn();
        insert_entity(&conn, "python:module:a", "module", Some("a.py"));
        insert_entity(&conn, "python:function:a.f", "function", Some("a.py"));
        insert_entity(&conn, "core:subsystem:abc", "subsystem", None);
        // A run + a finding attached to the module.
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES ('run1', '2026-01-01T00:00:00.000Z', '2026-01-02T00:00:00.000Z', '{}', '{}', 'completed')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO findings \
             (id, tool, tool_version, run_id, rule_id, kind, severity, entity_id, \
              related_entities, message, evidence, properties, supports, supported_by, status, created_at, updated_at) \
             VALUES ('f1','loomweave','1.0','run1','R1','defect','WARN','python:module:a', \
                     '[]','m','{}','{}','[]','[]','open','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();

        let snap = project_snapshot(&conn, std::path::Path::new("/nonexistent-root"));
        assert!(snap.db_present);
        assert_eq!(snap.entity_count, 3);
        assert_eq!(snap.subsystem_count, 1);
        assert_eq!(snap.finding_count, 1);
        // No source files exist on disk under /nonexistent-root, and there IS a
        // completed run, so staleness degrades to Unknown (stat failures fold
        // to Unknown, never an error).
        assert_eq!(snap.staleness, Staleness::Unknown);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-mcp snapshot`
Expected: FAIL — module/items not defined (compile error until `pub mod snapshot;` + the items exist).

- [ ] **Step 3: Write minimal implementation**

Top of `crates/loomweave-mcp/src/snapshot.rs`:

```rust
//! Shared project snapshot: entity/subsystem/finding counts + index staleness.
//!
//! One function, two callers: the `loomweave hook session-start` subcommand and
//! the MCP `loomweave://context` resource. Infallible by design — every failure
//! folds into the snapshot (zero counts, `Staleness::Unknown`) so the fail-soft
//! hook never has to handle an error.

use std::path::Path;
use std::time::SystemTime;

use rusqlite::Connection;
use serde::Serialize;

/// Freshness of the `.loomweave/` index relative to the source files Loomweave
/// ingested. See the plan's Decision Point (b) for the algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// No completed analyze run has ever been recorded.
    NeverAnalyzed,
    /// At least one ingested source file is newer than the latest run.
    Stale,
    /// No ingested source file is newer than the latest run.
    Fresh,
    /// Could not determine (stat/parse/IO error) — degrade, don't fail.
    Unknown,
}

/// Counts + freshness for one Loomweave project, safe to serialize into the MCP
/// resource or print from the hook.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSnapshot {
    pub db_present: bool,
    pub entity_count: i64,
    pub subsystem_count: i64,
    pub finding_count: i64,
    pub staleness: Staleness,
    /// Latest run `completed_at` (ISO-8601) if any, else `None`.
    pub last_analyzed_at: Option<String>,
}

/// Build a snapshot from an already-open migrated `Connection`.
///
/// `db_present` is always `true` here (the caller opened the connection); the
/// `false` case is produced by the caller when the db file is missing.
#[must_use]
pub fn project_snapshot(conn: &Connection, project_root: &Path) -> ProjectSnapshot {
    let entity_count = scalar_count(conn, "SELECT COUNT(*) FROM entities");
    let subsystem_count = scalar_count(
        conn,
        "SELECT COUNT(*) FROM entities WHERE kind = 'subsystem'",
    );
    let finding_count = scalar_count(conn, "SELECT COUNT(*) FROM findings");

    let last_analyzed_at = latest_completed_run(conn);
    let staleness = compute_staleness(conn, project_root, last_analyzed_at.as_deref());

    ProjectSnapshot {
        db_present: true,
        entity_count,
        subsystem_count,
        finding_count,
        staleness,
        last_analyzed_at,
    }
}

/// A missing-database snapshot: all zeros, `NeverAnalyzed`, no timestamp.
#[must_use]
pub fn missing_db_snapshot() -> ProjectSnapshot {
    ProjectSnapshot {
        db_present: false,
        entity_count: 0,
        subsystem_count: 0,
        finding_count: 0,
        staleness: Staleness::NeverAnalyzed,
        last_analyzed_at: None,
    }
}

fn scalar_count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .unwrap_or(0)
}

fn latest_completed_run(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT completed_at FROM runs \
         WHERE completed_at IS NOT NULL AND status = 'completed' \
         ORDER BY completed_at DESC LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn compute_staleness(
    conn: &Connection,
    project_root: &Path,
    last_analyzed_at: Option<&str>,
) -> Staleness {
    let Some(run_iso) = last_analyzed_at else {
        return Staleness::NeverAnalyzed;
    };
    let Some(run_time) = parse_iso8601_to_systemtime(run_iso) else {
        return Staleness::Unknown;
    };

    // Distinct source files Loomweave actually ingested.
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT source_file_path FROM entities \
         WHERE source_file_path IS NOT NULL",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Staleness::Unknown,
    };
    let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(_) => return Staleness::Unknown,
    };

    let mut saw_any_file = false;
    for rel in rows.flatten() {
        let abs = if Path::new(&rel).is_absolute() {
            std::path::PathBuf::from(&rel)
        } else {
            project_root.join(&rel)
        };
        match abs.metadata().and_then(|m| m.modified()) {
            Ok(mtime) => {
                saw_any_file = true;
                if mtime > run_time {
                    return Staleness::Stale;
                }
            }
            Err(_) => return Staleness::Unknown,
        }
    }
    if saw_any_file {
        Staleness::Fresh
    } else {
        // A completed run but no resolvable source files on disk.
        Staleness::Unknown
    }
}

/// Parse a strict `%Y-%m-%dT%H:%M:%S(.fff)Z` UTC timestamp (the format
/// `strftime('%Y-%m-%dT%H:%M:%fZ','now')` writes into `runs.completed_at`) to a
/// `SystemTime`. Returns `None` on any deviation.
fn parse_iso8601_to_systemtime(iso: &str) -> Option<SystemTime> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    let odt = OffsetDateTime::parse(iso, &Rfc3339).ok()?;
    Some(SystemTime::from(odt))
}
```

Add to `crates/loomweave-mcp/src/lib.rs` near the other `pub mod` lines (top of file, after `pub mod filigree;`):

```rust
pub mod snapshot;
```

Confirm `time` is a dependency of `loomweave-mcp` (it is — `lib.rs` already `use time::...`). Confirm `serde` is too (it is — `use serde::{Deserialize, Serialize};`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-mcp snapshot`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/snapshot.rs crates/loomweave-mcp/src/lib.rs
git commit -m "feat(mcp): shared project_snapshot (counts + staleness)"
```

### Task 2.2: Staleness — fresh vs stale against on-disk source mtimes

**Files:**
- Modify: none (logic already in 2.1; this task adds the proving tests)
- Test: inline `#[cfg(test)]` in `crates/loomweave-mcp/src/snapshot.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/loomweave-mcp/src/snapshot.rs`:

```rust
#[test]
fn never_analyzed_when_no_completed_run() {
    let conn = migrated_conn();
    insert_entity(&conn, "python:module:a", "module", Some("a.py"));
    let snap = project_snapshot(&conn, std::path::Path::new("/tmp"));
    assert_eq!(snap.staleness, Staleness::NeverAnalyzed);
    assert!(snap.last_analyzed_at.is_none());
}

#[test]
fn fresh_when_all_sources_older_than_run() {
    let dir = tempfile::tempdir().unwrap();
    // Create the source file FIRST (old), then record a run AFTER it.
    let src = dir.path().join("a.py");
    std::fs::write(&src, "x = 1\n").unwrap();

    let conn = migrated_conn();
    insert_entity(&conn, "python:module:a", "module", Some("a.py"));
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES ('r', '2099-01-01T00:00:00.000Z', '2099-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
        [],
    )
    .unwrap();

    let snap = project_snapshot(&conn, dir.path());
    assert_eq!(snap.staleness, Staleness::Fresh, "{snap:?}");
    assert_eq!(snap.last_analyzed_at.as_deref(), Some("2099-01-01T00:00:00.000Z"));
}

#[test]
fn stale_when_a_source_is_newer_than_run() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.py");
    std::fs::write(&src, "x = 1\n").unwrap();

    let conn = migrated_conn();
    insert_entity(&conn, "python:module:a", "module", Some("a.py"));
    // Run completed in the distant past; the on-disk file is newer.
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES ('r', '2000-01-01T00:00:00.000Z', '2000-01-01T00:00:00.000Z', '{}', '{}', 'completed')",
        [],
    )
    .unwrap();

    let snap = project_snapshot(&conn, dir.path());
    assert_eq!(snap.staleness, Staleness::Stale, "{snap:?}");
}
```

- [ ] **Step 2: Run test to verify it fails or passes**

Run: `cargo nextest run -p loomweave-mcp snapshot`
Expected: PASS — the staleness logic from Task 2.1 already satisfies these. (If any fail, the bug is in 2.1's `compute_staleness`; fix it there, do not weaken the test.) This task exists to lock the fresh/stale boundary with explicit fixtures.

- [ ] **Step 3: (no new implementation — logic landed in 2.1)**

Skip; the tests prove the existing implementation.

- [ ] **Step 4: Re-run to confirm green**

Run: `cargo nextest run -p loomweave-mcp snapshot`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/snapshot.rs
git commit -m "test(mcp): lock fresh/stale boundary for project_snapshot"
```

### Task 2.3: Phase 2 gate

- [ ] **Step 1: Run the crate gate**

Run: `cargo fmt --all -- --check && cargo clippy -p loomweave-mcp --all-targets --all-features -- -D warnings && cargo nextest run -p loomweave-mcp`
Expected: PASS, no warnings.

- [ ] **Step 2: Commit fixes if any**

```bash
git add -A
git commit -m "style(mcp): fmt/clippy after snapshot module"
```

---

## PHASE 3 — `loomweave hook session-start` subcommand

Fail-soft: always exits 0. Re-syncs the skill on drift, prints the snapshot + nudge.

### Task 3.1: CLI `Hook { SessionStart }` subcommand wired to a no-op handler

**Files:**
- Modify: `crates/loomweave-cli/src/cli.rs` (add `Hook` variant + `HookCommand` enum)
- Create: `crates/loomweave-cli/src/hook.rs` (handler)
- Modify: `crates/loomweave-cli/src/main.rs` (add `mod hook;` + dispatch)
- Test: `crates/loomweave-cli/tests/hook.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/loomweave-cli/tests/hook.rs`:

```rust
//! `loomweave hook session-start` integration tests.

use assert_cmd::Command;

fn loomweave_bin() -> Command {
    Command::cargo_bin("loomweave").expect("loomweave binary")
}

#[test]
fn hook_session_start_exits_zero_without_loomweave_db() {
    // Fail-soft: no .loomweave/ at all must still exit 0 and nudge.
    let dir = tempfile::tempdir().unwrap();
    let assert = loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        out.contains("loomweave analyze"),
        "missing analyze nudge in: {out}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli --test hook`
Expected: FAIL — `hook` is an unknown subcommand (clap nonzero exit).

- [ ] **Step 3: Write minimal implementation**

In `crates/loomweave-cli/src/cli.rs`, add a new variant to `enum Command` (after `Serve { ... }`):

```rust
    /// Agent-lifecycle hook entrypoints. Always exit 0 (fail-soft) so a
    /// misbehaving hook never blocks session start.
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
```

And add this enum at the bottom of `crates/loomweave-cli/src/cli.rs`:

```rust
#[derive(Subcommand)]
pub enum HookCommand {
    /// Print a project snapshot and re-sync the skill pack on drift.
    SessionStart {
        /// Project directory containing .loomweave/loomweave.db.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
}
```

Create `crates/loomweave-cli/src/hook.rs`:

```rust
//! `loomweave hook session-start` — fail-soft session-start orientation.
//!
//! Never returns an error to the caller: the SessionStart hook must never
//! block an agent's session start. All failures degrade to a printed note.

use std::path::Path;

/// Run `loomweave hook session-start`. Always returns `Ok(())`.
pub fn session_start(path: &Path) -> anyhow::Result<()> {
    println!("Loomweave: orientation hook (snapshot wired in Task 3.2).");
    println!("If briefings look empty, run `loomweave analyze {}`.", path.display());
    Ok(())
}
```

In `crates/loomweave-cli/src/main.rs`, add `mod hook;` near the other modules, and add the dispatch arm after the `Serve` arm:

```rust
        cli::Command::Hook { command } => match command {
            cli::HookCommand::SessionStart { path } => hook::session_start(&path),
        },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli --test hook`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/cli.rs crates/loomweave-cli/src/hook.rs crates/loomweave-cli/src/main.rs crates/loomweave-cli/tests/hook.rs
git commit -m "feat(cli): add fail-soft loomweave hook session-start subcommand"
```

### Task 3.2: Snapshot output + skill resync in the hook

**Files:**
- Modify: `crates/loomweave-cli/src/hook.rs` (full snapshot + resync)
- Test: `crates/loomweave-cli/tests/hook.rs` (add)

- [ ] **Step 1: Write the failing test**

Add to `crates/loomweave-cli/tests/hook.rs`:

```rust
#[test]
fn hook_session_start_prints_counts_for_installed_project() {
    let dir = tempfile::tempdir().unwrap();
    // Initialise .loomweave/ (bare install).
    loomweave_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let assert = loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    // Empty db: 0 entities, never analyzed → nudge present.
    assert!(out.contains("entities"), "missing entity count line: {out}");
    assert!(out.contains("loomweave analyze"), "missing nudge: {out}");
}

#[test]
fn hook_session_start_resyncs_skill_when_present_and_drifted() {
    let dir = tempfile::tempdir().unwrap();
    // Install the skill, then corrupt it to simulate drift.
    loomweave_bin()
        .args(["install", "--skills", "--path"])
        .arg(dir.path())
        .assert()
        .success();
    let skill = dir
        .path()
        .join(".claude/skills/loomweave-workflow/SKILL.md");
    std::fs::write(&skill, "STALE").unwrap();

    loomweave_bin()
        .args(["hook", "session-start", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let body = std::fs::read_to_string(&skill).unwrap();
    assert!(
        body.contains("name: loomweave-workflow"),
        "hook did not repair drifted skill: {body}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli --test hook`
Expected: FAIL — current stub prints neither counts nor resyncs (the new assertions fail).

- [ ] **Step 3: Write minimal implementation**

Replace the body of `crates/loomweave-cli/src/hook.rs` with:

```rust
//! `loomweave hook session-start` — fail-soft session-start orientation.
//!
//! Never returns an error to the caller: the SessionStart hook must never
//! block an agent's session start. All failures degrade to a printed note.

use std::path::Path;

use loomweave_mcp::snapshot::{ProjectSnapshot, Staleness, missing_db_snapshot, project_snapshot};
use rusqlite::{Connection, OpenFlags};

/// Run `loomweave hook session-start`. Always returns `Ok(())`.
pub fn session_start(path: &Path) -> anyhow::Result<()> {
    // (1) Re-sync the skill pack ONLY if it's already installed and drifted.
    //     We don't install where absent — that's `loomweave install --skills`'s
    //     job. A drift repair keeps an installed copy honest across upgrades.
    resync_skill_if_present(path);

    // (2) Snapshot.
    let snapshot = load_snapshot(path);
    print_snapshot(path, &snapshot);
    Ok(())
}

fn resync_skill_if_present(project_root: &Path) {
    let installed = project_root
        .join(".claude/skills/loomweave-workflow/SKILL.md")
        .exists()
        || project_root
            .join(".agents/skills/loomweave-workflow/SKILL.md")
            .exists();
    if !installed {
        return;
    }
    if let Err(err) = crate::skill_pack::install_skill_pack(project_root) {
        // Fail-soft: log, never propagate.
        tracing::warn!(error = %err, "loomweave-workflow skill resync failed");
    }
}

fn load_snapshot(project_root: &Path) -> ProjectSnapshot {
    let db_path = project_root.join(".loomweave").join("loomweave.db");
    if !db_path.exists() {
        return missing_db_snapshot();
    }
    match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => {
            let root = project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf());
            project_snapshot(&conn, &root)
        }
        Err(err) => {
            tracing::warn!(error = %err, "open .loomweave/loomweave.db read-only failed");
            missing_db_snapshot()
        }
    }
}

fn print_snapshot(project_root: &Path, snapshot: &ProjectSnapshot) {
    if !snapshot.db_present {
        println!(
            "Loomweave: no index at {}/.loomweave/loomweave.db. \
             Run `loomweave install --path {}` then `loomweave analyze {}`.",
            project_root.display(),
            project_root.display(),
            project_root.display()
        );
        return;
    }
    println!(
        "Loomweave index: {} entities, {} subsystems, {} findings.",
        snapshot.entity_count, snapshot.subsystem_count, snapshot.finding_count
    );
    match snapshot.staleness {
        Staleness::Fresh => {
            println!(
                "Index is fresh (last analyzed {}). Ask Loomweave before re-exploring \
                 the tree; see the loomweave-workflow skill.",
                snapshot.last_analyzed_at.as_deref().unwrap_or("unknown")
            );
        }
        Staleness::Stale => {
            println!(
                "Index may be stale: source files changed since the last run. \
                 Run `loomweave analyze {}` to refresh.",
                project_root.display()
            );
        }
        Staleness::NeverAnalyzed => {
            println!(
                "No analysis recorded yet. Run `loomweave analyze {}` to build the index.",
                project_root.display()
            );
        }
        Staleness::Unknown => {
            println!(
                "Index freshness unknown. If briefings look empty, run \
                 `loomweave analyze {}`.",
                project_root.display()
            );
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli --test hook`
Expected: PASS (all three hook tests green). The empty-db case yields `NeverAnalyzed` → prints "loomweave analyze".

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/hook.rs crates/loomweave-cli/tests/hook.rs
git commit -m "feat(cli): hook session-start prints snapshot and resyncs skill on drift"
```

### Task 3.3: Phase 3 gate

- [ ] **Step 1: Run the crate gate**

Run: `cargo fmt --all -- --check && cargo clippy -p loomweave-cli --all-targets --all-features -- -D warnings && cargo nextest run -p loomweave-cli`
Expected: PASS, no warnings.

- [ ] **Step 2: Commit fixes if any**

```bash
git add -A
git commit -m "style(cli): fmt/clippy after hook subcommand"
```

---

## PHASE 4 — `loomweave install --hooks` (settings.json merge) + `--all`

Merge a SessionStart hook into `.claude/settings.json` without clobbering existing keys.

### Task 4.1: Pure settings.json merge function

**Files:**
- Create: `crates/loomweave-cli/src/hooks_settings.rs`
- Modify: `crates/loomweave-cli/src/main.rs` (add `mod hooks_settings;`)
- Test: inline `#[cfg(test)]` in `crates/loomweave-cli/src/hooks_settings.rs`

The verified `.claude/settings.json` shape (from the settings JSON schema): `hooks` is an object keyed by event name; `hooks.SessionStart` is an array of matcher-groups; each group is `{ "matcher"?: string, "hooks": [ { "type": "command", "command": "..." } ] }`. `matcher` is optional. Idempotency predicate: skip if any SessionStart group has a `hooks[]` entry whose `command` contains `loomweave hook session-start`.

- [ ] **Step 1: Write the failing test**

Create `crates/loomweave-cli/src/hooks_settings.rs` with this test block at the bottom:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{HOOK_COMMAND, merge_session_start_hook};

    #[test]
    fn adds_hook_to_empty_settings() {
        let mut settings = json!({});
        let changed = merge_session_start_hook(&mut settings);
        assert!(changed, "should report a change");
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains(HOOK_COMMAND), "command was: {cmd}");
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
    }

    #[test]
    fn is_idempotent_when_hook_already_present() {
        let mut settings = json!({});
        assert!(merge_session_start_hook(&mut settings));
        // Second merge must be a no-op.
        assert!(!merge_session_start_hook(&mut settings));
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "must not duplicate the hook");
    }

    #[test]
    fn preserves_unrelated_hooks_and_top_level_keys() {
        // A Stop hook + an UNRELATED SessionStart command must both survive.
        let mut settings = json!({
            "model": "opus",
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "echo bye"}]}
                ],
                "SessionStart": [
                    {"hooks": [{"type": "command", "command": "echo unrelated-greeting"}]}
                ]
            }
        });

        let changed = merge_session_start_hook(&mut settings);
        assert!(changed);

        // Top-level key preserved.
        assert_eq!(settings["model"], "opus");
        // Stop hook untouched.
        assert_eq!(
            settings["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo bye"
        );
        // SessionStart now has BOTH the unrelated entry and ours.
        let groups = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "must append, not replace");
        let cmds: Vec<&str> = groups
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .map(|h| h["command"].as_str().unwrap())
            .collect();
        assert!(cmds.iter().any(|c| c.contains("unrelated-greeting")));
        assert!(cmds.iter().any(|c| c.contains(HOOK_COMMAND)));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli hooks_settings`
Expected: FAIL — `merge_session_start_hook`/`HOOK_COMMAND` not defined (compile error).

- [ ] **Step 3: Write minimal implementation**

Top of `crates/loomweave-cli/src/hooks_settings.rs`:

```rust
//! `.claude/settings.json` SessionStart-hook merge.
//!
//! Merge semantics (never clobber): parse existing JSON, append a SessionStart
//! matcher-group running `loomweave hook session-start` only if no existing
//! SessionStart entry already runs that command, and preserve every other key.
//!
//! Verified against the Claude Code settings schema: `hooks.SessionStart` is an
//! array of matcher-groups, each `{ "matcher"?, "hooks": [ {type,command} ] }`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

/// Substring that identifies Loomweave's own SessionStart hook command.
pub const HOOK_COMMAND: &str = "loomweave hook session-start";

/// Merge Loomweave's SessionStart hook into a parsed settings `Value` in place.
/// Returns `true` if a change was made, `false` if the hook was already present.
#[must_use]
pub fn merge_session_start_hook(settings: &mut Value) -> bool {
    // Ensure `settings` is an object.
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    let obj = settings.as_object_mut().expect("settings is object");

    // Ensure `hooks` is an object.
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().expect("hooks is object");

    // Ensure `SessionStart` is an array.
    let groups = hooks
        .entry("SessionStart")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !groups.is_array() {
        *groups = Value::Array(Vec::new());
    }
    let groups = groups.as_array_mut().expect("SessionStart is array");

    // Idempotency: skip if any existing entry already runs our command.
    let already_present = groups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|inner| {
                inner.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains(HOOK_COMMAND))
                })
            })
    });
    if already_present {
        return false;
    }

    groups.push(json!({
        "hooks": [
            {
                "type": "command",
                "command": "loomweave hook session-start"
            }
        ]
    }));
    true
}

/// Read `.claude/settings.json` under `project_root` (creating an empty object
/// if absent), merge Loomweave's SessionStart hook, and write it back
/// pretty-printed. Returns `true` if the file changed.
///
/// # Errors
///
/// Returns an error if the existing file is present but unparseable, or if any
/// directory create / read / write fails.
pub fn install_session_start_hook(project_root: &Path) -> Result<bool> {
    let claude_dir = project_root.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    let mut settings: Value = if settings_path.exists() {
        let raw = fs::read_to_string(&settings_path)
            .with_context(|| format!("read {}", settings_path.display()))?;
        if raw.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parse {}", settings_path.display()))?
        }
    } else {
        Value::Object(Map::new())
    };

    let changed = merge_session_start_hook(&mut settings);
    if !changed {
        return Ok(false);
    }

    fs::create_dir_all(&claude_dir)
        .with_context(|| format!("mkdir {}", claude_dir.display()))?;
    let serialized = serde_json::to_string_pretty(&settings)
        .context("serialize .claude/settings.json")?;
    fs::write(&settings_path, format!("{serialized}\n"))
        .with_context(|| format!("write {}", settings_path.display()))?;
    Ok(true)
}
```

Add to `crates/loomweave-cli/src/main.rs` near the modules:

```rust
mod hooks_settings;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli hooks_settings`
Expected: PASS (all three merge tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/hooks_settings.rs crates/loomweave-cli/src/main.rs
git commit -m "feat(cli): non-clobbering SessionStart hook merge for .claude/settings.json"
```

### Task 4.2: Wire `--hooks` and `--all` into `install::run`

**Files:**
- Modify: `crates/loomweave-cli/src/install.rs` (`run`)
- Test: `crates/loomweave-cli/tests/skills.rs` (add)

- [ ] **Step 1: Write the failing test**

Add to `crates/loomweave-cli/tests/skills.rs`:

```rust
#[test]
fn install_hooks_merges_session_start_without_clobbering() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-seed an unrelated Stop hook.
    let claude = dir.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    fs::write(
        claude.join("settings.json"),
        r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#,
    )
    .unwrap();

    loomweave_bin()
        .args(["install", "--hooks", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let raw = fs::read_to_string(claude.join("settings.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    // Unrelated key + Stop hook preserved.
    assert_eq!(parsed["model"], "opus");
    assert_eq!(
        parsed["hooks"]["Stop"][0]["hooks"][0]["command"],
        "echo bye"
    );
    // Our SessionStart hook added.
    let cmds: Vec<String> = parsed["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["hooks"].as_array().unwrap())
        .map(|h| h["command"].as_str().unwrap().to_string())
        .collect();
    assert!(cmds.iter().any(|c| c.contains("loomweave hook session-start")));
    // --hooks alone must NOT init .loomweave/.
    assert!(!dir.path().join(".loomweave").exists());
}

#[test]
fn install_all_does_init_skills_and_hooks() {
    let dir = tempfile::tempdir().unwrap();
    loomweave_bin()
        .args(["install", "--all", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    assert!(dir.path().join(".loomweave/loomweave.db").exists(), "no db");
    assert!(
        dir.path()
            .join(".claude/skills/loomweave-workflow/SKILL.md")
            .exists(),
        "no skill"
    );
    let raw = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(raw.contains("loomweave hook session-start"), "no hook: {raw}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-cli --test skills`
Expected: FAIL — `--hooks` currently does nothing (the comment placeholder from Task 1.3); the SessionStart assertion fails.

- [ ] **Step 3: Write minimal implementation**

In `crates/loomweave-cli/src/install.rs`, replace the placeholder line:

```rust
    // --hooks wired in Phase 4.
```

with:

```rust
    if components.hooks {
        let changed = crate::hooks_settings::install_session_start_hook(&project_root)
            .context("merge SessionStart hook into .claude/settings.json")?;
        if changed {
            println!(
                "Added loomweave SessionStart hook to {}/.claude/settings.json",
                project_root.display()
            );
        } else {
            println!("loomweave SessionStart hook already present");
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-cli --test skills`
Expected: PASS (both new tests green plus the Phase-1 skills tests).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-cli/src/install.rs crates/loomweave-cli/tests/skills.rs
git commit -m "feat(cli): wire loomweave install --hooks and --all"
```

### Task 4.3: Phase 4 gate

- [ ] **Step 1: Run the crate gate**

Run: `cargo fmt --all -- --check && cargo clippy -p loomweave-cli --all-targets --all-features -- -D warnings && cargo nextest run -p loomweave-cli`
Expected: PASS, no warnings. Existing `tests/install.rs` still green.

- [ ] **Step 2: Commit fixes if any**

```bash
git add -A
git commit -m "style(cli): fmt/clippy after hooks install"
```

---

## PHASE 5 — MCP `instructions` + `resources` + (optional) `prompt`

`loomweave://context` reuses `project_snapshot`. The prompt duplicates `SKILL.md` and is the droppable piece.

### Task 5.1: `initialize` gains `instructions` + extended `capabilities`

**Files:**
- Modify: `crates/loomweave-mcp/src/lib.rs` (`initialize_result` ~2059, embed `SKILL.md`)
- Test: inline `#[cfg(test)]` in `crates/loomweave-mcp/src/lib.rs` (extend `initialize_returns_server_info_and_tools_capability`, ~2901)

- [ ] **Step 1: Write the failing test**

In `crates/loomweave-mcp/src/lib.rs`, extend the existing test `initialize_returns_server_info_and_tools_capability` (after the final `assert!(response["result"]["capabilities"]["tools"].is_object());`, before the closing `}`):

```rust
        // Orientation instructions present and mention the skill + entity model.
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("initialize result has instructions");
        assert!(
            instructions.contains("loomweave-workflow"),
            "instructions should point at the skill"
        );
        assert!(
            instructions.contains("entity"),
            "instructions should describe the entity model"
        );
        // Prompts + resources capabilities advertised alongside tools.
        assert!(response["result"]["capabilities"]["prompts"].is_object());
        assert!(response["result"]["capabilities"]["resources"].is_object());
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-mcp initialize_returns_server_info`
Expected: FAIL — `instructions` is absent (`as_str()` on Null panics → test fails); `capabilities.prompts`/`resources` absent.

- [ ] **Step 3: Write minimal implementation**

In `crates/loomweave-mcp/src/lib.rs`, near the top (after `const EMPTY_GUIDANCE_FINGERPRINT`), add the embedded skill text + the instructions constant:

```rust
/// The bundled loomweave-workflow skill text, embedded for the `prompts/get`
/// surface and reused as the canonical orientation reference. Same file the
/// CLI installs on disk.
pub const LOOMWEAVE_WORKFLOW_SKILL: &str =
    include_str!("../../loomweave-cli/assets/skills/loomweave-workflow/SKILL.md");

/// Static orientation text returned in the MCP `initialize` result's
/// `instructions` field. Kept consistent with `list_tools()` and the
/// loomweave-workflow skill.
const SERVER_INSTRUCTIONS: &str = "\
Loomweave is a code-archaeology server: it has pre-extracted this project into a \
queryable map of entities (functions, classes, modules, files), the call / \
reference / import edges between them, and subsystem clusters. Ask Loomweave \
instead of re-reading or grepping the tree.

Entity IDs are `{plugin}:{kind}:{qualified_name}` (e.g. \
`python:function:pkg.mod.func`); subsystems are `core:subsystem:{hash}`. You \
almost never type IDs — get one from `find_entity` or `entity_at`, then copy it \
verbatim into the next tool.

Tools: find_entity, entity_at, callers_of, neighborhood, execution_paths_from, \
subsystem_members, summary, issues_for. `callers_of` / `neighborhood` / \
`execution_paths_from` take a `confidence` tier (resolved | ambiguous | \
inferred; default resolved).

For the full workflow see the loomweave-workflow skill (installed by \
`loomweave install --skills`), or read the `loomweave-workflow` prompt. Live \
project counts and index freshness are in the `loomweave://context` resource.";
```

Replace `initialize_result` (lines ~2059-2070) with:

```rust
fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {},
            "prompts": {},
            "resources": {}
        },
        "serverInfo": {
            "name": "loomweave",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": SERVER_INSTRUCTIONS
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p loomweave-mcp initialize`
Expected: PASS. Also confirm the existing `tools_list_exposes_exact_docstrings` (asserts `tools.len() == 8`) still passes — `list_tools()` is untouched:
Run: `cargo nextest run -p loomweave-mcp tools_list_exposes_exact_docstrings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/lib.rs
git commit -m "feat(mcp): initialize advertises instructions + prompts/resources capabilities"
```

### Task 5.2: `resources/list` returns `loomweave://context`

**Files:**
- Modify: `crates/loomweave-mcp/src/lib.rs` (`ServerState::handle_json_rpc` match ~255)
- Test: inline `#[cfg(test)]` in `crates/loomweave-mcp/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Look at how existing `ServerState` tests build state (the test module imports `ServerState`, `ReaderPool`, `pragma`, `schema` — see `crates/loomweave-mcp/src/lib.rs:2845-2852`). Add this test to the `mod tests` block. It builds a migrated db on disk, opens a `ReaderPool`, and drives `handle_json_rpc`:

```rust
    #[tokio::test]
    async fn resources_list_includes_loomweave_context() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "resources/list",
                "params": {}
            }))
            .await
            .expect("response");

        let resources = response["result"]["resources"].as_array().unwrap();
        assert!(
            resources
                .iter()
                .any(|r| r["uri"] == "loomweave://context"),
            "loomweave://context not listed: {resources:?}"
        );
    }
```

(Confirm `tempfile` is a dev-dependency of `loomweave-mcp`. If `cargo nextest` reports it missing, add `tempfile.workspace = true` under `[dev-dependencies]` in `crates/loomweave-mcp/Cargo.toml` and `git add` it with this task.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p loomweave-mcp resources_list`
Expected: FAIL — `resources/list` hits the `_ => error_response(..., "method not found")` arm, so `result.resources` is absent.

- [ ] **Step 3: Write minimal implementation**

In `crates/loomweave-mcp/src/lib.rs`, extend the `ServerState::handle_json_rpc` match (the `Some(match method { ... })` at ~255) by adding arms before the final `_ =>`:

```rust
            "resources/list" => result_response(&id, &resources_list()),
            "resources/read" => self.handle_resources_read(&id, request.get("params")).await,
            "prompts/list" => result_response(&id, &prompts_list()),
            "prompts/get" => prompts_get(&id, request.get("params")),
```

Add these free functions near `initialize_result` (any module-level location in `lib.rs`):

```rust
fn resources_list() -> Value {
    json!({
        "resources": [
            {
                "uri": "loomweave://context",
                "name": "Loomweave project context",
                "description": "Live entity / subsystem / finding counts and index freshness for this project.",
                "mimeType": "application/json"
            }
        ]
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

The test still won't pass until `handle_resources_read` and the prompt fns exist (the match arms reference them and the crate won't compile). Implement `handle_resources_read` minimally now so the crate compiles and this test passes; the read body is fully fleshed in Task 5.3. Add to `impl ServerState` (near `handle_tool_call`):

```rust
    async fn handle_resources_read(&self, id: &Value, params: Option<&Value>) -> Value {
        let Some(uri) = params
            .and_then(Value::as_object)
            .and_then(|p| p.get("uri"))
            .and_then(Value::as_str)
        else {
            return error_response(id, -32602, "invalid resources/read params: missing uri");
        };
        if uri != "loomweave://context" {
            return error_response(id, -32602, &format!("unknown resource: {uri}"));
        }
        let snapshot_json = self.context_snapshot_json().await;
        result_response(
            id,
            &json!({
                "contents": [
                    {
                        "uri": "loomweave://context",
                        "mimeType": "application/json",
                        "text": snapshot_json
                    }
                ]
            }),
        )
    }

    async fn context_snapshot_json(&self) -> String {
        let project_root = self.project_root.clone();
        let snapshot = self
            .readers
            .with_reader(move |conn| {
                Ok(crate::snapshot::project_snapshot(conn, &project_root))
            })
            .await;
        match snapshot {
            Ok(snap) => serde_json::to_string(&snap)
                .unwrap_or_else(|_| "{\"db_present\":true,\"staleness\":\"unknown\"}".to_owned()),
            Err(err) => {
                tracing::warn!(error = %err, "loomweave://context snapshot failed");
                serde_json::json!({
                    "db_present": true,
                    "entity_count": 0,
                    "subsystem_count": 0,
                    "finding_count": 0,
                    "staleness": "unknown",
                    "last_analyzed_at": serde_json::Value::Null
                })
                .to_string()
            }
        }
    }
```

Add minimal prompt fns (fleshed in 5.5) so the match compiles:

```rust
fn prompts_list() -> Value {
    json!({
        "prompts": [
            {
                "name": "loomweave-workflow",
                "description": "How to use Loomweave's MCP tools to navigate this codebase."
            }
        ]
    })
}

fn prompts_get(id: &Value, params: Option<&Value>) -> Value {
    let name = params
        .and_then(Value::as_object)
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str);
    if name != Some("loomweave-workflow") {
        return error_response(id, -32602, "unknown prompt");
    }
    result_response(
        id,
        &json!({
            "description": "How to use Loomweave's MCP tools to navigate this codebase.",
            "messages": [
                {
                    "role": "user",
                    "content": { "type": "text", "text": LOOMWEAVE_WORKFLOW_SKILL }
                }
            ]
        }),
    )
}
```

Run: `cargo nextest run -p loomweave-mcp resources_list`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/lib.rs crates/loomweave-mcp/Cargo.toml
git commit -m "feat(mcp): resources/list advertises loomweave://context"
```

### Task 5.3: `resources/read` of `loomweave://context` returns the snapshot

**Files:**
- Modify: none (handler landed in 5.2)
- Test: inline `#[cfg(test)]` in `crates/loomweave-mcp/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[tokio::test]
    async fn resources_read_returns_context_snapshot_json() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
            conn.execute(
                "INSERT INTO entities \
                 (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
                 VALUES ('python:module:m','python','module','m','m','{}', \
                         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "resources/read",
                "params": {"uri": "loomweave://context"}
            }))
            .await
            .expect("response");

        let text = response["result"]["contents"][0]["text"]
            .as_str()
            .expect("snapshot text");
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["db_present"], true);
        assert_eq!(parsed["entity_count"], 1);
        assert_eq!(parsed["staleness"], "never_analyzed");
    }

    #[tokio::test]
    async fn resources_read_rejects_unknown_uri() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "resources/read",
                "params": {"uri": "loomweave://nope"}
            }))
            .await
            .expect("response");
        assert!(response["error"].is_object(), "expected an error envelope");
    }
```

- [ ] **Step 2: Run test to verify it fails or passes**

Run: `cargo nextest run -p loomweave-mcp resources_read`
Expected: PASS (handler from 5.2 already serves this). If `staleness` is not `never_analyzed`, the bug is in 5.2's snapshot wiring — fix there. This task locks the read contract.

- [ ] **Step 3: (no new implementation)**

Skip.

- [ ] **Step 4: Re-run to confirm green**

Run: `cargo nextest run -p loomweave-mcp resources_read`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/lib.rs
git commit -m "test(mcp): lock resources/read loomweave://context contract"
```

### Task 5.4: e2e — assert `instructions` and `resources/read` over the wire

**Files:**
- Modify: `tests/e2e/sprint_2_mcp_surface.sh` (add a request + assertions)
- Test: the script itself

- [ ] **Step 1: Add the request + assertions**

In `tests/e2e/sprint_2_mcp_surface.sh`, add a new entry to the `requests` Python list (after the `"issues"` tuple, before the closing `]` at ~line 362):

```python
    (
        "context",
        {
            "jsonrpc": "2.0",
            "id": "context",
            "method": "resources/read",
            "params": {"uri": "loomweave://context"},
        },
    ),
```

Then add assertions after the existing `assert responses["initialize"]["result"]["protocolVersion"] == "2025-11-25"` line (~379):

```python
init_result = responses["initialize"]["result"]
assert "loomweave-workflow" in init_result["instructions"], init_result.get("instructions")
assert isinstance(init_result["capabilities"]["resources"], dict), init_result["capabilities"]
assert isinstance(init_result["capabilities"]["prompts"], dict), init_result["capabilities"]
```

And add, after the `issues` assertions block near the end (after line ~441):

```python
context = responses["context"]["result"]
ctx_text = context["contents"][0]["text"]
ctx = json.loads(ctx_text)
assert ctx["db_present"] is True, ctx
assert ctx["entity_count"] >= 1, ctx
assert "staleness" in ctx, ctx
```

- [ ] **Step 2: Run the e2e script to verify it (initially) reflects reality**

Run: `bash tests/e2e/sprint_2_mcp_surface.sh`
Expected: PASS — the binary now serves `instructions`, the `resources` capability, and `resources/read loomweave://context`. (If you run this BEFORE Phase 5 code lands, it FAILs on the `instructions` KeyError — that is the failing-test step; run it after 5.1–5.3 are committed.)

- [ ] **Step 3: (no implementation — covered by Phase 5 Rust tasks)**

Skip.

- [ ] **Step 4: Confirm green**

Run: `bash tests/e2e/sprint_2_mcp_surface.sh`
Expected: PASS, final log line "PASS: MCP stdio surface ...".

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/sprint_2_mcp_surface.sh
git commit -m "test(e2e): assert MCP initialize instructions and loomweave://context read"
```

### Task 5.5 (OPTIONAL — droppable): prove `prompts/list` + `prompts/get`

The handler already landed in 5.2; this task only adds proving tests. **Skip under time pressure** — the prompt duplicates the installed `SKILL.md` (Decision Point f).

**Files:**
- Modify: none
- Test: inline `#[cfg(test)]` in `crates/loomweave-mcp/src/lib.rs`

- [ ] **Step 1: Write the test**

```rust
    #[tokio::test]
    async fn prompts_get_returns_skill_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "prompts/get",
                "params": {"name": "loomweave-workflow"}
            }))
            .await
            .expect("response");
        let text = response["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("name: loomweave-workflow"), "not the skill text");
    }
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p loomweave-mcp prompts_get`
Expected: PASS (handler from 5.2 serves it).

- [ ] **Step 3: (no implementation)**

Skip.

- [ ] **Step 4: Confirm**

Run: `cargo nextest run -p loomweave-mcp prompts_get`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-mcp/src/lib.rs
git commit -m "test(mcp): prompts/get serves loomweave-workflow skill text"
```

### Task 5.6: Phase 5 gate (full workspace)

- [ ] **Step 1: Run the full Rust gate**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```
Expected: all PASS. (`cargo deny check` confirms no new dependency was added — `include_str!` route.)

- [ ] **Step 2: Run both e2e scripts**

Run: `bash tests/e2e/sprint_2_mcp_surface.sh && bash tests/e2e/sprint_1_walking_skeleton.sh`
Expected: PASS.

- [ ] **Step 3: Commit fixes if any**

```bash
git add -A
git commit -m "style: fmt/clippy after MCP orientation surface"
```

---

## PHASE 6 — Docs + Filigree bookkeeping

### Task 6.1: Document the new install/hook/MCP surface

**Files:**
- Modify: `docs/operator/getting-started.md` (the `loomweave serve` / MCP-client section ~155-185)

- [ ] **Step 1: Add an orientation subsection**

After the MCP-client registration block (~line 185, before "## 4. Ask"), add:

```markdown
### Agent orientation (optional but recommended)

Give consult-mode agents a head start:

```bash
loomweave install --skills --path /tmp/requests-2.32.4   # bundle the loomweave-workflow skill
loomweave install --hooks --path /tmp/requests-2.32.4    # add a SessionStart snapshot hook
loomweave install --all   --path /tmp/requests-2.32.4    # .loomweave/ init + skills + hooks
```

`--skills` writes `.claude/skills/loomweave-workflow/` and `.agents/skills/loomweave-workflow/`.
`--hooks` merges a SessionStart entry into `.claude/settings.json` (existing
hooks are preserved) that runs `loomweave hook session-start` — a fail-soft
command printing live entity/subsystem/finding counts and index freshness.

Over MCP, the same orientation is available without install: the `initialize`
result carries an `instructions` field, the `loomweave://context` resource returns
the live snapshot, and the `loomweave-workflow` prompt returns the skill text.
```

- [ ] **Step 2: Verify the doc renders (no test; visual check)**

Run: `grep -n "loomweave install --skills" docs/operator/getting-started.md`
Expected: the new lines are present.

- [ ] **Step 3: Commit**

```bash
git add docs/operator/getting-started.md
git commit -m "docs(operator): document install --skills/--hooks/--all and MCP orientation"
```

### Task 6.2: File the dogfood product gaps in Filigree

**Files:**
- None (Filigree CLI/MCP only)

- [ ] **Step 1: File the two gaps as children of the orientation epic**

These are out-of-scope for this slice (Decision Point e) but must be tracked, not dropped. Run (CLI form; use `--actor` for attribution):

```bash
filigree create-issue \
  --title "find_entity has no kind filter" \
  --body "Dogfooding loomweave-workflow showed find_entity cannot constrain by entity kind (e.g. only subsystems). Today agents must search a package name and eyeball the result whose kind is 'subsystem'. Add an optional kind filter to find_entity's inputSchema + query. Documented as a gotcha in crates/loomweave-cli/assets/skills/loomweave-workflow/SKILL.md (lines 73-75). Cites: MCP tool surface in crates/loomweave-mcp/src/lib.rs list_tools()." \
  --label release:1.1 --actor "$USER"

filigree create-issue \
  --title "No module->subsystem reverse lookup" \
  --body "neighborhood does not return an entity's subsystem; membership is only reachable forward via subsystem_members(subsystem_id). Add a reverse lookup (subsystem_for_member is already in loomweave-storage query.rs — surface it as an MCP tool or neighborhood field). Documented as a gotcha in the loomweave-workflow SKILL.md." \
  --label release:1.1 --actor "$USER"
```

Then add each as a child/dependency of epic `clarion-8fe3060d4c` (use the IDs printed by the create commands):

```bash
filigree add-dependency <new-issue-id-1> --depends-on clarion-8fe3060d4c --actor "$USER"
filigree add-dependency <new-issue-id-2> --depends-on clarion-8fe3060d4c --actor "$USER"
```

(If `add-dependency`'s direction differs in this filigree version, run `filigree add-dependency --help` and link so the two gaps are children of `clarion-8fe3060d4c`. If a `--parent`/epic verb exists, prefer it.)

- [ ] **Step 2: Confirm they're tracked**

Run: `filigree list-issues --label release:1.1`
Expected: both new issues listed.

- [ ] **Step 3: (no commit — Filigree state is in `.filigree/`, managed separately)**

Skip git commit; Filigree manages its own store.

---

## Self-Review

**Spec coverage:**
- (1) `install --skills` atomic temp→rename, both roots, fingerprint-aware → Phase 1 (Tasks 1.2, 1.3). ✓
- (2) `install --hooks` non-clobbering settings.json merge → Phase 4 (Tasks 4.1, 4.2). ✓
- (3) `install --all` + bare unchanged → Task 1.3 (`InstallComponents::from_flags`) + Task 4.2; existing `install.rs` tests prove bare unchanged. ✓
- (4) `loomweave hook session-start` fail-soft, resync + snapshot + nudge → Phase 3. ✓
- (5) MCP `instructions` + `prompts`/`resources` capabilities + `prompts/list|get` + `resources/list|read loomweave://context` → Phase 5. ✓
- (6) shared snapshot module used by both hook and MCP resource → Phase 2 (`project_snapshot`), consumed in Task 3.2 (hook) and Task 5.2 (`context_snapshot_json`). ✓
- Decision Points (include_str! vs include_dir; dogfood gaps; prompt optional) → top section + Task 6.2. ✓
- e2e wire assertions → Task 5.4. ✓

**Placeholder scan:** No "TBD"/"add error handling"/"similar to Task N". The "wired in Phase 4" comment in Task 1.3 is a real intermediate literal that Task 4.2 replaces with shown code. Tasks 2.2/3.3-style "no new implementation" steps are explicit (logic landed earlier) — not placeholders.

**Type consistency:** `InstallComponents{init_loomweave,skills,hooks}` + `from_flags(skills,hooks,all)` consistent across cli.rs/install.rs/main.rs. `install_skill_pack -> SkillInstallReport{copied}` consistent across skill_pack.rs/install.rs/hook.rs. `project_snapshot(conn, project_root) -> ProjectSnapshot{db_present,entity_count,subsystem_count,finding_count,staleness,last_analyzed_at}` + `Staleness{NeverAnalyzed,Stale,Fresh,Unknown}` (serde snake_case) + `missing_db_snapshot()` consistent across snapshot.rs/hook.rs/lib.rs. `merge_session_start_hook(&mut Value)->bool` + `install_session_start_hook(&Path)->Result<bool>` + `HOOK_COMMAND` consistent across hooks_settings.rs/install.rs. `HookCommand::SessionStart{path}` consistent across cli.rs/main.rs/hook.rs. `LOOMWEAVE_WORKFLOW_SKILL`/`SERVER_INSTRUCTIONS`/`resources_list`/`prompts_list`/`prompts_get`/`handle_resources_read`/`context_snapshot_json` all defined in lib.rs Phase 5. ✓
