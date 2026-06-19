use std::fmt;

use crate::{formatter::Formatter, has_side_effects, LocalRw, RcLocal, Traverse};

use super::RValue;

#[derive(Debug, PartialEq, Clone, Default)]
pub struct Return {
    pub values: Vec<RValue>,
}

has_side_effects!(Return);

impl Return {
    pub fn new(values: Vec<RValue>) -> Self {
        Self { values }
    }
}

impl Traverse for Return {
    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        self.values.iter_mut().collect()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        self.values.iter().collect()
    }
}

impl LocalRw for Return {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.values.iter().flat_map(|r| r.values_read()).collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.values
            .iter_mut()
            .flat_map(|r| r.values_read_mut())
            .collect()
    }
}

impl fmt::Display for Return {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
        }
        .format_return(self)
    }
}
