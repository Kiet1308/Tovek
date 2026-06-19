use parking_lot::Mutex;
use triomphe::Arc;

use crate::{formatter::Formatter, LocalRw, RcLocal, SideEffects, Traverse};

use super::{Block, RValue};

use std::fmt;

#[derive(Debug, Clone)]
pub struct If {
    pub condition: RValue,
    pub then_block: Arc<Mutex<Block>>,
    pub else_block: Arc<Mutex<Block>>,
}

impl PartialEq for If {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: compare block
        false
    }
}

impl If {
    pub fn new(condition: RValue, then_block: Block, else_block: Block) -> Self {
        Self {
            condition,
            then_block: Arc::new(then_block.into()),
            else_block: Arc::new(else_block.into()),
        }
    }
}

impl Traverse for If {
    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.condition]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.condition]
    }
}

impl SideEffects for If {
    // TODO: side effects for blocks
    fn has_side_effects(&self) -> bool {
        true
    }
}

impl LocalRw for If {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.condition.values_read()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.condition.values_read_mut()
    }
}

impl fmt::Display for If {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
        }
        .format_if(self)
    }
}
