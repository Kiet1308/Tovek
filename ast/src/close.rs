use itertools::Itertools;

use crate::{LocalRw, RcLocal, SideEffects, Traverse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    pub locals: Vec<RcLocal>,
}

impl std::fmt::Display for Close {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "__close_uv({})", self.locals.iter().join(", "))
    }
}

impl LocalRw for Close {}
impl SideEffects for Close {}
impl Traverse for Close {}
