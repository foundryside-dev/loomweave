# Pyright Address-Space Limit + Warm-Up Call Timeout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Take elspeth's residual degraded-resolution set from 5 files to 1 by (A) stopping the plugin host's inherited 2 GiB `RLIMIT_AS` from killing pyright's node process on large files, and (B) letting the first LSP query on a large file take longer than 5 s.

**Architecture:** Two independent, small changes shipped as two PRs against `release/1.5.0`. (A) is a Rust change in `loomweave-core`'s plugin host: language-server plugins (those declaring `[capabilities.runtime.pyright]`) get a separate, much larger address-space ceiling because V8 reserves virtual space far beyond its RSS; every other plugin keeps today's 2 GiB. A new fixture-plugin knob proves both branches end-to-end. (B) is a one-constant change in the Python plugin: `PYRIGHT_CALL_TIMEOUT_SECS` 5 → 30. The per-request grant is already `min(call_timeout, remaining file budget)` and the file budget (≤ 90 s) sits under the host's 120 s watchdog, so the wedge/backstop story is unchanged.

**Tech Stack:** Rust 1.88 / edition 2024 (`nix` for `setrlimit`/`mmap`), Python 3.11+ plugin (pytest, mypy --strict, ruff), Filigree tickets **clarion-353c5b9aa5** (A) and **clarion-5d83413c36** (B).

**Spec:** the two Filigree tickets above (their descriptions carry the measurements). Governing ADRs: `docs/loomweave/adr/ADR-021-plugin-authority-hybrid.md` §2d (RLIMIT_AS), `docs/loomweave/adr/ADR-035-operational-tuning-discipline.md` (every operational constant needs basis / override surface / retune trigger / coupling), `docs/loomweave/adr/ADR-057-pyright-restart-attribution.md` (timeout attribution).

## Global Constraints

- Merge target is `release/1.5.0`, never literal `main`. Branch from it; PR into it; merge with `gh pr merge --admin --merge` only after every CI leg is green. Do not delete remote branches without the owner's OK.
- CI floor (ADR-023) must be green before claiming done:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo build --workspace --bins            # BEFORE nextest: e2e tests exec the built bins
  cargo nextest run --workspace --all-features
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
  cargo deny check
  plugins/python/.venv/bin/ruff check plugins/python
  plugins/python/.venv/bin/ruff format --check plugins/python
  plugins/python/.venv/bin/mypy --strict plugins/python
  plugins/python/.venv/bin/pytest plugins/python
  uv run --project plugins/python --extra dev python scripts/check-b4-gate-result.py --run-b5-smoke
  ```
- `unsafe_code = "deny"` workspace-wide; the only allowed unsafe is inside `pre_exec` / the fixture's `mmap` probe, each with a `SAFETY:` comment. Clippy runs `pedantic -D warnings`.
- No version bump: neither change touches the wire protocol (same as PRs #118–#120).
- Every operational constant touched must carry an ADR-035 four-axis comment (stated basis, override surface, retune trigger, coupling).
- Commit trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Deploy trap: the installed `loomweave` discovers `~/.local/bin/loomweave-plugin-python` (the standalone uv tool). Refresh BOTH venvs (see Task 8).
- Use Filigree atomically: `work_start <id> --advance` (bugs start at `triage`), close with `issue_close` after the merge.

---

## Part A — clarion-353c5b9aa5: language-server address-space ceiling

### File map (Part A)

- Modify `crates/loomweave-core/src/plugin/limits.rs` — add `LANGUAGE_SERVER_MAX_AS_MIB`, ADR-035 comments on it and `DEFAULT_MAX_RSS_MIB`.
- Modify `crates/loomweave-core/src/plugin/host.rs:103-109, 539-560` — add `effective_as_mib(&Manifest)` beside `effective_max_nproc`, use it in `pre_exec`; unit test.
- Modify `crates/loomweave-plugin-fixture/src/main.rs` — new opt-in knob `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB=<n>`: map `n` MiB `PROT_NONE` once, keep it, continue normally.
- Modify `crates/loomweave-cli/tests/wp2_e2e.rs` — two e2e tests (language-server manifest survives a 3 GiB reservation; plain manifest is OOM-killed by the same reservation).
- Modify `docs/loomweave/adr/ADR-021-plugin-authority-hybrid.md` §2d and the "Language-server plugins" consequence bullet.
- Modify `docs/loomweave/adr/ADR-035-operational-tuning-discipline.md` constant inventory.
- Modify `plugins/python/plugin.toml` comment on `expected_max_rss_mb`.

### Task 1: `LANGUAGE_SERVER_MAX_AS_MIB` + `effective_as_mib`

**Files:**
- Modify: `crates/loomweave-core/src/plugin/limits.rs:255-262`
- Modify: `crates/loomweave-core/src/plugin/host.rs:70, 103-109, 539-560, ~1394`

**Interfaces:**
- Produces: `pub const LANGUAGE_SERVER_MAX_AS_MIB: u64 = 8 * 1024;` in `limits.rs`; `fn effective_as_mib(manifest: &Manifest) -> u64` in `host.rs` (private, same visibility as `effective_max_nproc`).

- [ ] **Step 1: Claim the ticket and branch**

```bash
cd /home/john/loomweave && git checkout release/1.5.0 && git pull --ff-only
git checkout -b fix/clarion-353c5b9aa5-language-server-rlimit-as
filigree start-work clarion-353c5b9aa5 --advance --assignee claude --actor claude
```

- [ ] **Step 2: Write the failing unit test in `host.rs`**

Add directly after `pyright_runtime_leaves_process_ceiling_uncapped_for_language_server` (host.rs ~line 1403):

```rust
    #[test]
    fn language_server_plugins_get_the_wide_address_space_ceiling() {
        use crate::plugin::limits::{DEFAULT_MAX_RSS_MIB, LANGUAGE_SERVER_MAX_AS_MIB};
        // Ordinary plugins: min(manifest, core default) exactly as before.
        assert_eq!(
            effective_as_mib(&compliant_manifest()),
            effective_rss_mib(
                compliant_manifest().capabilities.runtime.expected_max_rss_mb,
                DEFAULT_MAX_RSS_MIB
            )
        );
        // Language-server plugins: V8 reserves virtual address space far beyond
        // its RSS (pyright died at 766 MB RSS under the 2 GiB RLIMIT_AS on a
        // 13.6k-line file), so the manifest's RSS expectation must NOT cap AS.
        assert_eq!(effective_as_mib(&pyright_manifest()), LANGUAGE_SERVER_MAX_AS_MIB);
        assert!(LANGUAGE_SERVER_MAX_AS_MIB > DEFAULT_MAX_RSS_MIB);
    }
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-core language_server_plugins_get_the_wide_address_space_ceiling`
Expected: compile error — `LANGUAGE_SERVER_MAX_AS_MIB` and `effective_as_mib` not found.

- [ ] **Step 4: Add the constant in `limits.rs`**

Replace the block at `limits.rs:255-262` with:

```rust
// ── apply_prlimit_as (ADR-021 §2d) ───────────────────────────────────────────

/// Default virtual-address space ceiling per ADR-021 §2d: **2 GiB**.
///
/// Applied via `RLIMIT_AS` in the plugin's child process before `exec`.
/// Task 6 calls `apply_prlimit_as` inside `CommandExt::pre_exec`.
///
/// ADR-035 declaration —
/// **Basis:** ADR-021 §2d; generous for a CPython/tree-sitter working set.
/// **Override surface:** none (internal; `loomweave.yaml:plugin_limits.max_rss_mib`
/// is promised by ADR-021 §4 and not yet implemented). Manifests may only lower it.
/// **Retune trigger:** an `LMWV-INFRA-PLUGIN-OOM-KILLED` finding from a
/// well-behaved non-language-server plugin.
/// **Coupling:** language-server plugins are exempt — see
/// [`LANGUAGE_SERVER_MAX_AS_MIB`] and `host::effective_as_mib`.
pub const DEFAULT_MAX_RSS_MIB: u64 = 2 * 1024; // 2 GiB

/// Address-space ceiling for plugins that declare
/// `[capabilities.runtime.pyright]` (they spawn a Node language server):
/// **8 GiB**, applied via `RLIMIT_AS` instead of [`DEFAULT_MAX_RSS_MIB`].
///
/// `RLIMIT_AS` bounds *virtual* address space. V8 reserves virtual regions
/// (code range, heap sandbox, per-isolate cages) far larger than the memory it
/// ever touches, so the 2 GiB ceiling killed `pyright-langserver` on a
/// 13.6k-line file while it was using ~766 MB of real memory — surfacing as a
/// self-inflicted `pyright_transport_failure` with no stderr (clarion-353c5b9aa5).
///
/// ADR-035 declaration —
/// **Basis:** measured on elspeth 2026-08-29: unlimited AS completed the file
/// (1,715 calls edges, 21.6 s); 2 GiB died at 766 MB RSS. 8 GiB leaves ~4×
/// headroom over Node's default old-space size on a 64-bit host.
/// **Override surface:** none (internal). The manifest's `expected_max_rss_mb`
/// is ignored for AS on these plugins — it documents RSS, not virtual space.
/// **Retune trigger:** an OOM-killed finding from a language-server plugin, or a
/// pyright major bump that changes V8's reservation strategy.
/// **Coupling:** `host::effective_as_mib`; `host::effective_max_nproc` (the
/// same manifest capability selects both exemptions); ADR-021 §2d.
pub const LANGUAGE_SERVER_MAX_AS_MIB: u64 = 8 * 1024; // 8 GiB
```

- [ ] **Step 5: Add `effective_as_mib` in `host.rs` and use it in `pre_exec`**

Extend the import at `host.rs:70` to
`use crate::plugin::limits::{DEFAULT_MAX_RSS_MIB, LANGUAGE_SERVER_MAX_AS_MIB, apply_prlimit_as, effective_rss_mib};`

Insert after `effective_max_nproc` (host.rs:109):

```rust
/// The `RLIMIT_AS` ceiling (MiB) to apply to a plugin child.
///
/// Plugins that declare the `pyright` runtime capability spawn a Node language
/// server whose V8 heap *reserves* far more virtual address space than it
/// touches; the manifest's `expected_max_rss_mb` describes resident memory and
/// must not cap virtual space for them. Every other plugin keeps ADR-021 §2d's
/// `min(manifest, DEFAULT_MAX_RSS_MIB)`.
fn effective_as_mib(manifest: &Manifest) -> u64 {
    if manifest.capabilities.runtime.pyright.is_some() {
        LANGUAGE_SERVER_MAX_AS_MIB
    } else {
        effective_rss_mib(
            manifest.capabilities.runtime.expected_max_rss_mb,
            DEFAULT_MAX_RSS_MIB,
        )
    }
}
```

Replace `host.rs:541-544`:

```rust
            let rss_mib = effective_rss_mib(
                manifest.capabilities.runtime.expected_max_rss_mb,
                DEFAULT_MAX_RSS_MIB,
            );
```
with
```rust
            let rss_mib = effective_as_mib(&manifest);
```

(`pre_exec` still captures `rss_mib` by Copy; the `SAFETY:` comment and the `apply_prlimit_as(rss_mib)?;` line are unchanged.)

- [ ] **Step 6: Run the unit tests**

Run: `cargo nextest run -p loomweave-core effective_ -- --no-capture 2>&1 | tail -5 && cargo nextest run -p loomweave-core language_server_plugins_get_the_wide_address_space_ceiling`
Expected: PASS (both the three existing `effective_rss_mib_*` tests and the new one).

- [ ] **Step 7: Update `host.rs` module doc (line ~27) and commit**

Change the doc sentence at host.rs:27 from “`CommandExt::pre_exec` to set `RLIMIT_AS` before `exec()`.” to
“`CommandExt::pre_exec` to set `RLIMIT_AS` before `exec()` (2 GiB by default; 8 GiB for language-server plugins — see `effective_as_mib`).”

```bash
cargo fmt --all && cargo clippy -p loomweave-core --all-targets --all-features -- -D warnings
git add crates/loomweave-core/src/plugin/limits.rs crates/loomweave-core/src/plugin/host.rs
git commit -m "fix(plugin-host): 8 GiB RLIMIT_AS ceiling for language-server plugins (clarion-353c5b9aa5)

V8 reserves virtual address space far beyond its RSS, so the 2 GiB
RLIMIT_AS inherited by pyright-langserver killed it at ~766 MB RSS on a
13.6k-line file. Plugins declaring [capabilities.runtime.pyright] now get
LANGUAGE_SERVER_MAX_AS_MIB; every other plugin keeps ADR-021 §2d's
min(manifest, 2 GiB).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

### Task 2: Fixture knob `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB`

**Files:**
- Modify: `crates/loomweave-plugin-fixture/src/main.rs:125-140, 280-330`

**Interfaces:**
- Produces: env knob `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB=<mib>` — at `analyze_file` dispatch, `mmap` `<mib>` MiB `PROT_NONE|MAP_PRIVATE|MAP_NORESERVE` once, leak the mapping, and continue the normal analyze path (emit `demo.sample` widget as usual). Under a 2 GiB `RLIMIT_AS` a 3 GiB request fails → the fixture reuses `terminate_after_rlimit_failure()` (SIGKILL) so the host reports `LMWV-INFRA-PLUGIN-OOM-KILLED` exactly as the existing OOM test expects. There is no Rust unit test for the fixture (it is a binary); Task 3's e2e tests cover it.

- [ ] **Step 1: Add the dispatch hook**

In `main.rs`, directly after the `LOOMWEAVE_FIXTURE_EXCEED_RLIMIT_AS` block (line ~136):

```rust
                // Reserve (but never touch) a large virtual mapping, then carry
                // on: models a Node/V8 language server whose *virtual* footprint
                // dwarfs its RSS. Under a tight RLIMIT_AS the mapping fails and
                // we die the way the real pyright did (clarion-353c5b9aa5).
                if let Some(mib) = std::env::var("LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB")
                    .ok()
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    #[cfg(unix)]
                    reserve_virtual_mib(mib);
                    #[cfg(not(unix))]
                    let _ = mib;
                }
```

- [ ] **Step 2: Add the helper next to `exceed_rlimit_as`**

```rust
/// Reserve `mib` MiB of untouched anonymous address space and keep it mapped
/// for the life of the process. Dies via [`terminate_after_rlimit_failure`]
/// when the kernel refuses (i.e. the host's `RLIMIT_AS` is below `mib`).
#[cfg(unix)]
fn reserve_virtual_mib(mib: usize) {
    use std::num::NonZeroUsize;

    let Some(length) = NonZeroUsize::new(mib.saturating_mul(1024 * 1024)) else {
        return;
    };
    // SAFETY: An anonymous PROT_NONE mapping is never dereferenced; it exists
    // only to charge the child's address-space accounting. The mapping is
    // intentionally leaked so it stays charged for the rest of the process.
    let mapped = {
        #[allow(unsafe_code)]
        unsafe {
            nix::sys::mman::mmap_anonymous(
                None,
                length,
                nix::sys::mman::ProtFlags::PROT_NONE,
                nix::sys::mman::MapFlags::MAP_PRIVATE | nix::sys::mman::MapFlags::MAP_NORESERVE,
            )
        }
    };
    if mapped.is_err() {
        terminate_after_rlimit_failure();
    }
}
```

- [ ] **Step 3: Build and manual-probe both outcomes**

```bash
cargo build -p loomweave-plugin-fixture
# survives with no limit (prints nothing, exits 0 on EOF):
printf '' | LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB=3072 target/debug/loomweave-plugin-fixture; echo "exit=$?"
# dies under a 2 GiB AS ceiling when analyze_file would run — verified end-to-end in Task 3
```
Expected: `exit=0` for the first command (no `analyze_file` was dispatched, so the hook is inert on an empty stream). If `MAP_NORESERVE` is not exposed by the pinned `nix` version, drop it — `PROT_NONE` alone is enough for the accounting.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all && cargo clippy -p loomweave-plugin-fixture --all-targets --all-features -- -D warnings
git add crates/loomweave-plugin-fixture/src/main.rs
git commit -m "test(fixture): LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB models a V8-style virtual footprint

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

### Task 3: e2e proof for both branches

**Files:**
- Modify: `crates/loomweave-cli/tests/wp2_e2e.rs` (after `wp2_rlimit_as_oom_kill_is_reported_as_host_finding`, ~line 322)

**Interfaces:**
- Consumes: Task 2's env knob; existing helpers `fixture_binary_path()`, `loomweave_bin()`, `setup_oom_plugin_dir()`; `FINDING_OOM_KILLED`.
- Produces: helper `setup_language_server_plugin_dir(&PathBuf) -> TempDir` (manifest identical to the OOM one but `plugin_id = "fixture"`, `extensions = ["ls"]`, `expected_max_rss_mb = 2048`, plus `[capabilities.runtime.pyright] pin = "1.1.409"`).

- [ ] **Step 1: Write the two failing tests**

```rust
#[cfg(target_os = "linux")]
fn setup_language_server_plugin_dir(fixture_bin: &PathBuf) -> TempDir {
    let plugin_dir = TempDir::new().expect("create langsrv plugin tempdir");
    let dest = plugin_dir.path().join("loomweave-plugin-langsrv");
    std::os::unix::fs::symlink(fixture_bin, &dest).expect("symlink loomweave-plugin-langsrv");
    let manifest = r#"
[plugin]
name = "loomweave-plugin-langsrv"
plugin_id = "fixture"
version = "0.1.0"
protocol_version = "1.0"
executable = "loomweave-plugin-langsrv"
language = "fixture"
extensions = ["ls"]

[capabilities.runtime]
expected_max_rss_mb = 2048
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[capabilities.runtime.pyright]
pin = "1.1.409"

[ontology]
entity_kinds = ["widget"]
edge_kinds = []
rule_id_prefix = "LMWV-LANGSRV-"
ontology_version = "0.1.0"
"#;
    fs::write(plugin_dir.path().join("plugin.toml"), manifest).expect("write langsrv plugin.toml");
    plugin_dir
}

/// clarion-353c5b9aa5: a language-server plugin's Node child reserves virtual
/// address space far beyond its RSS. The host must not kill it at 2 GiB.
#[test]
#[cfg(target_os = "linux")]
fn wp2_language_server_plugin_survives_a_3gib_virtual_reservation() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_language_server_plugin_dir(&fixture_bin);
    let project_dir = TempDir::new().expect("create project tempdir");
    loomweave_bin().args(["install", "--path"]).arg(project_dir.path()).assert().success();
    fs::write(project_dir.path().join("demo.ls"), b"sample\n").expect("write demo.ls");
    let new_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");

    let out = loomweave_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .env("LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB", "3072")
        .assert()
        .success();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        !stderr.contains(FINDING_OOM_KILLED),
        "language-server plugin was OOM-killed by RLIMIT_AS.\nstderr: {stderr}"
    );
    let conn = Connection::open(project_dir.path().join(".weft/loomweave/loomweave.db")).unwrap();
    let widgets: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities WHERE kind = 'widget'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(widgets, 1, "the reserved-but-untouched mapping must not stop extraction");
}

/// The exemption is keyed on the pyright capability alone: the same 3 GiB
/// reservation from an ordinary plugin still trips ADR-021 §2d's 2 GiB ceiling.
#[test]
#[cfg(target_os = "linux")]
fn wp2_ordinary_plugin_is_still_oom_killed_by_a_3gib_virtual_reservation() {
    let fixture_bin = fixture_binary_path();
    let plugin_dir = setup_oom_plugin_dir(&fixture_bin);
    let project_dir = TempDir::new().expect("create project tempdir");
    loomweave_bin().args(["install", "--path"]).arg(project_dir.path()).assert().success();
    fs::write(project_dir.path().join("demo.oom"), b"sample\n").expect("write demo.oom");
    let new_path =
        env::join_paths(std::iter::once(plugin_dir.path().to_path_buf())).expect("join_paths");

    let out = loomweave_bin()
        .args(["analyze"])
        .arg(project_dir.path())
        .env("PATH", &new_path)
        .env("LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB", "3072")
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains(FINDING_OOM_KILLED), "expected OOM finding.\nstderr: {stderr}");
}
```

- [ ] **Step 2: Run them to verify the first fails on the pre-fix host**

`git stash` is hard-blocked in this repo — instead verify the discriminating direction by temporarily running with the exemption disabled is NOT required; run as-is:

Run: `cargo build --workspace --bins && cargo nextest run -p loomweave-cli wp2_language_server_plugin_survives wp2_ordinary_plugin_is_still_oom_killed`
Expected: both PASS with Task 1 applied. To confirm the survive-test actually discriminates, run once with the ceiling forced low: `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB` cannot bypass the host, so instead temporarily change `LANGUAGE_SERVER_MAX_AS_MIB` to `2 * 1024`, re-run, expect the survive-test to FAIL with the OOM finding, then restore `8 * 1024`.

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
git add crates/loomweave-cli/tests/wp2_e2e.rs
git commit -m "test(e2e): language-server plugins survive a 3 GiB virtual reservation; ordinary plugins do not

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

### Task 4: ADR-021 / ADR-035 / plugin.toml documentation

**Files:**
- Modify: `docs/loomweave/adr/ADR-021-plugin-authority-hybrid.md:58, 132`
- Modify: `docs/loomweave/adr/ADR-035-operational-tuning-discipline.md` (constant inventory, the `DEFAULT_MAX_NOFILE` block ~line 120)
- Modify: `plugins/python/plugin.toml:13-17`

- [ ] **Step 1: Amend ADR-021 §2d (line 58)**

Append to the §2d paragraph:

```markdown
**Amendment 2026-08-29 (clarion-353c5b9aa5).** `RLIMIT_AS` bounds *virtual* address space, and a Node/V8 language server reserves virtual regions far larger than its resident set: the 2 GiB ceiling killed `pyright-langserver` at ~766 MB RSS on a 13.6k-line file. Plugins declaring `[capabilities.runtime.pyright]` therefore receive `LANGUAGE_SERVER_MAX_AS_MIB` (**8 GiB**) instead of `min(manifest, 2 GiB)`; the manifest's `expected_max_rss_mb` is documentation of RSS for those plugins and does not cap AS. Selection lives in `host::effective_as_mib`, keyed on the same capability that already exempts these plugins from `RLIMIT_NPROC`. Every other plugin keeps the 2 GiB rule above.
```

- [ ] **Step 2: Amend the consequence bullet (line 132)**

Replace “`RLIMIT_AS` (per-process memory) and the crash-loop counter remain in force;” with “`RLIMIT_AS` remains in force at the wider 8 GiB language-server ceiling (§2d amendment) together with the crash-loop counter;”.

- [ ] **Step 3: Add the constant to the ADR-035 inventory**

In the Rust inventory block that lists `DEFAULT_MAX_NOFILE (limits.rs)` / `DEFAULT_MAX_NPROC (limits.rs)`, add a line `LANGUAGE_SERVER_MAX_AS_MIB      (limits.rs, 8 GiB, language-server AS ceiling — clarion-353c5b9aa5)` and mark `DEFAULT_MAX_RSS_MIB` and `LANGUAGE_SERVER_MAX_AS_MIB` as carrying the four-axis declaration.

- [ ] **Step 4: Fix the `plugin.toml` comment**

Replace lines 13-17 of `plugins/python/plugin.toml` with:

```toml
# Plugin's declared RSS envelope (MiB). For plugins declaring
# [capabilities.runtime.pyright] this documents resident memory only: the
# host applies its 8 GiB language-server RLIMIT_AS ceiling
# (LANGUAGE_SERVER_MAX_AS_MIB, ADR-021 §2d amendment) because pyright's
# Node/V8 process reserves virtual space far beyond its RSS.
expected_max_rss_mb = 2048
```

- [ ] **Step 5: Commit**

```bash
git add docs/loomweave/adr/ADR-021-plugin-authority-hybrid.md docs/loomweave/adr/ADR-035-operational-tuning-discipline.md plugins/python/plugin.toml
git commit -m "docs(adr): ADR-021 §2d amendment — language-server RLIMIT_AS ceiling (clarion-353c5b9aa5)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

### Task 5: Part A gate, PR, merge

- [ ] **Step 1: Full CI floor** — run every command in Global Constraints; all must pass (nextest count ≥ 2406: 2404 + 2 new e2e; python 394 unchanged).

- [ ] **Step 2: Push and open the PR**

```bash
git push -u origin fix/clarion-353c5b9aa5-language-server-rlimit-as
gh pr create --base release/1.5.0 --title "fix(plugin-host): 8 GiB RLIMIT_AS ceiling for language-server plugins (clarion-353c5b9aa5)" --body "$(cat <<'EOF'
## Why
pyright-langserver (Node/V8) inherits the plugin's 2 GiB RLIMIT_AS and dies at ~766 MB RSS on elspeth's 13.6k-line sessions/service.py: V8 reserves virtual space far beyond its resident set. Measured: unlimited AS → 1,715 calls edges, complete, 21.6 s; 2 GiB → 13 edges, transport failure.

## What
- `LANGUAGE_SERVER_MAX_AS_MIB = 8 GiB` for plugins declaring `[capabilities.runtime.pyright]`; every other plugin keeps ADR-021 §2d's min(manifest, 2 GiB). Selection in `host::effective_as_mib`, keyed on the capability that already exempts these plugins from RLIMIT_NPROC.
- Fixture knob `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB`; two e2e tests prove the exemption and that ordinary plugins are still killed.
- ADR-021 §2d amendment, ADR-035 four-axis declarations, plugin.toml comment.

Ticket: clarion-353c5b9aa5. No wire change, no version bump.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Wait for CI, merge, record** — `gh pr checks --watch`; confirm with `gh run view <id> --json conclusion` (the `watch` exit code lies). Then `gh pr merge --admin --merge`, `git checkout release/1.5.0 && git pull --ff-only`, and `filigree comment clarion-353c5b9aa5 --actor claude "PR #<n> merged to release/1.5.0 @<sha>"`. Do NOT close the ticket until Task 8's elspeth acceptance.

---

## Part B — clarion-5d83413c36: warm-up call timeout

### File map (Part B)

- Modify `plugins/python/src/loomweave_plugin_python/pyright_session.py:189` — constant + ADR-035 comment.
- Modify `plugins/python/tests/test_pyright_session.py` — two tests near `test_pyright_session_file_deadline_scales_with_function_count` (~line 2200).
- Modify `docs/loomweave/adr/ADR-035-operational-tuning-discipline.md` inventory line; `docs/loomweave/adr/ADR-057-pyright-restart-attribution.md` “Operational note”.

### Task 6: Raise `PYRIGHT_CALL_TIMEOUT_SECS` to 30 s

**Files:**
- Modify: `plugins/python/src/loomweave_plugin_python/pyright_session.py:189`
- Test: `plugins/python/tests/test_pyright_session.py`

**Interfaces:**
- Consumes: `PyrightSession._budgeted_timeout(deadline) -> float` (line 1084, `min(self.call_timeout_secs, remaining)`), `PyrightSession._deadline_for_file(path, n_functions)`.
- Produces: `PYRIGHT_CALL_TIMEOUT_SECS == 30.0`.

- [ ] **Step 1: Claim and branch (from the updated release branch)**

```bash
cd /home/john/loomweave && git checkout release/1.5.0 && git pull --ff-only
git checkout -b fix/clarion-5d83413c36-warmup-call-timeout
filigree start-work clarion-5d83413c36 --advance --assignee claude --actor claude
```

- [ ] **Step 2: Write the failing tests**

Add after `test_pyright_session_file_deadline_scales_with_function_count`:

```python
def test_default_call_timeout_admits_a_large_file_warm_up_query() -> None:
    """clarion-5d83413c36: pyright's FIRST callHierarchy query on a big file
    triggers full analysis of that file and took >5 s on elspeth's 5k-line
    modules, aborting the whole calls pass. 30 s covers the measured warm-up
    (the files then finish in ~11 s total) while the file budget still bounds
    a wedged server.
    """
    assert PYRIGHT_CALL_TIMEOUT_SECS == 30.0
    assert PYRIGHT_CALL_TIMEOUT_SECS < PYRIGHT_FILE_TIMEOUT_CAP_SECS


def test_budgeted_timeout_grants_the_call_timeout_when_the_file_budget_is_larger(
    tmp_path: Path,
) -> None:
    session = PyrightSession(tmp_path, executable=sys.executable)
    path = tmp_path / "big.py"
    # 290 functions → base 10 + 0.25*290 = 82.5 s file budget (< 90 s cap).
    deadline = session._deadline_for_file(path, n_functions=290)  # noqa: SLF001
    grant = session._budgeted_timeout(deadline)  # noqa: SLF001
    assert 30.0 - 0.5 <= grant <= 30.0
    # ...and never more than what is left of the file budget.
    session._file_deadlines[path] = session._now() + 4.0  # noqa: SLF001
    assert session._budgeted_timeout(session._file_deadlines[path]) <= 4.0  # noqa: SLF001
```

Add `PYRIGHT_CALL_TIMEOUT_SECS` and `PYRIGHT_FILE_TIMEOUT_CAP_SECS` to the existing `from loomweave_plugin_python.pyright_session import (...)` block at the top of the test file if not already imported.

- [ ] **Step 3: Run to verify failure**

Run: `plugins/python/.venv/bin/pytest plugins/python/tests/test_pyright_session.py -k "warm_up_query or grants_the_call_timeout" -v`
Expected: the first FAILS (`5.0 == 30.0`); the second FAILS (`grant` is 5.0).

- [ ] **Step 4: Change the constant with its four-axis declaration**

Replace `pyright_session.py:189` (`PYRIGHT_CALL_TIMEOUT_SECS = 5.0`) with:

```python
# Per-LSP-request grant. The FIRST callHierarchy/definition query after
# ``didOpen`` on a large file makes pyright analyse the whole file before it
# answers; on 5k-line modules that warm-up alone exceeded 5 s, so one timeout
# aborted the calls pass with almost no evidence (clarion-5d83413c36) even
# though the file completed in ~11 s total once the first answer landed.
# ADR-035 —
# Basis: elspeth 2026-08-29, guided.py / pipeline_planner.py /
#   guided_chat_atomic.py: 5 s → 28/3/20 calls edges (degraded); 120 s →
#   633/461/153 edges (complete) in 11.2 / 12.6 / 10.3 s wall.
# Override surface: none (internal); ``PyrightSession(call_timeout_secs=...)``
#   for tests.
# Retune trigger: a ``pyright_timeout`` whose single request exceeded the
#   grant on a file that later completes with a larger grant.
# Coupling: the effective grant is ``min(this, remaining file budget)``
#   (``_budgeted_timeout``), so ``PYRIGHT_FILE_TIMEOUT_*`` and the host's
#   ``DEFAULT_PLUGIN_FILE_TIMEOUT`` (120 s) still bound a wedged server; the
#   ADR-057 wedge breaker counts files, not requests, and is unaffected.
PYRIGHT_CALL_TIMEOUT_SECS = 30.0
```

- [ ] **Step 5: Run the whole plugin suite**

Run: `plugins/python/.venv/bin/pytest plugins/python -q`
Expected: 396 passed (394 + 2). If any existing test pinned `5.0` implicitly through the default (search: `grep -n "5\.0" plugins/python/tests/test_pyright_session.py`), pass `call_timeout_secs=5.0` explicitly in that test's session constructor — the tests at lines 3852-3858 already pass `5.0` explicitly and need no change.

- [ ] **Step 6: Update ADR-035 inventory + ADR-057 operational note**

ADR-035: change `PYRIGHT_CALL_TIMEOUT_SECS        (5.0,    pyright_session.py:47)` to `PYRIGHT_CALL_TIMEOUT_SECS        (30.0,   pyright_session.py, four-axis declared — clarion-5d83413c36)`.

ADR-057 “Operational note” section: append one paragraph:

```markdown
**Per-request grant (2026-08-29, clarion-5d83413c36).** `PYRIGHT_CALL_TIMEOUT_SECS` is 30 s, not 5 s: the first query on a large file pays for pyright's whole-file analysis and routinely exceeded 5 s, which read as a self-inflicted `pyright_timeout` although the file completes in ~11 s once warm. The effective grant is still `min(30 s, remaining file budget)`, so a truly wedged server is detected within the file budget (≤ 90 s) and the three-file wedge breaker above is unchanged.
```

- [ ] **Step 7: Python gates + B.5 smoke, then commit**

```bash
plugins/python/.venv/bin/ruff check plugins/python && plugins/python/.venv/bin/ruff format --check plugins/python && plugins/python/.venv/bin/mypy --strict plugins/python
uv run --project plugins/python --extra dev python scripts/check-b4-gate-result.py --run-b5-smoke
git add plugins/python/src/loomweave_plugin_python/pyright_session.py plugins/python/tests/test_pyright_session.py docs/loomweave/adr/ADR-035-operational-tuning-discipline.md docs/loomweave/adr/ADR-057-pyright-restart-attribution.md
git commit -m "fix(python-plugin): 30 s per-request grant so a large file's warm-up query survives (clarion-5d83413c36)

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

### Task 7: Part B gate, PR, merge

- [ ] **Step 1: Full CI floor** (Global Constraints) — Rust legs are unaffected but run them anyway; python 396 passed; B.5 smoke passes.

- [ ] **Step 2: Push, PR, merge**

```bash
git push -u origin fix/clarion-5d83413c36-warmup-call-timeout
gh pr create --base release/1.5.0 --title "fix(python-plugin): 30 s per-request grant for large-file warm-up (clarion-5d83413c36)" --body "$(cat <<'EOF'
## Why
The first callHierarchy query after didOpen on a 5k-line file makes pyright analyse the whole file; it exceeded the 5 s per-request grant, and one timeout aborts the calls pass. Measured on elspeth: with a 120 s grant the same files complete in ~11 s total (guided.py 28 → 633 edges, pipeline_planner.py 3 → 461, guided_chat_atomic.py 20 → 153).

## What
- `PYRIGHT_CALL_TIMEOUT_SECS` 5 → 30 with an ADR-035 four-axis declaration. Effective grant stays `min(30 s, remaining file budget ≤ 90 s)` under the 120 s host watchdog; ADR-057 wedge breaker unchanged.
- Tests pin the constant and the min() interaction; ADR-035 inventory + ADR-057 operational note updated.

Ticket: clarion-5d83413c36. No wire change.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
gh pr checks --watch   # then confirm: gh run view <id> --json conclusion
gh pr merge --admin --merge
git checkout release/1.5.0 && git pull --ff-only
```

---

## Task 8: Deploy + elspeth acceptance + close both tickets

**Files:** none in-repo. Runs against `/home/john/elspeth` (`.weft/loomweave/loomweave.db`).

- [ ] **Step 1: Build and deploy the binary and BOTH plugin copies**

```bash
cd /home/john/loomweave && git checkout release/1.5.0 && git pull --ff-only
cargo build --release -p loomweave-cli
VENV=~/.local/share/uv/tools/loomweave
cp target/release/loomweave $VENV/bin/loomweave.tmp && mv -f $VENV/bin/loomweave.tmp $VENV/bin/loomweave
uv pip install --python $VENV/bin/python --no-deps --reinstall ./plugins/python
uv tool install --reinstall --force ./plugins/python
grep -n "PYRIGHT_CALL_TIMEOUT_SECS = 30.0" ~/.local/share/uv/tools/loomweave-plugin-python/lib/python*/site-packages/loomweave_plugin_python/pyright_session.py
strings $VENV/bin/loomweave | grep -c LANGUAGE_SERVER_MAX_AS_MIB || strings $VENV/bin/loomweave | grep -q "language-server" && echo binary-ok
```

- [ ] **Step 2: Reset exhausted redispatch budgets and run one analyze (detached)**

```bash
cd /home/john/elspeth
loomweave doctor --fix . 2>&1 | grep -i "redispatch\|resolution_coverage"
setsid nohup loomweave analyze . > /home/john/elspeth/.weft/loomweave/manual-analyze-acceptance.log 2>&1 < /dev/null &
```
Watch the log (`Monitor` on the file, or `tail -f`) until it prints the run summary; confirm the "discovered plugin … executable=/home/john/.local/bin/loomweave-plugin-python" line and no `LMWV-INFRA-PLUGIN-OOM-KILLED` / watchdog kill.

- [ ] **Step 3: Acceptance query**

```bash
sqlite3 -header /home/john/elspeth/.weft/loomweave/loomweave.db "
select source_file_id, calls_status, calls_reason, references_status, references_reason, redispatch_attempts
from source_file_resolution_coverage
where calls_status!='complete' or references_status!='complete';
select count(*) as transport_failures from source_file_resolution_coverage where calls_reason='pyright_transport_failure' or references_reason='pyright_transport_failure';
select count(*) as collateral from source_file_resolution_coverage where calls_collateral=1 or references_collateral=1;"
```
Expected: `transport_failures = 0`, `collateral = 0`; `sessions/service.py`, `guided.py`, `pipeline_planner.py`, `guided_chat_atomic.py` all `calls=complete`; residual rows = `tool_batch.py` (calls, `pyright_timeout`) and `sessions/service.py` (references, `reference_site_cap`) only. If `tool_batch.py` also completes, note it on clarion-bf3986e301.

- [ ] **Step 4: Record and close**

```bash
filigree comment clarion-353c5b9aa5 --actor claude "Acceptance on elspeth @<run id>: <paste the three query results>"
filigree comment clarion-5d83413c36 --actor claude "Acceptance on elspeth @<run id>: <paste>"
filigree update clarion-353c5b9aa5 --status <next working status per 'filigree transitions'> --actor claude   # walk to a closable status
filigree close clarion-353c5b9aa5 --actor claude
filigree close clarion-5d83413c36 --actor claude
filigree comment clarion-bf3986e301 --actor claude "Deps landed; re-measured residual: <tool_batch status>. Ready to confirm."
filigree comment clarion-731846eed1 --actor claude "Deps landed; sessions/service.py calls now complete; references still capped at 2000/4473 sites. Ready to decide."
```

Then update memory `partial-evidence-budget-shipped.md` (or a new `pyright-limits-shipped.md`) with the outcome and the elspeth counts.

---

## Self-review

- **Spec coverage:** ticket A — constant, selection function, pre_exec use, e2e both branches, ADR-021 amendment, plugin.toml comment, elspeth acceptance (`transport_failures = 0`, service.py complete) → Tasks 1-5, 8. Ticket B — constant, doc comment, tests, ADR-035 inventory, B.5 smoke, elspeth acceptance (three files complete) → Tasks 6-8. The ticket's optional "scale by file size" is deliberately not done (flat bump suffices on the evidence).
- **Placeholders:** none; the only "fill in" values are PR numbers / run ids / query outputs that exist only at execution time.
- **Type consistency:** `effective_as_mib(&Manifest) -> u64` is used identically in Task 1 test and pre_exec; `LANGUAGE_SERVER_MAX_AS_MIB: u64` throughout; env knob name `LOOMWEAVE_FIXTURE_RESERVE_VIRTUAL_MIB` identical in Tasks 2 and 3; `PYRIGHT_CALL_TIMEOUT_SECS` / `_budgeted_timeout` / `_deadline_for_file` names match `pyright_session.py`.
- **Risk noted for the executor:** if `nix`'s `MapFlags::MAP_NORESERVE` is unavailable on the pinned version, omit it (Task 2 step 3). If CI's runner has a low `ulimit -v` of its own, the survive-test could fail for a reason unrelated to the host — check `ulimit -v` in the CI log before debugging the host.
