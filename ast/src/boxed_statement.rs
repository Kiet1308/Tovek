//! Keep uncommon loop payloads out of every Statement slot. The forwarding
//! implementations preserve each payload's selectors and early-stop behavior.
use crate::{GenericForNext, LValue, LocalRw, NumForInit, NumForNext, NumericFor, RValue, RcLocal, SideEffects, Statement, Traverse};
use crate::traverse::PreOrPost;
use itertools::Either;

macro_rules! boxed_statement {
    ($($node:ident),+ $(,)?) => { $(
        impl From<$node> for Statement {
            fn from(node: $node) -> Self { Self::$node(Box::new(node)) }
        }
        impl TryFrom<Statement> for $node {
            type Error = &'static str;
            fn try_from(statement: Statement) -> Result<Self, Self::Error> {
                <Statement as TryInto<Box<$node>>>::try_into(statement).map(|node| *node)
            }
        }
        impl SideEffects for Box<$node> {
            fn has_side_effects(&self) -> bool { self.as_ref().has_side_effects() }
        }
        impl LocalRw for Box<$node> {
            fn visit_local_reads<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool { self.as_ref().visit_local_reads(visit) }
            fn visit_local_reads_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool { self.as_mut().visit_local_reads_mut(visit) }
            fn any_local_read(&self, predicate: &mut dyn FnMut(&RcLocal) -> bool) -> bool { self.as_ref().any_local_read(predicate) }
            fn visit_local_writes<'a>(&'a self, visit: &mut dyn FnMut(&'a RcLocal) -> bool) -> bool { self.as_ref().visit_local_writes(visit) }
            fn visit_local_writes_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RcLocal) -> bool) -> bool { self.as_mut().visit_local_writes_mut(visit) }
            fn any_local_write(&self, predicate: &mut dyn FnMut(&RcLocal) -> bool) -> bool { self.as_ref().any_local_write(predicate) }
            fn values_read(&self) -> Vec<&RcLocal> { self.as_ref().values_read() }
            fn values_read_mut(&mut self) -> Vec<&mut RcLocal> { self.as_mut().values_read_mut() }
            fn values_written(&self) -> Vec<&RcLocal> { self.as_ref().values_written() }
            fn values_written_mut(&mut self) -> Vec<&mut RcLocal> { self.as_mut().values_written_mut() }
            fn values(&self) -> Vec<&RcLocal> { self.as_ref().values() }
            fn replace_values_read(&mut self, old: &RcLocal, new: &RcLocal) { self.as_mut().replace_values_read(old, new); }
            fn replace_values_written(&mut self, old: &RcLocal, new: &RcLocal) { self.as_mut().replace_values_written(old, new); }
            fn replace_values(&mut self, old: &RcLocal, new: &RcLocal) { self.as_mut().replace_values(old, new); }
        }
        impl Traverse for Box<$node> {
            fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a LValue) -> bool) -> bool { self.as_ref().visit_lvalues(visit) }
            fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut LValue) -> bool) -> bool { self.as_mut().visit_lvalues_mut(visit) }
            fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a RValue) -> bool) -> bool { self.as_ref().visit_rvalues(visit) }
            fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RValue) -> bool) -> bool { self.as_mut().visit_rvalues_mut(visit) }
            fn lvalues(&self) -> Vec<&LValue> { self.as_ref().lvalues() }
            fn lvalues_mut(&mut self) -> Vec<&mut LValue> { self.as_mut().lvalues_mut() }
            fn rvalues(&self) -> Vec<&RValue> { self.as_ref().rvalues() }
            fn rvalues_mut(&mut self) -> Vec<&mut RValue> { self.as_mut().rvalues_mut() }
            fn traverse_rvalues_ref<F>(&self, callback: &mut F) where F: FnMut(&RValue) { self.as_ref().traverse_rvalues_ref(callback); }
            fn traverse_rvalues<F>(&mut self, callback: &mut F) where F: FnMut(&mut RValue) { self.as_mut().traverse_rvalues(callback); }
            fn post_traverse_rvalues<F, R>(&mut self, callback: &mut F) -> Option<R> where F: FnMut(&mut RValue) -> Option<R> { self.as_mut().post_traverse_rvalues(callback) }
            fn post_traverse_values<F, R>(&mut self, callback: &mut F) -> Option<R> where F: FnMut(Either<&mut LValue, &mut RValue>) -> Option<R> { self.as_mut().post_traverse_values(callback) }
            fn traverse_values<F, R>(&mut self, callback: &mut F) -> Option<R> where F: FnMut(PreOrPost, Either<&mut LValue, &mut RValue>) -> Option<R> { self.as_mut().traverse_values(callback) }
        }
    )+ };
}

boxed_statement!(NumForInit, NumForNext, NumericFor, GenericForNext);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Call, Closure, Function, Literal, Upvalue};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn compare<T: LocalRw + Traverse + SideEffects>(statement: &Statement, payload: &T) {
        let ids = |values: Vec<&RcLocal>| values.iter().map(|local| local.stable_id()).collect::<Vec<_>>();
        assert_eq!(ids(statement.values_read()), ids(payload.values_read()));
        assert_eq!(ids(statement.values_written()), ids(payload.values_written()));
        assert_eq!(ids(statement.values()), ids(payload.values()));
        assert_eq!(statement.has_side_effects(), payload.has_side_effects());
        assert_eq!(statement.rvalues().iter().map(|value| *value as *const RValue).collect::<Vec<_>>(),
            payload.rvalues().iter().map(|value| *value as *const RValue).collect::<Vec<_>>());
        assert_eq!(statement.lvalues().iter().map(|value| *value as *const LValue).collect::<Vec<_>>(),
            payload.lvalues().iter().map(|value| *value as *const LValue).collect::<Vec<_>>());
        for limit in 0..8 {
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            let complete = statement.visit_local_reads(&mut |local| { actual.push(local.stable_id()); actual.len() <= limit });
            let reference_complete = payload.visit_local_reads(&mut |local| { expected.push(local.stable_id()); expected.len() <= limit });
            assert_eq!((actual, complete), (expected, reference_complete));
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            let complete = statement.visit_local_writes(&mut |local| { actual.push(local.stable_id()); actual.len() <= limit });
            let reference_complete = payload.visit_local_writes(&mut |local| { expected.push(local.stable_id()); expected.len() <= limit });
            assert_eq!((actual, complete), (expected, reference_complete));
        }
    }

    #[test]
    fn compact_layout_and_boxed_dispatch_preserve_payload_selectors() {
        assert!(std::mem::size_of::<Statement>() < std::mem::size_of::<NumForInit>());
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<Statement>(), 128);
        let a = RcLocal::default(); let b = RcLocal::default(); let c = RcLocal::default();
        let statements: Vec<Statement> = vec![
            NumForInit::new(a.clone(), b.clone(), c.clone()).into(),
            NumForNext::new(a.clone(), b.clone().into(), c.clone().into()).into(),
            NumericFor::new(a.clone().into(), b.clone().into(), c.clone().into(), a.clone(), Block::default()).into(),
            GenericForNext::new(vec![a.clone(), a.clone()], b.clone().into(), c.clone(), a.clone()).into(),
        ];
        for statement in &statements {
            match statement {
                Statement::NumForInit(payload) => compare(statement, payload.as_ref()),
                Statement::NumForNext(payload) => compare(statement, payload.as_ref()),
                Statement::NumericFor(payload) => compare(statement, payload.as_ref()),
                Statement::GenericForNext(payload) => compare(statement, payload.as_ref()),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn boxing_and_unboxing_move_payload_while_clone_keeps_ancestry_and_aliases() {
        let local = RcLocal::default();
        let function = Arc::new(Mutex::new(Function::default()));
        let mut call = Call::new(Closure { node_origin: Default::default(),
            function: ByAddress(function.clone()), upvalues: vec![Upvalue::Ref(local.clone())] }.into(),
            vec![Literal::Number(f64::from_bits(0x7ff8_0000_0000_00a5)).into()]);
        call.node_origin = crate::node_origins::Origin::synthesized("layout_fixture");
        call.reconstruction_event = 73;
        call.callee_after_arguments = true;
        let payload = NumericFor::new(call.into(), Literal::Number(-0.0).into(), Literal::Number(1.0).into(), local.clone(), Block::default());
        let debug = format!("NumericFor({payload:?})");
        let owners = (Arc::count(&local.0.0), Arc::strong_count(&function));
        let statement: Statement = payload.into();
        assert_eq!(format!("{statement:?}"), debug);
        assert_eq!((Arc::count(&local.0.0), Arc::strong_count(&function)), owners);
        let cloned = statement.clone();
        let copy = cloned.as_numeric_for().unwrap();
        let copy_call = copy.initial.as_call().unwrap();
        let original_call = statement.as_numeric_for().unwrap().initial.as_call().unwrap();
        assert!(copy_call.node_origin.0.as_ref().unwrap().cloned);
        assert!(!original_call.node_origin.0.as_ref().unwrap().cloned);
        assert_eq!(copy_call.node_origin.0.as_ref().unwrap().synthesized, Some("layout_fixture"));
        assert_eq!((copy_call.reconstruction_event, copy_call.callee_after_arguments), (73, true));
        assert_eq!(copy_call.value.as_closure().unwrap().function, original_call.value.as_closure().unwrap().function);
        assert_eq!(copy_call.arguments[0].as_literal().unwrap().as_number().unwrap().to_bits(), 0x7ff8_0000_0000_00a5);
        drop(cloned);
        let recovered: NumericFor = statement.try_into().unwrap();
        assert_eq!(recovered.limit.as_literal().unwrap().as_number().unwrap().to_bits(), (-0.0f64).to_bits());
        assert!(!recovered.initial.as_call().unwrap().node_origin.0.as_ref().unwrap().cloned);
        assert_eq!((Arc::count(&local.0.0), Arc::strong_count(&function)), owners);
    }
}
