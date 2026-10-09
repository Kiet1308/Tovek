use parking_lot::Mutex;
use triomphe::Arc;

use crate::{Block, LocalRw, RValue, RcLocal, Traverse, formatter::Formatter, has_side_effects};
use std::fmt;

#[derive(Debug, Clone)]
pub struct While {
    pub condition: RValue,
    pub block: Arc<Mutex<Block>>,
}

impl PartialEq for While {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: compare block
        false
    }
}

has_side_effects!(While);

impl While {
    pub fn new(condition: RValue, block: Block) -> Self {
        Self {
            condition,
            block: Arc::new(block.into()),
        }
    }
}

impl Traverse for While {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.condition)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.condition)
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.condition]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.condition]
    }
}

impl LocalRw for While {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.condition.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.condition.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl fmt::Display for While {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Default::default(),
            position_query: None,
            closure_observer: None,
            emission_map: None,
            layout_budget: None,
            compact_annotations: false,
            inferred_calls: Default::default(),
        }
        .format_while(self)
    }
}
