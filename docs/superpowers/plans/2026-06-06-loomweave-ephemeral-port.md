# Loomweave Read-API Ephemeral Port Publication — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `loomweave serve` bind a per-project deterministic read-API port (ephemeral fallback), publish the live port to `.loomweave/ephemeral.port` as a normative cross-product file contract, resolve it consume-time, and stop the installer pinning `9111` — so two projects can `serve` concurrently without the `9111` collision (ADR-044, clarion-7f574bc34f).

**Architecture:** Mirror Filigree's `.filigree/ephemeral.port` convention symmetrically for Loomweave's own read API. A new `loomweave-federation::loomweave_port` module owns the deterministic-port computation (blake3, band `9400–10399`, disjoint from Filigree's `8400–9399`), the atomic publish/remove, and the validated read. The producer (`http_read.rs`) binds the deterministic port — falling back to OS-assigned `:0` only when the port was *auto-selected*, not when an operator set it explicitly — then publishes the actually-bound port loopback-only via an RAII guard that unlinks on drop. `HttpReadConfig.bind` becomes `Option<SocketAddr>` so "operator chose a port" is distinguishable from "auto." The installer and the local dogfood bindings stop hardcoding `9111`.

**Tech Stack:** Rust (workspace edition 2024, rust 1.88), `blake3` (already a workspace dep, Loomweave's SEI hash), `tokio` TCP bind, `axum` serve, `serde`/`serde_norway` config, `cargo nextest`.

**Branch:** Work on the current branch `feat/serve-no-index-chirp` (ADR-044 already lives here, unpushed). The user may split at push time.

**The band is internal, never part of the contract.** Consumers read the published file; nobody recomputes a peer's port. The `9400` band number appears only in code, never in the ADR's normative section.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/loomweave-federation/Cargo.toml` | add `blake3` dep | 1 |
| `crates/loomweave-federation/src/loomweave_port.rs` (CREATE) | deterministic port, atomic publish/remove, validated read | 1 |
| `crates/loomweave-federation/src/lib.rs` | declare `pub mod loomweave_port;` (+ `loomweave_url` in Task 6) | 1, 6 |
| `crates/loomweave-federation/src/config.rs` | `bind: Option<SocketAddr>`, method + default updates | 2 |
| `crates/loomweave-cli/src/http_read.rs` | candidate resolution; auto-fallback; publish RAII | 2, 3 |
| `crates/loomweave-cli/src/install.rs` | YAML stub drops `bind: 9111` | 4 |
| `crates/loomweave-cli/tests/install.rs` | install-stub + bindings assertions | 4, 5 |
| `crates/loomweave-cli/src/integration_bindings.rs` | deterministic `loomweave.url`; drop fixed bind | 5 |
| `crates/loomweave-cli/tests/doctor.rs` | bindings-repair assertion | 5, 6 |
| `crates/loomweave-federation/src/loomweave_url.rs` (CREATE) | `resolve_loomweave_url` (file>config>none) | 6 |
| `crates/loomweave-cli/src/doctor.rs` | `check_http_config_json` reports published port | 6 |
| `docs/operator/loomweave-http-read-api.md`, `docs/operator/secret-scanning.md`, `docs/federation/contracts.md` | auto-port wording | 7 |
| `loomweave.yaml`, `wardline.yaml` (repo root) | revert `9112` stopgap | 7 |
| `docs/loomweave/adr/ADR-044-*.md`, `docs/loomweave/adr/README.md`, `docs/suite/glossary.md` | ADR Proposed→Accepted + glossary verdict | 7 |

---

## Task 1: Shared ephemeral-port module (`loomweave-federation`)

**Files:**
- Modify: `crates/loomweave-federation/Cargo.toml`
- Create: `crates/loomweave-federation/src/loomweave_port.rs`
- Modify: `crates/loomweave-federation/src/lib.rs`

This task ships pure, fully-unit-tested functions with no dependents yet, so the tree stays green standalone.

- [ ] **Step 1: Add the `blake3` dependency**

In `crates/loomweave-federation/Cargo.toml`, under `[dependencies]` (alphabetical-ish, after `loomweave-core`), add:

```toml
blake3.workspace = true
```

The workspace already pins `blake3 = "1.8.5"` (root `Cargo.toml:39`); `.workspace = true` inherits it.

- [ ] **Step 2: Write the failing tests for the new module**

Create `crates/loomweave-federation/src/loomweave_port.rs` with ONLY the test module first (the `use super::*;` will fail to resolve the items until Step 4):

```rust
//! Loomweave read-API ephemeral-port contract (ADR-044).
//!
//! The twin of Filigree's `.filigree/ephemeral.port` convention, applied to
//! Loomweave's own federation HTTP read API. `serve` binds a per-project
//! deterministic port (ephemeral `:0` fallback) and publishes the *actually
//! bound* port to `<project_root>/.loomweave/ephemeral.port`. Cross-product
//! consumers (notably Wardline, which is Python) read this file; nobody
//! recomputes a peer's port. The deterministic band here is an implementation
//! detail, never part of the file contract.
//!
//! File contract (ADR-044, normative): a single plain-ASCII integer TCP port,
//! optional trailing `\n`, written atomically (temp + rename), present only
//! while `serve` holds a loopback bind. Host (`127.0.0.1`) and scheme (`http`)
//! are implied, sound only because publication is loopback-only.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_port_is_stable_and_in_band() {
        let dir = tempfile::tempdir().unwrap();
        let a = deterministic_port(dir.path());
        let b = deterministic_port(dir.path());
        assert_eq!(a, b, "same path must yield the same port");
        assert!(
            (PORT_BAND_BASE..PORT_BAND_BASE + PORT_BAND_SPAN).contains(&a),
            "port {a} must land in the loomweave band [{PORT_BAND_BASE}, {})",
            PORT_BAND_BASE + PORT_BAND_SPAN
        );
        // Disjoint from Filigree's 8400-9399 band.
        assert!(a >= 9400, "port {a} must not overlap Filigree's 8400-9399 band");
    }

    #[test]
    fn deterministic_port_differs_by_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Distinct tempdirs almost always hash to distinct ports; assert the
        // function is path-sensitive by checking the inputs differ and the
        // computation is a pure function of the (canonical) path.
        assert_ne!(a.path(), b.path());
        let pa = deterministic_port(a.path());
        let pb = deterministic_port(b.path());
        // Not guaranteed distinct (1/1000 collision), but the band membership
        // and determinism are what matter; assert both are in-band.
        assert!(pa >= 9400 && pb >= 9400);
    }

    #[test]
    fn publish_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).expect("publish");
        assert_eq!(read_published_port(dir.path()), Some(9412));
        // Published content is the bare port plus a single trailing newline.
        let raw = std::fs::read_to_string(published_port_path(dir.path())).unwrap();
        assert_eq!(raw, "9412\n");
    }

    #[test]
    fn publish_creates_loomweave_dir_if_absent() {
        let dir = tempfile::tempdir().unwrap();
        // No .loomweave/ yet.
        assert!(!dir.path().join(".loomweave").exists());
        publish_port(dir.path(), 10000).expect("publish creates .loomweave/");
        assert_eq!(read_published_port(dir.path()), Some(10000));
    }

    #[test]
    fn read_tolerates_trailing_whitespace_and_newline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".loomweave")).unwrap();
        std::fs::write(published_port_path(dir.path()), "  9500  \n").unwrap();
        assert_eq!(read_published_port(dir.path()), Some(9500));
    }

    #[test]
    fn read_rejects_malformed_zero_and_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".loomweave")).unwrap();
        for bad in ["", "not-a-port", "0", "65536", "70000", "-1", "12.5"] {
            std::fs::write(published_port_path(dir.path()), bad).unwrap();
            assert_eq!(
                read_published_port(dir.path()),
                None,
                "malformed/out-of-range content {bad:?} must fold to None (fail-soft)"
            );
        }
    }

    #[test]
    fn read_absent_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_published_port(dir.path()), None);
    }

    #[test]
    fn remove_is_idempotent_and_clears_the_file() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9999).unwrap();
        assert!(published_port_path(dir.path()).exists());
        remove_published_port(dir.path());
        assert!(!published_port_path(dir.path()).exists());
        // Second remove on an absent file is a no-op, not an error.
        remove_published_port(dir.path());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo nextest run -p loomweave-federation loomweave_port`
Expected: FAIL — `cannot find function deterministic_port`, etc.

- [ ] **Step 4: Implement the module functions**

Insert the implementation *above* the `#[cfg(test)] mod tests` block in `loomweave_port.rs`:

```rust
/// Base of Loomweave's deterministic read-API port band. Chosen to sit
/// **above** Filigree's `8400–9399` band so the two products never contend for
/// the same number. Internal only — never part of the cross-product file
/// contract (consumers read the published file, never recompute).
pub const PORT_BAND_BASE: u16 = 9400;
/// Width of the band: ports land in `[9400, 10400)` i.e. `9400..=10399`.
pub const PORT_BAND_SPAN: u16 = 1000;

/// Canonical path of the published port file for a project root.
#[must_use]
pub fn published_port_path(project_root: &Path) -> PathBuf {
    project_root.join(".loomweave").join("ephemeral.port")
}

/// Deterministic-but-unpredictable read-API port for a project, derived from
/// the canonical project path. Stable across runs (so a consumer's static
/// config can match it) yet path-specific (so two projects differ). Mirrors
/// Filigree's `8400 + hash % 1000`, in a disjoint band, using Loomweave's own
/// hash (blake3, as for SEI). The bound port is published; this computation is
/// the producer's *starting guess*, not a value any consumer recomputes.
#[must_use]
pub fn deterministic_port(project_root: &Path) -> u16 {
    // Best-effort canonicalize so every caller (serve, install, doctor) agrees
    // regardless of whether it pre-canonicalized; fall back to the path as-given.
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let bytes = canonical.to_string_lossy();
    let hash = blake3::hash(bytes.as_bytes());
    let head = u64::from_le_bytes(
        hash.as_bytes()[..8]
            .try_into()
            .expect("blake3 digest is 32 bytes, so [..8] is 8 bytes"),
    );
    let offset = u16::try_from(head % u64::from(PORT_BAND_SPAN))
        .expect("remainder of % 1000 is < 1000, which fits u16");
    PORT_BAND_BASE + offset
}

/// Read and validate the published port. Any missing / non-integer /
/// out-of-range / zero content folds to `None` (fail-soft, ADR-044). A `u16`
/// parse already bounds `1..=65535` except `0`, which we reject explicitly.
#[must_use]
pub fn read_published_port(project_root: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(published_port_path(project_root)).ok()?;
    raw.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

/// Atomically publish `port` to `<project_root>/.loomweave/ephemeral.port`.
/// Writes a temp file in the same directory and `rename(2)`s it into place, so
/// a concurrent reader never observes a torn value. Creates `.loomweave/` if
/// absent. The caller is responsible for the loopback-only invariant (only call
/// this when the bound address is loopback).
///
/// # Errors
/// Returns the underlying I/O error if the directory cannot be created or the
/// temp file cannot be written/renamed.
pub fn publish_port(project_root: &Path, port: u16) -> std::io::Result<()> {
    let dir = project_root.join(".loomweave");
    std::fs::create_dir_all(&dir)?;
    // One `serve` per process publishes, so the PID makes the temp name unique
    // within this directory without needing a random suffix.
    let tmp = dir.join(format!("ephemeral.port.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{port}\n"))?;
    std::fs::rename(&tmp, dir.join("ephemeral.port"))?;
    Ok(())
}

/// Best-effort removal of the published port file. A missing file is not an
/// error (idempotent). Called on clean shutdown; SIGKILL leaves a stale file,
/// which `read_published_port` validation + the ADR-034 instance-ID guard
/// handle (a stale file degrades, never corrupts).
pub fn remove_published_port(project_root: &Path) {
    let _ = std::fs::remove_file(published_port_path(project_root));
}
```

- [ ] **Step 5: Declare the module**

In `crates/loomweave-federation/src/lib.rs`, add `pub mod loomweave_port;` after `pub mod filigree_url;`:

```rust
//! Shared federation/config helpers used by CLI and MCP surfaces.

pub mod config;
pub mod filigree;
pub mod filigree_url;
pub mod loomweave_port;
pub mod scan_results;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-federation loomweave_port`
Expected: PASS (8 tests).

- [ ] **Step 7: Lint + commit**

```bash
cargo fmt --all
cargo clippy -p loomweave-federation --all-targets --all-features -- -D warnings
git add crates/loomweave-federation/Cargo.toml crates/loomweave-federation/src/loomweave_port.rs crates/loomweave-federation/src/lib.rs
git commit -m "feat(federation): loomweave_port — deterministic read-API port + atomic publish (ADR-044)"
```

---

## Task 2: `HttpReadConfig.bind` → `Option<SocketAddr>` (green-tree migration)

**Files:**
- Modify: `crates/loomweave-federation/src/config.rs`
- Modify: `crates/loomweave-cli/src/http_read.rs`

`None` means *auto* (deterministic + fallback + publish, wired in Tasks 2–3). `Some(addr)` means an explicit operator override. This is one atomic task: the type change plus every construction site, ending on a green tree. Producer *behavior* (fallback/publish) is Task 3 — here, `spawn` only resolves `None` to the deterministic candidate so it compiles and runs.

- [ ] **Step 1: Write the failing config tests**

In `crates/loomweave-federation/src/config.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn http_bind_defaults_to_none_auto_select() {
        // ADR-044: the installer no longer pins a port; an unset bind means
        // "auto-select a per-project deterministic port and publish it".
        assert_eq!(HttpReadConfig::default().bind, None);
    }

    #[test]
    fn http_bind_none_is_treated_as_loopback() {
        // Auto-select always binds 127.0.0.1, so an absent bind is loopback and
        // must satisfy the loopback-trust gate without allow_non_loopback.
        let cfg = HttpReadConfig {
            enabled: true,
            bind: None,
            ..HttpReadConfig::default()
        };
        assert!(cfg.is_loopback_bind());
        assert!(cfg.validate_loopback_trust().is_ok());
    }

    #[test]
    fn http_explicit_bind_still_parses() {
        let cfg = McpConfig::from_yaml_str(
            "serve:\n  http:\n    enabled: true\n    bind: \"127.0.0.1:9412\"\n",
        )
        .expect("parse explicit bind");
        assert_eq!(
            cfg.serve.http.bind,
            Some(SocketAddr::from(([127, 0, 0, 1], 9412)))
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p loomweave-federation http_bind`
Expected: FAIL to compile — `bind` is `SocketAddr`, not `Option`.

- [ ] **Step 3: Change the field type and the default**

In `config.rs`, change the `HttpReadConfig.bind` field:

```rust
    #[serde(default, deserialize_with = "deserialize_optional_socket_addr")]
    pub bind: Option<SocketAddr>,
```

Change the `Default` impl:

```rust
impl Default for HttpReadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: None,
            allow_non_loopback: false,
            token_env: "WEFT_TOKEN".to_owned(),
            identity_token_env: None,
            wardline_taint_write: false,
        }
    }
}
```

- [ ] **Step 4: Update the loopback methods to treat `None` as loopback**

Replace `validate_loopback_trust` and `is_loopback_bind`:

```rust
    pub fn validate_loopback_trust(&self) -> Result<(), ConfigError> {
        if self.enabled && !self.allow_non_loopback && !self.is_loopback_bind() {
            return Err(ConfigError::NonLoopbackHttpBind {
                code: "LMWV-CONFIG-HTTP-NON-LOOPBACK",
                // Safe: is_loopback_bind() is false only when bind is Some(non-loopback).
                bind: self.bind.expect("non-loopback bind implies an explicit address"),
            });
        }
        Ok(())
    }
```

```rust
    /// `None` (auto-select) always binds `127.0.0.1`, so it is loopback.
    #[must_use]
    pub fn is_loopback_bind(&self) -> bool {
        self.bind.is_none_or(|addr| addr.ip().is_loopback())
    }
```

`validate_auth_trust` already calls `self.is_loopback_bind()` and only reads `self.bind` inside the `NonLoopbackHttpNoAuth` error arm, which is reached only when `is_loopback_bind()` is false (i.e. `Some(non-loopback)`). Update that one read:

```rust
        Err(ConfigError::NonLoopbackHttpNoAuth {
            code: "LMWV-CONFIG-HTTP-NO-AUTH",
            bind: self.bind.expect("non-loopback bind implies an explicit address"),
            token_env: self.token_env.clone(),
        })
```

- [ ] **Step 5: Add the optional-socket deserializer**

Below the existing `deserialize_socket_addr` in `config.rs`, add:

```rust
fn deserialize_optional_socket_addr<'de, D>(deserializer: D) -> Result<Option<SocketAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    match raw {
        None => Ok(None),
        Some(raw) => raw.parse().map(Some).map_err(|err| {
            serde::de::Error::custom(format!("invalid serve.http.bind {raw:?}: {err}"))
        }),
    }
}
```

The old `deserialize_socket_addr` is now unused — delete it (clippy `dead_code` would otherwise fire). The `invalid_http_bind_fails_config_load` test still passes because the new deserializer emits the same `invalid serve.http.bind` message.

- [ ] **Step 6: Fix the existing config tests that build/parse `bind`**

In `config.rs` `mod tests`, update the two tests that assert a parsed bind value:

`http_bind_is_parsed_when_config_loads`:
```rust
        assert_eq!(
            cfg.serve.http.bind,
            Some(SocketAddr::from(([127, 0, 0, 1], 0)))
        );
```

The non-loopback / IPv6 / allow-non-loopback parse tests (`enabled_non_loopback_http_bind_requires_allow_non_loopback`, `enabled_lan_http_bind_requires_allow_non_loopback`, `enabled_ipv6_loopback_http_bind_is_allowed_by_default`, `enabled_non_loopback_http_bind_allows_explicit_opt_in`, `invalid_http_bind_fails_config_load`) all set `bind:` in YAML strings — those parse into `Some(..)` and need no change.

- [ ] **Step 7: Fix `http_read.rs` construction + spawn sites**

In `crates/loomweave-cli/src/http_read.rs`:

(a) `spawn_with_env` currently does `let bind = config.bind;`. Replace with deterministic resolution (behavior-minimal — no fallback/publish yet; that's Task 3). The `project_root` is already a parameter:

```rust
    // ADR-044: an unset bind means auto-select a per-project deterministic
    // read-API port. An explicit bind is honored verbatim. (Task 3 adds the
    // ephemeral fallback + published-file lifecycle.)
    let auto_port = config.bind.is_none();
    let bind = config.bind.unwrap_or_else(|| {
        std::net::SocketAddr::from((
            [127, 0, 0, 1],
            loomweave_federation::loomweave_port::deterministic_port(&project_root),
        ))
    });
```

Thread `auto_port` and `project_root` (clone before it is moved) into `run_http_read_server`. `project_root` is currently moved into the thread closure; capture a clone for publication in Task 3. For Task 2, just add the `auto_port: bool` parameter to `run_http_read_server`'s signature and ignore it with a leading underscore at the call site is not allowed for a named param — instead accept it and bind it to `_auto_port` inside the fn body for now:

In `run_http_read_server` signature add (after `bind`):
```rust
    auto_port: bool,
```
And at the top of `run_http_read_server` body, until Task 3 consumes it:
```rust
    let _auto_port = auto_port;
```
Pass `auto_port` at the call site inside the spawned thread closure in `spawn_with_env`.

(b) The three `#[cfg(test)]` tests in `http_read.rs` that build `HttpReadConfig { ..., bind, ... }` (`spawn_emits_loopback_no_token_trust_warning`, `spawn_with_taint_writer_shuts_down_cleanly`, `check_running_surfaces_supervisor_signal_after_runtime_panic`) each set `bind` to a probed `SocketAddr`. Wrap each in `Some(...)`:

```rust
            let config = HttpReadConfig {
                enabled: true,
                bind: Some(bind),
                allow_non_loopback: false,
                // ...rest unchanged
            };
```

(There are three such literals; update all three. `spawn_with_taint_writer_shuts_down_cleanly` and `check_running_surfaces_supervisor_signal_after_runtime_panic` use `bind, ..HttpReadConfig::default()` shorthand — change `bind,` to `bind: Some(bind),`.)

- [ ] **Step 8: Run the affected suites**

Run:
```bash
cargo nextest run -p loomweave-federation
cargo nextest run -p loomweave-cli --lib http_read
```
Expected: PASS. Then a workspace build to catch any other construction site:
```bash
cargo build --workspace --all-features --tests
```
Expected: compiles clean. If any other `HttpReadConfig { bind: <SocketAddr> }` or `.bind` read surfaces, wrap/adapt it the same way.

- [ ] **Step 9: Lint + commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/loomweave-federation/src/config.rs crates/loomweave-cli/src/http_read.rs
git commit -m "feat(config): serve.http.bind is Option<SocketAddr>; None auto-selects per-project port (ADR-044)"
```

---

## Task 3: Producer — ephemeral fallback + publish RAII

**Files:**
- Modify: `crates/loomweave-cli/src/http_read.rs`

Add: auto-port falls back to `:0` on `AddrInUse`; the actually-bound port is published loopback-only via an RAII guard that unlinks on drop (covers graceful shutdown, error-return, and panic-unwind in one place).

- [ ] **Step 1: Write the failing producer tests**

In `http_read.rs` `mod tests`, add. These reuse the `http_runtime_test_guard()` and `ReaderPool` patterns already in the file:

```rust
    /// ADR-044: with `bind: None`, two serves on distinct project paths each
    /// bind their own deterministic port and publish their own
    /// `.loomweave/ephemeral.port`. Neither fails to bind.
    #[test]
    fn auto_port_publishes_distinct_ports_per_project() {
        use loomweave_federation::config::HttpReadConfig;
        use loomweave_federation::loomweave_port::read_published_port;
        use loomweave_storage::ReaderPool;

        let _guard = http_runtime_test_guard();

        let make = |id: &str| {
            let dir = tempfile::tempdir().expect("tempdir");
            let db = dir.path().join("loomweave.db");
            let readers = ReaderPool::open(&db, 4).expect("reader pool");
            let cfg = HttpReadConfig {
                enabled: true,
                bind: None,
                ..HttpReadConfig::default()
            };
            let iid = crate::instance::parse_instance_id_for_test(id).expect("iid");
            let server = spawn(dir.path().to_path_buf(), db, readers, iid, &cfg)
                .expect("spawn")
                .expect("enabled => Some");
            (dir, server)
        };

        let (dir_a, server_a) = make("00000000-0000-4000-8000-0000000000a1");
        let (dir_b, server_b) = make("00000000-0000-4000-8000-0000000000a2");

        let port_a = read_published_port(dir_a.path()).expect("a published a port");
        let port_b = read_published_port(dir_b.path()).expect("b published a port");
        assert!(port_a >= 9400 && port_b >= 9400, "ports in the loomweave band");
        // Two live servers => two live ports => they cannot be equal.
        assert_ne!(port_a, port_b, "concurrent serves must hold distinct ports");

        server_a.shutdown().expect("shutdown a");
        server_b.shutdown().expect("shutdown b");
    }

    /// The published file is removed on clean shutdown.
    #[test]
    fn auto_port_file_removed_on_clean_shutdown() {
        use loomweave_federation::config::HttpReadConfig;
        use loomweave_federation::loomweave_port::{published_port_path, read_published_port};
        use loomweave_storage::ReaderPool;

        let _guard = http_runtime_test_guard();

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("loomweave.db");
        let readers = ReaderPool::open(&db, 4).expect("reader pool");
        let cfg = HttpReadConfig {
            enabled: true,
            bind: None,
            ..HttpReadConfig::default()
        };
        let iid = crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-0000000000a3")
            .expect("iid");
        let server = spawn(dir.path().to_path_buf(), db, readers, iid, &cfg)
            .expect("spawn")
            .expect("enabled => Some");

        assert!(read_published_port(dir.path()).is_some(), "published while serving");
        server.shutdown().expect("shutdown");
        assert!(
            !published_port_path(dir.path()).exists(),
            "published port file must be gone after clean shutdown"
        );
    }

    /// An explicit (operator-set) bind that is already in use is a HARD error —
    /// the operator asked for that specific port. Only auto-select falls back.
    #[test]
    fn explicit_bind_in_use_is_a_hard_error() {
        use loomweave_federation::config::HttpReadConfig;
        use loomweave_storage::ReaderPool;
        use std::net::{SocketAddr, TcpListener};

        let _guard = http_runtime_test_guard();

        // Hold a real listener so the address is genuinely occupied.
        let held = TcpListener::bind(("127.0.0.1", 0)).expect("hold a port");
        let bind: SocketAddr = held.local_addr().expect("addr");

        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("loomweave.db");
        let readers = ReaderPool::open(&db, 4).expect("reader pool");
        let cfg = HttpReadConfig {
            enabled: true,
            bind: Some(bind),
            ..HttpReadConfig::default()
        };
        let iid = crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-0000000000a4")
            .expect("iid");

        let result = spawn(dir.path().to_path_buf(), db, readers, iid, &cfg);
        assert!(
            result.is_err(),
            "an explicit in-use bind must fail, not silently fall back to :0"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p loomweave-cli --lib http_read::tests::auto_port`
Expected: FAIL — no port is published yet (Task 2 binds the deterministic port but does not publish), and the auto/explicit fallback split is not implemented.

- [ ] **Step 3: Add the RAII publish guard**

Near the top of `http_read.rs` (after the imports, before `HttpReadServer`), add:

```rust
/// Removes the published `.loomweave/ephemeral.port` on drop — covering
/// graceful shutdown, error return, and panic-unwind in one place. Only
/// SIGKILL can strand a stale file, which the read-side validation and the
/// ADR-034 instance-ID guard tolerate (a stale file degrades, never corrupts).
struct PublishedPortGuard {
    project_root: PathBuf,
}

impl Drop for PublishedPortGuard {
    fn drop(&mut self) {
        loomweave_federation::loomweave_port::remove_published_port(&self.project_root);
    }
}
```

- [ ] **Step 4: Implement fallback + publish in `run_http_read_server`**

`run_http_read_server` now needs `auto_port: bool` (added in Task 2) and a clone of `project_root` for publication. `project_root: PathBuf` is already a parameter and is moved into `AppState` later — capture the publish path *before* that move.

Replace the bind block (currently a single `tokio::net::TcpListener::bind(bind)`) with auto-fallback, and add publication right after `local_addr` is known. Inside the `runtime.block_on(async move { ... })`:

```rust
        // ADR-044: auto-selected ports fall back to an OS-assigned ephemeral
        // port if the deterministic port is taken; an explicit operator bind
        // does NOT fall back (a taken explicit port is a hard error).
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(err) if auto_port && err.kind() == std::io::ErrorKind::AddrInUse => {
                let fallback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
                match tokio::net::TcpListener::bind(fallback).await {
                    Ok(listener) => listener,
                    Err(err) => {
                        let _ = ready_tx
                            .send(Err(anyhow!("bind HTTP read API ephemeral fallback: {err}")));
                        return Err(anyhow!("bind HTTP read API ephemeral fallback: {err}"));
                    }
                }
            }
            Err(err) => {
                let _ = ready_tx.send(Err(anyhow!("bind HTTP read API on {bind}: {err}")));
                return Err(anyhow!("bind HTTP read API on {bind}: {err}"));
            }
        };
        let local_addr = match listener.local_addr() {
            Ok(addr) => addr,
            Err(err) => {
                let _ = ready_tx.send(Err(anyhow!("read HTTP read API local addr: {err}")));
                return Err(anyhow!("read HTTP read API local addr: {err}"));
            }
        };
        // Publish the ACTUALLY-bound port loopback-only (ADR-044 file contract).
        // A non-loopback bind publishes NO file — consumers fall back to their
        // configured URL. The guard unlinks the file when this scope unwinds.
        let _published_port_guard = if local_addr.ip().is_loopback() {
            if let Err(err) =
                loomweave_federation::loomweave_port::publish_port(&project_root, local_addr.port())
            {
                // Publication is best-effort enrichment: a failure to write the
                // discovery file must not take the read API down.
                tracing::warn!(
                    error = %err,
                    port = local_addr.port(),
                    "failed to publish .loomweave/ephemeral.port; consumers will fall back to configured URL"
                );
                None
            } else {
                Some(PublishedPortGuard {
                    project_root: project_root.clone(),
                })
            }
        } else {
            None
        };
        let _ = ready_tx.send(Ok(HttpReadReady {
            local_addr,
            readers_identity,
        }));
```

Note the `_published_port_guard` binding lives for the rest of the `block_on` async scope (through `serve_future`), so it drops — and unlinks — exactly when serving ends (graceful, error, or panic). Delete the old `let _auto_port = auto_port;` placeholder line from Task 2 now that `auto_port` is consumed.

- [ ] **Step 5: Run the producer tests**

Run: `cargo nextest run -p loomweave-cli --lib http_read`
Expected: PASS — including the three new tests and all pre-existing ones.

- [ ] **Step 6: Lint + commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/loomweave-cli/src/http_read.rs
git commit -m "feat(serve): auto-select read-API port with ephemeral fallback; publish .loomweave/ephemeral.port (ADR-044)"
```

---

## Task 4: Installer stub stops pinning `9111`

**Files:**
- Modify: `crates/loomweave-cli/src/install.rs`
- Modify: `crates/loomweave-cli/tests/install.rs`

- [ ] **Step 1: Update the YAML stub**

In `install.rs`, the `LOOMWEAVE_YAML_STUB` ends with:

```
serve:
  mcp:
    enable_write_tools: false
  http:
    enabled: false
    bind: 127.0.0.1:9111
";
```

Replace the `http:` block (drop the `bind:` line, add an explanatory comment):

```
serve:
  mcp:
    enable_write_tools: false
  http:
    enabled: false
    # The read-API port is auto-selected per project (deterministic, with an
    # ephemeral fallback) and published to .loomweave/ephemeral.port while
    # serving. Set `bind:` explicitly only to pin a fixed port (ADR-044).
";
```

- [ ] **Step 2: Update the install test that asserts the stub bind**

There is no dedicated stub test asserting `serve.http.bind` in `tests/install.rs` for the bare `install` path; the `9111` assertions are all in the `--all` bindings test (Task 5) and `doctor.rs` (Task 5/6). Confirm with:

Run: `grep -n "9111\|http\"\]\[\"bind" crates/loomweave-cli/tests/install.rs`
If a bare-stub test asserts the bind, change it to assert the key is absent:
```rust
        assert!(loomweave_yaml["serve"]["http"].get("bind").is_none());
```

- [ ] **Step 3: Run + commit**

```bash
cargo nextest run -p loomweave-cli --test install
cargo fmt --all
git add crates/loomweave-cli/src/install.rs crates/loomweave-cli/tests/install.rs
git commit -m "feat(install): YAML stub no longer pins serve.http.bind 9111 (ADR-044)"
```

(If Step 2 found nothing to change, commit `install.rs` alone.)

---

## Task 5: `integration_bindings` writes the deterministic URL

**Files:**
- Modify: `crates/loomweave-cli/src/integration_bindings.rs`
- Modify: `crates/loomweave-cli/tests/install.rs`
- Modify: `crates/loomweave-cli/tests/doctor.rs`

`loomweave install --all` currently stamps `bind: 9111` into `loomweave.yaml` and `loomweave.url: http://127.0.0.1:9111` into `wardline.yaml` + `.mcp.json` — the real cross-project root cause. After this task: it stops writing a fixed `bind` (so auto-port + fallback engages), and writes the **deterministic** `loomweave.url` (the best static target until Wardline adopts consume-time resolution; the published file overrides it at runtime).

- [ ] **Step 1: Compute the deterministic Loomweave URL per project**

In `integration_bindings.rs`, delete the two fixed constants:

```rust
const LOOMWEAVE_HTTP_BIND: &str = "127.0.0.1:9111";
const LOOMWEAVE_HTTP_URL: &str = "http://127.0.0.1:9111";
```

Add the deterministic URL to `DesiredBindings` and compute it in `desired_bindings`:

```rust
struct DesiredBindings {
    filigree_base_url: String,
    wardline_filigree_url: String,
    loomweave_url: String,
}
```

```rust
fn desired_bindings(project_root: &Path) -> DesiredBindings {
    let filigree_base_url = live_filigree_base_url(project_root)
        .or_else(|| configured_filigree_base_url(project_root))
        .unwrap_or_else(|| DEFAULT_FILIGREE_BASE_URL.to_owned());
    let wardline_filigree_url = format!(
        "{}/api/weft/scan-results",
        filigree_base_url.trim_end_matches('/')
    );
    // ADR-044: seed the consumer's static target with this project's
    // deterministic read-API port. serve binds the same port (barring an
    // ephemeral fallback), and the published .loomweave/ephemeral.port file
    // overrides this at runtime once a consumer resolves consume-time.
    let port = loomweave_federation::loomweave_port::deterministic_port(project_root);
    let loomweave_url = format!("http://127.0.0.1:{port}");
    DesiredBindings {
        filigree_base_url,
        wardline_filigree_url,
        loomweave_url,
    }
}
```

- [ ] **Step 2: Stop writing a fixed `bind` into `loomweave.yaml`**

In `install_loomweave_yaml`, the `serve.http` block currently inserts `bind`. Remove that line:

```rust
    let serve = ensure_object(root, "serve")?;
    let http = ensure_object(serve, "http")?;
    http.insert("enabled".to_owned(), json!(true));
    http.insert("wardline_taint_write".to_owned(), json!(true));
    write_yaml_if_changed(&path, &value)
```

In `loomweave_yaml_ok`, drop the `bind` predicate from the `serve.http` check:

```rust
        && value
            .get("serve")
            .and_then(|serve| serve.get("http"))
            .is_some_and(|http| {
                http.get("enabled").and_then(Value::as_bool) == Some(true)
                    && http.get("wardline_taint_write").and_then(Value::as_bool) == Some(true)
            }))
```

- [ ] **Step 3: Write the deterministic URL into `wardline.yaml` + `.mcp.json`**

`install_wardline_yaml`:
```rust
    loomweave.insert("url".to_owned(), json!(desired.loomweave_url));
```

`wardline_yaml_ok`:
```rust
    Ok(value
        .get("loomweave")
        .and_then(|loomweave| loomweave.get("url"))
        .and_then(Value::as_str)
        == Some(desired.loomweave_url.as_str())
        && value
            .get("filigree")
            .and_then(|filigree| filigree.get("url"))
            .and_then(Value::as_str)
            == Some(desired.wardline_filigree_url.as_str()))
```

`desired_wardline_args`:
```rust
fn desired_wardline_args(desired: &DesiredBindings) -> Value {
    json!([
        "mcp",
        "--root",
        ".",
        "--loomweave-url",
        desired.loomweave_url,
        "--filigree-url",
        desired.wardline_filigree_url
    ])
}
```

- [ ] **Step 4: Update the `--all` bindings test**

In `tests/install.rs`, `install_all_wires_three_way_integration_bindings`: the install canonicalizes `--path`, so compute the expected URL the same way. Replace the `bind`/`loomweave-url` assertions:

```rust
    // ADR-044: no fixed bind is written; the port is auto-selected at serve time.
    assert!(loomweave_yaml["serve"]["http"].get("bind").is_none());
    assert_eq!(
        loomweave_yaml["serve"]["http"]["wardline_taint_write"],
        serde_json::json!(true)
    );

    let expected_port = loomweave_federation::loomweave_port::deterministic_port(
        &dir.path().canonicalize().unwrap(),
    );
    let expected_loomweave_url = format!("http://127.0.0.1:{expected_port}");

    let wardline_yaml = read_yaml(&dir.path().join("wardline.yaml"));
    assert_eq!(wardline_yaml["loomweave"]["url"], expected_loomweave_url);
    assert_eq!(
        wardline_yaml["filigree"]["url"],
        "http://127.0.0.1:8749/api/weft/scan-results"
    );

    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["wardline"]["args"],
        serde_json::json!([
            "mcp",
            "--root",
            ".",
            "--loomweave-url",
            expected_loomweave_url,
            "--filigree-url",
            "http://127.0.0.1:8749/api/weft/scan-results"
        ])
    );
```

Confirm `tests/install.rs` can reach the helper: `loomweave-cli` depends on `loomweave-federation`, so `loomweave_federation::loomweave_port::deterministic_port` is in scope from an integration test. If the import path errors, add `use loomweave_federation::loomweave_port::deterministic_port;` and call it unqualified.

- [ ] **Step 5: Update the `doctor.rs` bindings-repair test**

In `tests/doctor.rs`, the repair test (around line 198–227) asserts `--loomweave-url http://127.0.0.1:9111`. Replace with the computed URL (the doctor test also operates on a tempdir; check whether it canonicalizes — match whatever the repaired files actually contain by computing from the same path the repair used):

```rust
    let expected_port = loomweave_federation::loomweave_port::deterministic_port(
        &dir.path().canonicalize().unwrap(),
    );
    let expected_loomweave_url = format!("http://127.0.0.1:{expected_port}");
    // ...
    assert_eq!(
        mcp["mcpServers"]["wardline"]["args"],
        serde_json::json!([
            "mcp",
            "--root",
            ".",
            "--loomweave-url",
            expected_loomweave_url,
            "--filigree-url",
            "http://127.0.0.1:8749/api/weft/scan-results"
        ])
    );
```

If the doctor test reads `loomweave.yaml["serve"]["http"]["bind"]` anywhere, change that to `.get("bind").is_none()`.

- [ ] **Step 6: Run + commit**

```bash
cargo nextest run -p loomweave-cli --test install --test doctor
cargo nextest run -p loomweave-cli --lib integration_bindings
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/loomweave-cli/src/integration_bindings.rs crates/loomweave-cli/tests/install.rs crates/loomweave-cli/tests/doctor.rs
git commit -m "feat(install): integration bindings use per-project deterministic loomweave URL, no fixed bind (ADR-044)"
```

---

## Task 6 (CUTTABLE): `resolve_loomweave_url` + its one caller (doctor)

**Files:**
- Create: `crates/loomweave-federation/src/loomweave_url.rs`
- Modify: `crates/loomweave-federation/src/lib.rs`
- Modify: `crates/loomweave-cli/src/doctor.rs`

The resolver is the reference reader of the file contract (the shape Wardline's Python twin mirrors), and `doctor`'s HTTP check is its one in-tree caller — so it ships *with* a caller, not as dead code. **This task is cuttable**: if it slips, the collision is already fixed by Tasks 1–5; defer resolver+caller as a unit.

- [ ] **Step 1: Write the failing resolver tests**

Create `crates/loomweave-federation/src/loomweave_url.rs`:

```rust
//! Resolve the live Loomweave read-API base URL (ADR-044).
//!
//! The reference reader of the `.loomweave/ephemeral.port` file contract and
//! the twin of [`crate::filigree_url`]. Precedence (consumer-side): the
//! published live port wins over a configured URL, which wins over nothing.
//! (ADR-044's higher "explicit flag/env" precedence level is realized by each
//! consumer's own CLI/env handling — e.g. Wardline's `--loomweave-url` — not by
//! this library function.) Fail-soft throughout: a missing/corrupt file folds
//! to the configured URL; absent both, `None` (federation simply degrades).

use std::path::Path;

use crate::loomweave_port::read_published_port;

/// The live published port file `.loomweave/ephemeral.port`.
pub const SOURCE_EPHEMERAL_PORT: &str = ".loomweave/ephemeral.port";
/// A statically configured URL (e.g. `wardline.yaml: loomweave.url`).
pub const SOURCE_CONFIG: &str = "config";
/// Neither a published file nor a configured URL — federation is absent.
pub const SOURCE_NONE: &str = "none";

/// Where a resolved Loomweave read-API URL came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoomweaveUrlResolution {
    /// The URL a consumer should call, or `None` when nothing resolves.
    pub resolved_url: Option<String>,
    /// One of the `SOURCE_*` labels.
    pub source: &'static str,
}

/// Resolve the read-API URL, preferring the live published port over the
/// configured URL. `configured_url` is the consumer's static fallback (pass
/// `None` if it has none).
#[must_use]
pub fn resolve_loomweave_url(
    configured_url: Option<&str>,
    project_root: &Path,
) -> LoomweaveUrlResolution {
    if let Some(port) = read_published_port(project_root) {
        return LoomweaveUrlResolution {
            resolved_url: Some(format!("http://127.0.0.1:{port}")),
            source: SOURCE_EPHEMERAL_PORT,
        };
    }
    match configured_url {
        Some(url) if !url.trim().is_empty() => LoomweaveUrlResolution {
            resolved_url: Some(url.to_owned()),
            source: SOURCE_CONFIG,
        },
        _ => LoomweaveUrlResolution {
            resolved_url: None,
            source: SOURCE_NONE,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loomweave_port::publish_port;

    #[test]
    fn published_port_beats_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        publish_port(dir.path(), 9412).unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path());
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9412"));
        assert_eq!(res.source, SOURCE_EPHEMERAL_PORT);
    }

    #[test]
    fn falls_back_to_configured_url_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path());
        assert_eq!(res.resolved_url.as_deref(), Some("http://127.0.0.1:9111"));
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn corrupt_file_folds_to_configured_url() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".loomweave")).unwrap();
        std::fs::write(
            dir.path().join(".loomweave").join("ephemeral.port"),
            "not-a-port",
        )
        .unwrap();
        let res = resolve_loomweave_url(Some("http://127.0.0.1:9111"), dir.path());
        assert_eq!(res.source, SOURCE_CONFIG);
    }

    #[test]
    fn nothing_resolves_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let res = resolve_loomweave_url(None, dir.path());
        assert_eq!(res.resolved_url, None);
        assert_eq!(res.source, SOURCE_NONE);
    }
}
```

Declare it in `lib.rs`:
```rust
pub mod loomweave_url;
```

- [ ] **Step 2: Run to verify failure, then pass**

Run: `cargo nextest run -p loomweave-federation loomweave_url`
Expected: FAIL (module not yet declared / functions absent) → after Step 1 is fully in place, PASS (4 tests).

- [ ] **Step 3: Write the failing doctor test**

In `crates/loomweave-cli/tests/doctor.rs`, add a test that a serving project's published port shows up in the HTTP check. Since spawning a real server in the doctor integration test is heavy, instead test the file-present branch by writing the file directly, then run `doctor_json` and assert the `http.config` check reports the published port:

```rust
#[test]
fn doctor_reports_published_ephemeral_port() {
    let dir = tempfile::tempdir().unwrap();
    install(&["install", "--all"], dir.path());
    // Simulate a live serve having published its port.
    let loomweave_dir = dir.path().join(".loomweave");
    std::fs::create_dir_all(&loomweave_dir).unwrap();
    std::fs::write(loomweave_dir.join("ephemeral.port"), "9876\n").unwrap();

    let (code, json) = doctor_json(dir.path(), false);
    assert_eq!(code, 0, "{json}");
    let http = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "http.config")
        .expect("http.config check present");
    assert_eq!(http["status"], "ok");
    assert!(
        http["message"].as_str().unwrap_or("").contains("9876"),
        "http.config should report the published live port: {http}"
    );
}
```

(`DoctorJsonCheck` serializes its human text as the `message` field — confirmed in `doctor.rs:105-110`.)

- [ ] **Step 4: Run to verify failure**

Run: `cargo nextest run -p loomweave-cli --test doctor doctor_reports_published`
Expected: FAIL — `check_http_config_json` does not read the published file yet.

- [ ] **Step 5: Wire the resolver into `check_http_config_json`**

Replace `check_http_config_json` in `doctor.rs`:

```rust
fn check_http_config_json(project_root: &Path) -> DoctorJsonCheck {
    let Some(config) = read_loomweave_yaml(project_root) else {
        return DoctorJsonCheck::warning("http.config", "loomweave.yaml is absent or unparseable");
    };
    let enabled = config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    if !enabled {
        return DoctorJsonCheck::warning("http.config", "HTTP serve config is disabled or incomplete");
    }
    // ADR-044: prefer the live published port over the (now usually absent)
    // static bind. A running serve publishes .loomweave/ephemeral.port.
    let resolution =
        loomweave_federation::loomweave_url::resolve_loomweave_url(None, project_root);
    if let Some(url) = resolution.resolved_url {
        return DoctorJsonCheck::ok(
            "http.config",
            format!("HTTP read API published on {url} ({})", resolution.source),
        );
    }
    let bind = config
        .get("serve")
        .and_then(|serve| serve.get("http"))
        .and_then(|http| http.get("bind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if bind.trim().is_empty() {
        DoctorJsonCheck::ok(
            "http.config",
            "HTTP enabled; read-API port auto-selected and published to .loomweave/ephemeral.port while serving",
        )
    } else {
        DoctorJsonCheck::ok("http.config", format!("HTTP configured on {bind} (auto-published while serving)"))
    }
}
```

- [ ] **Step 6: Run resolver + doctor suites**

Run:
```bash
cargo nextest run -p loomweave-federation loomweave_url
cargo nextest run -p loomweave-cli --test doctor
```
Expected: PASS.

- [ ] **Step 7: Lint + commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/loomweave-federation/src/loomweave_url.rs crates/loomweave-federation/src/lib.rs crates/loomweave-cli/src/doctor.rs crates/loomweave-cli/tests/doctor.rs
git commit -m "feat(doctor): resolve_loomweave_url + doctor reports live published read-API port (ADR-044)"
```

---

## Task 7: Docs, ADR acceptance, stopgap revert

**Files:**
- Modify: `docs/operator/loomweave-http-read-api.md`, `docs/operator/secret-scanning.md`, `docs/federation/contracts.md`
- Modify: `loomweave.yaml`, `wardline.yaml` (repo root)
- Modify: `docs/loomweave/adr/ADR-044-read-api-ephemeral-port-publication.md`, `docs/loomweave/adr/README.md`, `docs/suite/glossary.md`

- [ ] **Step 1: Update operator docs**

In each of `docs/operator/loomweave-http-read-api.md`, `docs/operator/secret-scanning.md`, `docs/federation/contracts.md`, replace the `bind: 127.0.0.1:9111` / `default: 127.0.0.1:9111` references with the auto-port description. Read each hit (from `grep -n 9111 <file>`) and rewrite in context, e.g.:

> The read-API port is auto-selected per project — a deterministic port in Loomweave's band (`9400–10399`, disjoint from Filigree's `8400–9399`) with an ephemeral fallback — and published to `.loomweave/ephemeral.port` while `serve` runs. Set `serve.http.bind` explicitly only to pin a fixed port. (ADR-044)

Leave `docs/loomweave/adr/ADR-044-*.md`'s own `9111` references (they describe the *problem*) and `docs/archive/**` (archived, non-normative) and the Filigree-side `docs/federation/filigree-side/ADR-014-*.md` (a Filigree example) unchanged.

- [ ] **Step 2: Revert the local stopgaps**

`loomweave.yaml` (repo root) currently has `serve.http.bind: 127.0.0.1:9112`. Remove the `bind:` line so this very project uses auto-port:

```yaml
serve:
  http:
    enabled: true
    wardline_taint_write: true
```

For `wardline.yaml`, make a conscious choice (advisor item 1): pin it to *this* project's deterministic port so local Wardline→Loomweave federation keeps working until the Wardline Python twin lands. Compute it:

```bash
cargo run -p loomweave-cli -- doctor --json /home/john/loomweave 2>/dev/null | grep -o '127.0.0.1:[0-9]*' | head -1
```
…or simpler, add a throwaway `#[test]` that prints `deterministic_port(Path::new("/home/john/loomweave"))`, or compute via a one-off `cargo run`. Then set:

```yaml
loomweave:
  # ADR-044: pinned to this project's deterministic read-API port. The published
  # .loomweave/ephemeral.port overrides this once Wardline resolves consume-time
  # (clarion-7f574bc34f follow-up). Until then this static target keeps local
  # wardline -> loomweave federation working.
  url: http://127.0.0.1:<COMPUTED_PORT>
```

Verify by starting serve and confirming the published file matches:
```bash
# In one shell:
cargo run -p loomweave-cli -- serve /home/john/loomweave &
sleep 2
cat /home/john/loomweave/.loomweave/ephemeral.port   # should equal <COMPUTED_PORT>
kill %1
```

- [ ] **Step 3: Glossary verdict (acceptance gate)**

`docs/loomweave/adr/README.md` requires a `glossary.md` verdict before an ADR moves Proposed→Accepted for any cross-product-visible term. `.loomweave/ephemeral.port` mirrors Filigree's `.filigree/ephemeral.port` — a **managed clash** (shared convention, distinct per-product paths). Read `docs/suite/glossary.md`, find the `ephemeral.port` / Filigree entry, and add a Loomweave row recording the managed-clash verdict and the mapping (`.filigree/ephemeral.port` ↔ `.loomweave/ephemeral.port`, identical format, loopback-only). If no such entry exists, add one under the federation-terms section.

- [ ] **Step 4: Flip ADR-044 to Accepted**

In `ADR-044-*.md`, change `**Status**: Proposed` → `**Status**: Accepted` and add a one-line acceptance note referencing the glossary verdict and the implementing commits. In `README.md`, change the ADR-044 row's trailing `| Proposed |` → `| Accepted |`.

- [ ] **Step 5: Full CI floor**

Run the complete gate (CLAUDE.md):
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```
Expected: all green.

- [ ] **Step 6: Wardline boundary gate**

This feature reads external input (the port file, config files). Run:
```bash
wardline scan . --fail-on ERROR
```
Expected: exit 0. If it trips, fix at the boundary (the `read_published_port` parse is already validated/fail-soft; address any new finding).

- [ ] **Step 7: Commit + close the issue**

```bash
git add docs/operator docs/federation/contracts.md docs/loomweave/adr docs/suite/glossary.md loomweave.yaml wardline.yaml
git commit -m "docs(adr): accept ADR-044; auto-port docs, glossary verdict, revert 9112 stopgap"
```

Close `clarion-7f574bc34f` with a summary comment (CLI): `filigree close clarion-7f574bc34f --actor opus`.

---

## Self-Review

**Spec coverage (ADR-044 §Decision + §Verification):**
- Decision 1 (deterministic port + ephemeral fallback) → Task 1 (`deterministic_port`) + Task 3 (fallback).
- Decision 2 (publish `.loomweave/ephemeral.port` per file contract) → Task 1 (`publish_port`, atomic, port-only, trailing `\n`) + Task 3 (loopback-only, lifecycle via RAII).
- Decision 3 (loomweave-side resolver, one of conforming readers) → Task 6 (`resolve_loomweave_url` + doctor caller).
- Decision 4 (installer stops pinning a port; explicit override honored) → Task 4 (stub) + Task 5 (bindings) + Task 2 (`Some` honored, `None` auto).
- Verification: distinct ports/no bind failure → T3 `auto_port_publishes_distinct_ports_per_project`; collision→ephemeral fallback reflects actual port → T3 (fallback path) + T1; file contract (bare port, temp+rename, no file on non-loopback) → T1 + T3 publish branch; precedence (file>config>none) → T6; fail-soft (malformed/out-of-range/refused) → T1 read tests + T6 corrupt test; removed on clean shutdown → T3 `auto_port_file_removed_on_clean_shutdown`; wardline scan against non-9111 serve → realized by Task 5 deterministic URL + Task 7 local verify.
- Resolved-but-refused (closed port) softness: covered behaviorally — `resolve_loomweave_url` returns the URL; the *connection attempt* is the consumer's (Wardline's) responsibility, and the ADR-034 instance-ID guard backstops a stale file. No in-tree consumer connects, so no Rust test asserts refusal; noted, not silently dropped.

**Placeholder scan:** every code step shows complete code. Two reads-to-confirm remain (doctor `DoctorJsonCheck` serialized field name in T6 Step 3; any bare-stub bind assertion in T4 Step 2) — both are explicit "read X, match the real name" instructions with the fallback spelled out, not hidden TODOs.

**Type consistency:** `deterministic_port(&Path) -> u16`, `read_published_port(&Path) -> Option<u16>`, `publish_port(&Path, u16) -> io::Result<()>`, `remove_published_port(&Path)`, `published_port_path(&Path) -> PathBuf`, `resolve_loomweave_url(Option<&str>, &Path) -> LoomweaveUrlResolution` are used identically across Tasks 1, 3, 5, 6. `HttpReadConfig.bind: Option<SocketAddr>` is consistent across Tasks 2–3 and all test sites. `auto_port: bool` is added in Task 2 and consumed in Task 3 (placeholder `_auto_port` removed there).
