use std::fmt;
/// Doc.
#[derive(Debug)]
pub struct Point<T> {
    x: T,
}
impl<T: Clone> fmt::Display for Point<T> {
    const N: usize = 1;
    pub(crate) fn new(
        x: T,
    ) -> Self {
        fn helper() {}
        let local = 1;
        Point { x }
    }
}
pub trait Shape { fn area(&self) -> f64; fn name(&self) -> &str { "s" } }
pub enum Kind { A, B(u8) }
mod tests {
    #[test]
    fn works() {}
}
macro_rules! square { ($x:expr) => { $x * $x }; }
extern "C" { fn abs(x: i32) -> i32; }
type Alias = Vec<u8>;
static GLOBAL: u8 = 1;
union U { a: u8 }
