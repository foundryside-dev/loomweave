# Loomweave PyPI Distribution — Design

**Date:** 2026-06-05
**Status:** Approved (brainstorm) — pending implementation plan
**Owner:** John Morrissey

## 1. Goal

Ship Loomweave through **PyPI as the canonical install channel**, so a developer
gets the whole tool — native Rust binary **and** the Python language plugin —
in **one command**, consistent with the rest of the Loom suite (Filigree,
Wardline) which are native-Python packages on PyPI.

Success criterion:

```bash
uvx loomweave install --path .      # or: pipx install loomweave && loomweave install --path .
```

installs the `loomweave` binary, brings the Python plugin into the same
environment, and a subsequent `loomweave analyze` **discovers and spawns the
plugin** with no extra flags, no URL pinning, and no manual platform choice.

The existing signed GitHub Release tarballs remain as the **secondary / offline
channel** (cargo-binstall, air-gapped installs, a future Homebrew tap).

## 2. Current state (v1.0.0)

- **Rust core** → `loomweave` binary (`crates/loomweave-cli`, a plain `bin` crate,
  no PyO3). `release.yml` cross-builds for `x86_64-unknown-linux-gnu` and
  `aarch64-apple-darwin` only, tarballs them, **cosign-signs + Rekor-verifies**,
  emits SHA256 checksums, attaches to a GitHub Release on `v*` tag (gated behind
  the `verify` job + tag-on-main ancestry check).
- **Python plugin** → `loomweave-plugin-python` (`plugins/python`, hatchling),
  currently built as an **sdist only** and attached to the same Release. It is a
  *separate spawned process* (ADR-021 plugin jail), ships `plugin.toml` as
  hatchling **shared-data** into `share/loomweave/plugins/python/`, exposes a
  `loomweave-plugin-python` console script, and depends on `pyright==1.1.409`
  (which pulls `nodeenv` → bootstraps a Node runtime on first run).
- **Install today** is a 3-step manual dance: download the right tarball →
  drop the binary in `~/.local/bin` → `pipx install <release-url>.tar.gz`.
- Versions are kept in lockstep by `scripts/check-workspace-version-lockstep.py`.

### Discovery mechanism (the load-bearing constraint)

`crates/loomweave-core/src/plugin/discovery.rs` (the only production discovery
path, called from `crates/loomweave-cli/src/analyze.rs:432`) is **`$PATH`-only**:

1. Scan each `$PATH` directory for executables named `loomweave-plugin-<suffix>`
   (refusing world-writable dirs per ADR-021).
2. For each, resolve the manifest via: **neighbor** `<dir>/plugin.toml` →
   **install-prefix** `<dir>/../share/loomweave/plugins/<suffix>/plugin.toml`
   (only when `<dir>` basename is `bin`) → **symlink-resolved install-prefix**
   (canonicalise the executable, retry install-prefix from the resolved venv).

There is **no** `current_exe()`-relative probe, no `LOOMWEAVE_PLUGIN_PATH`, no
explicit registry. Every test exercises discovery by setting
`.env("PATH", plugin_path)`.

**Consequence:** `pipx install loomweave` / `uvx loomweave` expose **only** the
`loomweave` entry point on the user's `$PATH`. The dependency's
`loomweave-plugin-python` script lands in the venv `bin/` but is *not* on `$PATH`
and the venv is not activated → current discovery finds **nothing**. This is the
single fact that forces a small core change (Section 4.2). It is empirically
gateable without PyPI (Section 7).

## 3. Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **PyPI is the canonical channel** | Suite consistency — Filigree/Wardline are PyPI-native |
| D2 | **Approach A: two PyPI projects**, `loomweave` depends on `loomweave-plugin-python` | Keeps the ADR-021 core/plugin boundary; one-command UX via dependency resolution; both already version in lockstep |
| D3 | **Plugin is a default (non-optional) dependency** for now | Python is the flagship language. Revisit an optional `[python]` extra when a *second* language plugin exists (YAGNI) |
| D4 | **Rust binary shipped via maturin `bindings = "bin"` platform wheels** | The ruff/uv pattern; `loomweave-cli` is already a plain bin crate |
| D5 | **Wheel matrix = standard 4**: linux `x86_64` + `aarch64` (manylinux), macOS `arm64` + `x86_64` | Best reach/cost balance; covers dev laptops + CI runners. Windows deferred |
| D6 | **Add a `current_exe()`-relative sibling discovery level** | The only way co-located (same-venv) plugin discovery works under both pipx *and* uv with zero flags (Section 4.2) |
| D7 | **Amend ADR-021** to document the new discovery level | The change touches the plugin-jail trust model; the security record stays honest |
| D8 | **PyPI Trusted Publishing (OIDC)** from GitHub Actions | No long-lived token; pairs with PEP 740 attestations; reuse existing `verify` + tag-on-main gating |
| D9 | **Keep cosign-signed GitHub Release tarballs** in parallel | Offline / cargo-binstall / future Homebrew; source-of-truth binaries |
| D10 | **Node/pyright: document the first-run fetch for v1**; file a follow-up to make it hermetic | Don't block the PyPI launch on hermetic Node; flag the supply-chain wrinkle |

## 4. Architecture

### 4.1 Package topology

Two PyPI distributions, versioned in lockstep with the Cargo workspace
(`1.0.0` today):

1. **`loomweave`** — platform wheels built by maturin (`bindings = "bin"`), each
   carrying the cross-compiled `loomweave` binary placed as a wheel script (installs
   into `<venv>/bin/loomweave`). Declares a pinned runtime dependency
   `loomweave-plugin-python == <workspace version>`.
2. **`loomweave-plugin-python`** — the existing pure-Python package, now built as a
   **wheel** in addition to the sdist. Unchanged shape: ships `plugin.toml` shared-data
   into `<venv>/share/loomweave/plugins/python/`, exposes the `loomweave-plugin-python`
   console script (installs into `<venv>/bin/`), depends on `pyright`/`pyyaml`/`packaging`.

**Resulting venv layout** (what `uvx loomweave` / `pipx install loomweave` produces):

```
<venv>/bin/loomweave                                  # maturin bin wheel
<venv>/bin/loomweave-plugin-python                    # plugin console script
<venv>/share/loomweave/plugins/python/plugin.toml     # plugin shared-data
```

`loomweave` and the plugin are co-located in the same venv `bin/` and `share/`.

### 4.2 Discovery change (D6) — `current_exe()`-relative sibling level

Add one discovery level to `discovery.rs`: in addition to scanning `$PATH`,
scan the directory containing the **running `loomweave` binary**
(`std::env::current_exe()?.parent()`) for `loomweave-plugin-*` executables, feeding
each through the *existing* `load_plugin` / `find_manifest` logic (which already
handles the `bin/ → ../share/loomweave/plugins/<suffix>/` install-prefix layout).

- This makes the co-located plugin discoverable with **no `$PATH` manipulation**:
  `current_exe()` is `<venv>/bin/loomweave`, its parent is `<venv>/bin`, and the
  existing install-prefix probe resolves `<venv>/share/loomweave/plugins/python/plugin.toml`.
- It works identically for `pipx` (real venv) and `uv tool`/`uvx`: on Linux
  `current_exe()` reads `/proc/self/exe` and resolves symlinks into the venv;
  the existing symlink-resolution branch covers the macOS path where the exposed
  entry is a symlink.
- **Security:** the new directory is subject to the **same world-writable
  refusal** as `$PATH` entries. The trust argument: the directory holding the
  running `loomweave` binary is implicitly as trusted as that binary — an attacker
  who can write a sibling `loomweave-plugin-*` there can already replace `loomweave`
  itself. Discovery results from this level are **merged** with `$PATH` results
  using the existing first-match-wins de-duplication (`seen_names`), so a
  legitimately PATH-installed plugin is never shadowed surprisingly. Behaviour
  ordering (`$PATH` first vs. `current_exe()` first) is specified in the plan;
  default proposal: `$PATH` entries first, then the `current_exe()` dir, so an
  operator's explicit PATH plugin wins.

This is the change ADR-021 is amended to record (D7).

### 4.3 Data flow (install → first analyze)

1. `uvx loomweave install --path .` → uv resolves the `loomweave` platform wheel +
   `loomweave-plugin-python` wheel into one environment; `loomweave install` does its
   existing setup (`.loomweave/`, skills, MCP config, hook).
2. `loomweave analyze` → `discover()` scans `$PATH` **and** `current_exe()` dir →
   finds `<venv>/bin/loomweave-plugin-python` → resolves
   `<venv>/share/loomweave/plugins/python/plugin.toml` → host spawns the plugin
   under the ADR-021 jail → analysis proceeds.
3. First spawn triggers pyright's `nodeenv` Node fetch if not cached (Section 6).

## 5. Build, publish & versioning (CI)

- **New wheels job(s)** (in `release.yml` or a dedicated `wheels.yml`), gated
  behind the existing `verify` job:
  - `loomweave` platform wheels via maturin (`bindings = "bin"`) for the standard-4
    matrix. linux-`aarch64` + bundled C (SQLite) uses native arm64 runners *or*
    maturin `--zig` for the C cross — chosen in the plan. `rusqlite` is `bundled`
    and `reqwest` is rustls, so manylinux/macOS wheels need no system libs.
  - `loomweave-plugin-python` wheel via `uv build` (pure Python, one wheel).
  - **sdist for `loomweave`**: produced, but gated by a **clean-room build test**
    (Section 7) that proves the workspace + `Cargo.lock` vendor correctly and
    compile from the sdist with a Rust toolchain. If that proves impractical,
    fall back to **no `loomweave` sdist** and route off-matrix platforms to the
    GitHub Release / cargo-binstall with a clear error. (Decision recorded in plan.)
- **Publish** via PyPI **Trusted Publishing (OIDC)**, with PEP 740 attestations.
- **Release ordering:** publish `loomweave-plugin-python` **before** `loomweave`, so
  the `loomweave` wheel's `== <version>` dependency resolves on the first release.
- **GitHub Release** (cosign tarballs) continues unchanged, in parallel.
- **Version lockstep:** extend `check-workspace-version-lockstep.py` to assert the
  two PyPI package versions **and** the `loomweave → loomweave-plugin-python` pin all
  equal the Cargo workspace version, so a release can't publish a core depending
  on a stale/absent plugin.

## 6. The Node / pyright wrinkle

`pyright==1.1.409` pulls `nodeenv`, which **downloads a Node runtime on first
run**. Implications:

- First `loomweave analyze` needs network to fetch Node (surprising; breaks
  air-gapped/offline installs).
- A runtime network fetch sits awkwardly with Loomweave's untrusted-corpus posture.

**v1 plan:** document the first-run fetch in the README install section, and
prime the pyright/Node cache during `loomweave install` (a cache-warm step) so the
fetch happens at a predictable, online moment rather than mid-analyze.

**Follow-up (out of scope here):** make Node hermetic by shipping it via a wheel
(e.g. `nodejs-wheel`) so the plugin install pulls a pinned Node with no runtime
fetch. Filed as a tracked issue, not blocking the PyPI launch.

## 7. Testing & validation gates

- **Discovery spike → permanent test (no PyPI needed):** an integration test that
  stages a `<tmp>/bin/loomweave` (the built binary) + `<tmp>/share/loomweave/plugins/python/plugin.toml`
  + `<tmp>/bin/loomweave-plugin-python`, runs `loomweave analyze` with the plugin dir
  **NOT on `$PATH`**, and asserts the plugin is discovered and spawned via the new
  `current_exe()` level. This is the empirical proof of D6 and a regression guard.
- **Clean-room sdist build test** (gates D5's sdist decision): in a minimal
  container with only a Rust toolchain, `pip install loomweave-<ver>.tar.gz` and run
  `loomweave --version`.
- **Per-platform install-and-spawn smoke test:** on each matrix OS, install the
  built wheels from a local index / **TestPyPI**, run `loomweave --version`,
  `loomweave install --path <tmp>`, and a tiny `analyze` proving plugin discovery in
  a *real* tool-venv layout (pipx and uv).
- **TestPyPI dry-run** of the full publish before the first real PyPI publish.

## 8. Documentation changes

- README install section collapses from the 3-step tarball dance to
  `uvx loomweave install --path .` (and the `pipx` equivalent), with the signed-tarball
  path retained as the **offline / verified-binary** alternative.
- Note the first-run Node fetch (Section 6).

## 9. Risks & mitigations

| Risk | Severity | Mitigation |
|------|----------|-----------|
| Co-located discovery fails under a real tool-venv (symlink / `current_exe()` quirk on macOS) | High | D6 + the discovery integration test + per-platform smoke test on TestPyPI *before* publish |
| `loomweave` sdist (workspace path-deps + bundled C) won't compile on a user box | Medium | Clean-room sdist build test; fall back to no-sdist + GitHub Release/cargo-binstall route |
| manylinux build of bundled SQLite/blake3 fails | Medium | Expected fine (bundled, rustls — no system libs); validate in CI matrix |
| linux-`aarch64` cross with bundled C | Medium | Native arm64 runner or maturin `--zig`; decided in plan |
| Node fetch breaks offline/air-gapped first run | Medium | Document + cache-warm in `loomweave install`; hermetic-Node follow-up |
| First-release dependency resolution (`loomweave` needs plugin already on PyPI) | Low | Publish plugin before core |

## 10. Out of scope (YAGNI)

- Windows wheels (revisit on demand; git/plugin-jail paths are Unix-tuned).
- Optional `[python]` extra / plugin-decoupling (revisit at second plugin).
- Homebrew tap, cargo-binstall metadata (the signed GitHub Release already
  supports binstall; a tap is a later nicety).
- Hermetic Node bundling (tracked follow-up).

## 11. ADR-021 amendment (D7)

Amend ADR-021 (plugin jail / discovery trust model) to record:

- A third discovery source: the directory of the running `loomweave` binary
  (`current_exe()` parent), in addition to `$PATH`.
- That the **world-writable refusal** and first-match-wins de-duplication apply
  to this source identically.
- The trust rationale: co-location with the trusted `loomweave` binary; an attacker
  who can write there can already replace `loomweave`.
- The merge ordering with `$PATH` results.
