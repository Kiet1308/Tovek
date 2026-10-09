use std::fmt;

use crate::{LocalRw, RcLocal, Traverse, formatter::Formatter, has_side_effects};

use super::RValue;

#[derive(PartialEq, Clone, Default)]
pub struct Return {
    pub node_origin: crate::node_origins::Origin,
    pub values: Vec<RValue>,
}

has_side_effects!(Return);

impl Return {
    pub fn new(values: Vec<RValue>) -> Self {
        Self { node_origin: Default::default(), values }
    }
}

impl Traverse for Return {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        self.values.iter().all(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        self.values.iter_mut().all(visit)
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.values.iter_mut().collect()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        self.values.iter().collect()
    }
}

impl LocalRw for Return {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.values.iter().all(|value| value.visit_local_reads(visit))
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.values.iter_mut().all(|value| value.visit_local_reads_mut(visit))
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl fmt::Display for Return {
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
        .format_return(self)
    }
}

crate::node_origins::semantic_debug!(Return; values);
