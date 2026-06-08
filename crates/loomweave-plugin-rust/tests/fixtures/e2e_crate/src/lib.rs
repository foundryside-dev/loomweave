pub mod sub;
pub struct Widget { pub n: i32 }
pub fn make() -> Widget { Widget { n: 0 } }
impl Widget { pub fn bump(&mut self) { self.n += 1; } }
