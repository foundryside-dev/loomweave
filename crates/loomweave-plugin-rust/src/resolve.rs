//! Phase 1b path resolver. Turns a `use`/trait path into a unique in-project
//! entity id, else Ambiguous (globs/aliases/re-exports — H5) or External
//! (out of project). NEVER fabricates a Resolved target; the host-side
//! seen-entity-set gate (Task 8) is the second line that drops anything the
//! host did not actually store.
use crate::symbol_table::SymbolTable;

/// The outcome of resolving one `use`/trait path against the symbol table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The path maps to EXACTLY ONE in-project entity — a real edge target.
    Resolved(String),
    /// Cannot be promoted to Resolved from syntax alone (glob / multi-kind), but
    /// carries a REAL in-project candidate id — never null, because `edges.to_id`
    /// is `NOT NULL`. A glob points at the in-project module; a multi-kind
    /// collision points at the first id by sorted order (deterministic).
    Ambiguous(String),
    /// The path resolves out of project (or to nothing in-project).
    External,
}

/// Resolves `use`/trait paths against a built [`SymbolTable`]'s reverse index.
pub struct Resolver<'t> {
    table: &'t SymbolTable,
}

impl<'t> Resolver<'t> {
    /// Wrap a built symbol table for path resolution.
    #[must_use]
    pub fn new(table: &'t SymbolTable) -> Self {
        Self { table }
    }

    /// Resolve a `use`/path string from `from_crate`. A glob over an in-project
    /// module → Ambiguous(module id); a glob over an external module → External;
    /// a path resolving to exactly one in-project id → Resolved; a same-qualname
    /// multi-kind collision → Ambiguous(first id); otherwise External.
    #[must_use]
    pub fn resolve_use_path(&self, from_crate: &str, path: &str) -> Resolution {
        if let Some(prefix) = path.strip_suffix("::*") {
            let dotted = normalize_path(from_crate, prefix);
            return self
                .table
                .ids_for_qualname(&dotted)
                .iter()
                .find(|id| id.starts_with("rust:module:"))
                .map_or(Resolution::External, |m| Resolution::Ambiguous(m.clone()));
        }
        self.resolve_non_glob(from_crate, path, |_| true)
    }

    /// Resolve a trait path from `from_crate`, keeping only `rust:trait:` ids.
    #[must_use]
    pub fn resolve_trait_path(&self, from_crate: &str, path: &str) -> Resolution {
        self.resolve_non_glob(from_crate, path, |id| id.starts_with("rust:trait:"))
    }

    /// Two-attempt resolution shared by `resolve_use_path` (non-glob) and
    /// `resolve_trait_path`:
    /// 1. **As-is** — `normalize_path`'s output, where the leading segment is a
    ///    crate name (or `crate`/`self` mapped to `from_crate`). A real
    ///    crate-qualified path resolves here.
    /// 2. **Crate-root-relative fallback** — only if attempt 1's RAW candidate
    ///    slice is empty, prepend `from_crate` and retry. A BARE name in
    ///    `impl Tr for Foo` or a `use` of a crate-root item is crate-root-relative,
    ///    so `Tr` → `c_crate.Tr`.
    ///
    /// Emptiness is checked on the raw slice (before the kind filter) — strictly
    /// more conservative for H5, and the recommended "try-1-then-try-2-on-empty"
    /// rule that needs no crate-name set: a crate-qualified path resolves at
    /// attempt 1, a bare name at attempt 2, and a genuinely external path
    /// (`serde::Serialize`) misses both. Both attempts route through
    /// `resolve_ids`, so >1 in-project candidate is `Ambiguous(first-sorted)`,
    /// never a guessed `Resolved` (H5). The approximation: a bare name is assumed
    /// crate-root-or-unique-in-project; the host seen-set gate (Task 8) is the
    /// second line of defense.
    fn resolve_non_glob(
        &self,
        from_crate: &str,
        path: &str,
        keep: impl Fn(&str) -> bool,
    ) -> Resolution {
        let dotted = normalize_path(from_crate, path);
        let mut ids = self.table.ids_for_qualname(&dotted);
        if ids.is_empty() {
            let fallback = format!("{from_crate}.{dotted}");
            ids = self.table.ids_for_qualname(&fallback);
        }
        resolve_ids(ids, keep)
    }
}

/// Shared id-slice → Resolution: 0 → External, exactly 1 (after filter) →
/// Resolved, >1 → Ambiguous(first by sorted order — deterministic). Inputs from
/// `ids_for_qualname` are already sorted; sort defensively.
fn resolve_ids(ids: &[String], keep: impl Fn(&str) -> bool) -> Resolution {
    let mut matched: Vec<&String> = ids.iter().filter(|id| keep(id)).collect();
    matched.sort();
    match matched.as_slice() {
        [] => Resolution::External,
        [one] => Resolution::Resolved((*one).clone()),
        [first, ..] => Resolution::Ambiguous((*first).clone()),
    }
}

/// `crate::a::B` / `self::B` / `super::B` / `c_crate::a::B` → `c_crate.a.B`.
/// A leading segment that is not `crate`/`self`/`super`/`from_crate` and not a
/// known in-project crate stays as-is (the caller's table lookup then misses →
/// External). Aliases (`use a::B as C`) are handled by the caller (it passes the
/// real path, not the alias).
///
/// `super::` full handling needs the defining module path of the `use`; for
/// 1b-minimal it is resolved conservatively (the `super` segment is dropped,
/// collapsing to a crate-root-relative path). A miss → External/Ambiguous is
/// fine; the H5 invariant is that this MUST NEVER produce a WRONG Resolved.
fn normalize_path(from_crate: &str, path: &str) -> String {
    let segs: Vec<&str> = path.split("::").collect();
    let mut out: Vec<String> = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        match (i, *s) {
            (0, "crate" | "self") => out.push(from_crate.to_owned()),
            (0, _) => out.push((*s).to_owned()),
            (_, "super") => { /* conservative: treat as crate-root in 1b-min */ }
            _ => out.push((*s).to_owned()),
        }
    }
    out.join(".")
}
