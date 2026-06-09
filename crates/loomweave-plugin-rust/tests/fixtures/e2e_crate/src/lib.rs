pub mod sub;
pub struct Widget { pub n: i32 }
pub fn make() -> Widget { Widget { n: 0 } }
impl Widget { pub fn bump(&mut self) { self.n += 1; } }
pub trait Bumpable { fn bump_by(&mut self, d: i32); }
impl Bumpable for Widget { fn bump_by(&mut self, d: i32) { self.n += d; } }
impl std::fmt::Display for Widget {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { write!(f, "{}", self.n) }
}
