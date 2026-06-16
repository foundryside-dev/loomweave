pub mod sub;
// In-project `use` -> a Resolved `imports` edge
// (rust:module:e2e_crate -> rust:module:e2e_crate.sub). `crate::sub` normalizes
// to `e2e_crate.sub`, which the symbol table holds as a module.
//
// NOTE (host finding, see analyze_e2e.rs): the import deliberately targets the
// MODULE `sub`, not the function `sub::helper`. The host's Python-era import
// filter (`filter_external_import_edges_by_module_refs`) drops any `imports`
// edge whose `to_id` is not a file-scope MODULE — so a function-target import
// (`use crate::sub::helper;`) resolves in-project yet is silently dropped
// (`imports_skipped_external_total += 1`). A module-target import survives and
// is the legitimate in-project `imports` edge this fixture exercises.
pub use crate::sub;
// Task 6 (Phase 2): the derive list mints `derives` edges — `Bumpable` is the
// in-project trait below (Resolved), `Debug` is external (dropped at emit, D1).
// The derive coexisting with the manual `impl Bumpable for Widget` would not
// compile, but this fixture is parsed, never compiled — and the two channels
// are distinct: the attribute mints `derives`, the impl mints `implements`.
#[derive(Debug, Bumpable)]
pub struct Widget { pub n: i32 }
pub fn make() -> Widget { Widget { n: 0 } }
impl Widget { pub fn bump(&mut self) { self.n += 1; } }
pub trait Bumpable { fn bump_by(&mut self, d: i32); }
impl Bumpable for Widget { fn bump_by(&mut self, d: i32) { self.n += d; } }
impl std::fmt::Display for Widget {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "{}", self.n) }
}
// The six remaining leaf entity kinds, exercising the full ontology.
pub enum Color { Red, Green }
pub type Count = i32;
pub const MAX: i32 = 10;
pub static NAME: &str = "widget";
#[macro_export]
macro_rules! twice { ($x:expr) => { $x + $x }; }
