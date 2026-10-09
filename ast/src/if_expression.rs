use std::fmt;

use crate::{LocalRw, RValue, RcLocal, Reduce, SideEffects, Traverse, formatter::Formatter};

#[derive(Clone, PartialEq)]
pub struct IfExpression {
    pub node_origin: crate::node_origins::Origin,
    pub condition: Box<RValue>,
    pub then_value: Box<RValue>,
    pub else_value: Box<RValue>,
}

impl IfExpression {
    pub fn new(condition: RValue, then_value: RValue, else_value: RValue) -> Self {
        Self {
            node_origin: Default::default(),
            condition: Box::new(condition),
            then_value: Box::new(then_value),
            else_value: Box::new(else_value),
        }
    }
}

impl Traverse for IfExpression {
    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a crate::RValue) -> bool) -> bool {
        visit(&self.condition) && visit(&self.then_value) && visit(&self.else_value)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut crate::RValue) -> bool) -> bool {
        visit(&mut self.condition) && visit(&mut self.then_value) && visit(&mut self.else_value)
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![
            &mut self.condition,
            &mut self.then_value,
            &mut self.else_value,
        ]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.condition, &self.then_value, &self.else_value]
    }
}

impl LocalRw for IfExpression {
    fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool {
        self.condition.visit_local_reads(visit)
            && self.then_value.visit_local_reads(visit)
            && self.else_value.visit_local_reads(visit)
    }

    fn values_read(&self) -> Vec<&RcLocal> {
        crate::local::collect_reads(self)
    }

    fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool {
        self.condition.visit_local_reads_mut(visit)
            && self.then_value.visit_local_reads_mut(visit)
            && self.else_value.visit_local_reads_mut(visit)
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        crate::local::collect_reads_mut(self)
    }
}

impl SideEffects for IfExpression {
    fn has_side_effects(&self) -> bool {
        self.condition.has_side_effects()
            || self.then_value.has_side_effects()
            || self.else_value.has_side_effects()
    }
}

impl Reduce for IfExpression {
    fn reduce(mut self) -> RValue {
        *self.condition = std::mem::replace(self.condition.as_mut(), crate::Literal::Nil.into()).reduce_condition();
        *self.then_value = std::mem::replace(self.then_value.as_mut(), crate::Literal::Nil.into()).reduce();
        *self.else_value = std::mem::replace(self.else_value.as_mut(), crate::Literal::Nil.into()).reduce();
        self.node_origin = Default::default();
        self.into()
    }

    fn reduce_condition(mut self) -> RValue {
        *self.condition = std::mem::replace(self.condition.as_mut(), crate::Literal::Nil.into()).reduce_condition();
        *self.then_value = std::mem::replace(self.then_value.as_mut(), crate::Literal::Nil.into()).reduce_condition();
        *self.else_value = std::mem::replace(self.else_value.as_mut(), crate::Literal::Nil.into()).reduce_condition();
        self.node_origin = Default::default();
        self.into()
    }
}

impl fmt::Display for IfExpression {
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
        .format_if_expression(self)
    }
}

crate::node_origins::semantic_debug!(IfExpression; condition,then_value,else_value);
