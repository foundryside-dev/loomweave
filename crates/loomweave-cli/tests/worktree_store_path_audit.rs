//! Grep-audit-backed regression test for worktree-index Task 7: no
//! production consumer may re-derive Loomweave's store path
//! (`store_dir()`/`db_path()`/`embeddings_db_path()`) from a project root
//! without going through `StorePaths`/`WorktreeContext` (or a documented,
//! deliberate exception) — a missed consumer would silently read or write
//! the WRONG store for a linked worktree.
//!
//! This is the audit's automated backbone: it re-runs (in Rust, over the
//! actual source tree) the same grep the worktree-index Task 7 report's
//! classified table is built from, and asserts the result set is EXACTLY
//! the reviewed allowlist below — not "no more than," not "roughly,"
//! exactly. A new call site that isn't in [`ALLOWED_HITS`] fails the test;
//! so does one that vanished from the source without the allowlist being
//! updated (both directions are checked — see
//! `every_runtime_leaf_resolves_from_store_paths` below).
//!
//! ## Scope and known limits (documented per the brief's request)
//!
//! - **Scans `crates/*/src/**/*.rs` only.** Integration tests under
//!   `crates/*/tests/` are excluded — the worktree-index Task 7 brief is
//!   explicit that "tests and fixtures may keep the low-level helpers," and
//!   the production/consumer surface this test protects lives entirely
//!   under `src/`. The much larger `tests/` surface is deliberately out of
//!   scope; auditing it would bloat this allowlist with fixture noise that
//!   says nothing about production routing.
//! - **Every line in an included file is scanned** — doc comments,
//!   `#[cfg(test)] mod tests` blocks, and (crucially) NOT truncated at any
//!   `#[cfg(test)]` marker. An earlier design truncated a file's scan at its
//!   first `#[cfg(test)]`, but that heuristic risks a false NEGATIVE:
//!   silently dropping real production code that happens to be declared
//!   *after* an inline test module in the same file. A false negative is
//!   worse for an audit gate than the larger, fully-enumerated allowlist
//!   this choice costs (which does include a number of in-file
//!   `#[cfg(test)] mod tests` hits, explicitly marked `Exempt` below) — see
//!   [`sentinel_line_is_present`] and the file/hit floor assertions in
//!   [`every_runtime_leaf_resolves_from_store_paths`], which exist
//!   specifically so a scan that silently walks zero files (or the wrong
//!   ones) fails loudly instead of passing vacuously.
//! - **A line is skipped only when its trimmed text starts with `//`** (a
//!   comment or doc comment) — this can only drop prose, never a real call.
//! - **Two files are excluded by name, not by heuristic**: `loomweave-core/
//!   src/store.rs` and `loomweave-storage/src/embeddings.rs` are where
//!   `store_dir`/`db_path`/`embeddings_db_path` are themselves DEFINED; a
//!   naive substring scan of their own bodies would just re-find their own
//!   implementations, which is not what this audit is checking for.
//! - **Line-text based, not AST based** — the worktree-index Task 7 brief is
//!   explicit that "a syn AST gate ... is scoped to a much larger refactor
//!   and is not warranted" for this task. A call spread across multiple
//!   lines, or reached through a renamed import alias, would not be caught.
//!   Every call site classified below is single-line and uses the canonical
//!   `loomweave_core::store::`/`loomweave_storage::` paths (or the bare
//!   `store_dir`/`embeddings_db_path` name after a `use` import), so this is
//!   a faithful audit of today's code, not a general guarantee against
//!   every conceivable way to re-derive a store path.
//! - **Substring false positives are real and are allowlisted, not
//!   filtered.** The patterns are plain substrings, so `embeddings_db_path(`
//!   also matches inside `open_in_store_dir(` — no, wait: `store_dir(` also
//!   matches inside `open_in_store_dir(` (a different, unrelated function
//!   whose name happens to end in `_store_dir`), and `db_path(` also
//!   matches inside this crate's own `resolve_effective_db_path(` /
//!   `effective_db_path(` helper names. Rather than special-case the
//!   pattern to dodge these, they are enumerated in [`ALLOWED_HITS`] with an
//!   explicit note — the same substring imprecision the brief's own literal
//!   `grep` command has.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The workspace root = two parents up from this crate's manifest dir
/// (`crates/loomweave-cli` -> `crates` -> repo root) — the same convention
/// `loomweave-plugin-rust`'s `dogfood_uniqueness.rs` uses for its own
/// whole-workspace scan.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The three call patterns this audit tracks — kept as substrings (not a
/// regex or `syn` parse) per the brief's explicit "a grep-based audit is
/// sufficient here" instruction.
const PATTERNS: &[&str] = &["store_dir(", "db_path(", "embeddings_db_path("];

/// worktree-index Task 7 fix-loop finding 4: `PATTERNS` above audits only
/// DIRECT calls to `store_dir`/`db_path`/`embeddings_db_path` — a "one-hop"
/// wrapper (a function that itself calls one of those three exactly once,
/// correctly, as its own implementation) is invisible under its own name.
/// `llm_traffic_log_path(project_root)`, `read_published_port(project_root)`,
/// `publish_port(project_root, ..)`, and `resolve_loomweave_url(.., project_root, ..)`
/// are exactly this shape: each internally re-derives `store_dir`/`db_path`
/// once (so THEIR OWN definition satisfies `PATTERNS`), but a CALLER of the
/// wrapper does not, because the caller's line contains none of `PATTERNS`'
/// three substrings. This is exactly how fix-loop findings 1 and 2 escaped
/// the original Task 7 audit: `serve.rs`'s `build_llm_provider` called
/// `llm_traffic_log_path(project_root)`, and `tools/status.rs`'s
/// `loomweave_read_api_json` called `resolve_loomweave_url(None, project_root, ..)`
/// — neither line contains `store_dir(`, `db_path(`, or
/// `embeddings_db_path(`, so neither was ever a candidate hit under
/// `PATTERNS` alone. (An earlier draft of this module's docs claimed
/// `loomweave_url.rs` was covered "per its own entry in this table" — false:
/// no such entry existed or could exist under `PATTERNS`, since
/// `read_published_port(` matches none of the three tracked substrings.
/// `WRAPPER_PATTERNS` below is what actually closes that gap.)
///
/// `open_in_store_dir(` is deliberately NOT added to `WRAPPER_PATTERNS`: it
/// already matches `PATTERNS`' `store_dir(` as a literal substring (see the
/// module docs' false-positive note above), so adding it again would just
/// double-count the same hits under a second pattern list.
///
/// Still not exhaustive: a *second*-hop wrapper (a function that calls one
/// of these five, rather than one of the original three) would evade both
/// pattern lists the same way. The brief's "a syn AST gate ... is not
/// warranted" holds for the same reason it did originally — this audit is a
/// grep, not a call-graph; each newly-found blind spot gets closed by naming
/// it explicitly here, not by chasing arbitrary call depth.
const WRAPPER_PATTERNS: &[&str] = &[
    "llm_traffic_log_path(",
    "read_published_port(",
    "publish_port(",
    "resolve_loomweave_url(",
    // `store_dir(` + override provenance (clarion-306ed41ce3). Escapes
    // `PATTERNS`' `store_dir(` because the char after `store_dir` is `_`,
    // not `(` — the textbook one-hop shape this list exists for.
    "store_dir_resolution(",
];

/// The two files where `store_dir`/`db_path`/`embeddings_db_path` are
/// themselves defined — excluded by name (see the module docs).
const DEFINITION_FILES: &[&str] = &[
    "crates/loomweave-core/src/store.rs",
    "crates/loomweave-storage/src/embeddings.rs",
];

/// One classified allowlist entry: `(file, exact trimmed line text, expected
/// occurrence count)`. A `(file, text)` pair is a set key on its own (Rust
/// has no duplicate-line problem across *different* files/texts), but
/// several files legitimately contain the SAME line text more than once
/// (e.g. `doctor.rs` has three functions that each independently open
/// `let db_path = loomweave_core::store::db_path(project_root);`) — a plain
/// `(file, text)` set would collapse those and could not detect two of the
/// three silently vanishing. The count makes both directions of drift
/// (a new hit appearing, or one disappearing) fail the comparison.
///
/// `Route` = production code that has been migrated to resolve through
/// `WorktreeContext`/`StorePaths` (the fallback arm inside a
/// `resolve_effective_*`/`effective_*` helper still legitimately calls the
/// low-level function ONCE, as the fallback target — that is what's
/// enumerated here, not a leftover bug).
/// `AlreadyRouted` = a prior task (3/4/5/6) already routed this correctly;
/// recorded here so a regression is still caught.
/// `IntentionalRootDerived` = deliberately, and by design, still root
/// -derived — either because the call site defines an explicit `_at`/leaf
/// -taking variant, is the one canonical resolution point
/// (`WorktreeContext::resolve` itself), or is out of this task's declared
/// scope with a documented reason (doctor.rs's other ~20 checks; install.rs
/// initialising at the literal given path is what `install` IS).
/// `TestExempt` = `#[cfg(test)] mod tests` / `#[test]` fixture code — the
/// brief: "Tests and fixtures may keep the low-level helpers."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Classification {
    Route,
    AlreadyRouted,
    IntentionalRootDerived,
    TestExempt,
}

struct AllowedHit {
    file: &'static str,
    text: &'static str,
    count: usize,
    #[allow(dead_code)] // documents the audit table; not read by the assertion itself
    classification: Classification,
}

/// The classified, reviewed allowlist — see the worktree-index Task 7
/// report for the per-file narrative this table backs. Every entry here was
/// individually read and classified during Task 7's implementation, not
/// generated after the fact to make the test pass.
#[rustfmt::skip]
const ALLOWED_HITS: &[AllowedHit] = &[
    // ---- crates/loomweave-cli/src/analyze.rs (test module) ----
    AllowedHit { file: "crates/loomweave-cli/src/analyze.rs", text: "&loomweave_storage::embeddings_db_path(project.path()),", count: 2, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/analyze.rs", text: "let db_path = loomweave_core::store::db_path(project.path());", count: 2, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/analyze.rs", text: "let store = EmbeddingStore::open_in_store_dir(project.path()).unwrap();", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/analyze.rs", text: "std::fs::create_dir_all(loomweave_core::store::store_dir(project.path())).unwrap();", count: 2, classification: Classification::TestExempt },

    // ---- crates/loomweave-cli/src/config.rs ----
    AllowedHit { file: "crates/loomweave-cli/src/config.rs", text: "let probed = loomweave_storage::embeddings_db_path(root);", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/config.rs", text: "let store = loomweave_storage::EmbeddingStore::open_in_store_dir(root).unwrap();", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/config.rs", text: "loomweave_storage::embeddings_db_path(path)", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/config.rs", text: "std::fs::create_dir_all(loomweave_core::store::store_dir(root)).unwrap();", count: 1, classification: Classification::TestExempt },

    // ---- crates/loomweave-cli/src/db.rs ----
    AllowedHit { file: "crates/loomweave-cli/src/db.rs", text: "fn resolve_effective_db_path(project_root: &Path) -> std::path::PathBuf {", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/db.rs", text: "let db_path = resolve_effective_db_path(project_root);", count: 2, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/db.rs", text: "loomweave_core::store::db_path(project_root)", count: 1, classification: Classification::Route },

    // ---- crates/loomweave-cli/src/doctor.rs ----
    // Additive worktree-store report — reuses the `_at` classifier.
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "classify_index_db_health_at(&loomweave_core::store::db_path(project_root))", count: 1, classification: Classification::IntentionalRootDerived },
    // The remaining doctor.rs hits: doctor's ~20 existing per-checkout
    // checks stay root-derived from the literal `--path` given, by explicit
    // task scope (only the additive worktree-store report + the `--fix`
    // guard in install.rs were in scope for GREEN) — see the Task 7 report.
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "crate::install::write_gitignore(&loomweave_core::store::store_dir(project_root))", count: 1, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "let db = loomweave_core::store::db_path(project_root);", count: 2, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "let db = loomweave_core::store::db_path(root);", count: 2, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "let db_path = loomweave_core::store::db_path(project_root);", count: 3, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "let loomweave_dir = loomweave_core::store::store_dir(project_root);", count: 1, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "let store = loomweave_core::store::store_dir(project_root);", count: 2, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "let store = loomweave_core::store::store_dir(root);", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/doctor.rs", text: "std::fs::read_to_string(loomweave_core::store::store_dir(root).join(\".gitignore\"))", count: 1, classification: Classification::TestExempt },
    // ---- crates/loomweave-cli/src/guidance.rs ----
    AllowedHit { file: "crates/loomweave-cli/src/guidance.rs", text: "fn resolve_effective_db_path(project_root: &Path) -> std::path::PathBuf {", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/guidance.rs", text: "let db_path = resolve_effective_db_path(project_root);", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/guidance.rs", text: "loomweave_core::store::db_path(project_root)", count: 1, classification: Classification::Route },

    // ---- crates/loomweave-cli/src/hook.rs ----
    AllowedHit { file: "crates/loomweave-cli/src/hook.rs", text: "fn resolve_effective_db_path(project_root: &Path) -> PathBuf {", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/hook.rs", text: "let db_path = resolve_effective_db_path(project_root);", count: 2, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-cli/src/hook.rs", text: "loomweave_core::store::db_path(project_root)", count: 1, classification: Classification::Route },

    // ---- crates/loomweave-cli/src/http_read.rs (test module) ----
    // `spawn`'s own port-path routing is already correct — it takes an
    // explicit `port_path: PathBuf` argument from its caller
    // (`serve.rs` passes `worktree_ctx.store_paths.port.clone()`), so
    // `http_read.rs` has no production `WRAPPER_PATTERNS` hit at all. These
    // four are all `read_published_port(dir.path())` reads inside its own
    // `#[cfg(test)] mod tests`, verifying what got published.
    AllowedHit { file: "crates/loomweave-cli/src/http_read.rs", text: "let port_a = read_published_port(dir_a.path()).expect(\"a published a port\");", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/http_read.rs", text: "let port_b = read_published_port(dir_b.path()).expect(\"b published a port\");", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/http_read.rs", text: "let published = read_published_port(dir.path()).expect(\"published a port\");", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-cli/src/http_read.rs", text: "read_published_port(dir.path()).is_some(),", count: 1, classification: Classification::TestExempt },

    // ---- crates/loomweave-cli/src/install.rs ----
    // `install` initialises AT the literal given path by definition — there
    // is no "effective store elsewhere" for it to resolve to.
    AllowedHit { file: "crates/loomweave-cli/src/install.rs", text: "let loomweave_dir = loomweave_core::store::store_dir(project_root);", count: 1, classification: Classification::IntentionalRootDerived },

    // ---- crates/loomweave-cli/src/secret_scan.rs (test module) ----
    AllowedHit { file: "crates/loomweave-cli/src/secret_scan.rs", text: "&loomweave_core::store::store_dir(tmp.path()),", count: 1, classification: Classification::TestExempt },

    // ---- crates/loomweave-cli/src/serve.rs ----
    // Main/standalone's own gate, byte-for-byte unchanged from before Task 5
    // — routed through the plain project root deliberately (pinned decision
    // 1: a linked worktree never reaches this line at all; `choose_serve_route`
    // sends it to `run_linked_worktree` first).
    AllowedHit { file: "crates/loomweave-cli/src/serve.rs", text: "let db_path = loomweave_core::store::db_path(path);", count: 1, classification: Classification::IntentionalRootDerived },
    // Test-only fixture sanity check (asserts the linked-worktree fixture's
    // SOURCE root genuinely has no local store, before asserting the route).
    AllowedHit { file: "crates/loomweave-cli/src/serve.rs", text: "let db_exists = loomweave_core::store::db_path(&ctx.source_root).exists();", count: 1, classification: Classification::TestExempt },
    // fix-loop finding 1's regression test: proves the traffic log is NOT
    // written under this (deliberately unrouted) re-derived path.
    AllowedHit { file: "crates/loomweave-cli/src/serve.rs", text: "let forbidden_log = loomweave_core::store::llm_traffic_log_path(&ctx.source_root);", count: 1, classification: Classification::TestExempt },

    // ---- crates/loomweave-core/src/worktree/context.rs ----
    // The canonical resolution point: this IS where `repository_store` is
    // computed from a root. Every other file in this table exists to route
    // AROUND re-deriving this, not to duplicate it.
    AllowedHit { file: "crates/loomweave-core/src/worktree/context.rs", text: "let (repository_store, store_dir_overridden) = store_dir_resolution(&primary_root);", count: 1, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-core/src/worktree/context.rs", text: "let (repository_store, store_dir_overridden) = store_dir_resolution(&source_root);", count: 1, classification: Classification::IntentionalRootDerived },

    // ---- crates/loomweave-federation/src/loomweave_port.rs ----
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "assert!(!loomweave_core::store::store_dir(dir.path()).exists());", count: 1, classification: Classification::TestExempt },
    // The project-root-keyed fallback Task 4 kept for callers with no
    // resolved `StorePaths` (doctor.rs, install.rs, integration_bindings.rs).
    // `resolve_loomweave_url`/`loomweave_url.rs` used to be named here too —
    // wrong (fix-loop finding 4): that caller's own line never matched
    // `PATTERNS` at all, so it was never actually covered by "its own entry
    // in this table" the way this comment used to claim. It is now a real
    // `WRAPPER_PATTERNS` entry below, and its own callers were fixed
    // (findings 1/2) or are the declared doctor.rs scope gap.
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "loomweave_core::store::store_dir(project_root).join(\"ephemeral.port\")", count: 1, classification: Classification::AlreadyRouted },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "std::fs::create_dir_all(loomweave_core::store::store_dir(dir.path())).unwrap();", count: 2, classification: Classification::TestExempt },
    // `WRAPPER_PATTERNS` hits (fix-loop finding 4): `publish_port`/
    // `read_published_port`'s own definitions each legitimately re-derive
    // `published_port_path(project_root)` once, by design — the accepted
    // root-taking convenience form for main/standalone; `publish_port_at`/
    // `read_published_port_at` are what a linked-worktree caller uses
    // instead (both already existed before this fix-loop).
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "pub fn publish_port(project_root: &Path, port: u16) -> std::io::Result<()> {", count: 1, classification: Classification::IntentionalRootDerived },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "pub fn read_published_port(project_root: &Path) -> Option<u16> {", count: 1, classification: Classification::IntentionalRootDerived },
    // The rest are this module's own `#[cfg(test)] mod tests` exercising the
    // root-taking wrappers directly.
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "assert_eq!(read_published_port(dir.path()), None);", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "assert_eq!(read_published_port(dir.path()), Some(10000));", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "assert_eq!(read_published_port(dir.path()), Some(9412));", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "assert_eq!(read_published_port(dir.path()), Some(9500));", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "assert_eq!(read_published_port(dir.path()), Some(9999));", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "publish_port(dir.path(), 10000).expect(\"publish creates .weft/loomweave/\");", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "publish_port(dir.path(), 9412).expect(\"publish\");", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "publish_port(dir.path(), 9412).unwrap();", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "publish_port(dir.path(), 9999).unwrap();", count: 2, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_port.rs", text: "read_published_port(dir.path()),", count: 2, classification: Classification::TestExempt },

    // ---- crates/loomweave-federation/src/loomweave_url.rs ----
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "let store = loomweave_core::store::store_dir(dir.path());", count: 1, classification: Classification::TestExempt },
    // `resolve_loomweave_url`'s own definition: the accepted root-taking
    // convenience form (rung 2's `weft.toml` lookup is legitimately
    // root-derived even for a linked worktree — see its doc comment).
    // `resolve_loomweave_url_at` (fix-loop finding 2) is what a caller with
    // an already-resolved port path — `loomweave-mcp`'s
    // `loomweave_read_api_json` — uses instead.
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "pub fn resolve_loomweave_url(", count: 1, classification: Classification::IntentionalRootDerived },
    // The rest are this module's own `#[cfg(test)] mod tests`.
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "let res = resolve_loomweave_url(None, dir.path(), |_| None);", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "let res = resolve_loomweave_url(None, dir.path(), |_| Some(\"  \".to_owned()));", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "let res = resolve_loomweave_url(Some(\"   \"), dir.path(), |_| None);", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "let res = resolve_loomweave_url(Some(\"http://127.0.0.1:9111\"), dir.path(), |_| None);", count: 4, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "let res = resolve_loomweave_url(Some(\"http://127.0.0.1:9111\"), dir.path(), |name| {", count: 1, classification: Classification::TestExempt },
    // fix-loop finding 2's regression test: proves the wrapper and
    // `resolve_loomweave_url_at` agree given the default port path.
    // rustfmt wraps the assignment across two lines (100-col limit), so the
    // call itself is the hit, not the `let via_wrapper =` line above it.
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "resolve_loomweave_url(Some(\"http://127.0.0.1:9111\"), dir.path(), |_| None);", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "publish_port(dir.path(), 9412).unwrap();", count: 4, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-federation/src/loomweave_url.rs", text: "publish_port(dir.path(), 9413).unwrap();", count: 1, classification: Classification::TestExempt },

    // ---- crates/loomweave-mcp/src/lib.rs ----
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "&loomweave_storage::embeddings_db_path(root),", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "fn effective_db_path(&self) -> PathBuf {", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "let db_path = self.effective_db_path();", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "let store = loomweave_storage::EmbeddingStore::open_in_store_dir(root).unwrap();", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "std::fs::create_dir_all(loomweave_core::store::store_dir(root)).unwrap();", count: 1, classification: Classification::TestExempt },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "|| embeddings_db_path(&self.project_root),", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "|| loomweave_core::store::db_path(&self.project_root),", count: 1, classification: Classification::Route },
    AllowedHit { file: "crates/loomweave-mcp/src/lib.rs", text: "|| loomweave_core::store::store_dir(&self.project_root).join(\"runs\"),", count: 1, classification: Classification::Route },

    // ---- crates/loomweave-mcp/src/tools/analyze.rs ----
    AllowedHit { file: "crates/loomweave-mcp/src/tools/analyze.rs", text: "let db_path = self.effective_db_path();", count: 1, classification: Classification::Route },

    // ---- crates/loomweave-mcp/src/tools/status.rs ----
    AllowedHit { file: "crates/loomweave-mcp/src/tools/status.rs", text: "let db_path = self.effective_db_path();", count: 1, classification: Classification::Route },
];

/// One `(file, trimmed line text)` -> occurrence-count entry found by
/// scanning the real source tree.
type HitMap = BTreeMap<(String, String), usize>;

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_rust_files(&path, out);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Scan every `crates/*/src/**/*.rs` file (excluding [`DEFINITION_FILES`])
/// for [`PATTERNS`] and [`WRAPPER_PATTERNS`], skipping comment lines.
/// Returns the hit map plus the number of files actually scanned (for the
/// floor assertion).
fn scan_production_src(workspace_root: &Path) -> (HitMap, usize) {
    let mut all_files = Vec::new();
    for entry in std::fs::read_dir(workspace_root.join("crates")).expect("read crates/") {
        let entry = entry.expect("crates/ entry");
        if !entry.file_type().expect("file type").is_dir() {
            continue;
        }
        let src_dir = entry.path().join("src");
        if src_dir.is_dir() {
            collect_rust_files(&src_dir, &mut all_files);
        }
    }

    let mut hits: HitMap = BTreeMap::new();
    let mut files_scanned = 0usize;
    for path in all_files {
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if DEFINITION_FILES.contains(&rel.as_str()) {
            continue;
        }
        files_scanned += 1;
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            if PATTERNS
                .iter()
                .chain(WRAPPER_PATTERNS)
                .any(|pattern| trimmed.contains(pattern))
            {
                *hits.entry((rel.clone(), trimmed.to_owned())).or_insert(0) += 1;
            }
        }
    }
    (hits, files_scanned)
}

fn allowed_map() -> HitMap {
    let mut map = HitMap::new();
    for entry in ALLOWED_HITS {
        map.insert((entry.file.to_owned(), entry.text.to_owned()), entry.count);
    }
    map
}

#[test]
fn every_runtime_leaf_resolves_from_store_paths() {
    let (found, files_scanned) = scan_production_src(&workspace_root());

    // Self-check (advisor-flagged risk: a scan that silently walks zero — or
    // the wrong — files would otherwise pass vacuously). 146 files exist
    // under `crates/*/src/` as of this test's writing; floor set well below
    // that so ordinary source growth doesn't make this brittle.
    assert!(
        files_scanned > 100,
        "expected to scan over 100 production .rs files under crates/*/src/, got {files_scanned} \
         — the file walk is probably broken (wrong root, or crates/ not found)"
    );
    let total_hits: usize = found.values().sum();
    assert!(
        total_hits > 40,
        "expected over 40 total store_dir()/db_path()/embeddings_db_path() occurrences across \
         the workspace, got {total_hits} — the scan likely walked the wrong files"
    );

    // The sentinel: WorktreeContext's own canonical resolution point must be
    // among the hits. If the walk or the comment-skip logic is broken in a
    // way that silently drops real production code, this specific,
    // known-present line is what catches it.
    let sentinel_key = (
        "crates/loomweave-core/src/worktree/context.rs".to_owned(),
        "let (repository_store, store_dir_overridden) = store_dir_resolution(&primary_root);"
            .to_owned(),
    );
    assert!(
        found.contains_key(&sentinel_key),
        "sentinel line missing from the scan — WorktreeContext::linked_store's own \
         `store_dir_resolution(&primary_root)` call was not found; the file walk or comment-skip \
         logic is probably broken. Found files: {files_scanned}, found hits: {found:#?}"
    );

    let allowed = allowed_map();

    // Direction 1: every hit found in the source must be in the allowlist,
    // with the SAME count — a new, unclassified call site (or one whose
    // count changed) fails here.
    let mut unexpected = Vec::new();
    for (key, count) in &found {
        match allowed.get(key) {
            Some(expected_count) if expected_count == count => {}
            Some(expected_count) => unexpected.push(format!(
                "{}:{:?} — found {count} occurrence(s), allowlist expects {expected_count}",
                key.0, key.1
            )),
            None => unexpected.push(format!(
                "{}:{:?} — found {count} occurrence(s), NOT in the allowlist at all",
                key.0, key.1
            )),
        }
    }
    assert!(
        unexpected.is_empty(),
        "unclassified or drifted store_dir()/db_path()/embeddings_db_path() call site(s) — \
         classify each one in ALLOWED_HITS (route it, or record why it's exempt) per the \
         worktree-index Task 7 audit:\n{}",
        unexpected.join("\n")
    );

    // Direction 2: every allowlist entry must still be found in the source
    // — a routed/removed call site that isn't pruned from the allowlist
    // would otherwise silently mask a REAL regression elsewhere (advisor's
    // "the allowlist shape can't detect removals" concern).
    let mut stale = Vec::new();
    for (key, expected_count) in &allowed {
        match found.get(key) {
            Some(actual_count) if actual_count == expected_count => {}
            Some(actual_count) => stale.push(format!(
                "{}:{:?} — allowlist expects {expected_count}, source now has {actual_count}",
                key.0, key.1
            )),
            None => stale.push(format!(
                "{}:{:?} — allowlist expects {expected_count}, source no longer has this line \
                 at all",
                key.0, key.1
            )),
        }
    }
    assert!(
        stale.is_empty(),
        "stale allowlist entries no longer matching the source — prune or update them in \
         ALLOWED_HITS (a stale entry can mask a real removal-then-reintroduction elsewhere):\n{}",
        stale.join("\n")
    );
}
