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

    /// Resolve a CALL path (the `func` of an `ExprCall` whose callee is a
    /// `Expr::Path`) from `from_crate`, keeping only `rust:function:` ids — the
    /// only id-kind a call edge may target (every callable is a `function`,
    /// ADR-049). Mirrors [`Self::resolve_trait_path`]'s shape: a uniquely-resolved
    /// in-project function → Resolved; a >1-function-id collision →
    /// Ambiguous(first by sorted order); 0 → External.
    ///
    /// A `Type::assoc` call (`Foo::new()`) misses here on purpose: the method's
    /// real qualname carries an `.impl#<>` / `.impl[Trait]` segment the call
    /// syntax lacks, so the exact-qualname lookup returns nothing → External (the
    /// caller records an `UnresolvedCallSite`). Resolving assoc calls is a
    /// deliberate fast-follow, NOT this MVP — no impl-suffix scan is attempted.
    #[must_use]
    pub fn resolve_call_path(&self, from_crate: &str, path: &str) -> Resolution {
        self.resolve_non_glob(from_crate, path, |id| id.starts_with("rust:function:"))
    }

    /// Two-attempt resolution shared by `resolve_use_path` (non-glob) and
    /// `resolve_trait_path`:
    /// 1. **As-is** — `normalize_path`'s output, where the leading segment is a
    ///    crate name (or `crate`/`self` mapped to `from_crate`). A real
    ///    crate-qualified path resolves here.
    /// 2. **Crate-root-relative fallback** — only if attempt 1's RAW candidate
    ///    slice is empty AND the ORIGINAL path is a BARE single segment (contains
    ///    no `::`), prepend `from_crate` and retry. A BARE name in
    ///    `impl Tr for Foo` or a `use` of a crate-root item is crate-root-relative,
    ///    so `Tr` → `c_crate.Tr`.
    ///
    /// The single-segment gate is an H5 guard: a multi-segment path
    /// (`serde::Serialize`) that misses attempt 1 must stay External, NEVER be
    /// re-prefixed with `from_crate`. Without the gate, an in-project module that
    /// shadows an external crate name (an in-project `mod serde` defining a
    /// `Serialize`) would WRONG-resolve a `use serde::Serialize` that means the
    /// EXTERNAL crate to `c_crate.serde.Serialize`. Gating on the bare path keeps
    /// only the legitimate crate-root-relative bare names; a multi-segment
    /// crate-root-relative import (rare) now under-resolves to External, which is
    /// H5-safe and acceptable for 1b.
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
        if ids.is_empty() && !path.contains("::") {
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

/// `crate::a::B` / `self::B` / `c_crate::a::B` → `c_crate.a.B`.
/// A leading segment that is not `crate`/`self`/`super`/`from_crate` and not a
/// known in-project crate stays as-is (the caller's table lookup then misses →
/// External). Aliases (`use a::B as C`) are handled by the caller (it passes the
/// real path, not the alias).
///
/// A leading `super::` path is a DELIBERATE 1b deferral: it returns a string that
/// deterministically MISSES the table (the reserved word `super` can never appear
/// in a stored qualname), so super-relative paths resolve to External. This is an
/// H5-safe under-resolution. Full `super::` handling needs the defining-module
/// path of the `use` site, which 1b does not thread through — and a conservative
/// crate-root collapse (dropping `super`) would INTRODUCE an H5 violation:
/// `super::a::S` from module `c_crate.m.n` means `c_crate.m.a.S`, but collapsing
/// to `c_crate.a.S` would WRONG-resolve if `c_crate.a.S` exists. Real super
/// handling is Phase 2; until then, External is the correct, intentional answer.
fn normalize_path(from_crate: &str, path: &str) -> String {
    let segs: Vec<&str> = path.split("::").collect();
    // Leading `super`: deliberate 1b deferral. Keep the `super` token verbatim so
    // the lookup deterministically misses → External. Do NOT collapse to a
    // crate-root-relative path (H5: would wrong-resolve a same-tail crate-root
    // entity). Full super-relative resolution is Phase 2.
    if segs.first() == Some(&"super") {
        return segs.join(".");
    }
    let mut out: Vec<String> = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        match (i, *s) {
            (0, "crate" | "self") => out.push(from_crate.to_owned()),
            _ => out.push((*s).to_owned()),
        }
    }
    out.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Ambiguous` arm of `resolve_call_path` is reachable only when a
    /// qualname maps to >1 surviving `rust:function:` id. The project symbol
    /// table can NEVER produce that through `build_symbol_table` — a distinct
    /// function id requires a distinct qualname (id = `rust:function:<qualname>`),
    /// so two same-qualname free fns collide to ONE id (the dup is recorded in
    /// `duplicate_ids`, not added twice to the reverse index), and the only
    /// multi-id qualname is multi-*kind* (struct + fn), which the
    /// `rust:function:` filter collapses to one → Resolved. So the spec'd
    /// "two free fns" integration test is architecturally infeasible. This unit
    /// test exercises the collapse logic the `Ambiguous` branch depends on
    /// directly: >1 surviving function id → `Ambiguous(first-by-sorted-order)`.
    #[test]
    fn resolve_ids_collapses_multiple_function_ids_to_ambiguous_first_sorted() {
        let ids = vec![
            "rust:function:c_crate.b".to_owned(),
            "rust:function:c_crate.a".to_owned(),
        ];
        let res = resolve_ids(&ids, |id| id.starts_with("rust:function:"));
        assert_eq!(
            res,
            Resolution::Ambiguous("rust:function:c_crate.a".to_owned()),
            "two surviving function ids collapse to Ambiguous(first by sorted order)",
        );
    }

    #[test]
    fn resolve_ids_single_surviving_id_is_resolved() {
        let ids = vec![
            "rust:struct:c_crate.S".to_owned(),
            "rust:function:c_crate.S".to_owned(),
        ];
        // The `rust:function:` filter (as `resolve_call_path` applies) keeps one.
        let res = resolve_ids(&ids, |id| id.starts_with("rust:function:"));
        assert_eq!(
            res,
            Resolution::Resolved("rust:function:c_crate.S".to_owned())
        );
    }

    #[test]
    fn resolve_ids_no_surviving_id_is_external() {
        let ids = vec!["rust:struct:c_crate.S".to_owned()];
        let res = resolve_ids(&ids, |id| id.starts_with("rust:function:"));
        assert_eq!(res, Resolution::External);
    }
}
