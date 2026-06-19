use std::fmt;

use ast::{RValue, RcLocal};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BranchType {
    #[default]
    Unconditional,
    Then,
    Else,
}

#[derive(Debug, Clone, Default)]
pub struct BlockEdge {
    pub branch_type: BranchType,
    // TODO: why is this not a hash map?
    pub arguments: Vec<(RcLocal, RValue)>,
}

impl BlockEdge {
    pub fn new(branch_type: BranchType) -> Self {
        Self {
            branch_type,
            arguments: Vec::new(),
        }
    }
}

impl fmt::Display for BlockEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.branch_type {
            BranchType::Unconditional => write!(f, "u"),
            BranchType::Then => write!(f, "t"),
            BranchType::Else => write!(f, "e"),
        }?;
        if !self.arguments.is_empty() {
            for (i, (local, new_local)) in self.arguments.iter().enumerate() {
                write!(f, "{} -> {}", local, new_local)?;
                if i + 1 != self.arguments.len() {
                    writeln!(f)?;
                }
            }
        }
        Ok(())
    }
}
