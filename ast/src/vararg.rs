use std::fmt;

use crate::{LocalRw, SideEffects, Traverse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarArg;

impl LocalRw for VarArg {}

impl SideEffects for VarArg {}

impl Traverse for VarArg {}

impl fmt::Display for VarArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "...")
    }
}
