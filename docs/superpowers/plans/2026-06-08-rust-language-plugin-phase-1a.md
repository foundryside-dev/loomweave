# Rust Language Plugin — Phase 1a (Identity Foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the identity foundation of the Rust language plugin — crate-root discovery, module-path resolution, the ADR-049 qualname canonicalization, an init-time project symbol table, syn-based extraction of `module`/`struct`/`function` entities with SEI signatures — and **prove it emits zero colliding locators over Loomweave's own 8-crate workspace.** That zero-collision proof is the Phase 1a exit gate; nothing in Phase 1b keys an edge on an unproven locator.

**Architecture:** A new in-workspace crate `crates/loomweave-plugin-rust` produces a subprocess binary speaking the existing Content-Length-framed JSON-RPC 2.0 protocol (same host, same wire structs as `loomweave-plugin-fixture` and the Python plugin). It parses each `.rs` file with `syn` (syntactic, build-free — no `cargo`, no network). At `initialize` it walks `project_root` once to build a path→entity-id symbol table; each `analyze_file` emits entities whose ids are assembled via `loomweave_core::entity_id()` from ADR-049 qualnames. The binary is named **off** the `loomweave-plugin-*` discovery glob (like the fixture) so a workspace dev build does not trip `current_exe()`-relative discovery; installation/staging maps it to the discovery name `loomweave-plugin-rust`.

**Tech Stack:** Rust (edition 2024, `rust-version = 1.88`), `syn` 2.x (full AST), `proc-macro2` (spans/byte ranges), `toml` (read `[package].name` as text — never `cargo metadata`), `serde_json`, `loomweave-core` (entity-id assembler + protocol types). Tests via `cargo nextest`. CI floor per CLAUDE.md (`fmt`, `clippy -D warnings`, `build`, `nextest`, `cargo doc -D warnings`, `cargo deny`).

---

## Reference: the wire contract (verified against source 2026-06-08)

The plugin emits JSON. The host deserialises each entity into `loomweave_core::plugin::host::RawEntity` (`crates/loomweave-core/src/plugin/host.rs:140`) and each edge into `RawEdge` (`:180`). **Phase 1a emits entities only** (the core auto-emits the file→module `contains` edge for `file_scope` entities; plugin-authored edges are Phase 1b).

**Entity JSON shape** (required keys unless noted):
```json
{
  "id": "rust:function:loomweave_core.config.helper",
  "kind": "function",
  "qualified_name": "loomweave_core.config.helper",
  "source": { "file_path": "<path sent in analyze_file>", "source_byte_start": 120, "source_byte_end": 240, "source_range": { "start_line": 8, "end_line": 12 } },
  "parent_id": "rust:module:loomweave_core.config",
  "signature": { "v": 1, "params": ["x: i32"], "return_ann": "bool", "generics": [] }
}
```
- `id`/`kind`/`qualified_name` are required strings. `source.file_path` is required and is **the exact path string the host sent in `AnalyzeFileParams.file_path`** (it lands in the host path jail — do not rewrite it). `source.source_byte_start`/`end` and `source.source_range` ride in `source` (extra fields, accepted verbatim). `parent_id` is `Some(id)` for nested entities, `None`/omitted for the top-level module. `signature` is an opaque object the core stores verbatim and compares by string equality (ADR-038); omit it for modules.
- `kind` MUST be one of the manifest's declared `entity_kinds`. `id` MUST equal `entity_id(plugin_id, kind, qualified_name)` — assemble it that way, never by hand.

**EdgeConfidence** serialises lowercase: `"resolved"` / `"ambiguous"` / `"inferred"` (`protocol.rs:46`). Not used in 1a.

**`entity_id()`** (`crates/loomweave-core/src/entity_id.rs:91`): `entity_id(plugin_id: &str, kind: &str, canonical_qualified_name: &str) -> Result<EntityId, EntityIdError>`. Validates `plugin_id`/`kind` against `[a-z][a-z0-9_]*`, rejects `:` in the qualname, rejects empty segments. `EntityId` derefs/`.as_str()`/`Display` to the `plugin:kind:qualname` string.

---

## File structure (created in this plan)

```
crates/loomweave-plugin-rust/
  Cargo.toml                 # workspace member; off-glob [[bin]] name = "loomweave-rust-plugin"
  plugin.toml                # manifest: plugin_id=rust, entity_kinds, roles, signature schemas
  src/
    main.rs                  # JSON-RPC loop (initialize/initialized/analyze_file/shutdown/exit)
    lib.rs                   # re-exports the testable modules below
    crate_roots.rs           # project_root -> { dir: crate_name } (reads Cargo.toml [package].name as TEXT)
    module_path.rs           # .rs file path + crate root -> dotted module path (#[path], file modules)
    qualname.rs              # ADR-049 canonicalization: free items, impl/trait discrimination, @cfg twins
    symbol_table.rs          # init-time project walk -> path->entity_id map
    extract.rs               # syn parse of one file -> Vec<entity JSON> (+ degraded-parse fallback)
    signature.rs             # ADR-038 SEI signature objects for function/struct
    spans.rs                 # proc-macro2 Span -> (byte_start, byte_end, start_line, end_line)
  tests/
    identity_stability.rs    # §4.3 stability invariant
    identity_uniqueness.rs   # §4.3 uniqueness invariant (all ADR-049 collision pairs)
    host_integration.rs      # handshake -> analyze_file -> shutdown (models host_subprocess.rs)
    dogfood_uniqueness.rs    # THE GATE: zero duplicate locators over crates/ of this repo
fixtures/entity_id.json      # (modify) add Rust parity rows
Cargo.toml                   # (modify) add the crate to [workspace].members
```

---

## Task 0: Crate skeleton, manifest, workspace wiring, and the byte-range spike

**Files:**
- Create: `crates/loomweave-plugin-rust/Cargo.toml`
- Create: `crates/loomweave-plugin-rust/src/main.rs`
- Create: `crates/loomweave-plugin-rust/src/lib.rs`
- Create: `crates/loomweave-plugin-rust/src/spans.rs`
- Create: `crates/loomweave-plugin-rust/plugin.toml`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Add the crate to the workspace members**

In the root `Cargo.toml`, add the member (keep the list alphabetical-ish, matching existing style):
```toml
members = [
    "crates/loomweave-analysis",
    "crates/loomweave-core",
    "crates/loomweave-federation",
    "crates/loomweave-storage",
    "crates/loomweave-cli",
    "crates/loomweave-mcp",
    "crates/loomweave-plugin-fixture",
    "crates/loomweave-plugin-rust",
    "crates/loomweave-scanner",
]
```

- [ ] **Step 2: Write the crate Cargo.toml with an OFF-GLOB binary name**

```toml
[package]
name = "loomweave-plugin-rust"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[lints]
workspace = true

# The cargo artifact is named OFF the `loomweave-plugin-*` discovery glob
# (see loomweave-core/src/plugin/discovery.rs). A `cargo build --workspace
# --bins` drops this binary into `target/<profile>/` next to `loomweave`; if it
# matched the glob, current_exe()-relative discovery would find a manifest-less
# "plugin" beside the running binary and fail the run. `loomweave install`
# stages it under the discovery name `loomweave-plugin-rust` with a neighbour
# `plugin.toml`; tests stage it the same way (see tests/host_integration.rs).
# This mirrors loomweave-plugin-fixture's deliberate off-glob naming.
[[bin]]
name = "loomweave-rust-plugin"
path = "src/main.rs"

[dependencies]
loomweave-core = { path = "../loomweave-core", version = "1.1.0-rc3" }
serde_json.workspace = true
syn = { version = "2", features = ["full", "extra-traits", "visit"] }
proc-macro2 = { version = "1", features = ["span-locations"] }
toml = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

Note: if `toml`/`tempfile` are not already `workspace.dependencies`, add them there (check `Cargo.toml [workspace.dependencies]` first; the Python-side guards do not cover Rust dep versions, but `cargo deny` will). `proc-macro2`'s `span-locations` feature is what makes byte/line offsets available outside a proc-macro context.

- [ ] **Step 3: Spike — confirm the proc-macro2 span → byte-range API, then write `spans.rs`**

Run a throwaway check to confirm the exact API on the pinned toolchain:
```bash
cargo add --dry-run --package loomweave-plugin-rust proc-macro2 --features span-locations
```
Then implement `src/spans.rs`. `proc_macro2::Span` exposes `.byte_range() -> std::ops::Range<usize>` and `.start()/.end() -> proc_macro2::LineColumn` when `span-locations` is on and the tokens came from a `proc_macro2`-parsed string (which `syn::parse_file` uses). Write:

```rust
//! proc-macro2 span → byte/line offsets for entity source ranges.
//!
//! `span-locations` must be enabled (see Cargo.toml). Offsets are relative to
//! the parsed source string, matching what the Python extractor emits as
//! `source.source_byte_start/end` and `source.source_range`.
use proc_macro2::Span;
use syn::spanned::Spanned;

/// Byte and 1-based line range for any spanned syn node.
#[derive(Debug, Clone, Copy)]
pub struct SourceRange {
    pub byte_start: i64,
    pub byte_end: i64,
    pub start_line: i64,
    pub end_line: i64,
}

pub fn source_range_of(node: &impl Spanned) -> SourceRange {
    range_of_span(node.span())
}

pub fn range_of_span(span: Span) -> SourceRange {
    let bytes = span.byte_range();
    let start = span.start();
    let end = span.end();
    SourceRange {
        byte_start: i64::try_from(bytes.start).unwrap_or(0),
        byte_end: i64::try_from(bytes.end).unwrap_or(0),
        start_line: i64::try_from(start.line).unwrap_or(0),
        end_line: i64::try_from(end.line).unwrap_or(0),
    }
}
```

If `byte_range()` is unavailable on the pinned toolchain, fall back to computing byte offsets from `(line, column)` against a precomputed line-start index of the source string, and record that decision in this file's doc comment. Do not leave the choice unverified — the gate test depends on these offsets being correct.

- [ ] **Step 4: Write a minimal `main.rs` JSON-RPC loop (skeleton, modelled on the fixture)**

Copy the protocol-loop scaffolding from `crates/loomweave-plugin-fixture/src/main.rs` (the `read_frame`/`write_frame`/`send_result` shape, `ContentLengthCeiling::DEFAULT`, the `has_id`/notification handling). Wire the four handlers to call into `lib.rs` (filled in later tasks). For now:
- `initialize` → return `InitializeResult { name: "loomweave-plugin-rust", version: env!("CARGO_PKG_VERSION"), ontology_version: "0.1.0", capabilities: json!({}) }`, and **stash `params.project_root`** (deserialise `InitializeParams`) in a local for the symbol table (Task 7 fills the build).
- `analyze_file` → for now return `AnalyzeFileResult { entities: vec![], edges: vec![], stats: AnalyzeFileStats::default(), findings: vec![] }` (Task 6 fills it).
- `shutdown` → `ShutdownResult {}`. `initialized` notification → no-op. `exit` notification → `std::process::exit(0)`.

```rust
use loomweave_core::plugin::{InitializeParams, InitializeResult};
// ...inside the "initialize" arm:
let params: InitializeParams = serde_json::from_value(
    raw.get("params").cloned().unwrap_or(serde_json::json!({})),
).unwrap_or(InitializeParams { project_root: String::new() });
let _project_root = params.project_root; // Task 7 builds the symbol table from this
```

- [ ] **Step 5: Write `src/lib.rs` re-exporting the (initially empty) modules**

```rust
//! Rust language plugin — Phase 1a: identity foundation.
pub mod crate_roots;
pub mod extract;
pub mod module_path;
pub mod qualname;
pub mod signature;
pub mod spans;
pub mod symbol_table;
```
Create each named module file with just a `//!` doc line so the crate compiles; later tasks fill them.

- [ ] **Step 6: Write the `plugin.toml` manifest (Phase 1a kind set)**

```toml
[plugin]
name = "loomweave-plugin-rust"
plugin_id = "rust"
version = "1.1.0rc3"
protocol_version = "1.0"
# Bare basename per ADR-021 — the host refuses any path component. This is the
# DISCOVERY name; the cargo artifact is `loomweave-rust-plugin` (off-glob).
executable = "loomweave-plugin-rust"
language = "rust"
extensions = ["rs"]

[capabilities.runtime]
# Measured basis filled by Task 14 (ADR-035); placeholder envelope until then.
expected_max_rss_mb = 1024
expected_entities_per_file = 5000
wardline_aware = false
reads_outside_project_root = false

[ontology]
# Phase 1a starter kinds. Phase 1b adds the remaining 7 (ADR-027 MINOR bump).
entity_kinds = ["module", "struct", "function"]
# The core auto-emits the file->module `contains` edge for file_scope entities.
edge_kinds = ["contains"]
rule_id_prefix = "LMWV-RUST-"
ontology_version = "0.1.0"

[ontology.roles]
file_scope = ["module"]
callable = ["function"]
syntax_degraded_module = ["module"]

[signature]
schema_version = 1

[signature.schemas.function]
v = 1
fields = ["params", "return_ann", "generics"]

[signature.schemas.struct]
v = 1
fields = ["fields"]
```

- [ ] **Step 7: Build and verify the off-glob name keeps a dev `analyze` healthy**

Run:
```bash
cargo build -p loomweave-plugin-rust
ls target/debug/loomweave-rust-plugin            # artifact exists under the OFF-glob name
ls target/debug/loomweave-plugin-rust 2>/dev/null # MUST NOT exist (would trip discovery)
cargo build --workspace --bins
```
Expected: the binary is `loomweave-rust-plugin`; no `loomweave-plugin-rust` artifact sits in `target/debug/`. Sanity-check discovery is unaffected:
```bash
cargo nextest run -p loomweave-core discovery
```
Expected: PASS (no new manifest-less plugin beside the test binaries).

- [ ] **Step 8: Validate the manifest parses**

Run:
```bash
cargo nextest run -p loomweave-core manifest
```
Then add a quick unit test in `src/lib.rs` (or `tests/manifest.rs`) that parses the shipped manifest:
```rust
#[test]
fn manifest_parses_and_declares_rust_plugin() {
    let bytes = include_bytes!("../plugin.toml");
    let m = loomweave_core::plugin::parse_manifest(bytes).expect("manifest parses");
    assert_eq!(m.plugin.plugin_id, "rust");
    assert_eq!(m.plugin.language, "rust");
    assert!(m.ontology.entity_kinds.contains(&"struct".to_owned()));
}
```
Run: `cargo nextest run -p loomweave-plugin-rust manifest_parses` — Expected: PASS. (Confirm the `Manifest`/`parse_manifest` field names against `crates/loomweave-core/src/plugin/manifest.rs`; adjust accessors if they differ.)

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/loomweave-plugin-rust/
git commit -m "feat(plugin-rust): crate skeleton, manifest, off-glob binary, span helper (Phase 1a Task 0)"
```

---

## Task 1: Crate-root discovery (read `Cargo.toml [package].name` as text)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/crate_roots.rs`
- Test: same file (`#[cfg(test)]`)

ADR-049: the leading qualname segment is the crate name, discovered by reading each `Cargo.toml`'s `[package].name` **as text** (the hard constraint forbids `cargo metadata` registry resolution, not reading a manifest file). Fallback: the directory containing `src/lib.rs`/`src/main.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn maps_each_crate_dir_to_its_package_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // crate A
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::write(root.join("crates/a/Cargo.toml"), "[package]\nname = \"loomweave_core\"\n").unwrap();
        fs::write(root.join("crates/a/src/lib.rs"), "").unwrap();
        // crate B (hyphenated name normalises to underscores)
        fs::create_dir_all(root.join("crates/b/src")).unwrap();
        fs::write(root.join("crates/b/Cargo.toml"), "[package]\nname = \"loomweave-cli\"\n").unwrap();
        fs::write(root.join("crates/b/src/main.rs"), "").unwrap();

        let roots = discover_crate_roots(root);
        assert_eq!(roots.crate_name_for(&root.join("crates/a/src/lib.rs")), Some("loomweave_core".to_owned()));
        assert_eq!(roots.crate_name_for(&root.join("crates/b/src/main.rs")), Some("loomweave_cli".to_owned()));
    }

    #[test]
    fn falls_back_to_dir_holding_lib_or_main_when_no_package_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap(); // no Cargo.toml [package]
        let roots = discover_crate_roots(root);
        // directory name underscored
        let name = roots.crate_name_for(&root.join("src/lib.rs"));
        assert!(name.is_some());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust crate_roots -v`
Expected: FAIL (`discover_crate_roots`/`CrateRoots` not defined).

- [ ] **Step 3: Implement crate-root discovery**

```rust
//! Crate-root discovery: map each `.rs` file to its crate name by reading
//! `Cargo.toml [package].name` as TEXT (never `cargo metadata`). ADR-049 §1.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Crate roots discovered under a project root: a map from each crate's source
/// root directory to its (underscored) crate name, longest-prefix matched.
pub struct CrateRoots {
    /// Sorted by path so longest-prefix lookup is deterministic.
    roots: Vec<(PathBuf, String)>,
}

impl CrateRoots {
    /// The crate name owning `file`, by longest directory-prefix match.
    pub fn crate_name_for(&self, file: &Path) -> Option<String> {
        self.roots
            .iter()
            .filter(|(dir, _)| file.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_os_str().len())
            .map(|(_, name)| name.clone())
    }
}

/// Underscore a crate name the way Rust does (`a-b` → `a_b`).
fn normalise(name: &str) -> String {
    name.replace('-', "_")
}

pub fn discover_crate_roots(project_root: &Path) -> CrateRoots {
    let mut roots: BTreeMap<PathBuf, String> = BTreeMap::new();
    visit(project_root, &mut roots);
    CrateRoots { roots: roots.into_iter().collect() }
}

fn visit(dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let cargo = dir.join("Cargo.toml");
    if cargo.is_file()
        && let Ok(text) = std::fs::read_to_string(&cargo)
        && let Ok(value) = text.parse::<toml::Value>()
        && let Some(name) = value.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str())
    {
        out.insert(dir.to_path_buf(), normalise(name));
    } else if dir.join("src/lib.rs").is_file() || dir.join("src/main.rs").is_file() {
        if let Some(base) = dir.file_name().and_then(|n| n.to_str()) {
            out.entry(dir.to_path_buf()).or_insert_with(|| normalise(base));
        }
    }
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !is_ignored(&path) {
            visit(&path, out);
        }
    }
}

/// Skip vendored / build / store directories the host also skips.
fn is_ignored(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("target" | ".git" | ".weft" | "node_modules")
    )
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust crate_roots -v`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/crate_roots.rs
git commit -m "feat(plugin-rust): crate-root discovery via Cargo.toml [package].name text read (Task 1)"
```

---

## Task 2: Module-path resolution (`mod` tree, `#[path]`, file modules)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/module_path.rs`
- Test: same file

Given a `.rs` file and its crate root, compute the dotted module path from the crate to that file's module. Handles `lib.rs`/`main.rs` → crate root; `foo.rs` and `foo/mod.rs` → `foo`. (Inline `mod {}` nesting is handled in `extract.rs` when walking items; this module resolves the *file*-level module path.)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn crate_root_files_map_to_the_crate_name() {
        assert_eq!(module_path_for("loomweave_core", Path::new("/p/crates/c/src"), Path::new("/p/crates/c/src/lib.rs")), "loomweave_core");
        assert_eq!(module_path_for("loomweave_core", Path::new("/p/crates/c/src"), Path::new("/p/crates/c/src/main.rs")), "loomweave_core");
    }

    #[test]
    fn nested_files_and_mod_rs_dot_join() {
        assert_eq!(module_path_for("k", Path::new("/p/src"), Path::new("/p/src/config.rs")), "k.config");
        assert_eq!(module_path_for("k", Path::new("/p/src"), Path::new("/p/src/plugin/host.rs")), "k.plugin.host");
        assert_eq!(module_path_for("k", Path::new("/p/src"), Path::new("/p/src/plugin/mod.rs")), "k.plugin");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust module_path -v`
Expected: FAIL (`module_path_for` not defined).

- [ ] **Step 3: Implement file-level module-path resolution**

```rust
//! File-level module-path resolution. ADR-049 §1: dotted, crate-rooted.
use std::path::Path;

/// Dotted module path from `crate_name` to the module defined by `file`,
/// where `src_root` is the crate's source root (the dir holding lib.rs/main.rs).
pub fn module_path_for(crate_name: &str, src_root: &Path, file: &Path) -> String {
    let Ok(rel) = file.strip_prefix(src_root) else {
        return crate_name.to_owned();
    };
    let mut segs: Vec<String> = Vec::new();
    let comps: Vec<_> = rel.components().collect();
    for (i, comp) in comps.iter().enumerate() {
        let part = comp.as_os_str().to_string_lossy();
        let is_last = i == comps.len() - 1;
        if is_last {
            let stem = Path::new(part.as_ref()).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            // lib.rs / main.rs / mod.rs contribute no segment
            if stem != "lib" && stem != "main" && stem != "mod" {
                segs.push(stem);
            }
        } else {
            segs.push(part.into_owned());
        }
    }
    std::iter::once(crate_name.to_owned()).chain(segs).collect::<Vec<_>>().join(".")
}
```
(Defer `#[path = "..."]` override handling to Phase 1b unless a dogfood-gate file needs it; add a `// TODO(1b): #[path] override` only if the Task-14 gate over this repo passes without it. If the gate needs it, implement it here before the gate task — do not ship a TODO that the gate trips on.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust module_path -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/module_path.rs
git commit -m "feat(plugin-rust): file-level module-path resolution (Task 2)"
```

---

## Task 3: Qualname canonicalization — free items (ADR-049 §1)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/qualname.rs`
- Test: same file

Assemble `<crate>.<module>.<item>` for free items (function, struct), then build the entity id via `entity_id()`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_function_and_struct_qualnames() {
        assert_eq!(free_item_qualname("loomweave_core.config", "helper"), "loomweave_core.config.helper");
        assert_eq!(free_item_qualname("loomweave_core.config", "Widget"), "loomweave_core.config.Widget");
    }

    #[test]
    fn cross_crate_same_module_item_are_distinct() {
        let a = free_item_qualname("loomweave_core.config", "X");
        let b = free_item_qualname("loomweave_cli.config", "X");
        assert_ne!(a, b);
    }

    #[test]
    fn builds_a_valid_entity_id() {
        let id = build_entity_id("struct", &free_item_qualname("loomweave_core.config", "Widget")).unwrap();
        assert_eq!(id.as_str(), "rust:struct:loomweave_core.config.Widget");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust qualname::tests -v`
Expected: FAIL (`free_item_qualname`/`build_entity_id` not defined).

- [ ] **Step 3: Implement free-item qualnames + the id builder**

```rust
//! ADR-049 qualname canonicalization. The qualname is the SEI *locator*; it
//! must be unique-within-a-run and stable across benign edits.
use loomweave_core::entity_id;
use loomweave_core::entity_id::{EntityId, EntityIdError};

pub const PLUGIN_ID: &str = "rust";

/// `<module-path>.<item-name>` for a free item (function, struct, enum, …).
pub fn free_item_qualname(module_path: &str, item_name: &str) -> String {
    format!("{module_path}.{item_name}")
}

/// Assemble the three-segment entity id. The qualname must already be canonical.
pub fn build_entity_id(kind: &str, qualname: &str) -> Result<EntityId, EntityIdError> {
    entity_id(PLUGIN_ID, kind, qualname)
}
```
(Confirm the `EntityId`/`EntityIdError` import path — `entity_id.rs` defines them in `loomweave_core::entity_id`; the crate re-exports `entity_id()` at `loomweave_core::entity_id`. Adjust `use` to match the actual re-export.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust qualname::tests -v`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/qualname.rs
git commit -m "feat(plugin-rust): free-item qualname canonicalization + entity-id builder (Task 3)"
```

---

## Task 4: Qualname — impl discrimination and member methods (ADR-049 §2)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/qualname.rs`
- Test: same file

This is the load-bearing anti-collision logic. Trait impls key by `impl[<TraitPath-with-concrete-generics>]`; inherent impls by `impl#<positional-De-Bruijn-generic-signature>` plus a stable ordinal; methods carry their impl's discriminator.

- [ ] **Step 1: Write the failing test (every B2 collision pair must be distinct)**

```rust
#[cfg(test)]
mod impl_tests {
    use super::*;

    #[test]
    fn trait_impl_methods_on_same_type_are_distinct() {
        let display_fmt = method_qualname("m.Foo", &ImplDisc::trait_impl("Display", &[]), "fmt");
        let debug_fmt = method_qualname("m.Foo", &ImplDisc::trait_impl("Debug", &[]), "fmt");
        assert_ne!(display_fmt, debug_fmt);
        assert_eq!(display_fmt, "m.Foo.impl[Display].fmt");
    }

    #[test]
    fn trait_generic_args_are_part_of_the_key() {
        let from_i32 = ImplDisc::trait_impl("From", &["i32".to_owned()]).key();
        let from_u32 = ImplDisc::trait_impl("From", &["u32".to_owned()]).key();
        assert_ne!(from_i32, from_u32);
        assert_eq!(from_i32, "impl[From<i32>]");
    }

    #[test]
    fn inherent_generic_param_rename_does_not_churn() {
        // impl<T> Foo<T> and impl<U> Foo<U> render identically (positional).
        let t = ImplDisc::inherent(&["T".to_owned()], /*ordinal*/ 0).key_with_positional();
        let u = ImplDisc::inherent(&["U".to_owned()], 0).key_with_positional();
        assert_eq!(t, u);
    }

    #[test]
    fn multiple_inherent_impls_get_distinct_ordinals() {
        let a = ImplDisc::inherent(&[], 0).key_with_positional();
        let b = ImplDisc::inherent(&[], 1).key_with_positional();
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust impl_tests -v`
Expected: FAIL (`ImplDisc`/`method_qualname` not defined).

- [ ] **Step 3: Implement the impl discriminator**

```rust
/// An impl block's stable discriminator (ADR-049 §2).
pub enum ImplDisc {
    /// `impl[<trait-with-generics>]`
    Trait { rendered: String },
    /// `impl#<positional-generics>` with a stable ordinal for ties.
    Inherent { positional_generics: String, ordinal: usize },
}

impl ImplDisc {
    pub fn trait_impl(trait_name: &str, generic_args: &[String]) -> Self {
        let rendered = if generic_args.is_empty() {
            trait_name.to_owned()
        } else {
            format!("{trait_name}<{}>", generic_args.join(","))
        };
        ImplDisc::Trait { rendered }
    }

    pub fn inherent(generic_param_names: &[String], ordinal: usize) -> Self {
        // De Bruijn: replace each declared generic param NAME by its position,
        // so a rename (<T> -> <U>) does not change the rendering.
        let positional: Vec<String> = (0..generic_param_names.len()).map(|i| format!("${i}")).collect();
        ImplDisc::Inherent { positional_generics: positional.join(","), ordinal }
    }

    /// The `impl[...]` / `impl#...` fragment (no leading type).
    pub fn key(&self) -> String {
        match self {
            ImplDisc::Trait { rendered } => format!("impl[{rendered}]"),
            ImplDisc::Inherent { positional_generics, ordinal } => {
                format!("impl#<{positional_generics}>#{ordinal}")
            }
        }
    }

    /// Test alias making the positional intent explicit.
    pub fn key_with_positional(&self) -> String { self.key() }
}

/// `<type-qualname>.<impl-disc>` — the impl entity's own qualname.
pub fn impl_qualname(type_qualname: &str, disc: &ImplDisc) -> String {
    format!("{type_qualname}.{}", disc.key())
}

/// `<type-qualname>.<impl-disc>.<method>` — a method carries its impl's disc.
pub fn method_qualname(type_qualname: &str, disc: &ImplDisc, method: &str) -> String {
    format!("{type_qualname}.{}.{method}", disc.key())
}
```

Adjust the literal renderings if the test expectations differ (e.g. whether inherent uses `#<...>#ordinal` vs `#ordinal`); the **invariants** that must hold are: (a) trait/Debug-vs-Display distinct, (b) trait generic args in key, (c) generic-param rename stable, (d) inherent ordinals distinct. Keep the literals and the §4.2 spec examples in sync — if you change the rendering, update `docs/superpowers/specs/2026-06-08-rust-language-plugin-design.md` §4.2 and ADR-049 §2 to match.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust impl_tests -v`
Expected: PASS (all four).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/qualname.rs
git commit -m "feat(plugin-rust): impl/trait discrimination + positional generics + method qualnames (Task 4)"
```

---

## Task 5: `@cfg` twin discriminant (closes the guaranteed cfg collision)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/qualname.rs`
- Test: same file

Because all cfg variants are visible (§5), two same-path items on mutually-exclusive cfgs would collide. Append a normalised `@cfg(<predicate>)` discriminant **only when a sibling shares the path**.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod cfg_tests {
    use super::*;

    #[test]
    fn normalises_a_cfg_predicate_deterministically() {
        assert_eq!(cfg_discriminant("unix"), "@cfg(unix)");
        // whitespace-stripped, args sorted
        assert_eq!(cfg_discriminant("any( windows , unix )"), "@cfg(any(unix,windows))");
    }

    #[test]
    fn cfg_twins_get_distinct_qualnames() {
        let unix = format!("{}{}", "m.f", cfg_discriminant("unix"));
        let win = format!("{}{}", "m.f", cfg_discriminant("windows"));
        assert_ne!(unix, win);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust cfg_tests -v`
Expected: FAIL (`cfg_discriminant` not defined).

- [ ] **Step 3: Implement the cfg normaliser**

```rust
/// Normalise a `#[cfg(<predicate>)]` predicate to a stable `@cfg(...)` suffix:
/// whitespace stripped, nested predicate arguments sorted. Applied only to an
/// item that shares a path with a sibling (extract.rs decides applicability).
pub fn cfg_discriminant(predicate: &str) -> String {
    format!("@cfg({})", normalise_pred(predicate))
}

fn normalise_pred(p: &str) -> String {
    let s: String = p.chars().filter(|c| !c.is_whitespace()).collect();
    // sort the args of any single `any(...)`/`all(...)` wrapper (1-level; the
    // common twin case). Deeper nesting falls back to the stripped string,
    // which is still deterministic.
    if let Some(inner) = s.strip_prefix("any(").and_then(|r| r.strip_suffix(')')) {
        let mut parts: Vec<&str> = inner.split(',').collect();
        parts.sort_unstable();
        return format!("any({})", parts.join(","));
    }
    if let Some(inner) = s.strip_prefix("all(").and_then(|r| r.strip_suffix(')')) {
        let mut parts: Vec<&str> = inner.split(',').collect();
        parts.sort_unstable();
        return format!("all({})", parts.join(","));
    }
    s
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust cfg_tests -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/qualname.rs
git commit -m "feat(plugin-rust): @cfg twin discriminant normalisation (Task 5)"
```

---

## Task 6: syn extraction of `module`/`struct`/`function` entities

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/extract.rs`
- Modify: `crates/loomweave-plugin-rust/src/signature.rs` (function/struct signature builders — minimal here, expanded in Task 8)
- Test: `crates/loomweave-plugin-rust/src/extract.rs` (`#[cfg(test)]`)

Parse one file with `syn`, walk top-level + inline-`mod` items, and emit entity JSON `Value`s matching the wire contract. The file-level `module` entity is `file_scope` (the core auto-emits its `contains` edge). Inherent/trait impl *methods* use Task 4 qualnames; the impl block itself becomes an `impl` entity in Phase 1b (Phase 1a emits `module`/`struct`/`function`, where `function` includes methods).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn ids(entities: &[Value]) -> Vec<String> {
        entities.iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn extracts_module_struct_and_free_function() {
        let src = "pub struct Widget { a: i32 }\npub fn helper(x: i32) -> bool { x > 0 }\n";
        let out = extract_file("loomweave_core", "loomweave_core.config", "/p/src/config.rs", src).unwrap();
        let got = ids(&out);
        assert!(got.contains(&"rust:module:loomweave_core.config".to_owned()));
        assert!(got.contains(&"rust:struct:loomweave_core.config.Widget".to_owned()));
        assert!(got.contains(&"rust:function:loomweave_core.config.helper".to_owned()));
    }

    #[test]
    fn trait_and_inherent_methods_are_distinct_functions() {
        let src = "struct Foo;\nimpl std::fmt::Display for Foo { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\nimpl std::fmt::Debug for Foo { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n";
        let out = extract_file("k", "k.m", "/p/src/m.rs", src).unwrap();
        let got = ids(&out);
        assert!(got.iter().any(|id| id.contains("Foo.impl[Display].fmt")));
        assert!(got.iter().any(|id| id.contains("Foo.impl[Debug].fmt")));
    }

    #[test]
    fn every_entity_carries_file_path_and_byte_range() {
        let src = "pub fn a() {}\n";
        let out = extract_file("k", "k.m", "/p/src/m.rs", src).unwrap();
        let f = out.iter().find(|e| e["kind"] == "function").unwrap();
        assert_eq!(f["source"]["file_path"], "/p/src/m.rs");
        assert!(f["source"]["source_byte_start"].as_i64().is_some());
        assert!(f["source"]["source_byte_end"].as_i64().unwrap() > 0);
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust extract::tests -v`
Expected: FAIL (`extract_file` not defined).

- [ ] **Step 3: Implement extraction**

```rust
//! syn-based extraction of module/struct/function entities. ADR-049 ids.
use serde_json::{json, Value};
use syn::{Item, ItemFn, ItemStruct, ItemImpl, ItemMod, ImplItem};

use crate::qualname::{build_entity_id, free_item_qualname, impl_disc_for, method_qualname, PLUGIN_ID};
use crate::signature::{function_signature, struct_signature};
use crate::spans::source_range_of;

/// Extract entities from one file's source. `module_path` is the file-level
/// dotted module (Task 2 output). Returns wire-shaped entity `Value`s.
pub fn extract_file(crate_name: &str, module_path: &str, file_path: &str, src: &str) -> Result<Vec<Value>, syn::Error> {
    let file = syn::parse_file(src)?;
    let mut out = Vec::new();
    // File-level module entity (file_scope; core emits its contains edge).
    out.push(entity(
        "module",
        module_path,
        file_path,
        &crate::spans::SourceRange { byte_start: 0, byte_end: i64::try_from(src.len()).unwrap_or(0), start_line: 1, end_line: i64::try_from(src.lines().count()).unwrap_or(1) },
        None,
        None,
    )?);
    let module_id = build_entity_id("module", module_path)?.as_str().to_owned();
    walk_items(&file.items, crate_name, module_path, &module_id, file_path, &mut out)?;
    Ok(out)
}

fn walk_items(items: &[Item], crate_name: &str, module_path: &str, parent_id: &str, file_path: &str, out: &mut Vec<Value>) -> Result<(), syn::Error> {
    for item in items {
        match item {
            Item::Fn(ItemFn { sig, .. }) => {
                let name = sig.ident.to_string();
                let q = free_item_qualname(module_path, &name);
                out.push(entity("function", &q, file_path, &source_range_of(item), Some(parent_id), Some(function_signature(sig)))?);
            }
            Item::Struct(ItemStruct { ident, fields, .. }) => {
                let q = free_item_qualname(module_path, &ident.to_string());
                out.push(entity("struct", &q, file_path, &source_range_of(item), Some(parent_id), Some(struct_signature(fields)))?);
            }
            Item::Impl(it) => {
                emit_impl_methods(it, module_path, file_path, out)?;
            }
            Item::Mod(ItemMod { ident, content: Some((_, inner)), .. }) => {
                let nested = format!("{module_path}.{ident}");
                out.push(entity("module", &nested, file_path, &source_range_of(item), Some(parent_id), None)?);
                let nested_id = build_entity_id("module", &nested)?.as_str().to_owned();
                walk_items(inner, crate_name, &nested, &nested_id, file_path, out)?;
            }
            _ => {} // const/static/enum/trait/etc. are Phase 1b
        }
    }
    Ok(())
}

fn emit_impl_methods(it: &ItemImpl, module_path: &str, file_path: &str, out: &mut Vec<Value>) -> Result<(), syn::Error> {
    // type qualname for the impl's self type (simple path types in 1a;
    // exotic self types fall back to a textual rendering).
    let type_q = format!("{module_path}.{}", crate::qualname::self_ty_name(&it.self_ty));
    let disc = impl_disc_for(it); // Task 4 builder reading it.trait_ + it.generics + an ordinal
    let impl_id = build_entity_id("function", &crate::qualname::impl_qualname(&type_q, &disc))?; // impl entity itself is 1b; here we parent methods
    for member in &it.items {
        if let ImplItem::Fn(m) = member {
            let q = method_qualname(&type_q, &disc, &m.sig.ident.to_string());
            out.push(entity("function", &q, file_path, &source_range_of(member), Some(impl_id.as_str()), Some(function_signature(&m.sig)))?);
        }
    }
    Ok(())
}

fn entity(kind: &str, qualname: &str, file_path: &str, range: &crate::spans::SourceRange, parent_id: Option<&str>, signature: Option<Value>) -> Result<Value, syn::Error> {
    let id = build_entity_id(kind, qualname).map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e.to_string()))?;
    let mut e = json!({
        "id": id.as_str(),
        "kind": kind,
        "qualified_name": qualname,
        "source": {
            "file_path": file_path,
            "source_byte_start": range.byte_start,
            "source_byte_end": range.byte_end,
            "source_range": { "start_line": range.start_line, "end_line": range.end_line }
        }
    });
    if let Some(p) = parent_id { e["parent_id"] = json!(p); }
    if let Some(s) = signature { e["signature"] = s; }
    Ok(e)
}
```

Add the small helpers this references to `qualname.rs`: `self_ty_name(&syn::Type) -> String` (render a path type's last segment; fall back to a stable textual form for non-path types), and `impl_disc_for(&ItemImpl) -> ImplDisc` (read `it.trait_` for the trait path + generic args, `it.generics` for inherent param names, and an ordinal — for Phase 1a a simple per-file running counter keyed by `(type, trait?)` suffices; the gate test will reveal if a stronger ordinal is needed). Write a focused unit test for `self_ty_name` and `impl_disc_for` alongside Task 4.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust extract::tests -v`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/extract.rs crates/loomweave-plugin-rust/src/qualname.rs crates/loomweave-plugin-rust/src/signature.rs
git commit -m "feat(plugin-rust): syn extraction of module/struct/function entities (Task 6)"
```

---

## Task 7: Init-time project symbol table

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/symbol_table.rs`
- Modify: `crates/loomweave-plugin-rust/src/main.rs` (build the table at `initialize`)
- Test: `crates/loomweave-plugin-rust/src/symbol_table.rs`

At `initialize`, walk `project_root`, one `syn` parse per `.rs` file, and build a map from every declared entity qualname → its id. Phase 1a does not yet resolve cross-file edges (that's 1b), but the table is built and proven now so 1b can resolve against it; building it here also lets the gate test (Task 14) assert global uniqueness.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn builds_a_table_over_a_two_crate_workspace_with_no_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("crates/a/src")).unwrap();
        fs::write(root.join("crates/a/Cargo.toml"), "[package]\nname=\"a_crate\"\n").unwrap();
        fs::write(root.join("crates/a/src/lib.rs"), "pub struct X;\n").unwrap();
        fs::create_dir_all(root.join("crates/b/src")).unwrap();
        fs::write(root.join("crates/b/Cargo.toml"), "[package]\nname=\"b_crate\"\n").unwrap();
        fs::write(root.join("crates/b/src/lib.rs"), "pub struct X;\n").unwrap();

        let table = build_symbol_table(root);
        // same item name in two crates -> two DISTINCT ids, no collision
        assert!(table.contains_id("rust:struct:a_crate.X"));
        assert!(table.contains_id("rust:struct:b_crate.X"));
        assert_eq!(table.duplicate_ids(), Vec::<String>::new());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust symbol_table -v`
Expected: FAIL (`build_symbol_table` not defined).

- [ ] **Step 3: Implement the symbol table**

```rust
//! Init-time project symbol table (spec §2.3). One syn parse per .rs file.
use std::collections::BTreeMap;
use std::path::Path;

use crate::crate_roots::discover_crate_roots;
use crate::extract::extract_file;
use crate::module_path::module_path_for;

pub struct SymbolTable {
    /// entity id -> qualified name (the resolution surface for Phase 1b edges).
    by_id: BTreeMap<String, String>,
    /// ids seen more than once during the walk (must be empty — the gate).
    duplicates: Vec<String>,
}

impl SymbolTable {
    pub fn contains_id(&self, id: &str) -> bool { self.by_id.contains_key(id) }
    pub fn duplicate_ids(&self) -> Vec<String> { self.duplicates.clone() }
    pub fn len(&self) -> usize { self.by_id.len() }
    pub fn is_empty(&self) -> bool { self.by_id.is_empty() }
}

pub fn build_symbol_table(project_root: &Path) -> SymbolTable {
    let roots = discover_crate_roots(project_root);
    let mut by_id: BTreeMap<String, String> = BTreeMap::new();
    let mut duplicates = Vec::new();
    for file in walk_rs_files(project_root) {
        let Some(crate_name) = roots.crate_name_for(&file) else { continue };
        let Some(src_root) = src_root_of(&roots, &file) else { continue };
        let module_path = module_path_for(&crate_name, &src_root, &file);
        let Ok(src) = std::fs::read_to_string(&file) else { continue };
        // degraded files contribute their single module entity (Task 9 path);
        // here we tolerate parse errors by skipping their items.
        if let Ok(entities) = extract_file(&crate_name, &module_path, &file.to_string_lossy(), &src) {
            for e in entities {
                let id = e["id"].as_str().unwrap_or_default().to_owned();
                let q = e["qualified_name"].as_str().unwrap_or_default().to_owned();
                if by_id.insert(id.clone(), q).is_some() {
                    duplicates.push(id);
                }
            }
        }
    }
    SymbolTable { by_id, duplicates }
}
```
Add `walk_rs_files(root) -> Vec<PathBuf>` (recursive, skipping `target`/`.git`/`.weft`/`node_modules`, extension `rs`) and `src_root_of(&CrateRoots, &Path) -> Option<PathBuf>` (the crate dir for `file` joined with `src`). Add a `crate_dir_for` accessor to `CrateRoots` if needed.

- [ ] **Step 4: Wire it into `main.rs initialize`**

In the `initialize` arm, after deserialising `InitializeParams`, build and stash the table:
```rust
let project_root = std::path::PathBuf::from(params.project_root);
let symbol_table = loomweave_plugin_rust::symbol_table::build_symbol_table(&project_root);
// hold `symbol_table` (and `project_root`) in the loop's outer scope for analyze_file
```
(Phase 1a does not consult the table during `analyze_file` yet — Phase 1b does. Building it at init proves the §2.3 walk works and powers the gate test. Keep the binding alive so clippy does not flag it; a `let _ = symbol_table.len();` trace line is acceptable, or log `symbol_table.len()` at `tracing::info`.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust symbol_table -v && cargo build -p loomweave-plugin-rust`
Expected: PASS + clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/loomweave-plugin-rust/src/symbol_table.rs crates/loomweave-plugin-rust/src/main.rs
git commit -m "feat(plugin-rust): init-time project symbol table (Task 7)"
```

---

## Task 8: SEI signatures for `function` and `struct` (ADR-038)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/signature.rs`
- Test: same file

Per the manifest schemas: `function` → `{v:1, params, return_ann, generics}`, `struct` → `{v:1, fields}`. The core stores these verbatim and compares by string equality, so they must be **deterministic** (stable field order, canonical rendering).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn function_signature_captures_params_return_and_generics() {
        let f: syn::ItemFn = parse_quote!(fn g<T>(x: i32, y: T) -> bool { true });
        let s = function_signature(&f.sig);
        assert_eq!(s["v"], 1);
        assert_eq!(s["params"][0], "x: i32");
        assert_eq!(s["return_ann"], "bool");
        assert_eq!(s["generics"][0], "T");
    }

    #[test]
    fn struct_signature_captures_named_fields() {
        let st: syn::ItemStruct = parse_quote!(struct W { a: i32, b: String });
        let s = struct_signature(&st.fields);
        assert_eq!(s["v"], 1);
        assert_eq!(s["fields"][0], "a: i32");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust signature -v`
Expected: FAIL (functions not defined / signatures empty).

- [ ] **Step 3: Implement deterministic signature builders**

```rust
//! ADR-038 SEI signatures for Rust entities. Deterministic rendering only.
use serde_json::{json, Value};
use quote::ToTokens;
use syn::{Fields, Signature};

pub fn function_signature(sig: &Signature) -> Value {
    let params: Vec<String> = sig.inputs.iter().map(|a| a.to_token_stream().to_string()).collect();
    let return_ann = match &sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(_, ty) => ty.to_token_stream().to_string(),
    };
    let generics: Vec<String> = sig.generics.params.iter().map(|p| match p {
        syn::GenericParam::Type(t) => t.ident.to_string(),
        syn::GenericParam::Lifetime(l) => format!("'{}", l.lifetime.ident),
        syn::GenericParam::Const(c) => c.ident.to_string(),
    }).collect();
    json!({ "v": 1, "params": params, "return_ann": return_ann, "generics": generics })
}

pub fn struct_signature(fields: &Fields) -> Value {
    let rendered: Vec<String> = match fields {
        Fields::Named(n) => n.named.iter().map(|f| f.to_token_stream().to_string()).collect(),
        Fields::Unnamed(u) => u.unnamed.iter().enumerate().map(|(i, f)| format!("{i}: {}", f.ty.to_token_stream())).collect(),
        Fields::Unit => Vec::new(),
    };
    json!({ "v": 1, "fields": rendered })
}
```
Add `quote = "1"` to `[dependencies]` (for `ToTokens`/`to_token_stream`). `to_token_stream().to_string()` is deterministic for a given AST. Trim incidental whitespace if the test expects `"x: i32"` exactly — `to_token_stream()` renders `x : i32`; normalise with a small `tidy()` that collapses `" : "`→`": "` and `" , "`→`", "`, or relax the test assertions to `contains`. Pick one and keep it consistent.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust signature -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/signature.rs crates/loomweave-plugin-rust/Cargo.toml
git commit -m "feat(plugin-rust): deterministic SEI signatures for function/struct (Task 8)"
```

---

## Task 9: Degraded-parse handling (review M3)

**Files:**
- Modify: `crates/loomweave-plugin-rust/src/extract.rs`
- Test: same file

On `syn::parse_file` failure, emit a **single `module` entity flagged degraded** plus a Warning finding — never an empty list, never a panic. The manifest already declares the `syntax_degraded_module` role on `module`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn malformed_file_yields_one_degraded_module_and_a_warning() {
    let src = "fn broken( {{{ this is not rust";
    let (entities, findings) = extract_file_degraded_aware("k", "k.m", "/p/src/m.rs", src);
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0]["kind"], "module");
    assert_eq!(entities[0]["id"], "rust:module:k.m");
    assert_eq!(entities[0]["parse_status"], "syntax_error");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["severity"], "warning");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p loomweave-plugin-rust degraded -v`
Expected: FAIL (`extract_file_degraded_aware` not defined).

- [ ] **Step 3: Implement the degraded path**

```rust
/// Extraction wrapper: on parse failure return one degraded module entity + a
/// Warning finding. Matches the Python plugin's syntax-error pattern.
pub fn extract_file_degraded_aware(crate_name: &str, module_path: &str, file_path: &str, src: &str) -> (Vec<Value>, Vec<Value>) {
    match extract_file(crate_name, module_path, file_path, src) {
        Ok(entities) => (entities, Vec::new()),
        Err(e) => {
            let id = build_entity_id("module", module_path).map(|i| i.as_str().to_owned()).unwrap_or_default();
            let entity = json!({
                "id": id,
                "kind": "module",
                "qualified_name": module_path,
                "parse_status": "syntax_error",
                "source": { "file_path": file_path, "source_byte_start": 0, "source_byte_end": 0, "source_range": { "start_line": 1, "end_line": 1 } }
            });
            let finding = json!({
                "rule_id": "LMWV-RUST-SYNTAX-ERROR",
                "severity": "warning",
                "message": format!("syn could not parse {file_path}: {e}"),
                "entity_id": id_str_or_default(module_path)
            });
            (vec![entity], vec![finding])
        }
    }
}

fn id_str_or_default(module_path: &str) -> String {
    build_entity_id("module", module_path).map(|i| i.as_str().to_owned()).unwrap_or_default()
}
```
Confirm the finding wire shape against `AnalyzeFileFinding` (`protocol.rs:421`) and `rule_id_prefix` rules (must start `LMWV-RUST-`); adjust keys (`rule_id`/`severity`/`message`/`entity_id`) to the actual `AnalyzeFileFinding` field names. Then wire `analyze_file` in `main.rs` to call `extract_file_degraded_aware` and put `findings` into `AnalyzeFileResult.findings`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p loomweave-plugin-rust degraded -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/loomweave-plugin-rust/src/extract.rs crates/loomweave-plugin-rust/src/main.rs
git commit -m "feat(plugin-rust): degraded-parse module entity + warning finding (Task 9)"
```

---

## Task 10: Identity stability invariant test (spec §4.3)

**Files:**
- Create: `crates/loomweave-plugin-rust/tests/identity_stability.rs`

Reordering impls, mutating a method set, and renaming a generic param must not change any *other* entity's id (and the renamed-generic entity's own id is unchanged).

- [ ] **Step 1: Write the test**

```rust
use loomweave_plugin_rust::extract::extract_file;
use serde_json::Value;
use std::collections::BTreeSet;

fn id_set(src: &str) -> BTreeSet<String> {
    extract_file("k", "k.m", "/p/src/m.rs", src).unwrap()
        .iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn reordering_impl_blocks_does_not_change_method_ids() {
    let a = "struct Foo;\nimpl A for Foo { fn x(&self){} }\nimpl B for Foo { fn y(&self){} }\ntrait A { fn x(&self); }\ntrait B { fn y(&self); }\n";
    let b = "struct Foo;\nimpl B for Foo { fn y(&self){} }\nimpl A for Foo { fn x(&self){} }\ntrait A { fn x(&self); }\ntrait B { fn y(&self); }\n";
    assert_eq!(id_set(a), id_set(b));
}

#[test]
fn renaming_a_generic_param_is_a_noop_for_inherent_impl_ids() {
    let t = "struct Foo<X>(X);\nimpl<T> Foo<T> { fn m(&self){} }\n";
    let u = "struct Foo<X>(X);\nimpl<U> Foo<U> { fn m(&self){} }\n";
    // the method id (which carries the inherent-impl positional signature) is unchanged
    let mt: BTreeSet<_> = id_set(t).into_iter().filter(|i| i.contains(".m")).collect();
    let mu: BTreeSet<_> = id_set(u).into_iter().filter(|i| i.contains(".m")).collect();
    assert_eq!(mt, mu);
}
```

- [ ] **Step 2: Run it**

Run: `cargo nextest run -p loomweave-plugin-rust --test identity_stability -v`
Expected: PASS. If FAIL, the impl-ordinal or positional-generic logic in Task 4/6 needs fixing — fix it there (the ordinal must derive from a *content* key, e.g. `(type, trait?, normalised-generics)`, not raw source order), then re-run.

- [ ] **Step 3: Commit**

```bash
git add crates/loomweave-plugin-rust/tests/identity_stability.rs
git commit -m "test(plugin-rust): identity stability invariant (§4.3, Task 10)"
```

---

## Task 11: Identity uniqueness invariant test (spec §4.3 — the anti-collision regression)

**Files:**
- Create: `crates/loomweave-plugin-rust/tests/identity_uniqueness.rs`

A corpus containing **every ADR-049 collision pair** must yield zero duplicate ids.

- [ ] **Step 1: Write the test**

```rust
use loomweave_plugin_rust::extract::extract_file;

/// One source string per ADR-049 collision family, plus the cross-crate case.
fn corpus() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        // (crate, module, src)
        ("k", "k.m", "struct Foo;\nimpl std::fmt::Display for Foo { fn fmt(&self,_:&mut std::fmt::Formatter)->std::fmt::Result{Ok(())} }\nimpl std::fmt::Debug for Foo { fn fmt(&self,_:&mut std::fmt::Formatter)->std::fmt::Result{Ok(())} }\n"),
        ("k", "k.m", "struct Foo;\nimpl From<i32> for Foo { fn from(_:i32)->Foo{Foo} }\nimpl From<u32> for Foo { fn from(_:u32)->Foo{Foo} }\n"),
        ("k", "k.m", "struct Foo;\nimpl Foo { fn a(&self){} }\nimpl Foo { fn b(&self){} }\n"),
        ("k", "k.m", "#[cfg(unix)] fn f(){}\n#[cfg(windows)] fn f(){}\n"),
    ]
}

#[test]
fn no_duplicate_ids_across_every_collision_family() {
    let mut all: Vec<String> = Vec::new();
    for (c, m, src) in corpus() {
        for e in extract_file(c, m, "/p/src/m.rs", src).unwrap() {
            all.push(e["id"].as_str().unwrap().to_owned());
        }
    }
    let mut sorted = all.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "duplicate locator(s) emitted: {:?}", duplicates(&all));
}

fn duplicates(ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    ids.iter().filter(|i| !seen.insert((*i).clone())).cloned().collect()
}

#[test]
fn cross_crate_same_item_distinct() {
    let a: Vec<String> = extract_file("loomweave_core", "loomweave_core.config", "/p/a.rs", "pub struct X;\n").unwrap().iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect();
    let b: Vec<String> = extract_file("loomweave_cli", "loomweave_cli.config", "/p/b.rs", "pub struct X;\n").unwrap().iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect();
    assert!(a.iter().all(|id| !b.contains(id)));
}
```

- [ ] **Step 2: Run it**

Run: `cargo nextest run -p loomweave-plugin-rust --test identity_uniqueness -v`
Expected: PASS. If FAIL, the `@cfg` discriminant (Task 5) is not being applied to the cfg-twin `fn f` (extract.rs must detect the path-sharing sibling and apply `cfg_discriminant`), or the inherent-impl ordinal collapses two blocks — fix in extract.rs/qualname.rs, re-run.

- [ ] **Step 3: Commit**

```bash
git add crates/loomweave-plugin-rust/tests/identity_uniqueness.rs
git commit -m "test(plugin-rust): identity uniqueness over all ADR-049 collision families (Task 11)"
```

---

## Task 12: Extend `fixtures/entity_id.json` (non-vacuous parity gate — review H1)

**Files:**
- Modify: `fixtures/entity_id.json`
- Create: `crates/loomweave-plugin-rust/tests/entity_id_parity.rs`

The shared fixture currently has one trivial Rust-agnostic row. Add Rust rows that exercise the ADR-049 scheme, and assert the plugin's id construction matches byte-for-byte.

- [ ] **Step 1: Add Rust rows to the fixture**

Append to the `entities` array in `fixtures/entity_id.json` (do not disturb existing Python rows):
```json
{ "description": "rust crate-rooted struct", "plugin_id": "rust", "kind": "struct", "canonical_qualified_name": "loomweave_core.config.Widget", "expected_entity_id": "rust:struct:loomweave_core.config.Widget" },
{ "description": "rust trait-impl method carries impl discriminator", "plugin_id": "rust", "kind": "function", "canonical_qualified_name": "loomweave_core.m.Foo.impl[Display].fmt", "expected_entity_id": "rust:function:loomweave_core.m.Foo.impl[Display].fmt" },
{ "description": "rust inherent-impl method positional generics + ordinal", "plugin_id": "rust", "kind": "function", "canonical_qualified_name": "k.m.Foo.impl#<$0>#0.m", "expected_entity_id": "rust:function:k.m.Foo.impl#<$0>#0.m" },
{ "description": "rust cfg twin discriminant", "plugin_id": "rust", "kind": "function", "canonical_qualified_name": "k.m.f@cfg(unix)", "expected_entity_id": "rust:function:k.m.f@cfg(unix)" }
```
Match the literal qualname renderings to whatever Task 4/5 actually produce — if they differ, change these rows (and the §4.2 spec / ADR-049 examples) so all three agree. This row set is the cross-artifact source of truth.

- [ ] **Step 2: Write the parity test**

```rust
use loomweave_core::entity_id;
use serde_json::Value;

#[test]
fn rust_rows_assemble_byte_for_byte() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/entity_id.json")).unwrap();
    let doc: Value = serde_json::from_str(&raw).unwrap();
    for row in doc["entities"].as_array().unwrap() {
        if row["plugin_id"] != "rust" { continue; }
        let id = entity_id(
            row["plugin_id"].as_str().unwrap(),
            row["kind"].as_str().unwrap(),
            row["canonical_qualified_name"].as_str().unwrap(),
        ).unwrap();
        assert_eq!(id.as_str(), row["expected_entity_id"].as_str().unwrap(), "row: {}", row["description"]);
    }
}
```
(Verify the relative path to `fixtures/entity_id.json` from the crate; adjust the `../../` depth. If a Rust-side check script enforces the fixture — e.g. a `scripts/check-*.py` — run it too.)

- [ ] **Step 3: Run it**

Run: `cargo nextest run -p loomweave-plugin-rust --test entity_id_parity -v`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add fixtures/entity_id.json crates/loomweave-plugin-rust/tests/entity_id_parity.rs
git commit -m "test(plugin-rust): non-vacuous entity_id.json parity rows for ADR-049 scheme (Task 12)"
```

---

## Task 13: Host-integration test (handshake → analyze_file → shutdown)

**Files:**
- Create: `crates/loomweave-plugin-rust/tests/host_integration.rs`
- Create: `crates/loomweave-plugin-rust/tests/fixtures/sample.rs` (a tiny analysable file)

Model on `crates/loomweave-core/tests/host_subprocess.rs`: locate the off-glob `loomweave-rust-plugin` binary, stage it under the discovery name `loomweave-plugin-rust` next to a `plugin.toml`, spawn via `PluginHost::spawn`, run the full handshake, `analyze_file` the sample, assert at least the expected struct/function ids come back, then `shutdown`/`exit` with code 0.

- [ ] **Step 1: Write the test (reuse the fixture-test scaffolding)**

Copy the `fixture_binary_path()` / `staged_fixture()` / target-dir-search helpers from `host_subprocess.rs`, renaming `loomweave-fixture-plugin` → `loomweave-rust-plugin` and the staged basename → `loomweave-plugin-rust`. Stage the crate's own `plugin.toml` (`include_bytes!("../plugin.toml")`) beside the staged binary. Then:
```rust
#[test]
fn handshake_analyze_shutdown_roundtrip() {
    let staged = staged_rust_plugin(); // dir containing loomweave-plugin-rust + plugin.toml
    let manifest = loomweave_core::plugin::parse_manifest(include_bytes!("../plugin.toml")).unwrap();
    let mut host = loomweave_core::PluginHost::spawn(&staged.binary, manifest, &staged.project_root).expect("spawn");
    host.initialize(/* project_root = the fixtures dir */).expect("initialize");
    host.initialized().expect("initialized");
    let result = host.analyze_file(&staged.sample_rs).expect("analyze_file");
    let ids: Vec<String> = result.entities.iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect();
    assert!(ids.iter().any(|id| id.starts_with("rust:struct:")));
    assert!(ids.iter().any(|id| id.starts_with("rust:function:")));
    host.shutdown().expect("shutdown");
    host.exit().expect("exit");
}
```
Adapt the exact `PluginHost` method names/signatures to the real API in `crates/loomweave-core/src/plugin/host.rs` (the fixture test is the authoritative example — match its calls). `sample.rs` content:
```rust
pub struct Gadget { pub n: i32 }
pub fn make() -> Gadget { Gadget { n: 0 } }
```

- [ ] **Step 2: Run it**

Run: `cargo nextest run -p loomweave-plugin-rust --test host_integration -v`
Expected: PASS (entities round-trip through the real host).

- [ ] **Step 3: Commit**

```bash
git add crates/loomweave-plugin-rust/tests/host_integration.rs crates/loomweave-plugin-rust/tests/fixtures/sample.rs
git commit -m "test(plugin-rust): host-integration handshake/analyze/shutdown roundtrip (Task 13)"
```

---

## Task 14: THE PHASE 1a GATE — zero collisions over Loomweave's own workspace

**Files:**
- Create: `crates/loomweave-plugin-rust/tests/dogfood_uniqueness.rs`

This is the exit gate the spec names: **proven zero-collision over Loomweave's 8-crate workspace.** Build the symbol table over this repo's `crates/` and assert no duplicate locators. While here, capture a rough RSS/entity-count figure to seed the manifest NFRs (review M6).

- [ ] **Step 1: Write the gate test**

```rust
use loomweave_plugin_rust::symbol_table::build_symbol_table;
use std::path::PathBuf;

/// The workspace root = three parents up from this crate's manifest dir
/// (crates/loomweave-plugin-rust -> crates -> repo root).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn rust_plugin_emits_zero_colliding_locators_over_this_workspace() {
    let table = build_symbol_table(&workspace_root().join("crates"));
    let dups = table.duplicate_ids();
    assert!(table.len() > 100, "expected a substantial entity set, got {}", table.len());
    assert_eq!(dups, Vec::<String>::new(), "PHASE 1a GATE FAILED — colliding locators: {dups:?}");
}
```

- [ ] **Step 2: Run the gate**

Run: `cargo nextest run -p loomweave-plugin-rust --test dogfood_uniqueness -v`
Expected: PASS. **If it FAILS, the printed duplicate locators are real collision bugs in the qualname scheme — do not weaken the assertion. Fix the offending case** (commonly: `#[path]` modules not handled in Task 2, exotic `self_ty` rendering colliding in Task 6, or inherent-impl ordinals collapsing). Each fix lands in the relevant earlier task's module, then re-run the gate.

- [ ] **Step 3: Record the measured NFR basis (review M6)**

Capture an approximate working-set figure and entity count to replace the placeholder manifest envelope:
```bash
/usr/bin/time -v cargo nextest run -p loomweave-plugin-rust --test dogfood_uniqueness 2>&1 | grep -E "Maximum resident|entity"
```
Update `crates/loomweave-plugin-rust/plugin.toml` `[capabilities.runtime] expected_max_rss_mb` to a measured value with a comment citing this gate run as the basis (ADR-035), and set `expected_entities_per_file` from the observed max.

- [ ] **Step 4: Run the full CI floor for the crate**

```bash
cargo fmt --all -- --check
cargo clippy -p loomweave-plugin-rust --all-targets --all-features -- -D warnings
cargo build --workspace --bins
cargo nextest run -p loomweave-plugin-rust --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p loomweave-plugin-rust --no-deps --all-features
cargo deny check
```
Expected: all green. Fix any clippy/doc/deny issues inline.

- [ ] **Step 5: Commit and merge to the active release branch**

```bash
git add crates/loomweave-plugin-rust/tests/dogfood_uniqueness.rs crates/loomweave-plugin-rust/plugin.toml
git commit -m "test(plugin-rust): Phase 1a GATE — zero-collision over Loomweave workspace + measured NFRs (Task 14)"
```
Then merge `feat/rust-plugin-spec` (this worktree's branch) into the active `rc3` release branch per the project's always-merge-to-working-release rule. Phase 1a is independently mergeable: it adds a crate that builds, tests green, and proves the identity foundation, with no edges keyed on it yet.

---

## Phase 1a self-review checklist (run before declaring the gate passed)

- [ ] **Spec coverage:** §2.1 crate/off-glob binary (Task 0) · §2.3 symbol table (Task 7) · §3.1 starter kinds (Task 6) · §3.3 manifest roles/no guidance fix (Task 0 manifest) · §4.1 crate-rooted qualname (Task 3) · §4.2 impl/method discrimination + cfg (Tasks 4, 5) · §4.3 stability + uniqueness (Tasks 10, 11) · §4.4 signatures (Task 8) · §5 cfg-twin collision closed (Task 5) · §6 Phase 1a gate (Task 14) · §7 fixture/tests (Tasks 12, 13) — all present.
- [ ] **No emitted `kind` outside the manifest's `entity_kinds`** (`module`/`struct`/`function` only in 1a).
- [ ] **Every entity id equals `entity_id(plugin, kind, qualname)`** — never hand-assembled.
- [ ] **The off-glob artifact name holds** — `target/debug/loomweave-plugin-rust` does not exist after a workspace build.
- [ ] **The gate assertion was not weakened** to make Task 14 pass.

---

## Forward outline (separate plans, gated behind Phase 1a)

These become their own `writing-plans` documents once the Phase 1a gate is green. Listed so the whole arc is visible; **not** bite-sized here.

- **Phase 1b — remaining entities + Resolved structural edges.** Add the 7 remaining kinds (`enum`, `trait`, `impl`, `type_alias`, `const`, `static`, `macro`); the `impl` entity itself; `contains` (plugin-authored where the core does not), `imports`/`implements` (Resolved only on the uniquely-resolvable subset, else Ambiguous — review H5), `derives`; consult the §2.3 symbol table during `analyze_file` to resolve cross-file targets, with the M5 staleness downgrade (differ → Inferred / `unresolved_call_sites`). Manifest `ontology_version` MINOR bump (ADR-027) + extended `entity_kinds`/`edge_kinds`. Golden-snapshot E2E over a small vendored workspace (review H2).
- **Phase 2 — `calls` + `references`.** Same-name candidates Ambiguous; unresolved sites via `unresolved_call_sites` for query-time Inferred resolution.
- **Phase 3 (optional) — rust-analyzer enrichment.** Gated, off the build-free path; additive for edge-confidence only; any newly-revealed macro entities go through the ADR-049 + SEI + parity-fixture contract (review H4).
- **Cross-cutting follow-ups (own tickets):** `resolve_wardline_qualnames` `plugin_id`-generic refactor before Rust `wardline_aware` (review B4); subsystem-membership queries made plugin-agnostic (review M1); `entity_at` span-ordering CASE extended for Rust kinds (review M2); `loomweave install` Rust-plugin PATH staging + neighbour-manifest discovery for the real (non-test) install path.
```
