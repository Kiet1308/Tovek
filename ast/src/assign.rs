use std::fmt;

use crate::{RcLocal, SideEffects, Traverse, formatter::Formatter};

use super::{LValue, LocalRw, RValue};

#[derive(Clone, PartialEq)]
pub struct Assign {
    pub node_origin: crate::node_origins::Origin,
    pub left: Vec<LValue>,
    pub right: Vec<RValue>,
    pub prefix: bool,
    pub parallel: bool,
    /// `target op= value`, with the target's base and key evaluated once.
    /// Set only for `t[i].k = t[i].k op value` shapes whose base is not
    /// repeatable; the formatter must not expand them.
    pub compound: bool,
}

impl Assign {
    pub fn new(left: Vec<LValue>, right: Vec<RValue>) -> Self {
        Self {
            node_origin: Default::default(),
            left,
            right,
            prefix: false,
            parallel: false,
            compound: false,
        }
    }

    /// Whether the value reads the local this assignment writes first, which
    /// pins the value to this statement. In SSA only a closure capturing its
    /// own target does (`local function f() ... f ... end`: NEWCLOSURE fills
    /// the register before CAPTURE VAL reads it), and that statement is the
    /// target's only definition; moved into an expression elsewhere, the
    /// capture names a local nothing assigns. In source, a value moved into
    /// its target's declaration would read an outer binding instead.
    pub fn reads_own_target(&self) -> bool {
        let Some(target) = self.left.first().and_then(LValue::as_local) else { return false };
        !self.right.iter().all(|value| value.visit_local_reads(&mut |read| read != target))
    }
}

impl Traverse for Assign {
    fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::LValue) -> bool) -> bool {
        self.left.iter().all(visit)
    }

    fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::LValue) -> bool) -> bool {
        self.left.iter_mut().all(visit)
    }

    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        self.right.iter().all(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        self.right.iter_mut().all(visit)
    }

    fn lvalues(&self) -> Vec<&LValue> {
        self.left.iter().collect()
    }
    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        self.left.iter_mut().collect()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.right.iter_mut().collect()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        self.right.iter().collect()
    }
}

impl SideEffects for Assign {
    fn has_side_effects(&self) -> bool {
        self.right.iter().any(|r| r.has_side_effects())
            || self.left.iter().any(|l| l.has_side_effects())
    }
}

impl LocalRw for Assign {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.left.iter().all(|value| value.visit_local_reads(visit))
            && self.right.iter().all(|value| value.visit_local_reads(visit))
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.left.iter_mut().all(|value| value.visit_local_reads_mut(visit))
            && self.right.iter_mut().all(|value| value.visit_local_reads_mut(visit))
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }

    fn visit_local_writes<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.left.iter().all(|value| value.visit_local_writes(visit))
    }

    fn visit_local_writes_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.left.iter_mut().all(|value| value.visit_local_writes_mut(visit))
    }

    fn values_written(&self) -> Vec<&RcLocal> {
        self.left.iter().flat_map(|l| l.values_written()).collect()
    }

    fn values_written_mut(&mut self) -> Vec<&mut RcLocal> {
        self.left
            .iter_mut()
            .flat_map(|l| l.values_written_mut())
            .collect()
    }
}

impl fmt::Display for Assign {
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
        .format_assign(self)
    }
}

crate::node_origins::semantic_debug!(Assign; left,right,prefix,parallel);
