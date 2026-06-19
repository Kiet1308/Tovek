use std::fmt;

use crate::{formatter::Formatter, LocalRw, RValue, RcLocal, Reduce, SideEffects, Traverse};

#[derive(Debug, Clone, PartialEq)]
pub struct IfExpression {
    pub condition: Box<RValue>,
    pub then_value: Box<RValue>,
    pub else_value: Box<RValue>,
}

impl IfExpression {
    pub fn new(condition: RValue, then_value: RValue, else_value: RValue) -> Self {
        Self {
            condition: Box::new(condition),
            then_value: Box::new(then_value),
            else_value: Box::new(else_value),
        }
    }
}

impl Traverse for IfExpression {
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
    fn values_read(&self) -> Vec<&RcLocal> {
        self.condition
            .values_read()
            .into_iter()
            .chain(self.then_value.values_read())
            .chain(self.else_value.values_read())
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.condition
            .values_read_mut()
            .into_iter()
            .chain(self.then_value.values_read_mut())
            .chain(self.else_value.values_read_mut())
            .collect()
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
    fn reduce(self) -> RValue {
        Self {
            condition: Box::new(self.condition.reduce_condition()),
            then_value: Box::new(self.then_value.reduce()),
            else_value: Box::new(self.else_value.reduce()),
        }
        .into()
    }

    fn reduce_condition(self) -> RValue {
        Self {
            condition: Box::new(self.condition.reduce_condition()),
            then_value: Box::new(self.then_value.reduce_condition()),
            else_value: Box::new(self.else_value.reduce_condition()),
        }
        .into()
    }
}

impl fmt::Display for IfExpression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
        }
        .format_if_expression(self)
    }
}
