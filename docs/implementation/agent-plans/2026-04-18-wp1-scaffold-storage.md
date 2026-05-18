# WP1 — Scaffold + Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the Sprint 1 walking-skeleton storage foundation — Cargo workspace, full SQLite schema migration, writer-actor with per-N-batch transactions, entity-ID assembler, and `clarion install` + `clarion analyze` CLI skeletons. Plugin spawning is WP2's concern; Sprint 1 WP1 must exit with `runs.status = 'skipped_no_plugins'`.

**Architecture:** Three-crate Cargo workspace: `clarion-core` (domain types + entity-ID + LlmProvider trait stub), `clarion-storage` (SQLite layer + writer-actor over a bounded tokio mpsc channel per ADR-011), `clarion-cli` (binary). Writer-actor is a `tokio::task` owning the sole write `rusqlite::Connection`; readers come from a `deadpool-sqlite` pool. Full schema from `detailed-design.md §3` ships in migration `0001_initial_schema.sql` even though Sprint 1 only writes `entities` + `runs`. The design pressure is applied now so Sprint 2+ doesn't face data-migration work. **Tooling baseline per ADR-023** lands with Task 1 before any other code — edition 2024, workspace `[lints]` pedantic, rustfmt/clippy configs, cargo-nextest, cargo-deny, GitHub Actions CI — so every subsequent commit passes the strict floor from day one.

**Tech Stack:** **Rust 2024** stable (ADR-023); workspace `[lints]` with `clippy::pedantic = "warn"` + `unsafe_code = "forbid"`; `rusqlite` (bundled SQLite); `deadpool-sqlite`; `tokio` (rt-multi-thread, macros, sync); `clap` (CLI); `thiserror` (library errors); `anyhow` (binary); `tracing` + `tracing-subscriber`; `assert_cmd` + `tempfile` (CLI integration tests); **`cargo-nextest`** (test runner); **`cargo-deny`** (supply chain); **GitHub Actions** (CI gates).

**Source spec:** `docs/implementation/sprint-1/wp1-scaffold.md`. This plan is its TDD execution walk. If the two disagree, the spec is authoritative on *what* to build; this plan is authoritative on *how* to build it step-by-step.

**ADR anchors:** ADR-001 (Rust + rusqlite + tokio), ADR-003 (entity-ID 3-segment form), ADR-011 (writer-actor + per-N-batch + PRAGMA set), ADR-022 (grammar on `plugin_id` and `kind`), **ADR-023 (tooling baseline — edition 2024, pedantic, cargo-deny, nextest, CI)**. ADR-005 is authored as a side effect of Task 5; ADR-023 is pre-authored and lands verbatim in Task 1.

**Resolved UQs before starting:**
- **UQ-WP1-01** rusqlite + bundled SQLite (ADR-011).
- **UQ-WP1-02** tokio from day one (ADR-011).
- **UQ-WP1-03** per-command oneshot ack. Commit-counter test hook uses an `Arc<AtomicUsize>` threaded through `Writer::spawn` — keeps the hook path identical in release and test builds; no `#[cfg(test)]` branches in the hot loop.
- **UQ-WP1-04** `.gitignore` seeded with: `tmp/`, `logs/`, `*.shadow.db`, `*.wal`, `*.shm`, `runs/*/log.jsonl`. Tracked: `clarion.db`, `config.json`, schema history in the DB.
- **UQ-WP1-05** `runs` row shape matches `detailed-design.md §3:695-701` fully; Sprint 1 inserts NULL/JSON-`{}` for plugin-invocation columns, WP2 fills them.
- **UQ-WP1-06** `clarion-storage` wraps `rusqlite::Error` in a crate-local `StorageError` via `thiserror`.
- **UQ-WP1-07** assembler rejects any segment containing `:` with an `EntityIdError::SegmentContainsColon` — documents the grammar contract as a type-checked invariant.
- **UQ-WP1-08** `clarion install` refuses if `.clarion/` exists without `--force`; `--force` is recognised in clap but returns `unimplemented in Sprint 1` at runtime.
- **UQ-WP1-09** **reopened and re-resolved by ADR-023**: Rust **edition 2024**, workspace `[lints]` block with `clippy::pedantic = "warn"` + `unsafe_code = "forbid"`, pinned `rustfmt.toml` + `clippy.toml`, **`cargo-nextest`** as the test runner, **`cargo-deny`** for supply-chain hygiene, **GitHub Actions CI** running fmt-check + pedantic clippy + nextest + cargo-doc + cargo-deny on every PR. `rust-toolchain.toml` pins `stable` + `clippy`/`rustfmt`/`llvm-tools-preview`. The original "fine to document and move on" framing was the tell for unexamined tech debt; adopted at the zero-code frontier where retrofit cost is zero.

**Scope note on entity-ID format:** ADR-003 fixes the 3-segment form as `{plugin_id}:{kind}:{canonical_qualified_name}`. The assembler (Task 2) validates `plugin_id` + `kind` against the ADR-022 grammar (`[a-z][a-z0-9_]*`) and rejects any segment containing `:`. It is **format-agnostic on `canonical_qualified_name`** — that segment's internal shape is the emitting plugin's concern (Python plugin: dotted qualname; core file-discovery: `{hash}@{path}`; etc.). Sprint 1 tests use simplified example strings to exercise concatenation; the plugin-specific shapes are validated by WP3 + the core file-discovery pass post-Sprint-1.

---

## Task 1: Workspace skeleton + ADR-023 tooling baseline

**Files:**
- Create: `/home/john/clarion/Cargo.toml` (workspace root, `[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`)
- Create: `/home/john/clarion/rust-toolchain.toml`
- Create: `/home/john/clarion/rustfmt.toml`
- Create: `/home/john/clarion/clippy.toml`
- Create: `/home/john/clarion/deny.toml`
- Create: `/home/john/clarion/.github/workflows/ci.yml`
- Create: `/home/john/clarion/.gitignore`
- Create: `/home/john/clarion/crates/clarion-core/Cargo.toml`
- Create: `/home/john/clarion/crates/clarion-core/src/lib.rs`
- Create: `/home/john/clarion/crates/clarion-storage/Cargo.toml`
- Create: `/home/john/clarion/crates/clarion-storage/src/lib.rs`
- Create: `/home/john/clarion/crates/clarion-cli/Cargo.toml`
- Create: `/home/john/clarion/crates/clarion-cli/src/main.rs`

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

Write `/home/john/clarion/Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = [
    "crates/clarion-core",
    "crates/clarion-storage",
    "crates/clarion-cli",
]

[workspace.package]
version = "0.1.0-dev"
edition = "2024"
license = "MIT OR Apache-2.0"
repository = "https://github.com/qacona/clarion"
rust-version = "1.85"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# Pragmatic allows per ADR-023 — revisit per WP if the floor gets too loud.
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"

[workspace.dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
deadpool-sqlite = { version = "0.8", features = ["rt_tokio_1"] }
rusqlite = { version = "0.31", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
assert_cmd = "2"
tempfile = "3"
```

The `resolver = "3"` value is required for edition 2024. Priority `-1` on `clippy::pedantic` lets the pragmatic allows override individual pedantic lints correctly (the `level = "warn"` group takes priority `0` by default, which means individual allow-lints of the same group would otherwise lose the tie).

- [ ] **Step 2: Pin the toolchain**

Write `/home/john/clarion/rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
profile = "minimal"
```

`llvm-tools-preview` is carried from Task 1 onward per ADR-023 so `cargo install cargo-llvm-cov` works first try in a later WP without a retrofit.

- [ ] **Step 3: Write `rustfmt.toml`**

Write `/home/john/clarion/rustfmt.toml`:

```toml
edition = "2024"
max_width = 100
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
```

- [ ] **Step 4: Write `clippy.toml`**

Write `/home/john/clarion/clippy.toml`:

```toml
cognitive-complexity-threshold = 15
too-many-arguments-threshold = 8
too-many-lines-threshold = 120
```

- [ ] **Step 5: Write `deny.toml`**

Write `/home/john/clarion/deny.toml`:

```toml
# deny.toml — cargo-deny v2 schema. Anything not in `allow` is denied.

[advisories]
version = 2
yanked = "deny"
ignore = []

[licenses]
version = 2
allow = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "Unicode-DFS-2016",
]
confidence-threshold = 0.8

[bans]
multiple-versions = "warn"
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

- [ ] **Step 6: Write the GitHub Actions CI workflow**

Write `/home/john/clarion/.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-D warnings"

jobs:
  rust:
    name: Rust
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - uses: Swatinem/rust-cache@v2

      - name: fmt
        run: cargo fmt --all -- --check

      - name: clippy
        run: cargo clippy --workspace --all-targets --all-features -- -D warnings

      - name: install cargo-nextest
        uses: taiki-e/install-action@cargo-nextest

      - name: test
        run: cargo nextest run --workspace --all-features

      - name: doc
        run: cargo doc --workspace --no-deps --all-features

      - name: install cargo-deny
        uses: taiki-e/install-action@cargo-deny

      - name: deny
        run: cargo deny check
```

`python-plugin` job is added by WP3 Task 1; Sprint-1 WP1 ships with the Rust job only.

- [ ] **Step 7: Write the repo-root `.gitignore`**

Write `/home/john/clarion/.gitignore`:

```
/target
**/*.rs.bk
Cargo.lock.bak

# SQLite working files (project-level .clarion/ is tracked per ADR-005)
*.db-journal
*.db-wal

# Rust-analyzer / IDE caches
/.idea
/.vscode
```

Note: we do **not** ignore `*.db` here. `.clarion/clarion.db` is tracked per ADR-005 (authored in Task 5); only write-ahead files are excluded.

- [ ] **Step 8: Write `clarion-core`'s `Cargo.toml` and `lib.rs`**

Write `/home/john/clarion/crates/clarion-core/Cargo.toml`:

```toml
[package]
name = "clarion-core"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
```

Write `/home/john/clarion/crates/clarion-core/src/lib.rs`:

```rust
//! clarion-core — domain types, identifiers, and provider traits.
//!
//! This crate is dependency-light and contains no I/O. Storage and CLI
//! crates depend on it; it depends on neither.
```

- [ ] **Step 9: Write `clarion-storage`'s `Cargo.toml` and `lib.rs`**

Write `/home/john/clarion/crates/clarion-storage/Cargo.toml`:

```toml
[package]
name = "clarion-storage"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
clarion-core = { path = "../clarion-core" }
deadpool-sqlite.workspace = true
rusqlite.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true

[dev-dependencies]
tempfile.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "sync", "time", "test-util"] }
```

Write `/home/john/clarion/crates/clarion-storage/src/lib.rs`:

```rust
//! clarion-storage — SQLite layer, writer-actor, reader pool.
//!
//! All mutations route through the writer actor (a single `tokio::task`
//! owning the sole write `rusqlite::Connection`). Readers come from a
//! `deadpool-sqlite` pool. See ADR-011.
```

- [ ] **Step 10: Write `clarion-cli`'s `Cargo.toml` and `main.rs`**

Write `/home/john/clarion/crates/clarion-cli/Cargo.toml`:

```toml
[package]
name = "clarion-cli"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[[bin]]
name = "clarion"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
clap.workspace = true
clarion-core = { path = "../clarion-core" }
clarion-storage = { path = "../clarion-storage" }
serde_json.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true

[dev-dependencies]
assert_cmd.workspace = true
tempfile.workspace = true
```

Write `/home/john/clarion/crates/clarion-cli/src/main.rs`:

```rust
//! clarion — command-line entry point.
//!
//! Real subcommand implementations land in Tasks 5 and 7. This Task-1
//! stub exists so the workspace compiles pedantic-clean from day one.

fn main() -> anyhow::Result<()> {
    eprintln!("clarion: unimplemented (Sprint 1 WP1 scaffold — Task 1)");
    std::process::exit(2);
}
```

- [ ] **Step 11: Install required dev tooling (one-time, local only)**

If `cargo nextest` and `cargo deny` are not yet installed on the dev machine:

```bash
cargo install cargo-nextest --locked
cargo install cargo-deny --locked
```

CI installs these via `taiki-e/install-action`; local execution needs them once per machine.

- [ ] **Step 12: Verify every ADR-023 gate passes locally**

Run each in sequence. Every one must exit zero before committing:

```bash
cd /home/john/clarion && cargo build --workspace
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo nextest run --workspace --all-features
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

Expected: all six commands exit 0. `cargo nextest run` reports "no tests to run" at this stage (Task 2 lands the first tests). `cargo deny check` may warn about `multiple-versions` if two transitive deps resolve different versions of the same crate — warnings are fine; only errors block.

If `cargo clippy` fires any pedantic warning (from `clippy::pedantic = "warn"` × `-D warnings` escalation), fix it in the offending file. Common Task-1 cases: missing `# Errors` doc on public fn (pragmatic-allowed, should not fire), `eprintln!` over `tracing` (a stub in `main.rs` — leave it), or unused imports (fix).

- [ ] **Step 13: Commit**

```bash
cd /home/john/clarion && git add Cargo.toml rust-toolchain.toml rustfmt.toml clippy.toml deny.toml .github/ .gitignore crates/ && git commit -m "$(cat <<'EOF'
feat(wp1): workspace skeleton + ADR-023 tooling baseline

Cargo workspace with clarion-core, clarion-storage, clarion-cli members,
edition 2024, resolver 3, workspace [lints] block with clippy::pedantic =
"warn" + unsafe_code = "forbid" (ADR-023). Every member crate declares
lints.workspace = true so a later-added crate cannot drift off the floor.

ADR-011 dep stack pinned at workspace level: rusqlite (bundled),
deadpool-sqlite, tokio, thiserror, clap, tracing. rust-toolchain.toml pins
stable with clippy + rustfmt + llvm-tools-preview components.

rustfmt.toml, clippy.toml, deny.toml configured per ADR-023. GitHub
Actions workflow runs fmt-check + pedantic clippy + cargo-nextest +
cargo-doc + cargo-deny on every PR. python-plugin job arrives with WP3
Task 1.

Resolves UQ-WP1-09 (reopened from the original "edition 2021, fine to
document and move on" framing).
EOF
)"
```

---

## Task 2: Entity-ID assembler (L2)

**Files:**
- Create: `/home/john/clarion/crates/clarion-core/src/entity_id.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/lib.rs`

- [ ] **Step 1: Write the failing unit tests**

Write `/home/john/clarion/crates/clarion-core/src/entity_id.rs`:

```rust
//! Entity-ID assembler.
//!
//! Per ADR-003 + ADR-022, every Clarion entity has a stable 3-segment ID:
//! `{plugin_id}:{kind}:{canonical_qualified_name}`.
//!
//! - `plugin_id` and `kind` must match the grammar `[a-z][a-z0-9_]*`.
//! - `canonical_qualified_name` is opaque to this assembler: its internal
//!   shape is the emitting plugin's concern (dotted qualnames for the
//!   Python plugin; content-addressed for core-minted file entities).
//! - No segment may contain a literal `:` — the separator is reserved.
//!   ADR-022's grammar precludes it in `plugin_id`/`kind`; `canonical_qualified_name`
//!   is checked at assembly time (UQ-WP1-07).

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntityId(String);

impl EntityId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EntityIdError {
    #[error("segment {field} empty")]
    EmptySegment { field: &'static str },

    #[error("segment {field} violates ADR-022 grammar [a-z][a-z0-9_]*: {value:?}")]
    GrammarViolation { field: &'static str, value: String },

    #[error("segment {field} contains reserved ':' separator: {value:?}")]
    SegmentContainsColon { field: &'static str, value: String },
}

/// Assemble an [`EntityId`] from its three segments.
///
/// `plugin_id` and `kind` are validated against the ADR-022 grammar.
/// `canonical_qualified_name` is opaque but may not contain `:`.
pub fn entity_id(
    plugin_id: &str,
    kind: &str,
    canonical_qualified_name: &str,
) -> Result<EntityId, EntityIdError> {
    validate_grammar("plugin_id", plugin_id)?;
    validate_grammar("kind", kind)?;
    validate_no_colon("canonical_qualified_name", canonical_qualified_name)?;
    if canonical_qualified_name.is_empty() {
        return Err(EntityIdError::EmptySegment {
            field: "canonical_qualified_name",
        });
    }
    Ok(EntityId(format!(
        "{plugin_id}:{kind}:{canonical_qualified_name}"
    )))
}

fn validate_grammar(field: &'static str, value: &str) -> Result<(), EntityIdError> {
    if value.is_empty() {
        return Err(EntityIdError::EmptySegment { field });
    }
    validate_no_colon(field, value)?;
    let mut chars = value.chars();
    let first = chars.next().expect("non-empty checked above");
    if !first.is_ascii_lowercase() {
        return Err(EntityIdError::GrammarViolation {
            field,
            value: value.to_owned(),
        });
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(EntityIdError::GrammarViolation {
                field,
                value: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_no_colon(field: &'static str, value: &str) -> Result<(), EntityIdError> {
    if value.contains(':') {
        return Err(EntityIdError::SegmentContainsColon {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_level_function() {
        let id = entity_id("python", "function", "demo.hello").unwrap();
        assert_eq!(id.as_str(), "python:function:demo.hello");
    }

    #[test]
    fn class_method() {
        let id = entity_id("python", "function", "demo.Foo.bar").unwrap();
        assert_eq!(id.as_str(), "python:function:demo.Foo.bar");
    }

    #[test]
    fn nested_function_uses_python_locals_marker() {
        let id = entity_id("python", "function", "demo.outer.<locals>.inner").unwrap();
        assert_eq!(id.as_str(), "python:function:demo.outer.<locals>.inner");
    }

    #[test]
    fn core_reserved_file_kind() {
        // The file-entity canonical_qualified_name shape is core-file-discovery's
        // concern (per detailed-design.md §2:229). Sprint 1 only tests the
        // assembler's concatenation; `src/demo.py` is a stand-in.
        let id = entity_id("core", "file", "src/demo.py").unwrap();
        assert_eq!(id.as_str(), "core:file:src/demo.py");
    }

    #[test]
    fn core_reserved_subsystem_kind() {
        let id = entity_id("core", "subsystem", "a1b2c3d4").unwrap();
        assert_eq!(id.as_str(), "core:subsystem:a1b2c3d4");
    }

    #[test]
    fn rejects_empty_plugin_id() {
        assert_eq!(
            entity_id("", "function", "demo.hello"),
            Err(EntityIdError::EmptySegment { field: "plugin_id" }),
        );
    }

    #[test]
    fn rejects_empty_kind() {
        assert_eq!(
            entity_id("python", "", "demo.hello"),
            Err(EntityIdError::EmptySegment { field: "kind" }),
        );
    }

    #[test]
    fn rejects_empty_qualified_name() {
        assert_eq!(
            entity_id("python", "function", ""),
            Err(EntityIdError::EmptySegment {
                field: "canonical_qualified_name",
            }),
        );
    }

    #[test]
    fn rejects_uppercase_plugin_id() {
        assert!(matches!(
            entity_id("Python", "function", "demo.hello"),
            Err(EntityIdError::GrammarViolation { field: "plugin_id", .. })
        ));
    }

    #[test]
    fn rejects_digit_prefixed_kind() {
        assert!(matches!(
            entity_id("python", "1function", "demo.hello"),
            Err(EntityIdError::GrammarViolation { field: "kind", .. })
        ));
    }

    #[test]
    fn rejects_hyphen_in_kind() {
        assert!(matches!(
            entity_id("python", "func-tion", "demo.hello"),
            Err(EntityIdError::GrammarViolation { field: "kind", .. })
        ));
    }

    #[test]
    fn rejects_colon_in_qualified_name() {
        assert!(matches!(
            entity_id("python", "function", "demo:hello"),
            Err(EntityIdError::SegmentContainsColon { field: "canonical_qualified_name", .. })
        ));
    }

    #[test]
    fn rejects_colon_in_plugin_id() {
        // Defence in depth: grammar check rejects this, but the colon
        // check fires first and produces a more descriptive error.
        let err = entity_id("py:thon", "function", "demo.hello").unwrap_err();
        assert!(matches!(
            err,
            EntityIdError::SegmentContainsColon { field: "plugin_id", .. }
        ));
    }

    #[test]
    fn entity_id_serialises_as_string() {
        let id = entity_id("python", "function", "demo.hello").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"python:function:demo.hello\"");
    }
}
```

Modify `/home/john/clarion/crates/clarion-core/src/lib.rs` to:

```rust
//! clarion-core — domain types, identifiers, and provider traits.
//!
//! This crate is dependency-light and contains no I/O. Storage and CLI
//! crates depend on it; it depends on neither.

pub mod entity_id;

pub use entity_id::{entity_id, EntityId, EntityIdError};
```

- [ ] **Step 2: Run the tests and confirm they pass**

The tests above compile and drive the implementation at the same time (the implementation is written alongside, not separately). Run:

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core -E 'test(entity_id)'
```

Expected: all 13 tests pass. If any test fails, the implementation above has a bug — fix the code, not the test.

- [ ] **Step 3: Confirm no clippy warnings**

```bash
cd /home/john/clarion && cargo clippy -p clarion-core --all-targets -- -D warnings
```

Expected: no warnings. If clippy complains about unused imports or dead code, address before committing.

- [ ] **Step 4: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp1): L2 entity-ID assembler per ADR-003 + ADR-022

entity_id() assembles {plugin_id}:{kind}:{canonical_qualified_name} with
ADR-022 grammar enforcement on plugin_id and kind. Rejects empty segments
and any segment containing ':' (UQ-WP1-07 resolution).

13 unit tests cover positive grammar cases (module fn, class method,
nested fn, core-reserved file/subsystem), empty-segment rejections,
grammar violations, and colon-in-segment detection.

canonical_qualified_name is validated for empty + colon only; its internal
shape is the emitting plugin's concern per ADR-022.
EOF
)"
```

---

## Task 3: Schema migration file (L1)

**Files:**
- Create: `/home/john/clarion/crates/clarion-storage/migrations/0001_initial_schema.sql`
- Create: `/home/john/clarion/crates/clarion-storage/src/error.rs`
- Create: `/home/john/clarion/crates/clarion-storage/src/schema.rs`
- Create: `/home/john/clarion/crates/clarion-storage/src/pragma.rs`
- Modify: `/home/john/clarion/crates/clarion-storage/src/lib.rs`
- Create: `/home/john/clarion/crates/clarion-storage/tests/schema_apply.rs`

- [ ] **Step 1: Write the migration SQL**

Write `/home/john/clarion/crates/clarion-storage/migrations/0001_initial_schema.sql`. The SQL below is transcribed directly from `detailed-design.md §3:593-755` plus the migration-framework `schema_migrations` meta table. Do not summarise, abbreviate, or drop anything — the full shape is load-bearing per the L1 lock-in.

```sql
-- ============================================================================
-- Clarion migration 0001 — initial schema.
--
-- Source: docs/clarion/v0.1/detailed-design.md §3 (Storage Implementation).
-- Sprint 1 walking skeleton writes only to `entities` and `runs`, but every
-- table, FTS5 virtual table, trigger, generated column, index, and view
-- is created here so the full shape is frozen at L1-lock time. See ADR-011
-- for the writer-actor + per-N-files transaction model this schema supports.
-- ============================================================================

BEGIN;

-- Meta: migration tracking. Not in detailed-design §3 — it's the runner's own
-- bookkeeping table. Applied migrations append a row here; re-runs are no-ops.
CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    applied_at  TEXT NOT NULL
);

-- Entities
CREATE TABLE entities (
    id                 TEXT PRIMARY KEY,
    plugin_id          TEXT NOT NULL,
    kind               TEXT NOT NULL,
    name               TEXT NOT NULL,
    short_name         TEXT NOT NULL,
    parent_id          TEXT REFERENCES entities(id),
    source_file_id     TEXT REFERENCES entities(id),
    source_byte_start  INTEGER,
    source_byte_end    INTEGER,
    source_line_start  INTEGER,
    source_line_end    INTEGER,
    properties         TEXT NOT NULL,
    content_hash       TEXT,
    summary            TEXT,
    wardline           TEXT,
    first_seen_commit  TEXT,
    last_seen_commit   TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX ix_entities_last_seen_commit ON entities(last_seen_commit);
CREATE INDEX ix_entities_kind              ON entities(kind);
CREATE INDEX ix_entities_plugin_kind       ON entities(plugin_id, kind);
CREATE INDEX ix_entities_parent            ON entities(parent_id);
CREATE INDEX ix_entities_source_file       ON entities(source_file_id);
CREATE INDEX ix_entities_content_hash      ON entities(content_hash);

-- Tags (denormalised)
CREATE TABLE entity_tags (
    entity_id  TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    tag        TEXT NOT NULL,
    PRIMARY KEY (entity_id, tag)
);
CREATE INDEX ix_entity_tags_tag ON entity_tags(tag);

-- Edges. Deduped by (kind, from_id, to_id); see detailed-design.md §3 note.
CREATE TABLE edges (
    id                 TEXT PRIMARY KEY,
    kind               TEXT NOT NULL,
    from_id            TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_id              TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    properties         TEXT,
    source_file_id     TEXT REFERENCES entities(id),
    source_byte_start  INTEGER,
    source_byte_end    INTEGER,
    UNIQUE (kind, from_id, to_id)
);
CREATE INDEX ix_edges_from_kind ON edges(from_id, kind);
CREATE INDEX ix_edges_to_kind   ON edges(to_id,   kind);
CREATE INDEX ix_edges_kind      ON edges(kind);

-- Findings
CREATE TABLE findings (
    id                  TEXT PRIMARY KEY,
    tool                TEXT NOT NULL,
    tool_version        TEXT NOT NULL,
    run_id              TEXT NOT NULL,
    rule_id             TEXT NOT NULL,
    kind                TEXT NOT NULL,
    severity            TEXT NOT NULL,
    confidence          REAL,
    confidence_basis    TEXT,
    entity_id           TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    related_entities    TEXT NOT NULL,
    message             TEXT NOT NULL,
    evidence            TEXT NOT NULL,
    properties          TEXT NOT NULL,
    supports            TEXT NOT NULL,
    supported_by        TEXT NOT NULL,
    status              TEXT NOT NULL,
    suppression_reason  TEXT,
    filigree_issue_id   TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX ix_findings_entity    ON findings(entity_id);
CREATE INDEX ix_findings_rule      ON findings(rule_id);
CREATE INDEX ix_findings_tool_rule ON findings(tool, rule_id);
CREATE INDEX ix_findings_run       ON findings(run_id);
CREATE INDEX ix_findings_status    ON findings(status);

-- Summary cache
CREATE TABLE summary_cache (
    entity_id             TEXT NOT NULL,
    content_hash          TEXT NOT NULL,
    prompt_template_id    TEXT NOT NULL,
    model_tier            TEXT NOT NULL,
    guidance_fingerprint  TEXT NOT NULL,
    summary_json          TEXT NOT NULL,
    cost_usd              REAL NOT NULL,
    tokens_input          INTEGER NOT NULL,
    tokens_output         INTEGER NOT NULL,
    created_at            TEXT NOT NULL,
    PRIMARY KEY (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)
);

-- Runs (provenance). Sprint 1 writes started_at/completed_at/config/stats/status;
-- WP2 will populate plugin-invocation fields inside `config` JSON (per UQ-WP1-05).
CREATE TABLE runs (
    id            TEXT PRIMARY KEY,
    started_at    TEXT NOT NULL,
    completed_at  TEXT,
    config        TEXT NOT NULL,
    stats         TEXT NOT NULL,
    status        TEXT NOT NULL
);

-- FTS5 for text search
CREATE VIRTUAL TABLE entity_fts USING fts5(
    entity_id UNINDEXED,
    name,
    short_name,
    summary_text,
    content_text,
    tokenize = 'porter unicode61'
);

-- FTS5 triggers keep entity_fts synchronised with entities.
CREATE TRIGGER entities_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entity_fts (entity_id, name, short_name, summary_text, content_text)
    VALUES (
        new.id,
        new.name,
        new.short_name,
        COALESCE(json_extract(new.summary, '$.briefing.purpose'), ''),
        ''
    );
END;
CREATE TRIGGER entities_au AFTER UPDATE ON entities BEGIN
    UPDATE entity_fts
    SET name         = new.name,
        short_name   = new.short_name,
        summary_text = COALESCE(json_extract(new.summary, '$.briefing.purpose'), '')
    WHERE entity_id = new.id;
END;
CREATE TRIGGER entities_ad AFTER DELETE ON entities BEGIN
    DELETE FROM entity_fts WHERE entity_id = old.id;
END;

-- Generated columns + partial indexes for hot JSON properties.
ALTER TABLE entities ADD COLUMN priority TEXT
    GENERATED ALWAYS AS (json_extract(properties, '$.priority')) VIRTUAL;
CREATE INDEX ix_entities_priority ON entities(priority) WHERE priority IS NOT NULL;

ALTER TABLE entities ADD COLUMN git_churn_count INTEGER
    GENERATED ALWAYS AS (json_extract(properties, '$.git_churn_count')) VIRTUAL;
CREATE INDEX ix_entities_churn ON entities(git_churn_count) WHERE git_churn_count IS NOT NULL;

-- View for guidance resolver. Note: this view references an `entity_tags`
-- join indirectly via the `tags` column — but detailed-design §3 writes
-- `tags` directly from entities, which does not exist as a column. To
-- honour the detailed-design literally the view joins through entity_tags
-- using a subquery that aggregates.
CREATE VIEW guidance_sheets AS
SELECT
    e.id,
    e.name,
    json_extract(e.properties, '$.priority')             AS priority,
    json_extract(e.properties, '$.scope.query_types')    AS query_types,
    json_extract(e.properties, '$.scope.token_budget')   AS token_budget,
    json_extract(e.properties, '$.match_rules')          AS match_rules,
    json_extract(e.properties, '$.content')              AS content,
    json_extract(e.properties, '$.expires')              AS expires,
    (
        SELECT json_group_array(tag)
        FROM entity_tags
        WHERE entity_id = e.id
    )                                                     AS tags
FROM entities e
WHERE e.kind = 'guidance';

-- Record the migration.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (1, '0001_initial_schema', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
```

**Note on the `guidance_sheets` view**: `detailed-design.md §3:746-755` references a bare `tags` column on `entities`, but the schema has `tags` normalised into `entity_tags`. The view above aggregates `entity_tags.tag` via a correlated subquery into a JSON array — this is the faithful join shape and produces the same row shape the design doc implies. If the detailed-design is updated post-Sprint-1 to match this, that's the design-doc's bug and not ours.

- [ ] **Step 2: Write the `StorageError` type**

Write `/home/john/clarion/crates/clarion-storage/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("connection-pool error: {0}")]
    Pool(String),

    #[error("migration {version} failed: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("channel closed — writer actor has exited")]
    WriterGone,

    #[error("writer actor returned no response")]
    WriterNoResponse,
}

pub type Result<T> = std::result::Result<T, StorageError>;
```

- [ ] **Step 3: Write the PRAGMA application helper**

Write `/home/john/clarion/crates/clarion-storage/src/pragma.rs`:

```rust
//! PRAGMAs applied at connection open per ADR-011 §SQLite PRAGMAs.

use rusqlite::Connection;

use crate::error::Result;

/// Apply the write-side PRAGMA set: WAL, synchronous=NORMAL, busy_timeout,
/// wal_autocheckpoint, foreign_keys. Called on the writer's connection once,
/// immediately after open.
pub fn apply_write_pragmas(conn: &Connection) -> Result<()> {
    // `journal_mode = WAL` needs query_row because it returns the new mode.
    let mode: String =
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    debug_assert_eq!(mode.to_ascii_lowercase(), "wal", "WAL not enabled");
    conn.execute_batch(concat!(
        "PRAGMA synchronous = NORMAL;",
        "PRAGMA busy_timeout = 5000;",
        "PRAGMA wal_autocheckpoint = 1000;",
        "PRAGMA foreign_keys = ON;",
    ))?;
    Ok(())
}

/// Apply the read-side PRAGMA set: busy_timeout + foreign_keys. Readers do not
/// set journal_mode (WAL is a database-level mode set by the first writer).
pub fn apply_read_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(concat!(
        "PRAGMA busy_timeout = 5000;",
        "PRAGMA foreign_keys = ON;",
    ))?;
    Ok(())
}
```

- [ ] **Step 4: Write the schema migration runner**

Write `/home/john/clarion/crates/clarion-storage/src/schema.rs`:

```rust
//! Schema migration runner.
//!
//! Migrations are embedded at compile time via `include_str!`. On apply, each
//! is run inside its own transaction if not already recorded in
//! `schema_migrations`. Running twice is a no-op.

use rusqlite::{params, Connection};

use crate::error::{Result, StorageError};

struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "0001_initial_schema",
    sql: include_str!("../migrations/0001_initial_schema.sql"),
}];

/// Apply every migration not already recorded in `schema_migrations`.
///
/// The first migration creates the `schema_migrations` table itself, so the
/// initial `SELECT` is tolerant of its absence.
pub fn apply_migrations(conn: &mut Connection) -> Result<()> {
    let applied = read_applied_versions(conn)?;
    for m in MIGRATIONS {
        if applied.contains(&m.version) {
            tracing::debug!(version = m.version, "migration already applied");
            continue;
        }
        apply_one(conn, m)?;
    }
    Ok(())
}

fn read_applied_versions(conn: &Connection) -> Result<Vec<u32>> {
    // The first migration creates schema_migrations; tolerate its absence.
    let table_exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
            [],
            |row| row.get(0),
        )
        .ok();
    if table_exists.is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .map(|r| r.map(|v| v as u32));
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn apply_one(conn: &mut Connection, m: &Migration) -> Result<()> {
    tracing::info!(version = m.version, name = m.name, "applying migration");
    // The migration file wraps its own BEGIN/COMMIT; execute_batch tolerates
    // multiple statements including the explicit transaction wrapper.
    conn.execute_batch(m.sql)
        .map_err(|source| StorageError::Migration {
            version: m.version,
            source,
        })?;
    // Defence in depth: some migrations may forget to insert into
    // schema_migrations. Upsert to guarantee idempotency.
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_at) \
         VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params![m.version as i64, m.name],
    )?;
    Ok(())
}

/// Count of applied migrations (for tests + install).
pub fn applied_count(conn: &Connection) -> Result<u32> {
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row.get(0))
        .unwrap_or(0);
    Ok(n as u32)
}
```

- [ ] **Step 5: Wire the modules into `lib.rs`**

Replace `/home/john/clarion/crates/clarion-storage/src/lib.rs` with:

```rust
//! clarion-storage — SQLite layer, writer-actor, reader pool.
//!
//! All mutations route through [`writer::Writer`] (a single `tokio::task`
//! owning the sole write `rusqlite::Connection`). Readers come from a
//! `deadpool-sqlite` pool. See ADR-011.

pub mod error;
pub mod pragma;
pub mod schema;

pub use error::{Result, StorageError};
```

- [ ] **Step 6: Write the integration test**

Write `/home/john/clarion/crates/clarion-storage/tests/schema_apply.rs`:

```rust
//! Schema-apply integration tests.
//!
//! Verifies that migration 0001 produces every table, index, trigger,
//! generated column, and view from detailed-design.md §3, and that
//! applying migrations a second time is a no-op.

use rusqlite::{params, Connection};

use clarion_storage::{pragma, schema};

fn open_fresh(tempdir: &tempfile::TempDir) -> Connection {
    let path = tempdir.path().join("clarion.db");
    let mut conn = Connection::open(&path).expect("open");
    pragma::apply_write_pragmas(&conn).expect("pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    conn
}

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn trigger_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn view_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='view' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn index_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type='index' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn migration_0001_creates_every_expected_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let tables = table_names(&conn);
    for expected in &[
        "edges",
        "entities",
        "entity_tags",
        "findings",
        "runs",
        "schema_migrations",
        "summary_cache",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table {expected} in {tables:?}"
        );
    }
}

#[test]
fn migration_0001_creates_entity_fts_virtual_table() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    // Virtual tables appear in sqlite_master as type='table' with sql starting "CREATE VIRTUAL".
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='entity_fts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(sql.contains("CREATE VIRTUAL TABLE"), "sql was: {sql}");
    // Queryable (shape check only — empty result is fine).
    conn.execute_batch("SELECT entity_id, name FROM entity_fts LIMIT 0")
        .expect("entity_fts queryable");
}

#[test]
fn migration_0001_creates_all_three_fts_triggers() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let triggers = trigger_names(&conn);
    for expected in &["entities_ad", "entities_ai", "entities_au"] {
        assert!(
            triggers.iter().any(|t| t == expected),
            "missing trigger {expected} in {triggers:?}"
        );
    }
}

#[test]
fn migration_0001_creates_guidance_sheets_view() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let views = view_names(&conn);
    assert!(views.iter().any(|v| v == "guidance_sheets"), "views: {views:?}");
    conn.execute_batch("SELECT id, name, priority FROM guidance_sheets LIMIT 0")
        .expect("guidance_sheets queryable");
}

#[test]
fn migration_0001_creates_partial_indexes() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let indexes = index_names(&conn);
    for expected in &["ix_entities_churn", "ix_entities_priority"] {
        assert!(
            indexes.iter().any(|i| i == expected),
            "missing index {expected} in {indexes:?}"
        );
    }
}

#[test]
fn entity_generated_columns_extract_from_properties_json() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let props = r#"{"priority": "P1", "git_churn_count": 42}"#;
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        params!["python:function:demo.f", "python", "function", "demo.f", "f", props],
    )
    .unwrap();
    let (priority, churn): (Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT priority, git_churn_count FROM entities WHERE id = ?1",
            params!["python:function:demo.f"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(priority.as_deref(), Some("P1"));
    assert_eq!(churn, Some(42));
}

#[test]
fn migrations_are_idempotent() {
    let tempdir = tempfile::tempdir().unwrap();
    let mut conn = open_fresh(&tempdir);
    // Second apply on the same connection.
    schema::apply_migrations(&mut conn).expect("second apply should be a no-op");
    assert_eq!(schema::applied_count(&conn).unwrap(), 1);
    let tables_after = table_names(&conn);
    assert!(tables_after.contains(&"entities".to_owned()));
}

#[test]
fn schema_migrations_records_one_row() {
    let tempdir = tempfile::tempdir().unwrap();
    let conn = open_fresh(&tempdir);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let name: String = conn
        .query_row(
            "SELECT name FROM schema_migrations WHERE version = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(name, "0001_initial_schema");
}
```

- [ ] **Step 7: Run the tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-storage --test schema_apply
```

Expected: 7 tests pass. If any fail, the migration SQL has a bug — fix `0001_initial_schema.sql`, not the tests.

**Checkpoint** (per the plan review): if `cargo nextest run -p clarion-storage --test schema_apply` isn't green by end of day 2 of WP1 execution, pause and reassess before proceeding to Task 4. A half-locked schema is worse than a one-day slip.

- [ ] **Step 8: Clippy clean**

```bash
cd /home/john/clarion && cargo clippy -p clarion-storage --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-storage/ && git commit -m "$(cat <<'EOF'
feat(wp1): L1 SQLite schema migration framework

Migration 0001 transcribes the full detailed-design.md §3 schema: tables
(entities, entity_tags, edges, findings, summary_cache, runs,
schema_migrations), entity_fts FTS5 virtual table, three FTS triggers
(_ai/_au/_ad), priority + git_churn_count generated columns with partial
indexes, and the guidance_sheets view (aggregating tags via correlated
subquery against entity_tags — reconciles §3 shape with the normalised
tag storage).

schema::apply_migrations() reads embedded SQL via include_str!, tolerates
re-runs (UQ idempotency), and records each apply in schema_migrations.
pragma::apply_write_pragmas() + apply_read_pragmas() centralise ADR-011
connection-open invariants. StorageError wraps rusqlite::Error via
thiserror (UQ-WP1-06 resolution).

7 integration tests in schema_apply.rs cover table/trigger/view presence,
FTS queryability, generated-column round-trip, and idempotency.
EOF
)"
```

---

## Task 4: Reader pool

**Files:**
- Create: `/home/john/clarion/crates/clarion-storage/src/reader.rs`
- Modify: `/home/john/clarion/crates/clarion-storage/src/lib.rs`
- Create: `/home/john/clarion/crates/clarion-storage/tests/reader_pool.rs`

- [ ] **Step 1: Write the reader pool wrapper**

Write `/home/john/clarion/crates/clarion-storage/src/reader.rs`:

```rust
//! Read-only connection pool wrapping `deadpool-sqlite` per ADR-011.
//!
//! Readers take a connection from the pool, run a query, and drop it. The
//! pool caps concurrent connections (default 16). WAL mode lets readers
//! see the committed snapshot at the moment they open; writes become
//! visible only after the next checkpoint or a fresh connection.

use std::path::Path;

use deadpool_sqlite::{Config, Pool, Runtime};

use crate::error::{Result, StorageError};
use crate::pragma;

pub struct ReaderPool {
    pool: Pool,
}

impl ReaderPool {
    /// Open a pool against an existing SQLite file.
    ///
    /// The database file must already exist and already have migrations
    /// applied — callers should run `schema::apply_migrations` on a write
    /// connection first.
    pub fn open(db_path: impl AsRef<Path>, max_size: usize) -> Result<Self> {
        let mut cfg = Config::new(db_path.as_ref());
        cfg.pool = Some(deadpool_sqlite::PoolConfig::new(max_size));
        let pool = cfg
            .create_pool(Runtime::Tokio1)
            .map_err(|e| StorageError::Pool(format!("create_pool: {e}")))?;
        Ok(Self { pool })
    }

    /// Acquire a reader and run a blocking closure on it. PRAGMAs are
    /// applied on every acquisition — it's cheap (a few PRAGMA statements)
    /// and guarantees busy_timeout + foreign_keys are always on.
    pub async fn with_reader<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let obj = self
            .pool
            .get()
            .await
            .map_err(|e| StorageError::Pool(format!("acquire: {e}")))?;
        obj.interact(move |conn| -> Result<T> {
            pragma::apply_read_pragmas(conn)?;
            f(conn)
        })
        .await
        .map_err(|e| StorageError::Pool(format!("interact: {e}")))?
    }
}
```

- [ ] **Step 2: Export from `lib.rs`**

Modify `/home/john/clarion/crates/clarion-storage/src/lib.rs` to add the new module:

```rust
//! clarion-storage — SQLite layer, writer-actor, reader pool.

pub mod error;
pub mod pragma;
pub mod reader;
pub mod schema;

pub use error::{Result, StorageError};
pub use reader::ReaderPool;
```

- [ ] **Step 3: Write the failing integration test**

Write `/home/john/clarion/crates/clarion-storage/tests/reader_pool.rs`:

```rust
//! Reader-pool concurrency tests.

use std::sync::Arc;

use rusqlite::Connection;

use clarion_storage::{pragma, schema, ReaderPool};

fn prepared_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("clarion.db");
    let mut conn = Connection::open(&path).expect("open");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("migrate");
    path
}

#[tokio::test]
async fn two_readers_run_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let pool = Arc::new(ReaderPool::open(&path, 2).expect("pool"));

    let p1 = pool.clone();
    let p2 = pool.clone();
    let (a, b) = tokio::join!(
        p1.with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
            Ok(n)
        }),
        p2.with_reader(|conn| {
            let n: i64 = conn.query_row("SELECT 2", [], |row| row.get(0))?;
            Ok(n)
        })
    );
    assert_eq!(a.unwrap(), 1);
    assert_eq!(b.unwrap(), 2);
}

#[tokio::test]
async fn reader_sees_committed_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);

    // Pre-seed an entity via a one-shot blocking connection.
    {
        let conn = Connection::open(&path).unwrap();
        pragma::apply_write_pragmas(&conn).unwrap();
        conn.execute(
            "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
             VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), NULL, '{}', '{}', 'running')",
            rusqlite::params!["run-1"],
        )
        .unwrap();
    }

    let pool = ReaderPool::open(&path, 2).expect("pool");
    let status: String = pool
        .with_reader(|conn| {
            let status: String = conn.query_row(
                "SELECT status FROM runs WHERE id = 'run-1'",
                [],
                |row| row.get(0),
            )?;
            Ok(status)
        })
        .await
        .unwrap();
    assert_eq!(status, "running");
}
```

- [ ] **Step 4: Run the tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-storage --test reader_pool
```

Expected: 2 tests pass.

- [ ] **Step 5: Clippy clean**

```bash
cd /home/john/clarion && cargo clippy -p clarion-storage --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-storage/ && git commit -m "$(cat <<'EOF'
feat(wp1): reader pool for concurrent read connections

ReaderPool wraps deadpool-sqlite (ADR-011 default max 16, configurable
per call site). with_reader() acquires a pooled connection, applies the
read-side PRAGMAs, and runs the caller's closure inside deadpool's
interact() block so the runtime yields during SQLite I/O.

Integration tests cover concurrent-read throughput (two readers via
tokio::join!) and committed-snapshot visibility (reader sees a row
committed on an out-of-band write connection).
EOF
)"
```

---

## Task 5: `clarion install` subcommand

**Files:**
- Create: `/home/john/clarion/crates/clarion-cli/src/install.rs`
- Create: `/home/john/clarion/crates/clarion-cli/src/cli.rs`
- Modify: `/home/john/clarion/crates/clarion-cli/src/main.rs`
- Create: `/home/john/clarion/crates/clarion-cli/tests/install.rs`
- Create: `/home/john/clarion/docs/clarion/adr/ADR-005-clarion-dir-tracking.md`
- Modify: `/home/john/clarion/docs/clarion/adr/README.md`

- [ ] **Step 1: Write the CLI command enum**

Write `/home/john/clarion/crates/clarion-cli/src/cli.rs`:

```rust
use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clarion", version, about = "Clarion code-archaeology tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialise .clarion/ in the current directory.
    Install {
        /// Overwrite an existing .clarion/ (not implemented in Sprint 1).
        #[arg(long)]
        force: bool,

        /// Directory to install into (default: current directory).
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// Run an analysis pass. Sprint 1: no plugins are loaded; run status is
    /// `skipped_no_plugins`. WP2 wires plugin spawning.
    Analyze {
        /// Path to analyse (default: current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}
```

- [ ] **Step 2: Write the install command implementation**

Write `/home/john/clarion/crates/clarion-cli/src/install.rs`:

```rust
//! `clarion install` — initialise .clarion/ in the target directory.
//!
//! Creates:
//! - `.clarion/clarion.db`        (migrated)
//! - `.clarion/config.json`       (internal state stub)
//! - `.clarion/.gitignore`        (UQ-WP1-04 rules; ADR-005)
//! - `<path>/clarion.yaml`        (user-edited config stub at project root
//!                                  per detailed-design.md §File layout)
//!
//! Refuses if `.clarion/` already exists (UQ-WP1-08). `--force` is accepted
//! by the CLI but currently returns an error — Sprint 1 does not implement
//! overwrite.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use clarion_storage::{pragma, schema};

const CONFIG_JSON_STUB: &str = r#"{
    "schema_version": 1,
    "last_run_id": null
}
"#;

const CLARION_YAML_STUB: &str = "# clarion.yaml — user-edited config.\n\
# Full schema TBD; see docs/clarion/v0.1 design. Sprint 1 walking skeleton\n\
# ignores most fields. Do not delete this file: later versions will require\n\
# it for model-tier mappings and analysis knobs.\n\
version: 1\n";

const GITIGNORE_CONTENTS: &str = "\
# Clarion .gitignore — ADR-005 tracked-vs-excluded list.
# Tracked (committed): clarion.db, config.json, .gitignore itself.
# Excluded (ignored): WAL sidecars, shadow DB, per-run logs, tmp scratch.

# SQLite write-ahead files never belong in the repo.
*-wal
*-shm
*.db-wal
*.db-shm

# Shadow DB intermediate (ADR-011 --shadow-db).
*.shadow.db
*.db.new

# Scratch / temp space.
tmp/

# Per-run log directories (see detailed-design §File layout). The run dir
# metadata (config.yaml, stats.json, partial.json) is tracked; only the
# raw LLM request/response log is excluded.
logs/
runs/*/log.jsonl
";

pub fn run(path: PathBuf, force: bool) -> Result<()> {
    if force {
        bail!(
            "--force is not implemented in Sprint 1. Remove .clarion/ manually \
             if you need a clean reinit."
        );
    }

    let project_root = path.canonicalize().with_context(|| {
        format!("cannot canonicalise --path {}", path.display())
    })?;
    let clarion_dir = project_root.join(".clarion");
    if clarion_dir.exists() {
        bail!(
            ".clarion/ already exists at {}. Delete it (or pass --force when \
             Sprint 2+ implements overwrite) and try again.",
            clarion_dir.display()
        );
    }

    fs::create_dir_all(&clarion_dir)
        .with_context(|| format!("mkdir {}", clarion_dir.display()))?;

    let db_path = clarion_dir.join("clarion.db");
    initialise_db(&db_path).context("initialise clarion.db")?;

    let config_path = clarion_dir.join("config.json");
    fs::write(&config_path, CONFIG_JSON_STUB)
        .with_context(|| format!("write {}", config_path.display()))?;

    let gitignore_path = clarion_dir.join(".gitignore");
    fs::write(&gitignore_path, GITIGNORE_CONTENTS)
        .with_context(|| format!("write {}", gitignore_path.display()))?;

    let yaml_path = project_root.join("clarion.yaml");
    if !yaml_path.exists() {
        fs::write(&yaml_path, CLARION_YAML_STUB)
            .with_context(|| format!("write {}", yaml_path.display()))?;
    }

    tracing::info!(
        clarion_dir = %clarion_dir.display(),
        "clarion install complete"
    );
    println!("Initialised {}", clarion_dir.display());
    Ok(())
}

fn initialise_db(path: &Path) -> Result<()> {
    let mut conn = Connection::open(path)?;
    pragma::apply_write_pragmas(&conn)?;
    schema::apply_migrations(&mut conn)?;
    Ok(())
}
```

- [ ] **Step 3: Wire the CLI into `main.rs`**

Replace `/home/john/clarion/crates/clarion-cli/src/main.rs` with:

```rust
mod cli;
mod install;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    init_tracing();
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Install { force, path } => install::run(path, force),
        cli::Command::Analyze { path: _ } => {
            // Task 7 implements this. Stubbed so `clarion analyze` is reachable.
            anyhow::bail!("clarion analyze — unimplemented (landing in Task 7)");
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}
```

- [ ] **Step 4: Write the integration tests**

Write `/home/john/clarion/crates/clarion-cli/tests/install.rs`:

```rust
//! `clarion install` integration tests.

use std::fs;

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn install_creates_clarion_dir_with_expected_contents() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let clarion = dir.path().join(".clarion");
    assert!(clarion.join("clarion.db").exists(), "clarion.db missing");
    assert!(clarion.join("config.json").exists(), "config.json missing");
    assert!(clarion.join(".gitignore").exists(), ".gitignore missing");
    assert!(
        dir.path().join("clarion.yaml").exists(),
        "clarion.yaml not at project root"
    );

    let config = fs::read_to_string(clarion.join("config.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(parsed["schema_version"], 1);
    assert!(parsed["last_run_id"].is_null());

    let gitignore = fs::read_to_string(clarion.join(".gitignore")).unwrap();
    for rule in &["*.shadow.db", "tmp/", "logs/", "runs/*/log.jsonl", "*-wal", "*-shm"] {
        assert!(
            gitignore.contains(rule),
            ".gitignore missing rule {rule}: {gitignore}"
        );
    }
}

#[test]
fn install_applies_migration_0001_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let conn = Connection::open(dir.path().join(".clarion/clarion.db")).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let version: i64 = conn
        .query_row("SELECT version FROM schema_migrations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

#[test]
fn install_refuses_to_overwrite_existing_clarion_dir() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    // Second install must fail with a clear message.
    let out = clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("already exists"),
        "error did not mention existing dir: {stderr}"
    );
    assert!(
        stderr.contains("--force"),
        "error did not mention --force escape hatch: {stderr}"
    );
}

#[test]
fn install_force_returns_unimplemented_in_sprint_one() {
    let dir = tempfile::tempdir().unwrap();
    let out = clarion_bin()
        .args(["install", "--force", "--path"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("not implemented in Sprint 1"),
        "expected Sprint 1 --force stub message: {stderr}"
    );
}
```

- [ ] **Step 5: Write ADR-005**

Write `/home/john/clarion/docs/clarion/adr/ADR-005-clarion-dir-tracking.md`:

```markdown
# ADR-005: `.clarion/` Directory Git-Tracking Policy

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: `clarion install` must write a `.gitignore` inside `.clarion/` that
separates committed analysis state from volatile per-run artefacts. Sprint 1 WP1
Task 5 is the authoring trigger; before this ADR, the rules were only proposed
in `docs/implementation/sprint-1/wp1-scaffold.md §UQ-WP1-04`.

## Summary

`.clarion/clarion.db` and `.clarion/config.json` are committed. WAL sidecars,
the shadow-DB intermediate, `tmp/`, `logs/`, and per-run raw LLM request/response
logs (`runs/*/log.jsonl`) are `.gitignore`d. `clarion.yaml` lives at the project
root and is tracked under the user's existing repo-root `.gitignore`, not under
`.clarion/.gitignore` (it's a user-edited config, not analysis state).

## Context

`.clarion/` mixes artefact kinds that want different tracking posture:

- **Shared analysis state** (entities, edges, briefings, guidance) — diff-friendly
  via `clarion db export --textual`; solo-developer and small-team cases benefit
  from having briefings versioned alongside the code they describe
  (`detailed-design.md §3 File layout`).
- **Runtime write-ahead files** (`*-wal`, `*-shm`) — SQLite bookkeeping that is
  process-local and meaningless on a different machine.
- **Shadow DB** (`clarion.db.new`, `*.shadow.db`) — ADR-011's `--shadow-db`
  intermediate; deleted on successful atomic rename, would leak as junk
  otherwise.
- **Per-run LLM bodies** (`runs/<run_id>/log.jsonl`) — raw request/response
  bodies for audit. May contain source excerpts fine to ship to Anthropic
  but not appropriate to commit to a public repo.
- **Scratch** (`tmp/`, `logs/`) — volatile by definition.

Without this ADR, `clarion install` has no normative place to look up the rules,
and every developer's install produces their own variant `.gitignore` by accident.

## Decision

`clarion install` writes `.clarion/.gitignore` with the following contents
(verbatim — the literal file lives at
`crates/clarion-cli/src/install.rs` and ships as the v0.1 baseline):

```
*-wal
*-shm
*.db-wal
*.db-shm
*.shadow.db
*.db.new
tmp/
logs/
runs/*/log.jsonl
```

### Tracked

- `.clarion/clarion.db` — the main analysis store. SQLite diffs poorly; the
  `clarion db export --textual` + `clarion db merge-helper` pattern (detailed
  design §3 File layout) handles the team case.
- `.clarion/config.json` — small, human-readable internal state (schema
  version, last run IDs).
- `.clarion/.gitignore` itself — this file.
- `.clarion/runs/<run_id>/config.yaml` — the snapshot of `clarion.yaml` at run
  time. Material for provenance replay.
- `.clarion/runs/<run_id>/stats.json` — run statistics.
- `.clarion/runs/<run_id>/partial.json` — present only for partial runs;
  material for `--resume`.

### Excluded

- All SQLite WAL + SHM sidecars.
- All shadow-DB intermediates.
- `tmp/` and `logs/` (volatile scratch).
- `runs/*/log.jsonl` (raw LLM bodies — audit-local, not commit-appropriate).

### Out of scope for `.clarion/.gitignore`

- `clarion.yaml` (the user-edited config) lives at the *project root*, not
  inside `.clarion/`. Its tracking is governed by the project's own repo-root
  `.gitignore`, which is the user's concern. Default posture: tracked.

### Opt-out for users who don't want the DB committed

`clarion.yaml:storage.commit_db: false` (post-Sprint-1 knob; WP6 authors the
full `clarion.yaml` schema). When false, Clarion writes an additional
`.clarion/.gitignore` line excluding `clarion.db`, and emits
`clarion db sync push/pull` commands. Not implemented in Sprint 1; the knob
is documented here so the future change has a home.

## Alternatives Considered

### Alternative 1: commit everything

**Pros**: no ignore list to maintain.

**Cons**: WAL sidecars break repos (they're process-local binary files); raw
LLM bodies may contain material the user does not want public.

**Why rejected**: blast radius of a single `git push` with `runs/*/log.jsonl`
committed is unbounded.

### Alternative 2: commit nothing

**Pros**: simplest — `.clarion/` becomes entirely machine-local.

**Cons**: loses the "shared analysis state" benefit — briefings and guidance
are derived outputs that are expensive to rebuild. Small teams especially
benefit from having them versioned alongside the code.

**Why rejected**: the "enterprise rigor at lack of scale" posture favours
committing analytic state for small-team workflows. Users who want machine-local
analysis only opt out via `storage.commit_db: false`.

### Alternative 3: commit the DB but use git-lfs by default

**Pros**: keeps small-git-diff UX (LFS handles the binary file).

**Cons**: requires git-lfs installed on every developer machine; makes `clarion
install` a multi-tool setup; adds failure modes (lfs server availability, large
file policy). v0.1 target workflows are solo/small-team where the straight-commit
path works; LFS is a v0.2+ knob.

**Why rejected**: premature infrastructure for the v0.1 audience.

## Consequences

### Positive

- Every `clarion install` produces the same `.gitignore`. Ends per-developer
  drift on "what should be committed."
- WAL sidecars cannot accidentally land in a commit.
- Raw LLM bodies stay local to the developer that ran the analysis.
- `--shadow-db` intermediates (ADR-011) are excluded by the same list, so
  users adopting that mode don't discover an ignore gap post-hoc.

### Negative

- Committed SQLite DBs diff poorly by default. Mitigation: the
  `clarion db export --textual` / merge-helper path (detailed-design §3) is
  the documented escape hatch.
- Adding a new excluded pattern requires either a Clarion release or a
  user-side `.clarion/.gitignore` edit. The post-v0.1 plan is to keep this
  file tool-owned; users adding their own ignores put them in the repo-root
  `.gitignore`, not here.

### Neutral

- `storage.commit_db: false` is a defined but unimplemented opt-out. Sprint 1
  ships with the commit-the-DB default only.

## Related Decisions

- [ADR-011](../../clarion/adr/ADR-011-writer-actor-concurrency.md) — names the shadow-DB
  intermediate; this ADR excludes it from git.
- [ADR-014](../../clarion/adr/ADR-014-filigree-registry-backend.md) — cross-tool references
  rely on `clarion.db` being available to readers (Filigree, Wardline); the
  commit-by-default posture keeps those references resolvable across machines.

## References

- [detailed-design.md §3 File layout](../v0.1/detailed-design.md#file-layout) —
  the prose version of this decision, now superseded by this ADR as the
  normative source.
- [wp1-scaffold.md UQ-WP1-04](../../implementation/sprint-1/wp1-scaffold.md) —
  the sprint-local resolution this ADR formalises.
```

- [ ] **Step 6: Update the ADR index**

Edit `/home/john/clarion/docs/clarion/adr/README.md` at line 32 (`| ADR-005 | ... | Backlog |`). Change to:

```
| ADR-005 | `.clarion/` git-committable by default; DB included, run logs excluded | Accepted |
```

If the ADR index has a separate "Accepted ADRs" list table elsewhere, also add a row for ADR-005 there (run `grep -n '^| ADR-' docs/clarion/adr/README.md` to locate). The important change is the status moving from `Backlog` to `Accepted`.

- [ ] **Step 7: Run the tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-cli --test install
```

Expected: 4 tests pass.

- [ ] **Step 8: Run clippy**

```bash
cd /home/john/clarion && cargo clippy -p clarion-cli --all-targets -- -D warnings
```

- [ ] **Step 9: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-cli/ docs/clarion/adr/ && git commit -m "$(cat <<'EOF'
feat(wp1): clarion install subcommand; author ADR-005

clarion install creates .clarion/{clarion.db,config.json,.gitignore} and
a clarion.yaml stub at the project root per detailed-design.md §File
layout. Refuses on existing .clarion/ (UQ-WP1-08). --force is recognised
by clap but errors out — Sprint 1 does not implement overwrite.

ADR-005 moved from Backlog to Accepted: .clarion/ git-tracking policy
(committed: clarion.db, config.json, .gitignore, runs/*/config.yaml,
stats.json, partial.json; excluded: WAL/SHM sidecars, *.shadow.db,
runs/*/log.jsonl, tmp/, logs/). UQ-WP1-04 resolved.

4 integration tests cover happy-path creation, migration-count
verification, overwrite refusal, and --force stub message.
EOF
)"
```

---

## Task 6: Writer-actor (L3)

**Files:**
- Create: `/home/john/clarion/crates/clarion-storage/src/commands.rs`
- Create: `/home/john/clarion/crates/clarion-storage/src/writer.rs`
- Modify: `/home/john/clarion/crates/clarion-storage/src/lib.rs`
- Create: `/home/john/clarion/crates/clarion-storage/tests/writer_actor.rs`

- [ ] **Step 1: Write the command enum and entity record**

Write `/home/john/clarion/crates/clarion-storage/src/commands.rs`:

```rust
//! Writer-actor command protocol (L3 lock-in).
//!
//! Per ADR-011, every persistent mutation is a `WriterCmd` variant. The
//! writer task owns the sole `rusqlite::Connection`; callers enqueue
//! commands via a bounded `mpsc::Sender<WriterCmd>`. Each variant carries
//! a `oneshot::Sender` for the per-command ack (UQ-WP1-03 resolution).
//!
//! Sprint 1 ships four variants: BeginRun, InsertEntity, CommitRun,
//! FailRun. Later WPs add InsertEdge, InsertFinding, etc. by appending
//! variants — the pattern is frozen here.

use tokio::sync::oneshot;

use crate::error::StorageError;

pub type Ack<T> = oneshot::Sender<Result<T, StorageError>>;

/// Run status values. Extended in later WPs; Sprint 1 uses only
/// `SkippedNoPlugins` (from `clarion analyze` without plugins wired) and
/// `Failed` (explicit FailRun).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Sprint 1 stub: analyze invoked with no plugins registered.
    SkippedNoPlugins,
    /// Normal successful completion.
    Completed,
    /// Explicit failure via FailRun.
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::SkippedNoPlugins => "skipped_no_plugins",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
        }
    }
}

/// Plain-old-data entity record as seen by the writer. Content-hash and
/// timestamps are supplied by callers; the writer does not compute them.
#[derive(Debug, Clone)]
pub struct EntityRecord {
    pub id: String,
    pub plugin_id: String,
    pub kind: String,
    pub name: String,
    pub short_name: String,
    pub parent_id: Option<String>,
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    pub source_line_start: Option<i64>,
    pub source_line_end: Option<i64>,
    /// JSON string; writer inserts verbatim.
    pub properties_json: String,
    pub content_hash: Option<String>,
    pub summary_json: Option<String>,
    pub wardline_json: Option<String>,
    pub first_seen_commit: Option<String>,
    pub last_seen_commit: Option<String>,
    /// ISO-8601 UTC; writer inserts verbatim.
    pub created_at: String,
    pub updated_at: String,
}

/// All writer operations as a single enum so the actor loop exhausts
/// everything via one match.
#[derive(Debug)]
pub enum WriterCmd {
    /// Open a new run. The writer inserts a row into `runs` with status
    /// `running`, begins an implicit transaction on the entities write
    /// path, and binds `run_id` into its state.
    BeginRun {
        run_id: String,
        config_json: String,
        started_at: String,
        ack: Ack<()>,
    },
    /// Insert an entity; also advances the per-batch insert counter and
    /// commits the in-flight transaction if the batch boundary is crossed.
    InsertEntity {
        entity: EntityRecord,
        ack: Ack<()>,
    },
    /// Commit the in-flight transaction, update the run row to the given
    /// terminal status + completed_at + stats_json, and clear per-run
    /// state.
    CommitRun {
        run_id: String,
        status: RunStatus,
        completed_at: String,
        stats_json: String,
        ack: Ack<()>,
    },
    /// Roll back the in-flight transaction, update the run row to
    /// `failed`, and clear per-run state.
    FailRun {
        run_id: String,
        reason: String,
        completed_at: String,
        ack: Ack<()>,
    },
}
```

- [ ] **Step 2: Write the writer-actor itself**

Write `/home/john/clarion/crates/clarion-storage/src/writer.rs`:

```rust
//! Writer-actor implementation (L3 lock-in) per ADR-011.
//!
//! The actor owns the sole write `rusqlite::Connection`. Callers submit
//! commands via `Writer::sender()`. The actor loop pulls one command at a
//! time, applies the mutation inside an implicit transaction bound to the
//! current run, and commits every `batch_size` entity inserts (the
//! "per-N-files" transaction pattern, default N=50 per ADR-011).
//!
//! UQ-WP1-03 resolution: the `commits_observed` `Arc<AtomicUsize>` is
//! incremented on every COMMIT issued by the actor. Tests read it to
//! verify batch-boundary commits fire at the expected cadence. It is
//! present in release builds as a no-op counter; no `#[cfg(test)]` gating
//! is used.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::commands::{Ack, EntityRecord, RunStatus, WriterCmd};
use crate::error::{Result, StorageError};
use crate::pragma;

/// Default transaction batch size per ADR-011.
pub const DEFAULT_BATCH_SIZE: usize = 50;

/// Default mpsc channel capacity per ADR-011.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

pub struct Writer {
    tx: mpsc::Sender<WriterCmd>,
    pub commits_observed: Arc<AtomicUsize>,
}

impl Writer {
    /// Spawn the writer-actor on the current tokio runtime.
    ///
    /// Returns the `Writer` handle and the `JoinHandle` of the actor task.
    /// Callers await the `JoinHandle` at shutdown to ensure the actor has
    /// flushed any pending commit.
    pub fn spawn(
        db_path: std::path::PathBuf,
        batch_size: usize,
        channel_capacity: usize,
    ) -> Result<(Self, JoinHandle<Result<()>>)> {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let commits_observed = Arc::new(AtomicUsize::new(0));
        let commits_for_actor = commits_observed.clone();
        let handle = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut conn = Connection::open(&db_path)?;
            pragma::apply_write_pragmas(&conn)?;
            run_actor(rx, &mut conn, batch_size, commits_for_actor)
        });
        Ok((
            Writer { tx, commits_observed },
            // spawn_blocking's JoinHandle has the same shape as spawn's for
            // `.await.map_err(...)?` purposes.
            handle,
        ))
    }

    pub fn sender(&self) -> mpsc::Sender<WriterCmd> {
        self.tx.clone()
    }

    /// Convenience: send a command and await its ack.
    pub async fn send_wait<T, F>(&self, build: F) -> Result<T>
    where
        F: FnOnce(oneshot::Sender<Result<T>>) -> WriterCmd,
        T: 'static,
    {
        let (tx, rx) = oneshot::channel();
        let cmd = build(tx);
        self.tx
            .send(cmd)
            .await
            .map_err(|_| StorageError::WriterGone)?;
        rx.await.map_err(|_| StorageError::WriterNoResponse)?
    }
}

fn run_actor(
    mut rx: mpsc::Receiver<WriterCmd>,
    conn: &mut Connection,
    batch_size: usize,
    commits_observed: Arc<AtomicUsize>,
) -> Result<()> {
    let mut state = ActorState::new(batch_size);

    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            WriterCmd::BeginRun {
                run_id,
                config_json,
                started_at,
                ack,
            } => {
                reply(ack, begin_run(conn, &mut state, &run_id, &config_json, &started_at));
            }
            WriterCmd::InsertEntity { entity, ack } => {
                let res = insert_entity(conn, &mut state, &entity, &commits_observed);
                reply(ack, res);
            }
            WriterCmd::CommitRun {
                run_id,
                status,
                completed_at,
                stats_json,
                ack,
            } => {
                let res = commit_run(
                    conn,
                    &mut state,
                    &run_id,
                    status,
                    &completed_at,
                    &stats_json,
                    &commits_observed,
                );
                reply(ack, res);
            }
            WriterCmd::FailRun {
                run_id,
                reason,
                completed_at,
                ack,
            } => {
                let res = fail_run(conn, &mut state, &run_id, &reason, &completed_at);
                reply(ack, res);
            }
        }
    }
    // Channel closed. Best-effort flush.
    if state.in_tx {
        let _ = conn.execute_batch("ROLLBACK");
    }
    Ok(())
}

fn reply<T>(ack: Ack<T>, result: Result<T>) {
    // If the caller dropped the receiver, we discard the result. This is
    // correct behaviour — the writer is still responsible for its own
    // durability, and the caller chose to stop caring.
    let _ = ack.send(result);
}

struct ActorState {
    batch_size: usize,
    /// Inserts accumulated in the current transaction.
    inserts_in_batch: usize,
    /// True if BEGIN has been issued and no COMMIT/ROLLBACK has fired.
    in_tx: bool,
    /// The run currently in progress, if any.
    current_run: Option<String>,
}

impl ActorState {
    fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            inserts_in_batch: 0,
            in_tx: false,
            current_run: None,
        }
    }
}

fn begin_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    config_json: &str,
    started_at: &str,
) -> Result<()> {
    if state.current_run.is_some() {
        return Err(StorageError::Sqlite(rusqlite::Error::InvalidQuery));
    }
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, ?2, NULL, ?3, '{}', 'running')",
        params![run_id, started_at, config_json],
    )?;
    conn.execute_batch("BEGIN")?;
    state.in_tx = true;
    state.inserts_in_batch = 0;
    state.current_run = Some(run_id.to_owned());
    Ok(())
}

fn insert_entity(
    conn: &mut Connection,
    state: &mut ActorState,
    entity: &EntityRecord,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if !state.in_tx {
        conn.execute_batch("BEGIN")?;
        state.in_tx = true;
    }
    conn.execute(
        "INSERT INTO entities ( \
            id, plugin_id, kind, name, short_name, \
            parent_id, source_file_id, \
            source_byte_start, source_byte_end, \
            source_line_start, source_line_end, \
            properties, content_hash, summary, wardline, \
            first_seen_commit, last_seen_commit, \
            created_at, updated_at \
         ) VALUES ( \
            ?1, ?2, ?3, ?4, ?5, \
            ?6, ?7, \
            ?8, ?9, \
            ?10, ?11, \
            ?12, ?13, ?14, ?15, \
            ?16, ?17, \
            ?18, ?19 \
         )",
        params![
            entity.id,
            entity.plugin_id,
            entity.kind,
            entity.name,
            entity.short_name,
            entity.parent_id,
            entity.source_file_id,
            entity.source_byte_start,
            entity.source_byte_end,
            entity.source_line_start,
            entity.source_line_end,
            entity.properties_json,
            entity.content_hash,
            entity.summary_json,
            entity.wardline_json,
            entity.first_seen_commit,
            entity.last_seen_commit,
            entity.created_at,
            entity.updated_at,
        ],
    )?;
    state.inserts_in_batch += 1;
    if state.inserts_in_batch >= state.batch_size {
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
        state.in_tx = false;
        state.inserts_in_batch = 0;
        // Open the next batch eagerly so the next insert doesn't pay
        // another BEGIN round-trip.
        conn.execute_batch("BEGIN")?;
        state.in_tx = true;
    }
    Ok(())
}

fn commit_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    status: RunStatus,
    completed_at: &str,
    stats_json: &str,
    commits_observed: &AtomicUsize,
) -> Result<()> {
    if state.in_tx {
        conn.execute_batch("COMMIT")?;
        commits_observed.fetch_add(1, Ordering::Relaxed);
        state.in_tx = false;
    }
    conn.execute(
        "UPDATE runs SET status = ?1, completed_at = ?2, stats = ?3 WHERE id = ?4",
        params![status.as_str(), completed_at, stats_json, run_id],
    )?;
    state.current_run = None;
    state.inserts_in_batch = 0;
    Ok(())
}

fn fail_run(
    conn: &mut Connection,
    state: &mut ActorState,
    run_id: &str,
    reason: &str,
    completed_at: &str,
) -> Result<()> {
    if state.in_tx {
        let _ = conn.execute_batch("ROLLBACK");
        state.in_tx = false;
    }
    let stats_json = serde_json::json!({ "failure_reason": reason }).to_string();
    conn.execute(
        "UPDATE runs SET status = 'failed', completed_at = ?1, stats = ?2 WHERE id = ?3",
        params![completed_at, stats_json, run_id],
    )?;
    state.current_run = None;
    state.inserts_in_batch = 0;
    Ok(())
}
```

- [ ] **Step 3: Export the new modules**

Modify `/home/john/clarion/crates/clarion-storage/src/lib.rs` to:

```rust
//! clarion-storage — SQLite layer, writer-actor, reader pool.

pub mod commands;
pub mod error;
pub mod pragma;
pub mod reader;
pub mod schema;
pub mod writer;

pub use commands::{EntityRecord, RunStatus, WriterCmd};
pub use error::{Result, StorageError};
pub use reader::ReaderPool;
pub use writer::{Writer, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY};
```

- [ ] **Step 4: Write the integration tests**

Write `/home/john/clarion/crates/clarion-storage/tests/writer_actor.rs`:

```rust
//! Writer-actor integration tests.
//!
//! Covers: round-trip insert, per-N-batch commit cadence, FailRun rollback.

use std::sync::atomic::Ordering;

use rusqlite::Connection;
use tokio::sync::oneshot;

use clarion_storage::{
    commands::{EntityRecord, RunStatus, WriterCmd},
    pragma, schema, ReaderPool, Writer,
};

fn prepared_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let path = dir.path().join("clarion.db");
    let mut conn = Connection::open(&path).unwrap();
    pragma::apply_write_pragmas(&conn).unwrap();
    schema::apply_migrations(&mut conn).unwrap();
    path
}

fn now_iso() -> String {
    // Fixed per-test timestamp keeps deterministic assertions trivial.
    "2026-04-18T00:00:00.000Z".to_owned()
}

fn make_entity(id: &str) -> EntityRecord {
    EntityRecord {
        id: id.to_owned(),
        plugin_id: "python".to_owned(),
        kind: "function".to_owned(),
        name: "demo.hello".to_owned(),
        short_name: "hello".to_owned(),
        parent_id: None,
        source_file_id: None,
        source_byte_start: None,
        source_byte_end: None,
        source_line_start: None,
        source_line_end: None,
        properties_json: "{}".to_owned(),
        content_hash: None,
        summary_json: None,
        wardline_json: None,
        first_seen_commit: None,
        last_seen_commit: None,
        created_at: now_iso(),
        updated_at: now_iso(),
    }
}

async fn send<T>(
    tx: &tokio::sync::mpsc::Sender<WriterCmd>,
    build: impl FnOnce(oneshot::Sender<Result<T, clarion_storage::StorageError>>) -> WriterCmd,
) -> Result<T, clarion_storage::StorageError> {
    let (ack_tx, ack_rx) = oneshot::channel();
    tx.send(build(ack_tx)).await.unwrap();
    ack_rx.await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_insert_persists_entity() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::InsertEntity {
        entity: make_entity("python:function:demo.hello"),
        ack,
    })
    .await
    .unwrap();

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);

    let kind: String = pool
        .with_reader(|conn| {
            let k: String = conn.query_row(
                "SELECT kind FROM entities WHERE id = ?1",
                rusqlite::params!["python:function:demo.hello"],
                |row| row.get(0),
            )?;
            Ok(k)
        })
        .await
        .unwrap();
    assert_eq!(kind, "function");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_size_fifty_commits_every_fifty_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-1".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    for i in 0..150 {
        let id = format!("python:function:demo.f{i:03}");
        send::<()>(&tx, |ack| WriterCmd::InsertEntity {
            entity: make_entity(&id),
            ack,
        })
        .await
        .unwrap();
    }

    // At 150 inserts with batch_size=50, three batch-boundary commits have
    // fired. CommitRun will fire a fourth on the trailing (empty) batch.
    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 3);

    send::<()>(&tx, |ack| WriterCmd::CommitRun {
        run_id: "run-1".into(),
        status: RunStatus::Completed,
        completed_at: now_iso(),
        stats_json: "{}".into(),
        ack,
    })
    .await
    .unwrap();

    // CommitRun opens no new tx; the batch is empty, so COMMIT is a no-op
    // from SQLite's view but our actor still issues one to close the tx
    // state. Contract: commits_observed now equals 4 (3 batch boundaries
    // + 1 CommitRun).
    assert_eq!(writer.commits_observed.load(Ordering::Relaxed), 4);

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(count, 150);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_run_rolls_back_pending_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let path = prepared_db(&dir);
    let (writer, handle) = Writer::spawn(path.clone(), 50, 256).unwrap();
    let tx = writer.sender();

    send::<()>(&tx, |ack| WriterCmd::BeginRun {
        run_id: "run-fail".into(),
        config_json: "{}".into(),
        started_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    for i in 0..10 {
        let id = format!("python:function:demo.g{i:03}");
        send::<()>(&tx, |ack| WriterCmd::InsertEntity {
            entity: make_entity(&id),
            ack,
        })
        .await
        .unwrap();
    }

    send::<()>(&tx, |ack| WriterCmd::FailRun {
        run_id: "run-fail".into(),
        reason: "deliberate test failure".into(),
        completed_at: now_iso(),
        ack,
    })
    .await
    .unwrap();

    drop(tx);
    drop(writer);
    handle.await.unwrap().unwrap();

    let pool = ReaderPool::open(&path, 2).unwrap();
    let entity_count: i64 = pool
        .with_reader(|conn| {
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();
    assert_eq!(entity_count, 0, "FailRun did not roll back inserts");

    let status: String = pool
        .with_reader(|conn| {
            let s: String = conn.query_row(
                "SELECT status FROM runs WHERE id = 'run-fail'",
                [],
                |row| row.get(0),
            )?;
            Ok(s)
        })
        .await
        .unwrap();
    assert_eq!(status, "failed");
}
```

- [ ] **Step 5: Run the tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-storage --test writer_actor
```

Expected: 3 tests pass. The `batch_size_fifty_commits_every_fifty_inserts` test is the L3 lock-in proof.

- [ ] **Step 6: Run every storage-crate test**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-storage
```

Expected: 12 tests pass in total (7 schema + 2 reader + 3 writer).

- [ ] **Step 7: Clippy clean**

```bash
cd /home/john/clarion && cargo clippy -p clarion-storage --all-targets -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-storage/ && git commit -m "$(cat <<'EOF'
feat(wp1): L3 writer-actor (tokio::task) with per-N transaction batch

Writer::spawn() starts a tokio::spawn_blocking task owning the sole write
rusqlite::Connection. Callers enqueue WriterCmd variants via a bounded
mpsc channel (ADR-011 defaults: capacity 256, batch size 50). Each command
carries a oneshot::Sender ack for per-command replies (UQ-WP1-03).

WriterCmd variants: BeginRun, InsertEntity, CommitRun, FailRun. InsertEntity
commits at batch boundaries; CommitRun/FailRun finalise run state. FailRun
rolls back the pending transaction before updating the runs row.

commits_observed (Arc<AtomicUsize>) counts COMMIT statements fired by the
actor — exposed as a normal field on Writer so tests can assert the per-N
cadence without #[cfg(test)] gating on the hot loop.

3 integration tests cover round-trip insert, batch-boundary commit cadence
(150 inserts → 3 in-flight commits + 1 CommitRun = 4), and FailRun rollback.
EOF
)"
```

---

## Task 7: `clarion analyze` skeleton (no plugin)

**Files:**
- Create: `/home/john/clarion/crates/clarion-cli/src/analyze.rs`
- Modify: `/home/john/clarion/crates/clarion-cli/src/main.rs`
- Create: `/home/john/clarion/crates/clarion-cli/tests/analyze.rs`

- [ ] **Step 1: Implement the analyze subcommand**

Write `/home/john/clarion/crates/clarion-cli/src/analyze.rs`:

```rust
//! `clarion analyze` — Sprint 1 skeleton.
//!
//! Opens .clarion/clarion.db, begins a run, logs a warning that no plugins
//! are wired, and commits the run with status `skipped_no_plugins`. WP2
//! replaces this body with real plugin spawning.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use tokio::sync::oneshot;
use uuid::Uuid;

use clarion_storage::{
    commands::{RunStatus, WriterCmd},
    Writer, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY,
};

pub async fn run(project_path: PathBuf) -> Result<()> {
    let project_root = project_path
        .canonicalize()
        .with_context(|| format!("cannot canonicalise path {}", project_path.display()))?;
    let clarion_dir = project_root.join(".clarion");
    if !clarion_dir.exists() {
        bail!(
            "{} has no .clarion/ directory. Run `clarion install` first.",
            project_root.display()
        );
    }
    let db_path = clarion_dir.join("clarion.db");

    let (writer, handle) =
        Writer::spawn(db_path, DEFAULT_BATCH_SIZE, DEFAULT_CHANNEL_CAPACITY)
            .context("spawn writer actor")?;
    let tx = writer.sender();
    let run_id = Uuid::new_v4().to_string();
    let now = chrono_like_now();

    let (ack_tx, ack_rx) = oneshot::channel();
    tx.send(WriterCmd::BeginRun {
        run_id: run_id.clone(),
        config_json: "{}".into(),
        started_at: now.clone(),
        ack: ack_tx,
    })
    .await
    .map_err(|_| anyhow::anyhow!("writer actor closed before BeginRun"))?;
    ack_rx
        .await
        .map_err(|_| anyhow::anyhow!("writer actor dropped ack"))?
        .context("BeginRun")?;

    tracing::info!(
        run_id = %run_id,
        "no plugins registered (WP2 will wire this)"
    );

    let (ack_tx, ack_rx) = oneshot::channel();
    tx.send(WriterCmd::CommitRun {
        run_id: run_id.clone(),
        status: RunStatus::SkippedNoPlugins,
        completed_at: now,
        stats_json: r#"{"entities_inserted":0}"#.into(),
        ack: ack_tx,
    })
    .await
    .map_err(|_| anyhow::anyhow!("writer actor closed before CommitRun"))?;
    ack_rx
        .await
        .map_err(|_| anyhow::anyhow!("writer actor dropped ack"))?
        .context("CommitRun")?;

    drop(tx);
    drop(writer);
    handle.await.map_err(|e| anyhow::anyhow!("writer actor panic: {e}"))??;

    println!("analyze complete: run {run_id} skipped_no_plugins");
    Ok(())
}

fn chrono_like_now() -> String {
    // We avoid adding a chrono dependency just to format a timestamp in
    // Sprint 1. SystemTime + a small formatter lands an ISO-8601 UTC
    // string adequate for the `runs.started_at` text column.
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970");
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    let (y, mo, da, h, mi, se) = gmtime(secs);
    format!("{y:04}-{mo:02}-{da:02}T{h:02}:{mi:02}:{se:02}.{millis:03}Z")
}

fn gmtime(mut secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let se = (secs % 60) as u32;
    secs /= 60;
    let mi = (secs % 60) as u32;
    secs /= 60;
    let h = (secs % 24) as u32;
    secs /= 24;
    // Days since epoch → date. Algorithm: Howard Hinnant's date. Adapted.
    let mut z = secs as i64;
    z += 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let da = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    (y as u32, mo, da, h, mi, se)
}
```

Add the `uuid` dependency to `/home/john/clarion/crates/clarion-cli/Cargo.toml`:

```toml
[dependencies]
# ... existing entries above ...
uuid = { version = "1", features = ["v4"] }
```

Also add `uuid` to the root `Cargo.toml` `[workspace.dependencies]`:

```toml
uuid = { version = "1", features = ["v4"] }
```

Then reference it from `clarion-cli` with `uuid.workspace = true`. (Prefer the workspace form — keeps version bumps centralised.)

- [ ] **Step 2: Wire analyze into `main.rs`**

Replace `/home/john/clarion/crates/clarion-cli/src/main.rs` with:

```rust
mod analyze;
mod cli;
mod install;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    init_tracing();
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Install { force, path } => install::run(path, force),
        cli::Command::Analyze { path } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(analyze::run(path))
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}
```

- [ ] **Step 3: Write the integration tests**

Write `/home/john/clarion/crates/clarion-cli/tests/analyze.rs`:

```rust
//! `clarion analyze` Sprint-1 integration test.

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn analyze_without_plugins_writes_skipped_run_row() {
    let dir = tempfile::tempdir().unwrap();
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    clarion_bin()
        .args(["analyze"])
        .arg(dir.path())
        .assert()
        .success();

    let conn = Connection::open(dir.path().join(".clarion/clarion.db")).unwrap();
    let (count, status): (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(status), '') FROM runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(status, "skipped_no_plugins");

    let entity_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entity_count, 0);
}

#[test]
fn analyze_fails_cleanly_if_clarion_dir_missing() {
    let dir = tempfile::tempdir().unwrap();
    let out = clarion_bin()
        .args(["analyze"])
        .arg(dir.path())
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("clarion install"),
        "error did not point operator at install: {stderr}"
    );
}
```

- [ ] **Step 4: Run the tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-cli --test analyze
```

Expected: 2 tests pass.

- [ ] **Step 5: Clippy clean**

```bash
cd /home/john/clarion && cargo clippy -p clarion-cli --all-targets -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
cd /home/john/clarion && git add Cargo.toml crates/clarion-cli/ && git commit -m "$(cat <<'EOF'
feat(wp1): clarion analyze skeleton (plugin wiring deferred to WP2)

clarion analyze opens .clarion/clarion.db, BeginRun → CommitRun with
status 'skipped_no_plugins'. Warns via tracing::info! that no plugins
are wired. Fails cleanly with a `clarion install`-pointing message if
.clarion/ is missing.

uuid v4 added as a workspace dependency for run-id generation. Minimal
inline gmtime() avoids pulling chrono solely for ISO-8601 formatting;
later WPs that need richer time handling can promote chrono to a
workspace dependency then.

2 integration tests cover the happy path (one runs row, zero entities)
and the missing-.clarion/ error path.
EOF
)"
```

---

## Task 8: LlmProvider trait stub

**Files:**
- Create: `/home/john/clarion/crates/clarion-core/src/llm_provider.rs`
- Modify: `/home/john/clarion/crates/clarion-core/src/lib.rs`

- [ ] **Step 1: Write the trait and test**

Write `/home/john/clarion/crates/clarion-core/src/llm_provider.rs`:

```rust
//! LlmProvider trait stub.
//!
//! WP6 (summary-cache + prompt dispatch) fills this out. Sprint 1 defines
//! the hook point so the trait has a stable import path from day one.
//! `NoopProvider` panics if its `name()` is called — Sprint 1 has no
//! code path that legitimately calls it, so panic is a louder bug signal
//! than a silent default.

pub trait LlmProvider: Send + Sync {
    /// Human-readable provider identifier.
    fn name(&self) -> &str;
}

/// Stub provider used in Sprint 1 code paths that take a provider
/// argument. Calling `name()` panics — if you see this panic, something
/// in the WP1 code is reaching for a real provider before WP6 lands.
pub struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn name(&self) -> &str {
        panic!("NoopProvider::name called — WP6 should have replaced this by now")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_implements_trait() {
        fn assert_trait<T: LlmProvider>(_: &T) {}
        let p = NoopProvider;
        assert_trait(&p);
    }

    #[test]
    #[should_panic(expected = "NoopProvider::name called")]
    fn noop_provider_panics_on_name() {
        let p = NoopProvider;
        let _ = p.name();
    }
}
```

- [ ] **Step 2: Export from `lib.rs`**

Modify `/home/john/clarion/crates/clarion-core/src/lib.rs` to:

```rust
//! clarion-core — domain types, identifiers, and provider traits.

pub mod entity_id;
pub mod llm_provider;

pub use entity_id::{entity_id, EntityId, EntityIdError};
pub use llm_provider::{LlmProvider, NoopProvider};
```

- [ ] **Step 3: Run the tests**

```bash
cd /home/john/clarion && cargo nextest run -p clarion-core
```

Expected: all clarion-core tests pass (13 entity_id + 2 llm_provider = 15).

- [ ] **Step 4: Clippy clean**

```bash
cd /home/john/clarion && cargo clippy -p clarion-core --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-core/ && git commit -m "$(cat <<'EOF'
feat(wp1): LlmProvider trait stub for WP6

LlmProvider + NoopProvider in clarion-core. NoopProvider::name() panics
loudly — if it ever fires, some WP1 code is reaching for a real provider
before WP6 is wired. 2 tests: trait implementation and panic contract.
EOF
)"
```

---

## Task 9: End-to-end WP1 smoke test

**Files:**
- Create: `/home/john/clarion/crates/clarion-cli/tests/wp1_e2e.rs`

- [ ] **Step 1: Write the end-to-end smoke test**

Write `/home/john/clarion/crates/clarion-cli/tests/wp1_e2e.rs`:

```rust
//! End-to-end WP1 smoke test — the minimum that must work at WP1 close.
//!
//! Mirrors docs/implementation/sprint-1/README.md §3 demo script for
//! Sprint 1 WP1 scope (no plugin, no entities — those land in WP2 + WP3).

use assert_cmd::Command;
use rusqlite::Connection;

fn clarion_bin() -> Command {
    Command::cargo_bin("clarion").expect("clarion binary")
}

#[test]
fn wp1_walking_skeleton_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    // Step 1: clarion install
    clarion_bin()
        .args(["install", "--path"])
        .arg(dir.path())
        .assert()
        .success();

    let clarion_dir = dir.path().join(".clarion");
    assert!(clarion_dir.join("clarion.db").exists());
    assert!(clarion_dir.join("config.json").exists());
    assert!(clarion_dir.join(".gitignore").exists());
    assert!(dir.path().join("clarion.yaml").exists());

    // Step 2: clarion analyze (no plugins yet — WP2 wires them)
    clarion_bin()
        .args(["analyze"])
        .arg(dir.path())
        .assert()
        .success();

    // Step 3: verify expected shape in the DB.
    let conn = Connection::open(clarion_dir.join("clarion.db")).unwrap();

    let migration_version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(migration_version, 1, "schema not on migration 1");

    let runs_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs_count, 1, "expected exactly one run row");

    let run_status: String = conn
        .query_row("SELECT status FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(run_status, "skipped_no_plugins");

    let entity_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entity_count, 0);

    // WP2+WP3 extend this test to assert a non-zero entity count with the
    // expected 3-segment ID (L2 format `python:function:demo.hello`).
}
```

- [ ] **Step 2: Run the full workspace test suite one last time**

```bash
cd /home/john/clarion && cargo nextest run --workspace --all-features
```

Expected: all tests green. Count should be ~20 (15 core + 2 reader + 7 schema + 3 writer + 4 install + 2 analyze + 1 e2e = in that neighbourhood; exact count depends on which negative cases expanded).

- [ ] **Step 3: Release-profile build**

```bash
cd /home/john/clarion && cargo build --workspace --release
```

Expected: clean compile, `target/release/clarion` exists. If any warning-as-error surfaces, fix it in the relevant crate's code path, not by downgrading the lint.

- [ ] **Step 4: Full ADR-023 gate sweep before commit**

Every gate below must exit 0 before Task 9 commits. CI runs the same set against the PR; running them locally first avoids PR-cycle churn.

```bash
cd /home/john/clarion && cargo fmt --all -- --check
cd /home/john/clarion && cargo clippy --workspace --all-targets --all-features -- -D warnings
cd /home/john/clarion && cargo doc --workspace --no-deps --all-features
cd /home/john/clarion && cargo deny check
```

If any gate fails, fix in-place (don't loosen the lint, don't add allowlist entries without a commit-message justification, don't skip `--all-features`).

- [ ] **Step 5: Commit**

```bash
cd /home/john/clarion && git add crates/clarion-cli/tests/wp1_e2e.rs && git commit -m "$(cat <<'EOF'
test(wp1): end-to-end smoke test

wp1_e2e.rs runs the README §3 demo script at WP1 scope: clarion install
creates .clarion/; clarion analyze commits a run with status
skipped_no_plugins and zero entities; migration 0001 recorded as
schema_version 1. WP2+WP3 extend this test with non-zero entity asserts.
EOF
)"
```

---

## Task 10: Sign-off ladder updates

**Files:**
- Modify: `/home/john/clarion/docs/implementation/sprint-1/signoffs.md`
- Modify: `/home/john/clarion/docs/implementation/sprint-1/README.md`
- Modify: `/home/john/clarion/docs/implementation/sprint-1/wp1-scaffold.md`

- [ ] **Step 1: Tick Tier A.1.* boxes in `signoffs.md`**

In `/home/john/clarion/docs/implementation/sprint-1/signoffs.md`, change each Tier A.1 checkbox from `- [ ]` to `- [x]` after verifying the cited proof (commit hash, test-run log, or doc commit). For lock-in rows A.1.3, A.1.4, A.1.5, fill in the `locked on ______` date — use the date the final WP1 commit lands (execute `git log -1 --format=%as HEAD` to get it). Also tick A.1.6, A.1.7, A.1.8, A.1.9, A.1.10.

Do **not** tick Tier A.2, A.3, A.4, A.5, A.6 — those belong to WP2/WP3/sprint-close.

- [ ] **Step 2: Stamp lock-in dates in README.md §4**

In `/home/john/clarion/docs/implementation/sprint-1/README.md` §4 Lock-in summary, annotate L1, L2, L3 rows with the same `locked on <date>` stamp added to signoffs.md.

- [ ] **Step 3: Mark UQ-WP1-* resolved in `wp1-scaffold.md §5`**

Each UQ-WP1-01 through UQ-WP1-09 entry gets a `**Resolved**: <outcome>` line that matches the UQ's current state:

- UQ-WP1-01 — resolved by ADR-011 (rusqlite).
- UQ-WP1-02 — resolved by ADR-011 (tokio from day one).
- UQ-WP1-03 — resolved in Task 6: per-command oneshot ack; `Arc<AtomicUsize>` commit counter.
- UQ-WP1-04 — resolved in Task 5 (ADR-005).
- UQ-WP1-05 — resolved in Task 3: full `runs` shape; plugin-invocation columns carried as JSON in the `config` column, populated by WP2.
- UQ-WP1-06 — resolved in Task 3: `StorageError` wraps `rusqlite::Error` via `thiserror`.
- UQ-WP1-07 — resolved in Task 2: `EntityIdError::SegmentContainsColon`.
- UQ-WP1-08 — resolved in Task 5: refuses if `.clarion/` exists; `--force` stub.
- UQ-WP1-09 — resolved in Task 1: Rust 2021 + stable via `rust-toolchain.toml`.

- [ ] **Step 4: Commit**

```bash
cd /home/john/clarion && git add docs/implementation/sprint-1/ && git commit -m "$(cat <<'EOF'
docs(sprint-1): tick WP1 sign-off and stamp L1/L2/L3 lock-ins

Tier A.1 boxes ticked in signoffs.md with the WP1-close commit date.
README.md §4 lock-in table stamped for L1/L2/L3. wp1-scaffold.md §5
UQ resolutions recorded inline with outcome + resolving task.

WP1 complete; ready for WP2 kickoff.
EOF
)"
```

---

## Self-review summary

**Spec coverage vs `wp1-scaffold.md`:**

| Spec task | Plan task | Status |
|---|---|---|
| §6.Task 1 Workspace skeleton | Task 1 | ✓ |
| §6.Task 2 Entity-ID assembler | Task 2 | ✓ |
| §6.Task 3 Schema migration | Task 3 | ✓ |
| §6.Task 4 Reader pool | Task 4 | ✓ |
| §6.Task 5 `clarion install` + ADR-005 | Task 5 | ✓ |
| §6.Task 6 Writer-actor | Task 6 | ✓ |
| §6.Task 7 `clarion analyze` | Task 7 | ✓ |
| §6.Task 8 LlmProvider stub | Task 8 | ✓ |
| §6.Task 9 E2E smoke | Task 9 | ✓ |
| §8 Exit criteria sign-off | Task 10 | ✓ |

Every lock-in (L1/L2/L3) has a Task that lands it. Every UQ-WP1-* has a designated resolution Task. Every exit-criteria bullet has a verification step.

**Type consistency spot-check:**

- `EntityId` / `entity_id()` signature identical between Task 2 definition and Task 8's stub (both re-exported from `clarion-core::lib`).
- `WriterCmd::{BeginRun,InsertEntity,CommitRun,FailRun}` — same four variants in `commands.rs` (Task 6 Step 1), `writer.rs` dispatch (Task 6 Step 2), and the test harness (Task 6 Step 4) and `analyze.rs` (Task 7 Step 1).
- `RunStatus::{SkippedNoPlugins,Completed,Failed}` — used in `writer.rs`, `commands.rs`, `analyze.rs`, and the integration tests. Strings match migration `0001` implicit values (`skipped_no_plugins`, `completed`, `failed`, plus `running` from `BeginRun`).
- `Writer::commits_observed` is a public `Arc<AtomicUsize>` — accessed by name in Task 6 tests.
- `DEFAULT_BATCH_SIZE = 50` and `DEFAULT_CHANNEL_CAPACITY = 256` are consistent with ADR-011.

**Placeholder scan:** no `TODO`, `TBD`, `implement later`, or "Similar to Task N" in the plan body. Every code step has a complete code block; every command step has a literal command + expected output. ADR-005 body is fully written (no "flesh out later"). Task 10's sign-off bullets name each UQ's resolution verbatim.

**Divergence from the spec that the executor should know about:**

1. `Writer::spawn` uses `tokio::task::spawn_blocking` rather than plain `tokio::spawn`, because `rusqlite::Connection` is `!Send` across await points. Equivalent to ADR-011's "or `deadpool-sqlite`'s `Object::interact` pattern" phrasing. Documented at Task 6 Step 2.
2. Task 7 adds a small inline `gmtime()` to avoid adding `chrono` to the workspace solely for timestamp formatting. If later WPs need richer time handling, promote `chrono` to `[workspace.dependencies]` at that time.
3. The `guidance_sheets` view aggregates `entity_tags.tag` into a JSON array via a correlated subquery, because the detailed-design §3 SQL references a `tags` column on `entities` that doesn't exist under the normalised tag schema. This is a faithful interpretation that preserves the view's row shape; flag it to the design-doc author post-Sprint-1.
4. `uuid` crate added as a workspace dependency in Task 7 for run-ID generation. Worth noting because the WP1 doc's §4 external-dependencies table doesn't list it — consider adding a row when updating the spec.
5. **ADR-023 tooling baseline** adopted before any code lands. Original UQ-WP1-09's "edition 2021, fine to document and move on" framing was reopened on 2026-04-18 and re-resolved: Task 1 now ships edition 2024, workspace `[lints]` pedantic, rustfmt + clippy + deny configs, cargo-nextest, GitHub Actions CI. The justification is the retrofit cost asymmetry (cheap at zero code, expensive later). See ADR-023 and the revised UQ-WP1-09 in `wp1-scaffold.md §5`. Every subsequent task's gate is pedantic-clean + fmt-clean + nextest-green; the CI workflow proves these invariants on every PR.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-18-wp1-scaffold-storage.md`.**

Execution approach chosen: **Subagent-Driven** (fresh subagent per task + two-stage review). Next step: dispatch the Task 1 implementer with the post-ADR-023 scope.
