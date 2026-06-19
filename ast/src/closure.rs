use std::fmt;

use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

use crate::{
    formatter::Formatter,
    type_system::{Infer, TypeSystem},
    Block, Literal, LocalRw, RcLocal, Reduce, SideEffects, Traverse, Type,
};

#[derive(Debug, PartialEq, Clone)]
pub enum Upvalue {
    Copy(RcLocal),
    Ref(RcLocal),
}

#[derive(Default, Debug, PartialEq, Clone)]
pub struct Function {
    pub name: Option<String>,
    pub parameters: Vec<RcLocal>,
    pub is_variadic: bool,
    pub body: Block,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Closure {
    pub function: ByAddress<Arc<Mutex<Function>>>,
    pub upvalues: Vec<Upvalue>,
}

impl Reduce for Closure {
    fn reduce(self) -> crate::RValue {
        self.into()
    }

    fn reduce_condition(self) -> crate::RValue {
        Literal::Boolean(true).into()
    }
}

impl Infer for Closure {
    fn infer<'a: 'b, 'b>(&'a mut self, _system: &mut TypeSystem<'b>) -> Type {
        todo!()
        // let return_values = system.analyze_block(&mut self.body);
        // let parameters = self
        //     .parameters
        //     .iter_mut()
        //     .map(|l| l.infer(system))
        //     .collect_vec();

        // Type::Function(parameters, return_values)
    }
}

impl fmt::Display for Closure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Formatter {
            indentation_level: 0,
            indentation_mode: Default::default(),
            output: f,
            colon_method_calls: Vec::new(),
        }
        .format_closure(self)
    }
}

impl LocalRw for Closure {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.upvalues
            .iter()
            .map(|u| match u {
                Upvalue::Copy(l) | Upvalue::Ref(l) => l,
            })
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.upvalues
            .iter_mut()
            .map(|u| match u {
                Upvalue::Copy(l) | Upvalue::Ref(l) => l,
            })
            .collect()
    }
}

impl SideEffects for Closure {}

impl Traverse for Closure {}
