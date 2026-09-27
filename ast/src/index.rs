use crate::{LocalRw, RcLocal, Traverse, formatter::Formatter, has_side_effects};

use super::RValue;
use std::fmt;

#[derive(Clone, PartialEq)]
pub struct Index {
    pub node_origin: crate::node_origins::Origin,
    pub left: Box<RValue>,
    pub right: Box<RValue>,
}

// this should be the same as MethodCall
has_side_effects!(Index);

impl Index {
    pub fn new(left: RValue, right: RValue) -> Self {
        Self {
            node_origin: Default::default(),
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

impl LocalRw for Index {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.left.visit_local_reads(visit) && self.right.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.left.visit_local_reads_mut(visit) && self.right.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl Traverse for Index {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.left) && visit(&self.right)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.left) && visit(&mut self.right)
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.left, &mut self.right]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.left, &self.right]
    }
}

impl fmt::Display for Index {
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
        }
        .format_index(self)
    }
}

crate::node_origins::semantic_debug!(Index; left,right);
