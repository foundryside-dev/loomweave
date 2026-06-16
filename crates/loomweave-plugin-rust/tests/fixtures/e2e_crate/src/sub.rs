// Task 6 (Phase 2): cross-module `references` sites, all resolving into the
// crate root —
//   - `Gauge.w`'s field TYPE mints struct -> struct (Gauge -> Widget),
//   - `helper`'s param TYPE mints fn -> struct (helper -> Widget),
//   - `helper`'s body `crate::MAX` path mints fn -> const (helper -> MAX).
// `i32` / the local `w` resolve External and are dropped + counted.
pub struct Gauge { pub w: crate::Widget }
pub fn helper(w: &crate::Widget) -> i32 { w.n + crate::MAX }
