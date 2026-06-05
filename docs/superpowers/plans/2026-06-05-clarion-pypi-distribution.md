# Clarion PyPI Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Clarion via PyPI so `uvx clarion install --path .` installs the Rust binary + Python plugin and `clarion analyze` discovers/spawns the plugin with no extra flags.

**Architecture:** Two PyPI packages — `clarion` (maturin `bindings="bin"` platform wheels) depending on `clarion-plugin-python` (pure-Python wheel). The enabling change is a new `current_exe()`-relative plugin-discovery level so the co-located plugin is found without being on `$PATH`. Publishing via PyPI Trusted Publishing; cosign GitHub Release tarballs stay as the offline channel.

**Tech Stack:** Rust (cargo workspace, `clarion-core`/`clarion-cli`), maturin + PyO3/maturin-action, hatchling, uv, GitHub Actions, PyPI Trusted Publishing (OIDC, PEP 740).

**Spec:** `docs/superpowers/specs/2026-06-05-clarion-pypi-distribution-design.md`

---

## File map

| File | Change | Responsibility |
|------|--------|----------------|
| `crates/clarion-core/src/plugin/discovery.rs` | Modify | Add `current_exe()`-relative discovery level + tests |
| `docs/adr/ADR-021-*.md` | Modify | Amend to record the new discovery source |
| `crates/clarion-cli/pyproject.toml` | Create | maturin `bin` wheel config for `clarion` |
| `plugins/python/pyproject.toml` | Modify | Confirm wheel build target (already present) |
| `scripts/check-workspace-version-lockstep.py` | Modify | Assert `clarion → clarion-plugin-python` pin == workspace version |
| `.github/workflows/wheels.yml` | Create | Build standard-4 wheels + plugin wheel + publish (Trusted Publishing) |
| `crates/clarion-cli/src/install.rs` (or equivalent) | Modify | Prime pyright/Node cache during `clarion install` |
| `crates/clarion-cli/tests/discovery_co_located.rs` | Create | Integration test: plugin found via co-location, not `$PATH` |
| `README.md` | Modify | Collapse install to `uvx clarion …`; note first-run Node fetch |

Locate the ADR file first: `ls docs/adr | grep -i 021`.

---

## Phase 0: Manual prerequisites (one-time, cannot be scripted)

### Task 0: Register PyPI Trusted Publishers

**Files:** none (PyPI web UI)

- [ ] **Step 1: Create/claim PyPI projects and configure trusted publishing**

On https://pypi.org for **both** `clarion` and `clarion-plugin-python`, add a
GitHub Actions trusted publisher:
- Owner: `tachyon-beep`, Repo: `clarion`
- Workflow filename: `wheels.yml`
- Environment name: `pypi` (matches the workflow `environment:` in Task 12)

Do the same on https://test.pypi.org for the TestPyPI dry-run (Task 14).

- [ ] **Step 2: Confirm the names are available / owned**

Verify `clarion` is registerable (or already owned). If taken, STOP and escalate
to the owner — the package name is load-bearing for the whole design.

---

## Phase 1: Discovery change (Rust, TDD — PyPI-independent foundation)

This is the enabling change and is fully testable with no packaging. Do it first.

### Task 1: Add a failing test for co-located (off-`$PATH`) discovery

**Files:**
- Test: `crates/clarion-core/src/plugin/discovery.rs` (test module, after `t2_install_prefix_fallback`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(all(test, unix))] mod tests` block:

```rust
    // ── T2b: current_exe() sibling level (install-prefix, NOT on $PATH) ────────

    #[test]
    fn t2b_exe_dir_install_prefix_found_when_not_on_path() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();

        make_executable(&bin.join("clarion-plugin-mocktest"));
        let share = tmp
            .path()
            .join("share/clarion/plugins/mocktest");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();

        // $PATH is EMPTY — the plugin is only reachable via the exe dir.
        let results = discover_on_path_and_exe_dir(
            std::ffi::OsStr::new(""),
            Some(bin.as_path()),
        );
        assert_eq!(results.len(), 1, "exe-dir plugin should be discovered");

        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(plugin.manifest.plugin.plugin_id, "mocktest");
        assert_eq!(
            plugin.manifest_path,
            tmp.path().join("share/clarion/plugins/mocktest/plugin.toml")
        );
    }

    #[test]
    fn t2c_path_entry_shadows_same_named_exe_dir_sibling() {
        // A plugin on $PATH wins over a same-named sibling next to the binary.
        let tmp = TempDir::new().unwrap();
        let path_bin = tmp.path().join("pathbin");
        let exe_bin = tmp.path().join("exebin");
        fs::create_dir_all(&path_bin).unwrap();
        fs::create_dir_all(&exe_bin).unwrap();

        make_executable(&path_bin.join("clarion-plugin-mocktest"));
        fs::write(path_bin.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();
        make_executable(&exe_bin.join("clarion-plugin-mocktest"));
        fs::write(exe_bin.join("plugin.toml"), minimal_manifest_toml("mocktest")).unwrap();

        let results =
            discover_on_path_and_exe_dir(&path_os(&[&path_bin]), Some(exe_bin.as_path()));
        assert_eq!(results.len(), 1, "duplicate name must be de-duplicated");
        let plugin = results.into_iter().next().unwrap().unwrap();
        assert_eq!(
            plugin.executable,
            path_bin.join("clarion-plugin-mocktest"),
            "$PATH entry must shadow the exe-dir sibling"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p clarion-core plugin::discovery::tests::t2b -- --nocapture`
Expected: FAIL — `cannot find function discover_on_path_and_exe_dir in this scope`.

### Task 2: Refactor the per-directory scan into a helper + add the new seam

**Files:**
- Modify: `crates/clarion-core/src/plugin/discovery.rs` (the `#[cfg(unix)] pub fn discover_on_path` and `pub fn discover`)

- [ ] **Step 1: Extract `scan_dir` and add `discover_on_path_and_exe_dir`**

Replace the existing `#[cfg(unix)] pub fn discover_on_path(...) { ... }` body
(the function spanning the for-loop over `split_paths`) with:

```rust
#[cfg(unix)]
pub fn discover_on_path(path_env: &OsStr) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    discover_on_path_and_exe_dir(path_env, None)
}

/// Like [`discover_on_path`], but additionally scans `exe_dir` (the directory of
/// the running `clarion` binary) **after** the `$PATH` entries. `$PATH` entries
/// are scanned first, so a plugin found on `$PATH` shadows a same-named sibling
/// next to the binary (first-match-wins, consistent with PATH shadowing).
///
/// This is the discovery source that makes a PyPI/venv install work: the plugin
/// console script is co-located in the same `bin/` as `clarion` but is not on
/// the user's `$PATH`. See ADR-021.
#[cfg(unix)]
pub fn discover_on_path_and_exe_dir(
    path_env: &OsStr,
    exe_dir: Option<&std::path::Path>,
) -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    let mut results = Vec::new();
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    let path_dirs: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    let exe_dirs = exe_dir.map(std::path::Path::to_path_buf).into_iter();
    for dir in path_dirs.into_iter().chain(exe_dirs) {
        scan_dir(&dir, &mut seen_dirs, &mut seen_names, &mut results);
    }

    results
}

/// Scan a single directory for `clarion-plugin-*` executables, appending results.
/// Shared by every discovery source; honours dir/name de-duplication and the
/// world-writable refusal (ADR-021).
#[cfg(unix)]
fn scan_dir(
    dir: &std::path::Path,
    seen_dirs: &mut HashSet<PathBuf>,
    seen_names: &mut HashSet<String>,
    results: &mut Vec<Result<DiscoveredPlugin, DiscoveryError>>,
) {
    if dir.as_os_str().is_empty() {
        return;
    }
    let canonical_dir = match dir.canonicalize() {
        Ok(c) => c,
        Err(_) => dir.to_path_buf(),
    };
    if !seen_dirs.insert(canonical_dir) {
        return;
    }
    if is_world_writable(dir) {
        results.push(Err(DiscoveryError::WorldWritableDir {
            path: dir.to_path_buf(),
        }));
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry_result in entries {
        let Ok(entry) = entry_result else {
            continue;
        };
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        let suffix = match extract_plugin_suffix(&file_name) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        if !seen_names.insert(file_name.clone()) {
            continue;
        }
        let exec_path = dir.join(&file_name);
        if !is_executable(&exec_path) {
            continue;
        }
        results.push(load_plugin(exec_path, &suffix));
    }
}
```

- [ ] **Step 2: Wire `discover()` to pass the current_exe dir**

Replace the `#[cfg(unix)] pub fn discover()` body with:

```rust
#[cfg(unix)]
pub fn discover() -> Vec<Result<DiscoveredPlugin, DiscoveryError>> {
    let path_val = std::env::var_os("PATH").unwrap_or_default();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    discover_on_path_and_exe_dir(&path_val, exe_dir.as_deref())
}
```

- [ ] **Step 3: Run the new tests to verify they pass**

Run: `cargo test -p clarion-core plugin::discovery::tests::t2b -- --nocapture`
Run: `cargo test -p clarion-core plugin::discovery::tests::t2c -- --nocapture`
Expected: PASS (both).

- [ ] **Step 4: Run the full discovery test module to verify no regressions**

Run: `cargo test -p clarion-core plugin::discovery`
Expected: all existing T1–T7 tests + the two new tests PASS.

- [ ] **Step 5: Lint + format**

Run: `cargo fmt --all -- --check && cargo clippy -p clarion-core --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/clarion-core/src/plugin/discovery.rs
git commit -m "feat(plugin): discover clarion-plugin-* next to the running binary

Adds a current_exe()-relative discovery level so a PyPI/venv-installed plugin
co-located in the same bin/ is found without being on \$PATH. \$PATH entries are
scanned first (shadowing preserved). World-writable refusal + de-dup unchanged."
```

### Task 3: End-to-end integration test (real binary, plugin off `$PATH`)

**Files:**
- Create: `crates/clarion-cli/tests/discovery_co_located.rs`

This proves the *production* `discover()` path (via `current_exe()`) finds a
co-located plugin when the staging dir is NOT on `$PATH` — the empirical gate
for the whole design (spec §7).

- [ ] **Step 1: Inspect an existing analyze/discovery integration test for the harness pattern**

Run: `sed -n '1,80p' crates/clarion-cli/tests/analyze_failure_modes.rs`
Note how it builds a fake plugin dir, sets `PATH`, and invokes the `clarion`
binary (via `assert_cmd`/`Command::cargo_bin` or `env!("CARGO_BIN_EXE_clarion")`).

- [ ] **Step 2: Write the failing test**

```rust
//! Proves the production discovery path finds a plugin co-located in the same
//! directory as the `clarion` binary even when that directory is NOT on $PATH.
//! This is the PyPI/venv install scenario (spec 2026-06-05-clarion-pypi-distribution).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Stage <stage>/clarion (copy of the test binary) + a sibling
/// clarion-plugin-mocktest + <stage>/../share/clarion/plugins/mocktest/plugin.toml,
/// then run `clarion` from <stage> with an empty PATH and assert the plugin is
/// listed as discovered.
#[test]
#[cfg(unix)]
fn co_located_plugin_discovered_off_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();

    // Copy the built clarion binary into the staged bin/.
    let real = env!("CARGO_BIN_EXE_clarion");
    let staged = bin.join("clarion");
    fs::copy(real, &staged).unwrap();
    set_exec(&staged);

    // Sibling plugin executable + install-prefix manifest.
    let plugin_exe = bin.join("clarion-plugin-mocktest");
    fs::write(&plugin_exe, b"#!/bin/sh\nexit 0\n").unwrap();
    set_exec(&plugin_exe);
    let share = tmp.path().join("share/clarion/plugins/mocktest");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("plugin.toml"), MOCK_MANIFEST).unwrap();

    // Run the staged binary with an EMPTY PATH (so discovery can only succeed
    // via the current_exe() level), using the plugin-list/doctor surface.
    let output = std::process::Command::new(&staged)
        .arg("doctor") // adjust to the subcommand that reports discovered plugins
        .env("PATH", "")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("mocktest"),
        "expected plugin 'mocktest' to be discovered off-PATH; got:\n{combined}"
    );
}

#[cfg(unix)]
fn set_exec(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

const MOCK_MANIFEST: &str = r#"[plugin]
name = "clarion-plugin-mocktest"
plugin_id = "mocktest"
version = "0.1.0"
protocol_version = "1.0"
executable = "clarion-plugin-mocktest"
language = "mocktest"
extensions = ["mt"]

[capabilities.runtime]
expected_max_rss_mb = 256
expected_entities_per_file = 100
wardline_aware = false
reads_outside_project_root = false

[ontology]
entity_kinds = ["function"]
edge_kinds = ["calls"]
rule_id_prefix = "CLA-MT-"
ontology_version = "0.1.0"
"#;
```

- [ ] **Step 3: Pick the reporting subcommand**

Run: `cargo run -p clarion-cli -- doctor --help` and `cargo run -p clarion-cli -- analyze --help`.
Choose the subcommand that surfaces discovered plugins (likely `doctor`); if
none prints plugin names, add a `--plugins`/`--json` reporting flag in this task
and update the assertion. Document the chosen command in the test comment.

- [ ] **Step 4: Run the test**

Run: `cargo test -p clarion-cli --test discovery_co_located`
Expected: PASS — proves co-located, off-`$PATH` discovery works end to end.

- [ ] **Step 5: Commit**

```bash
git add crates/clarion-cli/tests/discovery_co_located.rs
git commit -m "test(cli): co-located plugin discovered off \$PATH (PyPI install scenario)"
```

---

## Phase 2: ADR-021 amendment

### Task 4: Record the new discovery source in ADR-021

**Files:**
- Modify: `docs/adr/ADR-021-*.md` (find exact name first)

- [ ] **Step 1: Append an amendment section**

Add (adjust heading level to the file's convention):

```markdown
## Amendment (2026-06-05): current_exe()-relative discovery source

In addition to `$PATH` scanning, plugin discovery scans the directory containing
the running `clarion` binary (`std::env::current_exe()` parent). This is required
for PyPI/venv installs, where the `clarion-plugin-*` console script is co-located
in the same `bin/` as `clarion` but is not on the user's `$PATH` and the venv is
not activated.

Trust model: unchanged in spirit. The directory holding the running `clarion`
binary is implicitly as trusted as that binary — an attacker who can write a
sibling `clarion-plugin-*` there can already replace `clarion` itself. The
**world-writable refusal** and the **first-match-wins de-duplication** apply to
this source identically. Ordering: `$PATH` entries are scanned first, then the
`current_exe()` directory, so an operator's explicit PATH-installed plugin
shadows a co-located sibling of the same name.
```

- [ ] **Step 2: Commit**

```bash
git add docs/adr/ADR-021-*.md
git commit -m "docs(adr-021): record current_exe()-relative plugin discovery source"
```

---

## Phase 3: Plugin wheel + version lockstep

### Task 5: Confirm the plugin builds a wheel

**Files:** `plugins/python/pyproject.toml` (already wheel-capable via hatchling)

- [ ] **Step 1: Build the wheel locally**

Run: `uv build --wheel --project plugins/python --out-dir dist/`
Expected: `dist/clarion_plugin_python-1.3.0-py3-none-any.whl` produced.

- [ ] **Step 2: Verify the wheel ships plugin.toml as shared-data**

Run: `python -m zipfile -l dist/clarion_plugin_python-1.3.0-py3-none-any.whl | grep -E 'plugin.toml|data/share'`
Expected: an entry like `clarion_plugin_python-1.3.0.data/data/share/clarion/plugins/python/plugin.toml`.
If absent, fix `[tool.hatch.build.targets.wheel.shared-data]` so the wheel (not just sdist) routes `plugin.toml`.

- [ ] **Step 3: Commit (only if pyproject changed)**

```bash
git add plugins/python/pyproject.toml
git commit -m "build(plugin): ensure wheel ships plugin.toml shared-data"
```

### Task 6: Extend the version-lockstep guard

**Files:**
- Modify: `scripts/check-workspace-version-lockstep.py`

- [ ] **Step 1: Read the current guard**

Run: `cat scripts/check-workspace-version-lockstep.py` — note how it parses the
workspace version and what it currently compares (likely `Cargo.toml` vs
`plugins/python/pyproject.toml`).

- [ ] **Step 2: Add an assertion for the clarion→plugin dependency pin**

Add a check that `crates/clarion-cli/pyproject.toml` (created in Task 7) declares
`clarion-plugin-python==<workspace version>` in `[project.dependencies]`, and a
self-test fixture (mirroring the file's existing `--self-test` style) for the
mismatch and match cases. Concretely, add:

```python
def check_clarion_plugin_pin(workspace_version: str, repo_root: Path) -> list[str]:
    """The clarion wheel must pin clarion-plugin-python to the workspace version."""
    pyproject = repo_root / "crates" / "clarion-cli" / "pyproject.toml"
    if not pyproject.exists():
        return [f"missing {pyproject} (clarion maturin wheel config)"]
    data = tomllib.loads(pyproject.read_text())
    deps = data.get("project", {}).get("dependencies", [])
    expected = f"clarion-plugin-python=={workspace_version}"
    if expected not in deps:
        return [
            f"{pyproject}: expected dependency '{expected}', found {deps!r}"
        ]
    return []
```

Wire its result into the script's existing error aggregation/exit path, and add
the `--self-test` fixtures matching the script's existing pattern.

- [ ] **Step 3: Run the guard self-test + live**

Run: `python scripts/check-workspace-version-lockstep.py --self-test && python scripts/check-workspace-version-lockstep.py`
Expected: self-test passes; live passes once Task 7 lands (run live again after Task 7).

- [ ] **Step 4: Commit**

```bash
git add scripts/check-workspace-version-lockstep.py
git commit -m "build: lockstep-guard the clarion->clarion-plugin-python pin"
```

---

## Phase 4: clarion maturin bin-wheel

### Task 7: Add maturin pyproject for the `clarion` binary

**Files:**
- Create: `crates/clarion-cli/pyproject.toml`

- [ ] **Step 1: Write the pyproject**

```toml
[build-system]
requires = ["maturin>=1.7,<2"]
build-backend = "maturin"

[project]
name = "clarion"
version = "1.3.0"
description = "Clarion — graph-aware code archaeology (Rust core)"
readme = "../../README.md"
requires-python = ">=3.11"
license = { text = "MIT" }
authors = [{ name = "John Morrissey", email = "qacona@gmail.com" }]
classifiers = [
    "Development Status :: 4 - Beta",
    "Programming Language :: Rust",
    "Programming Language :: Python :: 3",
]
dependencies = ["clarion-plugin-python==1.3.0"]

[project.urls]
Repository = "https://github.com/tachyon-beep/clarion"

[tool.maturin]
# Package the `clarion` binary into the wheel (the ruff/uv pattern), not a
# Python extension module.
bindings = "bin"
manifest-path = "Cargo.toml"
# Build only the `clarion` bin target.
bins = ["clarion"]
strip = true
```

Note: keep `version` in sync via the Task 6 lockstep guard. If maturin rejects
`bins` for the installed version, drop that key (maturin builds the crate's
default bin; `clarion-cli` defines a single `[[bin]] name = "clarion"`).

- [ ] **Step 2: Build a wheel locally for the host platform**

Run: `cd crates/clarion-cli && uvx maturin build --release && cd ../..`
Expected: a wheel under `target/wheels/clarion-1.3.0-*.whl`.

- [ ] **Step 3: Verify the wheel carries the binary as a script**

Run: `python -m zipfile -l target/wheels/clarion-1.3.0-*.whl | grep -E 'clarion(\.exe)?$|scripts/clarion'`
Expected: a `clarion-1.3.0.data/scripts/clarion` entry (the binary).

- [ ] **Step 4: Full local install-and-spawn proof (the critical gate)**

```bash
python -m venv /tmp/clarion-venv
/tmp/clarion-venv/bin/pip install \
  dist/clarion_plugin_python-1.3.0-py3-none-any.whl \
  target/wheels/clarion-1.3.0-*.whl
# Run with the venv bin NOT on PATH except for clarion itself via absolute path:
env -i HOME=/tmp /tmp/clarion-venv/bin/clarion doctor
```
Expected: `doctor` reports the `python` plugin discovered (via the Task 2
`current_exe()` level — the venv `bin/` is not on the scrubbed `$PATH`). This is
the local equivalent of the spec §7 smoke test; if it fails, STOP — the topology
assumption is broken and must be revisited before CI.

- [ ] **Step 5: Run the live lockstep guard**

Run: `python scripts/check-workspace-version-lockstep.py`
Expected: PASS (clarion pyproject now exists with the correct pin).

- [ ] **Step 6: Commit**

```bash
git add crates/clarion-cli/pyproject.toml
git commit -m "build(clarion): maturin bin-wheel config; depends on clarion-plugin-python"
```

---

## Phase 5: CI — build wheels & publish

### Task 8: Wheels workflow — build matrix (standard-4)

**Files:**
- Create: `.github/workflows/wheels.yml`

- [ ] **Step 1: Write the build jobs**

```yaml
name: wheels

on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  build-plugin:
    name: Build clarion-plugin-python wheel
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: astral-sh/setup-uv@v5
      - run: uv build --wheel --sdist --project plugins/python --out-dir dist/
      - uses: actions/upload-artifact@v4
        with:
          name: dist-plugin
          path: dist/*

  build-clarion:
    name: Build clarion wheel (${{ matrix.target }})
    runs-on: ${{ matrix.runner }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            runner: ubuntu-latest
            manylinux: "2_28"
          - target: aarch64-unknown-linux-gnu
            runner: ubuntu-latest
            manylinux: "2_28"
          - target: universal2-apple-darwin   # covers arm64 + x86_64 macs
            runner: macos-14
            manylinux: "off"
    steps:
      - uses: actions/checkout@v4
      - uses: PyO3/maturin-action@v1
        with:
          command: build
          args: --release --manifest-path crates/clarion-cli/Cargo.toml --out dist
          target: ${{ matrix.target }}
          manylinux: ${{ matrix.manylinux }}
          # aarch64 linux + bundled C (SQLite) cross-compiles via zig.
          sccache: "true"
      - uses: actions/upload-artifact@v4
        with:
          name: dist-clarion-${{ matrix.target }}
          path: dist/*
```

Note: `maturin-action` reads `crates/clarion-cli/pyproject.toml` via the
`--manifest-path`. For aarch64-linux with bundled SQLite, if zig cross fails,
switch that matrix entry to a native arm64 runner (`ubuntu-24.04-arm`) and drop
`manylinux` cross.

- [ ] **Step 2: Validate the workflow on a branch via workflow_dispatch**

Push the branch, then:
Run: `gh workflow run wheels.yml --ref <branch>` then `gh run watch`
Expected: all `build-clarion` matrix legs + `build-plugin` succeed and upload
artifacts. (No publish yet — that's Task 12.)

- [ ] **Step 3: Download and smoke one Linux wheel**

Run: `gh run download <run-id> -n dist-clarion-x86_64-unknown-linux-gnu`
then repeat the Task 7 Step 4 install-and-spawn proof on the downloaded wheel.
Expected: plugin discovered.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/wheels.yml
git commit -m "ci: build standard-4 clarion wheels + plugin wheel"
```

### Task 9: Clean-room sdist build test (decide keep/drop sdist)

**Files:** `.github/workflows/wheels.yml` (add a job)

- [ ] **Step 1: Add an sdist build-from-source job**

```yaml
  test-sdist:
    name: clarion sdist builds from source
    runs-on: ubuntu-latest
    needs: build-clarion
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: astral-sh/setup-uv@v5
      - name: build clarion sdist
        run: uvx maturin sdist --manifest-path crates/clarion-cli/Cargo.toml --out dist
      - name: install sdist in a clean venv (compiles from source)
        run: |
          python -m venv /tmp/sd && /tmp/sd/bin/pip install dist/clarion-*.tar.gz
          /tmp/sd/bin/clarion --version
```

- [ ] **Step 2: Run it via workflow_dispatch**

Run: `gh workflow run wheels.yml --ref <branch>` and watch `test-sdist`.
- If it PASSES: keep the sdist; include it in the publish set (Task 12).
- If it FAILS (workspace path-deps / bundled C don't vendor cleanly): **drop the
  clarion sdist** — remove this job and the sdist from the publish set, and ensure
  the README routes unsupported platforms to the GitHub Release / cargo-binstall
  with a clear message. Record the decision in the spec's §5.

- [ ] **Step 3: Commit the decision**

```bash
git add .github/workflows/wheels.yml docs/superpowers/specs/2026-06-05-clarion-pypi-distribution-design.md
git commit -m "ci: clarion sdist clean-room build test (records sdist keep/drop decision)"
```

### Task 10: Publish job — Trusted Publishing (gated)

**Files:** `.github/workflows/wheels.yml` (add a publish job)

- [ ] **Step 1: Add the publish job (plugin first, then clarion)**

```yaml
  publish:
    name: Publish to PyPI
    needs: [build-plugin, build-clarion, test-sdist]
    if: github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    environment: pypi
    permissions:
      id-token: write   # OIDC for Trusted Publishing
    steps:
      - uses: actions/download-artifact@v4
        with: { path: artifacts }
      - name: stage plugin dist
        run: mkdir -p plugin-dist && cp artifacts/dist-plugin/* plugin-dist/
      - name: stage clarion dist
        run: |
          mkdir -p clarion-dist
          cp artifacts/dist-clarion-*/* clarion-dist/
      # Publish the plugin FIRST so clarion's ==pin resolves on first release.
      - name: publish clarion-plugin-python
        uses: pypa/gh-action-pypi-publish@release/v1
        with:
          packages-dir: plugin-dist
          attestations: true
      - name: publish clarion
        uses: pypa/gh-action-pypi-publish@release/v1
        with:
          packages-dir: clarion-dist
          attestations: true
```

- [ ] **Step 2: Reuse the release gate**

Confirm the existing `release.yml` `verify` job (tag-on-main ancestry + full CI)
still runs on the same `v*` tag. The `wheels.yml` publish must not race ahead of
`verify`. Either (a) add the same `verify` job as a `needs:` dependency inside
`wheels.yml`, or (b) merge the wheels jobs into `release.yml` after `verify`.
Recommended: option (b) — one tag-triggered workflow, single gate. If merging,
move Tasks 8–10 jobs under `release.yml` with `needs: verify`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/
git commit -m "ci: publish clarion + plugin via PyPI Trusted Publishing (gated on verify)"
```

---

## Phase 6: `clarion install` Node cache-warm + docs

### Task 11: Prime the pyright/Node cache during `clarion install`

**Files:**
- Modify: `crates/clarion-cli/src/install.rs` (or the module implementing `Command::Install`; find with `grep -rn "fn .*install" crates/clarion-cli/src`)

- [ ] **Step 1: Find the install entry point and plugin location**

Run: `grep -rn "Install" crates/clarion-cli/src/cli.rs crates/clarion-cli/src/*.rs | head`
Identify where `clarion install` runs its setup steps.

- [ ] **Step 2: Add a best-effort cache-warm step**

After the existing install steps, spawn the discovered python plugin's
cache-warm path (it already depends on pyright/nodeenv). Add a step that runs the
plugin's pyright once on an empty input so `nodeenv` fetches Node at a
predictable online moment, e.g.:

```rust
// Best-effort: prime pyright's Node runtime so the first `analyze` doesn't
// block on a surprise network fetch (spec §6). Failure is non-fatal — offline
// installs simply defer the fetch to first analyze.
if let Err(e) = warm_pyright_cache() {
    tracing::info!(error = %e, "pyright/node cache warm skipped (will fetch on first analyze)");
}
```

Implement `warm_pyright_cache()` to invoke the discovered `clarion-plugin-python`
with its cache-warm/`--version` subcommand (confirm the plugin exposes one with
`clarion-plugin-python --help`; if not, add a `warm`/`--prime` subcommand to the
plugin in this task). Gate the whole step behind a `--no-cache-warm` install flag
and skip automatically when offline detection fails.

- [ ] **Step 3: Test**

Run: `cargo test -p clarion-cli install`
Expected: existing install tests pass; add a test asserting `--no-cache-warm`
skips the step and that a failing warm does not fail `install`.

- [ ] **Step 4: Commit**

```bash
git add crates/clarion-cli/src/ plugins/python/
git commit -m "feat(install): prime pyright/Node cache (best-effort, --no-cache-warm to skip)"
```

### Task 12: README install rewrite

**Files:**
- Modify: `README.md` (the install section around lines 60–95)

- [ ] **Step 1: Replace the 3-step tarball dance**

```markdown
## Install

```bash
# Recommended: one command (Linux x86_64/aarch64, macOS).
uvx clarion install --path .
# or, persistent:
pipx install clarion && clarion install --path .
```

`clarion install` initialises `.clarion/`, installs the agent skills, writes
Claude Code / Codex MCP config, and installs the SessionStart hook.

> **First run fetches a Node runtime.** The Python plugin uses pyright, which
> downloads a pinned Node runtime on first use. `clarion install` primes this
> cache while you're online; pass `--no-cache-warm` to defer it.

### Offline / verified-binary install

Prefer a cosign-signed binary, or on an unsupported platform:

```bash
TAG=v1.3.0
curl -L -O "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-x86_64-unknown-linux-gnu.tar.gz"
# verify signature (see release notes), extract, place on PATH, then:
pipx install "https://github.com/tachyon-beep/clarion/releases/download/${TAG}/clarion-plugin-python-1.3.0.tar.gz"
```
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: PyPI one-command install; note first-run Node fetch"
```

---

## Phase 7: Dry-run gate

### Task 13: TestPyPI dry-run before first real publish

**Files:** none (operational), optionally a temporary `publish` target override

- [ ] **Step 1: Publish to TestPyPI from a pre-release tag or dispatch**

Temporarily point the publish job at TestPyPI (`repository-url:
https://test.pypi.org/legacy/`) via a `workflow_dispatch` input, or push a
pre-release tag. Ensure the TestPyPI trusted publisher (Task 0) is configured.

- [ ] **Step 2: Install from TestPyPI on each platform and prove discovery**

On a Linux x86_64, Linux aarch64, and macOS host (or CI matrix), run:
```bash
uvx --index-url https://test.pypi.org/simple/ \
    --extra-index-url https://pypi.org/simple/ \
    clarion doctor
```
Expected: `clarion` runs and reports the `python` plugin discovered. The
`--extra-index-url` lets the real PyPI satisfy pyright/transitive deps.

- [ ] **Step 3: Revert the TestPyPI override**

Restore the publish job to real PyPI. Commit any workflow revert.

- [ ] **Step 4: Final go/no-go**

If all platforms install-and-discover cleanly, the design is proven. Cut the real
`v*` tag to publish to PyPI (plugin first via job ordering). Confirm
`pip index versions clarion` and `clarion-plugin-python` both show the version.

---

## Self-review notes

- **Spec coverage:** D1–D10 each map to a task — D2/D3→Task 7; D4/D5→Task 8;
  D6→Tasks 1–3; D7→Task 4; D8→Task 10; D9 (GitHub Release) is unchanged existing
  `release.yml`; D10 (Node)→Tasks 11–12; lockstep→Task 6; sdist decision→Task 9;
  validation gates→Tasks 7(step4), 8(step3), 13.
- **Open mechanics deliberately left to execution (with criteria stated):**
  aarch64-linux zig-vs-native runner (Task 8 note); sdist keep/drop (Task 9);
  the exact plugin-reporting subcommand (Task 3 step 3) and plugin cache-warm
  entry point (Task 11 step 2) — each task says how to discover the real symbol.
- **Risk-first ordering:** Phase 1 proves the discovery change locally (no PyPI);
  Task 7 step 4 proves install-and-spawn locally before any CI; Task 13 proves it
  on real tooling before the first real publish.
```
