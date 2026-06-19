use parking_lot::Mutex;
use triomphe::Arc;

use crate::{formatter::Formatter, has_side_effects, Block, LocalRw, RValue, RcLocal, Traverse};
use std::fmt;

// TODO: move condition after block
#[derive(Debug, Clone)]
pub struct Repeat {
    pub condition: RValue,
    pub block: Arc<Mutex<Block>>,
}

impl PartialEq for Repeat {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: compare block
        false
    }
}

has_side_effects!(Repeat);

impl Repeat {
    pub fn new(condition: RValue, block: Block) -> Self {
        Self {
            condition,
            block: Arc::new(block.into()),
        }
    }
}

impl Traverse for Repeat {
    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.condition]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.condition]
    }
}

impl LocalRw for Repeat {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.condition.values_read()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.condition.values_read_mut()
    }
}

impl fmt::Display for Repeat {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
        }
        .format_repeat(self)
    }
}
