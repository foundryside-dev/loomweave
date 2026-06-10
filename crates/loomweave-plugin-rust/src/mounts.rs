//! `#[path]`-mounted module routing (ADR-049 Amendment 8, clarion-bdb1eccf48).
//!
//! `module_path_for` derives dotted module paths purely from the filesystem,
//! so a file mounted under a different name via `#[path = "…"] mod name;`
//! (tokio's `src/process/unix/mod.rs` mounted as `mod imp;`) routed to the
//! WRONG dotted path — and collided with the inline facade module
//! (`pub(crate) mod unix { use super::imp::*; }`) emitted at the same path
//! from the declaring file. Two producers, one `rust:module:` id, silent
//! intra-run data loss (or a `LMWV-INFRA-PARENT-CONTAINS-MISMATCH` `FailRun`).
//!
//! The fix is a **targeted mount overlay with a filesystem default**:
//! [`discover_mounts`] walks the project once at `initialize` (the same
//! symlink-safe, ADR-050-guarded walk as `symbol_table`), collects every
//! `#[path = "…"] mod name;` declaration, resolves each target file's
//! *logical* dotted path through a memoized fixed point (mounts can chain),
//! and registers the result in a [`ModMounts`] overlay.
//! [`crate::module_path::logical_module_path`] consults the overlay first and
//! falls back to `module_path_for` — so a project with zero `#[path]` mounts
//! routes byte-identically to before.
//!
//! Rules (all adjudicated in the Amendment-8 design review):
//!
//! - **Target resolution** follows rustc's relative-path rule: a file-level
//!   `#[path]` is relative to the declaring file's directory; one inside an
//!   inline `mod` block is relative to the would-be directory of the
//!   inline-module nesting (which starts at the declaring file's directory
//!   for a mod-rs file — `mod.rs`/`lib.rs`/`main.rs` — and at the file's
//!   stem directory for any other file).
//! - **Twin rule (cross-producer):** module-name occurrences are counted per
//!   declaring item list across BOTH inline `mod n { … }` and declaration
//!   `mod n;` forms. A mount whose name is a twin appends the normalised
//!   `@cfg(...)` discriminant ([`crate::qualname::cfg_discriminant`]), so
//!   tokio's `#[cfg(unix)] mod imp;` / `#[cfg(windows)] mod imp;` pair splits
//!   into `…imp@cfg(unix)` / `…imp@cfg(windows)`.
//! - **Subtree:** a `<dir>/mod.rs` target registers `<dir>/` as a logical
//!   prefix (children rewrite under the mount); an `x.rs` target registers
//!   the exact file plus `<target_dir>/x/` for its child directory.
//! - **One target, two mounts (R5):** deterministic pick — first by sorted
//!   (declaring-file relative path, byte offset); the host's
//!   first-claim-wins machinery stays as backstop.
//! - **Out-of-src targets (R6):** a mount whose target falls outside the
//!   declaring crate's `src/` scope (what `emittable_scope` would reject) is
//!   ignored — the target routes by filesystem. Documented limitation.
//! - **Cycles:** a mount participating in a resolution cycle is dropped (its
//!   target falls back to the filesystem route); resolution always
//!   terminates.
//! - **Macro-wrapped mounts are invisible by dialect rule:** a `#[path]`
//!   inside an unexpanded macro invocation does not exist for either
//!   producer (syn does not expand macros, and the second producer must be
//!   able to reproduce the route from one file). NO macro expansion is ever
//!   attempted — the target routes by filesystem.
//!
//! Known limitation: an inline-`mod`-nested mount composes its logical path
//! from the *bare* inline module names; if such an intermediate inline mod is
//! itself a cfg twin (and so carries `@cfg` in its own entity path), the
//! mounted file's dotted path will not include that suffix. Ids stay unique;
//! only dotted-path agreement with the twin facade is affected (the same
//! pre-existing cfg-twin resolver-mismatch class as the twin-mount rule
//! itself).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use syn::Item;

use crate::crate_roots::CrateRoots;
use crate::extract::cfg_suffix;
use crate::module_path::module_path_for;
use crate::scope::crate_src_scope;
use crate::spans::source_range_of;

/// The `#[path]` mount overlay: an exact-file map plus directory prefixes for
/// mounted subtrees. Built once per project at `initialize` by
/// [`discover_mounts`]; consulted by
/// [`crate::module_path::logical_module_path`] before the filesystem default.
#[derive(Default)]
pub struct ModMounts {
    /// Exact mounted file → its logical dotted module path.
    by_file: BTreeMap<PathBuf, String>,
    /// Mounted subtree directory → the logical dotted path of its root
    /// module; files under the directory rewrite their remaining components
    /// onto it (longest prefix wins).
    dir_prefixes: Vec<(PathBuf, String)>,
}

impl ModMounts {
    /// An empty overlay (every lookup misses — pure filesystem routing).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The logical dotted module path of `file` under the mount overlay, or
    /// `None` when no mount covers it (caller falls back to
    /// [`module_path_for`]): exact-file lookup first, then the longest
    /// matching subtree prefix with the remaining components rewritten onto
    /// the mount's logical path (a trailing `mod` stem contributes no
    /// segment, exactly as in the filesystem route).
    #[must_use]
    pub fn logical_path_for(&self, file: &Path) -> Option<String> {
        if let Some(logical) = self.by_file.get(file) {
            return Some(logical.clone());
        }
        let (dir, logical) = self
            .dir_prefixes
            .iter()
            .filter(|(dir, _)| file.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_os_str().len())?;
        let rel = file.strip_prefix(dir).ok()?;
        Some(rewrite_remainder(logical, rel))
    }
}

/// Append `rel`'s components onto a mount's logical dotted path: directories
/// verbatim, the final component by file stem, and a trailing `mod` stem
/// contributing no segment (mirrors [`module_path_for`]'s collapse rule).
fn rewrite_remainder(base: &str, rel: &Path) -> String {
    let mut out = base.to_owned();
    let comps: Vec<_> = rel.components().collect();
    for (i, comp) in comps.iter().enumerate() {
        let part = comp.as_os_str().to_string_lossy();
        if i == comps.len() - 1 {
            let stem = Path::new(part.as_ref())
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            if stem != "mod" {
                out.push('.');
                out.push_str(&stem);
            }
        } else {
            out.push('.');
            out.push_str(&part);
        }
    }
    out
}

/// Walk `project_root` and build the `#[path]` mount overlay. Reuses the
/// `symbol_table` walk discipline (symlink-safe, ignored dirs, ADR-050
/// pre-parse guards with the same silent-skip semantics) plus a cheap byte
/// gate: only files whose bytes can contain a `#[path` attribute (tolerating
/// whitespace between `#`, `[`, and `path`) are syn-parsed at all.
#[must_use]
pub fn discover_mounts(project_root: &Path, roots: &CrateRoots) -> ModMounts {
    let mut raw: Vec<RawMount> = Vec::new();
    for file in crate::symbol_table::walk_rs_files(project_root) {
        // A declaring file outside the emittable crate scope (tests/, benches/,
        // a redundant main.rs, …) contributes no mounts, exactly as it
        // contributes no entities.
        let Some((_, src_root)) = crate_src_scope(roots, &file) else {
            continue;
        };
        // ADR-050 pre-parse guards, same silent-skip semantics as the
        // symbol-table walk: an oversize file is skipped without reading it; a
        // depth/prefix bomb before it can overflow the parser stack.
        if crate::parse_guard::check_file_size(&file).is_err() {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !may_contain_path_attr(&src) {
            continue; // the cheap byte gate — no syn parse for #[path]-free files
        }
        if crate::parse_guard::scan_source(&src).is_err() {
            continue;
        }
        // Parse AND collect on the pinned 16 MiB stack (ADR-050): syn has no
        // recursion limit, this walk runs inside the host's `initialize`
        // handshake, and `syn::File` is `!Send` — so the whole
        // parse-and-collect stays on the dedicated thread and only the
        // (Send) collected mounts cross back.
        let collected = crate::parse_guard::with_pinned_stack(|| {
            syn::parse_file(&src).ok().map(|ast| {
                let mut out = Vec::new();
                collect_file_mounts(&file, project_root, &src_root, roots, &ast, &mut out);
                out
            })
        });
        let Some(mut collected) = collected else {
            continue; // parse error: same silent skip as the table walk
        };
        raw.append(&mut collected);
    }
    finalize_mounts(raw, roots)
}

/// One collected `#[path = "…"] mod name;` declaration, pre-resolution.
struct RawMount {
    /// R5 deterministic ordering key: (declaring-file path relative to the
    /// project root, byte offset of the declaration).
    sort_key: (PathBuf, usize),
    /// The file holding the declaration (its logical path is the mount's
    /// logical parent).
    declaring_file: PathBuf,
    /// Inline-`mod` names enclosing the declaration, outermost first (empty
    /// for a file-level declaration). Composed into the logical path between
    /// the declaring file's path and the mount name.
    inline_prefix: Vec<String>,
    /// The declared module name (`imp` in `mod imp;`).
    name: String,
    /// The twin `@cfg(...)` discriminant, present iff the name is shared by
    /// another `mod` (inline or decl) in the same item list AND the decl
    /// carries a `#[cfg]`.
    suffix: Option<String>,
    /// The mounted file, lexically normalised, absolute.
    target: PathBuf,
}

/// Cheap pre-parse gate: could these bytes contain a `#[path …]` attribute?
/// Tolerates arbitrary whitespace between `#`, `[`, and `path` — `# [ path`
/// is legal Rust even though rustfmt always renders `#[path` — so the gate
/// can under-approximate parses but never miss a mount. A false positive
/// (e.g. `#[path…]` inside a string literal) only costs one syn parse.
fn may_contain_path_attr(src: &str) -> bool {
    let bytes = src.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'#' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'[' {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes[j..].starts_with(b"path") {
            return true;
        }
    }
    false
}

/// Collect every `#[path]` mount declared in one parsed file, walking the
/// top-level item list AND inline `mod` blocks (rustc resolves `#[path]`
/// inside inline mods against the would-be directory of the nesting).
fn collect_file_mounts(
    file: &Path,
    project_root: &Path,
    src_root: &Path,
    roots: &CrateRoots,
    ast: &syn::File,
    out: &mut Vec<RawMount>,
) {
    let file_dir = file.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
    // rustc's mod-rs / non-mod-rs distinction: inline-nested `#[path]` targets
    // start at the file's own directory for `mod.rs`/`lib.rs`/`main.rs`
    // ("mod-rs" files; the crate roots this plugin models are src/lib.rs and
    // src/main.rs), and at the file's STEM directory for any other file.
    let is_mod_rs = matches!(
        file.file_name().and_then(|n| n.to_str()),
        Some("mod.rs" | "lib.rs" | "main.rs")
    );
    let inline_base = if is_mod_rs {
        file_dir.clone()
    } else {
        file.file_stem()
            .map_or_else(|| file_dir.clone(), |stem| file_dir.join(stem))
    };
    let rel = file
        .strip_prefix(project_root)
        .unwrap_or(file)
        .to_path_buf();
    let cx = CollectCx {
        file,
        file_dir: &file_dir,
        inline_base: &inline_base,
        src_root,
        roots,
        rel: &rel,
    };
    collect_in_items(&ast.items, &[], &cx, out);
}

/// Shared per-file collection context (one lifetime, threaded by reference).
struct CollectCx<'a> {
    file: &'a Path,
    /// Base directory for FILE-LEVEL `#[path]` targets (the declaring file's
    /// directory — rustc's rule for decls not inside inline blocks).
    file_dir: &'a Path,
    /// Base directory for INLINE-NESTED targets before the inline-mod
    /// components are appended (mod-rs: the file's dir; else its stem dir).
    inline_base: &'a Path,
    src_root: &'a Path,
    roots: &'a CrateRoots,
    /// Declaring file relative to the project root (the R5 sort key's first
    /// component).
    rel: &'a Path,
}

fn collect_in_items(
    items: &[Item],
    inline_names: &[String],
    cx: &CollectCx<'_>,
    out: &mut Vec<RawMount>,
) {
    // TWIN KEY (cross-producer rule): module-name occurrences in THIS item
    // list across BOTH inline `mod n { … }` and decl `mod n;` forms — the
    // same counting `extract.rs`'s module twin counter performs, so the
    // mounted file's @cfg suffix and an inline twin's @cfg suffix are decided
    // by one rule.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in items {
        if let Item::Mod(m) = item {
            *counts.entry(m.ident.to_string()).or_insert(0) += 1;
        }
    }
    for item in items {
        let Item::Mod(m) = item else { continue };
        if let Some((_, inner)) = &m.content {
            // Inline `mod n { … }`: recurse with the nesting recorded (its
            // decls resolve against the would-be directory of the nesting).
            let mut names = inline_names.to_vec();
            names.push(m.ident.to_string());
            collect_in_items(inner, &names, cx, out);
            continue;
        }
        // Declaration `mod n;` — a mount iff it carries `#[path = "…"]`.
        let Some(lit) = path_attr_literal(&m.attrs) else {
            continue;
        };
        let base = if inline_names.is_empty() {
            cx.file_dir.to_path_buf()
        } else {
            let mut b = cx.inline_base.to_path_buf();
            for n in inline_names {
                b.push(n);
            }
            b
        };
        let target = normalize_lexically(&base.join(&lit));
        // R6: a target outside the declaring crate's src/ scope (whatever
        // `emittable_scope` would reject, or another crate's tree) registers
        // no mount — the file routes by filesystem. Documented limitation.
        let Some((_, target_src)) = crate_src_scope(cx.roots, &target) else {
            continue;
        };
        if target_src != cx.src_root {
            continue;
        }
        let name = m.ident.to_string();
        let suffix = if counts.get(&name).copied().unwrap_or(0) > 1 {
            cfg_suffix(&m.attrs)
        } else {
            None
        };
        let byte = usize::try_from(source_range_of(item).byte_start).unwrap_or(0);
        out.push(RawMount {
            sort_key: (cx.rel.to_path_buf(), byte),
            declaring_file: cx.file.to_path_buf(),
            inline_prefix: inline_names.to_vec(),
            name,
            suffix,
            target,
        });
    }
}

/// The string literal of an outer `#[path = "…"]` attribute, if any.
fn path_attr_literal(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if let syn::Meta::NameValue(nv) = &attr.meta
            && nv.path.is_ident("path")
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            Some(s.value())
        } else {
            None
        }
    })
}

/// Lexical `.`/`..` normalisation (no filesystem access; symlinks are the
/// walk's concern, not the literal's).
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The subtree directory a resolved mount claims (STATIC — derivable from the
/// target path alone): `<dir>/mod.rs` claims `<dir>/`; `x.rs` claims
/// `<target_dir>/x/` (rustc's non-mod-rs child rule).
fn static_dir(target: &Path) -> PathBuf {
    let parent = target
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    if target.file_name().and_then(|n| n.to_str()) == Some("mod.rs") {
        parent
    } else {
        target
            .file_stem()
            .map_or_else(|| parent.clone(), |stem| parent.join(stem))
    }
}

/// Resolve the collected mounts to logical dotted paths (memoised fixed
/// point) and build the final overlay. R5 first-claim-wins per target;
/// cycle participants are dropped (their targets route by filesystem).
fn finalize_mounts(mut raw: Vec<RawMount>, roots: &CrateRoots) -> ModMounts {
    // R5: deterministic pick — first by sorted (declaring-file relative path,
    // byte offset); later claims on the same target are discarded (the host's
    // first-claim-wins machinery stays as backstop for shapes the overlay
    // cannot split).
    raw.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    let mut by_target: BTreeMap<PathBuf, RawMount> = BTreeMap::new();
    for m in raw {
        by_target.entry(m.target.clone()).or_insert(m);
    }
    // Static subtree dirs, used both for declaring-file dependency edges and
    // for the final prefix registration.
    let dirs: Vec<(PathBuf, PathBuf)> = by_target
        .keys()
        .map(|target| (static_dir(target), target.clone()))
        .collect();
    let dropped = cyclic_targets(&by_target, &dirs);
    let mut resolver = MountResolver {
        by_target: &by_target,
        dirs: &dirs,
        dropped: &dropped,
        roots,
        memo: BTreeMap::new(),
    };
    let mut by_file = BTreeMap::new();
    let mut dir_prefixes = Vec::new();
    for target in by_target.keys() {
        if let Some(logical) = resolver.mount_logical(target) {
            by_file.insert(target.clone(), logical.clone());
            dir_prefixes.push((static_dir(target), logical));
        }
    }
    ModMounts {
        by_file,
        dir_prefixes,
    }
}

/// Each mount's logical parent depends on AT MOST one other mount (the one
/// covering its declaring file, exactly or via a subtree dir) — a functional
/// graph, so cycles are simple loops. Walk each dependency chain with
/// three-state colouring and mark every member of a detected loop as dropped:
/// resolution then always terminates and every cycle participant falls back
/// to the filesystem route ("cycle → drop the mount, fall back").
fn cyclic_targets(
    by_target: &BTreeMap<PathBuf, RawMount>,
    dirs: &[(PathBuf, PathBuf)],
) -> BTreeSet<PathBuf> {
    let dep_of = |target: &Path| -> Option<PathBuf> {
        let d = &by_target[target].declaring_file;
        if by_target.contains_key(d.as_path()) {
            return Some(d.clone());
        }
        dirs.iter()
            .filter(|(dir, _)| d.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_os_str().len())
            .map(|(_, owner)| owner.clone())
    };
    let mut dropped: BTreeSet<PathBuf> = BTreeSet::new();
    // 1 = on the chain currently being walked, 2 = fully classified.
    let mut state: BTreeMap<PathBuf, u8> = BTreeMap::new();
    for start in by_target.keys() {
        if state.contains_key(start) {
            continue;
        }
        let mut chain: Vec<PathBuf> = Vec::new();
        let mut cur = start.clone();
        loop {
            match state.get(&cur).copied() {
                Some(1) => {
                    // A loop within the current chain: drop it whole.
                    let pos = chain
                        .iter()
                        .position(|c| *c == cur)
                        .expect("state 1 implies membership in the current chain");
                    for c in &chain[pos..] {
                        dropped.insert(c.clone());
                    }
                    break;
                }
                Some(2) => break, // joins an already-classified chain
                _ => {
                    state.insert(cur.clone(), 1);
                    chain.push(cur.clone());
                    match dep_of(&cur) {
                        Some(next) => cur = next,
                        None => break,
                    }
                }
            }
        }
        for c in chain {
            state.insert(c, 2);
        }
    }
    dropped
}

/// Memoised logical-path resolution over the (post-drop, acyclic) mount set.
struct MountResolver<'a> {
    by_target: &'a BTreeMap<PathBuf, RawMount>,
    dirs: &'a [(PathBuf, PathBuf)],
    dropped: &'a BTreeSet<PathBuf>,
    roots: &'a CrateRoots,
    /// target → resolved logical (`None` = mount dropped).
    memo: BTreeMap<PathBuf, Option<String>>,
}

impl MountResolver<'_> {
    /// The logical dotted path a mount assigns its target, or `None` when the
    /// mount is dropped (cycle, or a declaring file that lost crate scope).
    fn mount_logical(&mut self, target: &Path) -> Option<String> {
        if let Some(memoised) = self.memo.get(target) {
            return memoised.clone();
        }
        let result = if self.dropped.contains(target) {
            None
        } else {
            let rec = &self.by_target[target];
            let declaring = rec.declaring_file.clone();
            self.file_logical(&declaring).map(|parent| {
                let mut q = parent;
                for n in &rec.inline_prefix {
                    q.push('.');
                    q.push_str(n);
                }
                q.push('.');
                q.push_str(&rec.name);
                if let Some(suffix) = &rec.suffix {
                    q.push_str(suffix);
                }
                q
            })
        };
        self.memo.insert(target.to_path_buf(), result.clone());
        result
    }

    /// The logical dotted path of an arbitrary file DURING resolution:
    /// its own mount if it is a (kept) target, a kept mount's subtree rewrite
    /// if one covers it, else the filesystem route. Mirrors what
    /// [`ModMounts::logical_path_for`] + the filesystem fallback compute once
    /// the overlay is final.
    fn file_logical(&mut self, file: &Path) -> Option<String> {
        if self.by_target.contains_key(file) {
            if let Some(logical) = self.mount_logical(file) {
                return Some(logical);
            }
            return self.fs_logical(file); // dropped mount → filesystem
        }
        let owner = self
            .dirs
            .iter()
            .filter(|(dir, _)| file.starts_with(dir))
            .max_by_key(|(dir, _)| dir.as_os_str().len())
            .map(|(dir, target)| (dir.clone(), target.clone()));
        if let Some((dir, target)) = owner {
            if let Some(base) = self.mount_logical(&target)
                && let Ok(rel) = file.strip_prefix(&dir)
            {
                return Some(rewrite_remainder(&base, rel));
            }
            return self.fs_logical(file); // owning mount dropped → filesystem
        }
        self.fs_logical(file)
    }

    /// The pure filesystem route (`None` when the file has no emittable crate
    /// scope — a mount declared from such a file was already discarded at
    /// collection, so this is defensive).
    fn fs_logical(&self, file: &Path) -> Option<String> {
        let (crate_name, src_root) = crate_src_scope(self.roots, file)?;
        Some(module_path_for(&crate_name, &src_root, file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crate_roots::discover_crate_roots;
    use crate::module_path::logical_module_path;
    use std::fs;

    /// Materialise a one-crate project (`[package] name = "k"`) from
    /// `(relative_path, source)` pairs and return `(tempdir, mounts)`.
    fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, ModMounts) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"k\"\n").unwrap();
        for (rel, src) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, src).unwrap();
        }
        let roots = discover_crate_roots(root);
        let mounts = discover_mounts(root, &roots);
        (tmp, mounts)
    }

    /// The mount-aware route of `rel` under the test project.
    fn route(tmp: &tempfile::TempDir, mounts: &ModMounts, rel: &str) -> String {
        let root = tmp.path();
        logical_module_path("k", &root.join("src"), &root.join(rel), mounts)
    }

    #[test]
    fn file_level_target_is_declaring_file_relative() {
        // Top-level decls resolve relative to the DECLARING file's directory —
        // for the crate root (src/lib.rs) and for a nested file (src/a/b.rs)
        // alike (the rustc rule's first table).
        let (tmp, mounts) = project(&[
            (
                "src/lib.rs",
                "mod a;\n#[path = \"renamed.rs\"]\nmod logical;\n",
            ),
            ("src/renamed.rs", "pub fn r() {}\n"),
            ("src/a/b.rs", "#[path = \"foo.rs\"]\nmod c;\n"),
            ("src/a/foo.rs", "pub fn f() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/renamed.rs"), "k.logical");
        assert_eq!(route(&tmp, &mounts, "src/a/foo.rs"), "k.a.b.c");
    }

    #[test]
    fn mod_rs_target_registers_its_directory_as_a_prefix() {
        let (tmp, mounts) = project(&[
            (
                "src/lib.rs",
                "#[path = \"engine_v2/mod.rs\"]\nmod engine;\n",
            ),
            ("src/engine_v2/mod.rs", "pub mod worker;\npub mod sub;\n"),
            ("src/engine_v2/worker.rs", "pub fn run() {}\n"),
            ("src/engine_v2/sub/mod.rs", "pub fn deep() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/engine_v2/mod.rs"), "k.engine");
        assert_eq!(
            route(&tmp, &mounts, "src/engine_v2/worker.rs"),
            "k.engine.worker"
        );
        // Trailing `mod` stem collapses inside the rewritten remainder too.
        assert_eq!(
            route(&tmp, &mounts, "src/engine_v2/sub/mod.rs"),
            "k.engine.sub"
        );
    }

    #[test]
    fn non_mod_rs_target_registers_file_and_stem_child_dir() {
        let (tmp, mounts) = project(&[
            ("src/lib.rs", "#[path = \"compat_v3.rs\"]\nmod compat;\n"),
            ("src/compat_v3.rs", "pub mod extra;\n"),
            ("src/compat_v3/extra.rs", "pub fn e() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/compat_v3.rs"), "k.compat");
        assert_eq!(
            route(&tmp, &mounts, "src/compat_v3/extra.rs"),
            "k.compat.extra"
        );
    }

    #[test]
    fn twin_mounts_split_by_cfg_discriminant() {
        // The tokio shape: two `mod imp;` decls are twins (the name count
        // spans BOTH decl and inline forms), so each mount appends its
        // normalised @cfg(...) suffix.
        let (tmp, mounts) = project(&[
            (
                "src/lib.rs",
                "#[cfg(unix)]\n#[path = \"unix/mod.rs\"]\nmod imp;\n\
                 #[cfg(windows)]\n#[path = \"windows/mod.rs\"]\nmod imp;\n\
                 #[cfg(unix)]\nmod unix {\n    pub(crate) use super::imp::*;\n}\n",
            ),
            ("src/unix/mod.rs", "pub fn spawn() {}\n"),
            ("src/windows/mod.rs", "pub fn spawn() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/unix/mod.rs"), "k.imp@cfg(unix)");
        assert_eq!(
            route(&tmp, &mounts, "src/windows/mod.rs"),
            "k.imp@cfg(windows)"
        );
    }

    #[test]
    fn lone_cfg_mount_takes_no_suffix() {
        // The @cfg discriminant is twin-gated (ADR-049 §3 discipline): a lone
        // `#[cfg(unix)] #[path] mod imp;` with no same-name sibling keeps the
        // bare mounted path.
        let (tmp, mounts) = project(&[
            (
                "src/lib.rs",
                "#[cfg(unix)]\n#[path = \"unix/mod.rs\"]\nmod imp;\n",
            ),
            ("src/unix/mod.rs", "pub fn spawn() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/unix/mod.rs"), "k.imp");
    }

    #[test]
    fn chained_mount_resolves_through_the_fixed_point() {
        let (tmp, mounts) = project(&[
            ("src/lib.rs", "#[path = \"stage_a.rs\"]\nmod first;\n"),
            ("src/stage_a.rs", "#[path = \"stage_b.rs\"]\nmod second;\n"),
            ("src/stage_b.rs", "pub fn b() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/stage_a.rs"), "k.first");
        assert_eq!(route(&tmp, &mounts, "src/stage_b.rs"), "k.first.second");
    }

    #[test]
    fn mount_cycle_drops_the_mounts_and_terminates() {
        // a.rs and b.rs mount each other; c.rs mounts itself. Resolution must
        // TERMINATE and every cycle participant falls back to its filesystem
        // route (the mounts are dropped).
        let (tmp, mounts) = project(&[
            ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
            ("src/a.rs", "#[path = \"b.rs\"]\nmod y;\n"),
            ("src/b.rs", "#[path = \"a.rs\"]\nmod z;\n"),
            ("src/c.rs", "#[path = \"c.rs\"]\nmod selfie;\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/a.rs"), "k.a");
        assert_eq!(route(&tmp, &mounts, "src/b.rs"), "k.b");
        assert_eq!(route(&tmp, &mounts, "src/c.rs"), "k.c");
    }

    #[test]
    fn macro_wrapped_mount_is_invisible() {
        // A #[path] inside an unexpanded macro invocation does not exist for
        // any producer (no macro expansion, by dialect rule) — the target
        // routes by filesystem.
        let (tmp, mounts) = project(&[
            (
                "src/lib.rs",
                "macro_rules! mount {\n    () => {\n        #[path = \"hidden_impl.rs\"]\n        mod hidden;\n    };\n}\nmount!();\n",
            ),
            ("src/hidden_impl.rs", "pub fn h() {}\n"),
        ]);
        assert_eq!(
            mounts.logical_path_for(&tmp.path().join("src/hidden_impl.rs")),
            None
        );
        assert_eq!(route(&tmp, &mounts, "src/hidden_impl.rs"), "k.hidden_impl");
    }

    #[test]
    fn out_of_src_target_is_ignored() {
        // R6: a target outside the declaring crate's src/ scope (whatever
        // emittable_scope would reject) registers NO mount.
        let (tmp, mounts) = project(&[
            ("src/lib.rs", "#[path = \"../outside.rs\"]\nmod out;\n"),
            ("outside.rs", "pub fn o() {}\n"),
        ]);
        assert_eq!(
            mounts.logical_path_for(&tmp.path().join("outside.rs")),
            None
        );
    }

    #[test]
    fn one_file_two_mounts_picks_the_first_deterministically() {
        // R5: a target claimed by several mounts takes the FIRST by sorted
        // (declaring-file relative path, byte offset) — both across files
        // ("src/lib.rs" sorts before "src/zeta.rs") and within one file
        // (lower byte offset wins).
        let (tmp, mounts) = project(&[
            (
                "src/lib.rs",
                "#[path = \"shared_impl.rs\"]\nmod alpha;\n\
                 #[path = \"shared2_impl.rs\"]\nmod first_in_file;\n\
                 #[path = \"shared2_impl.rs\"]\nmod second_in_file;\n\
                 mod zeta;\n",
            ),
            ("src/zeta.rs", "#[path = \"shared_impl.rs\"]\nmod beta;\n"),
            ("src/shared_impl.rs", "pub fn s() {}\n"),
            ("src/shared2_impl.rs", "pub fn s2() {}\n"),
        ]);
        assert_eq!(route(&tmp, &mounts, "src/shared_impl.rs"), "k.alpha");
        assert_eq!(
            route(&tmp, &mounts, "src/shared2_impl.rs"),
            "k.first_in_file"
        );
    }
}
