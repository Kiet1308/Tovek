use crate::{LValue, RValue};
use enum_dispatch::enum_dispatch;
use itertools::Either;

pub enum PreOrPost {
    Pre,
    Post,
}

#[enum_dispatch]
pub trait Traverse {
    /// Direct children only, in the same order as `lvalues` / `rvalues`.
    /// Return false from the callback to stop without allocating a child list.
    fn visit_lvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a LValue) -> bool) -> bool {
        self.lvalues().into_iter().all(visit)
    }

    fn visit_lvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut LValue) -> bool) -> bool {
        self.lvalues_mut().into_iter().all(visit)
    }

    fn visit_rvalues<'a>(&'a self, visit: &mut dyn FnMut(&'a RValue) -> bool) -> bool {
        self.rvalues().into_iter().all(visit)
    }

    fn visit_rvalues_mut<'a>(&'a mut self, visit: &mut dyn FnMut(&'a mut RValue) -> bool) -> bool {
        self.rvalues_mut().into_iter().all(visit)
    }

    fn lvalues(&self) -> Vec<&LValue> {
        Vec::new()
    }

    /// Borrow expressions for analysis; do not clone the AST to use a mutable
    /// walker. As with the mutable walker, closure/block bodies are separate.
    fn traverse_rvalues_ref<F>(&self, callback: &mut F)
    where
        F: FnMut(&RValue),
    {
        self.visit_lvalues(&mut |lvalue| { lvalue.traverse_rvalues_ref(callback); true });
        self.visit_rvalues(&mut |rvalue| {
            callback(rvalue);
            rvalue.traverse_rvalues_ref(callback);
            true
        });
    }

    fn lvalues_mut(&mut self) -> Vec<&mut LValue> {
        Vec::new()
    }

    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        Vec::new()
    }

    fn rvalues(&self) -> Vec<&RValue> {
        Vec::new()
    }

    // fn traverse_lvalues(
    //     &mut self,
    //     lvalue_callback: &impl Fn(&mut LValue),
    //     rvalue_callback: &impl Fn(&mut RValue),
    // ) {
    //     self.rvalues_mut().into_iter().for_each(rvalue_callback);
    //     self.lvalues_mut().into_iter().for_each(lvalue_callback);
    //     self.lvalues_mut().into_iter().for_each(|lvalue| {
    //         lvalue.traverse_lvalues(lvalue_callback, rvalue_callback);
    //     });
    // }

    fn traverse_rvalues<F>(&mut self, callback: &mut F)
    where F: FnMut(&mut RValue),
    {
        self.visit_lvalues_mut(&mut |lvalue| { lvalue.traverse_rvalues(callback); true });
        self.visit_rvalues_mut(&mut |rvalue| {
            callback(rvalue);
            rvalue.traverse_rvalues(callback);
            true
        });
    }

    fn post_traverse_rvalues<F, R>(&mut self, callback: &mut F) -> Option<R>
    where F: FnMut(&mut RValue) -> Option<R>,
    {
        let mut result = None;
        self.visit_lvalues_mut(&mut |lvalue| {
            result = lvalue.post_traverse_rvalues(callback);
            result.is_none()
        });
        if result.is_none() {
            self.visit_rvalues_mut(&mut |rvalue| {
                result = rvalue.post_traverse_rvalues(callback).or_else(|| callback(rvalue));
                result.is_none()
            });
        }
        result
    }

    fn post_traverse_values<F, R>(&mut self, callback: &mut F) -> Option<R>
    where F: FnMut(Either<&mut LValue, &mut RValue>) -> Option<R>,
    {
        let mut result = None;
        self.visit_lvalues_mut(&mut |lvalue| {
            result = lvalue.post_traverse_values(callback).or_else(|| callback(Either::Left(lvalue)));
            result.is_none()
        });
        if result.is_none() {
            self.visit_rvalues_mut(&mut |rvalue| {
                result = rvalue.post_traverse_values(callback).or_else(|| callback(Either::Right(rvalue)));
                result.is_none()
            });
        }
        result
    }

    fn traverse_values<F, R>(&mut self, callback: &mut F) -> Option<R>
    where F: FnMut(PreOrPost, Either<&mut LValue, &mut RValue>) -> Option<R>,
    {
        let mut result = None;
        self.visit_lvalues_mut(&mut |lvalue| {
            result = callback(PreOrPost::Pre, Either::Left(lvalue))
                .or_else(|| lvalue.traverse_values(callback))
                .or_else(|| callback(PreOrPost::Post, Either::Left(lvalue)));
            result.is_none()
        });
        if result.is_none() {
            self.visit_rvalues_mut(&mut |rvalue| {
                result = callback(PreOrPost::Pre, Either::Right(rvalue))
                    .or_else(|| rvalue.traverse_values(callback))
                    .or_else(|| callback(PreOrPost::Post, Either::Right(rvalue)));
                result.is_none()
            });
        }
        result
    }
}

#[cfg(test)]
mod sink_tests {
    fn legacy_traverse_rvalues<F>(owner: &mut impl Traverse, callback: &mut F)
    where
        F: FnMut(&mut RValue),
    {
        for lvalue in owner.lvalues_mut() {
            legacy_traverse_rvalues(lvalue, callback);
        }
        for rvalue in owner.rvalues_mut() {
            callback(rvalue);
            legacy_traverse_rvalues(rvalue, callback);
        }
    }

    fn legacy_post_traverse_rvalues<F, R>(owner: &mut impl Traverse, callback: &mut F) -> Option<R>
    where
        F: FnMut(&mut RValue) -> Option<R>,
    {
        for lvalue in owner.lvalues_mut() {
            if let Some(res) = legacy_post_traverse_rvalues(lvalue, callback) {
                return Some(res);
            }
        }
        for rvalue in owner.rvalues_mut() {
            if let Some(res) = legacy_post_traverse_rvalues(rvalue, callback) {
                return Some(res);
            }
            if let Some(res) = callback(rvalue) {
                return Some(res);
            }
        }

        None
    }

    fn legacy_post_traverse_values<F, R>(owner: &mut impl Traverse, callback: &mut F) -> Option<R>
    where
        // TODO: REFACTOR: use an enum called Value instead of Either
        F: FnMut(Either<&mut LValue, &mut RValue>) -> Option<R>,
    {
        for lvalue in owner.lvalues_mut() {
            if let Some(res) = legacy_post_traverse_values(lvalue, callback) {
                return Some(res);
            }
            if let Some(res) = callback(Either::Left(lvalue)) {
                return Some(res);
            }
        }
        for rvalue in owner.rvalues_mut() {
            if let Some(res) = legacy_post_traverse_values(rvalue, callback) {
                return Some(res);
            }
            if let Some(res) = callback(Either::Right(rvalue)) {
                return Some(res);
            }
        }

        None
    }

    fn legacy_traverse_values<F, R>(owner: &mut impl Traverse, callback: &mut F) -> Option<R>
    where
        // TODO: REFACTOR: use an enum called Value instead of Either
        F: FnMut(PreOrPost, Either<&mut LValue, &mut RValue>) -> Option<R>,
    {
        for lvalue in owner.lvalues_mut() {
            if let Some(res) = callback(PreOrPost::Pre, Either::Left(lvalue)) {
                return Some(res);
            }
            if let Some(res) = legacy_traverse_values(lvalue, callback) {
                return Some(res);
            }
            if let Some(res) = callback(PreOrPost::Post, Either::Left(lvalue)) {
                return Some(res);
            }
        }
        for rvalue in owner.rvalues_mut() {
            if let Some(res) = callback(PreOrPost::Pre, Either::Right(rvalue)) {
                return Some(res);
            }
            if let Some(res) = legacy_traverse_values(rvalue, callback) {
                return Some(res);
            }
            if let Some(res) = callback(PreOrPost::Post, Either::Right(rvalue)) {
                return Some(res);
            }
        }

        None
    }

    use super::*;
    use crate::{Assign, Binary, BinaryOperation, Block, Call, Closure, Function, GenericForNext,
        Global, IfExpression, Index, Literal, LocalRw, MethodCall, Return, RcLocal, Select,
        SetList, Statement, Table, Unary, UnaryOperation, Upvalue};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    #[test]
    fn read_sinks_and_borrowed_traversal_match_legacy_mutable_order() {
        let locals: Vec<_> = (0..14).map(|_| RcLocal::default()).collect();
        let value = |index: usize| RValue::Local(locals[index].clone());
        let closure = Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function { body: Block(vec![Return::new(vec![value(13)]).into()]), ..Function::default() }))),
            upvalues: vec![Upvalue::Copy(locals[9].clone()), Upvalue::Ref(locals[10].clone())],
        };
        let expression = Table::new(vec![
            (Some(value(0)), Binary::new(value(1), Unary::new(value(2), UnaryOperation::Not).into(), BinaryOperation::And).into()),
            (None, Call::new(value(3), vec![Index::new(value(4), value(5)).into()]).into()),
            (None, Select::MethodCall(MethodCall::new(value(6), "method".into(), vec![value(7)])).into()),
            (None, IfExpression::new(value(8), closure.into(), Literal::Nil.into()).into()),
            (None, Select::VarArg(crate::VarArg).into()),
        ]);
        let statements: Vec<Statement> = vec![
            Assign::new(vec![Index::new(value(11), value(12)).into()], vec![expression.into()]).into(),
            GenericForNext::new(vec![locals[11].clone()], value(0), locals[1].clone(), locals[2].clone()).into(),
            SetList::new(locals[0].clone(), 1, vec![value(1)], Some(value(2))).into(),
            Return::new(vec![Global::from("global").into(), Select::Call(Call::new(value(3), vec![value(4)])).into()]).into(),
        ];
        for statement in statements {
            let reads: Vec<_> = statement.values_read().into_iter().map(RcLocal::stable_id).collect();
            let mut legacy = statement.clone();
            let expected: Vec<_> = legacy.values_read_mut().into_iter().map(|local| local.stable_id()).collect();
            assert_eq!(reads, expected);
            assert!(!reads.contains(&locals[13].stable_id()), "closure body is outside shallow reads");
            let mut seen = 0;
            assert!(!statement.visit_local_reads(&mut |_| { seen += 1; false }));
            assert_eq!(seen, 1, "the read sink stops at its first refusal");
            let mut actual = Vec::new();
            statement.traverse_rvalues_ref(&mut |node| actual.push(std::mem::discriminant(node)));
            let mut expected = Vec::new();
            legacy_traverse_rvalues(&mut legacy, &mut |node| expected.push(std::mem::discriminant(node)));
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn mutable_walkers_match_legacy_mutations_and_early_exits() {
        let input = Assign::new(vec![Index::new(Global::from("out").into(), Literal::Number(0.0).into()).into()],
            vec![Table::new(vec![(Some(Literal::Number(1.0).into()),
                Binary::new(Literal::Number(2.0).into(),
                    Select::Call(Call::new(Global::from("call").into(), vec![Literal::Number(3.0).into()])).into(),
                    BinaryOperation::Add).into())]).into()]);
        let mutate = |node: &mut RValue| {
            if let RValue::Literal(Literal::Number(number)) = node { *number += 100.0; }
        };
        let mut expected = input.clone(); let mut actual = input.clone();
        legacy_traverse_rvalues(&mut expected, &mut |node| mutate(node));
        actual.traverse_rvalues(&mut |node| mutate(node));
        assert_eq!(actual, expected);
        for stop in 0..40 {
            let mut expected = input.clone(); let mut actual = input.clone();
            let mut seen_expected = 0; let mut seen_actual = 0;
            let expected_result = legacy_post_traverse_rvalues(&mut expected, &mut |node| {
                mutate(node); seen_expected += 1; (seen_expected == stop).then_some(seen_expected)
            });
            let actual_result = actual.post_traverse_rvalues(&mut |node| {
                mutate(node); seen_actual += 1; (seen_actual == stop).then_some(seen_actual)
            });
            assert_eq!((actual_result, seen_actual), (expected_result, seen_expected));
            assert_eq!(actual, expected);
            let mut expected = input.clone(); let mut actual = input.clone();
            let mut seen_expected = 0; let mut seen_actual = 0;
            let expected_result = legacy_post_traverse_values(&mut expected, &mut |node| {
                if let Either::Right(node) = node { mutate(node); }
                seen_expected += 1; (seen_expected == stop).then_some(seen_expected)
            });
            let actual_result = actual.post_traverse_values(&mut |node| {
                if let Either::Right(node) = node { mutate(node); }
                seen_actual += 1; (seen_actual == stop).then_some(seen_actual)
            });
            assert_eq!((actual_result, seen_actual), (expected_result, seen_expected));
            assert_eq!(actual, expected);
            let mut expected = input.clone(); let mut actual = input.clone();
            let mut seen_expected = 0; let mut seen_actual = 0;
            let expected_result = legacy_traverse_values(&mut expected, &mut |phase, node| {
                if let (PreOrPost::Post, Either::Right(node)) = (phase, node) { mutate(node); }
                seen_expected += 1; (seen_expected == stop).then_some(seen_expected)
            });
            let actual_result = actual.traverse_values(&mut |phase, node| {
                if let (PreOrPost::Post, Either::Right(node)) = (phase, node) { mutate(node); }
                seen_actual += 1; (seen_actual == stop).then_some(seen_actual)
            });
            assert_eq!((actual_result, seen_actual), (expected_result, seen_expected));
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn legacy_local_read_implementations_and_mutable_early_stop_remain_supported() {
        struct Wrapper(RcLocal);
        impl LocalRw for Wrapper {
            fn values_read(&self) -> Vec<&RcLocal> { vec![&self.0] }
            fn values_read_mut(&mut self) -> Vec<&mut RcLocal> { vec![&mut self.0] }
        }
        let original = RcLocal::default(); let replacement = RcLocal::default();
        let mut custom = Wrapper(original.clone());
        assert!(custom.any_local_read(&mut |local| local == &original));
        assert!(!custom.visit_local_reads_mut(&mut |local| { *local = replacement.clone(); false }));
        assert_eq!(custom.0, replacement);
        let mut value = Binary::new(original.clone().into(), original.clone().into(), BinaryOperation::Add);
        let mut seen = 0;
        assert!(!value.visit_local_reads_mut(&mut |local| { *local = replacement.clone(); seen += 1; false }));
        assert_eq!(seen, 1);
        assert_eq!(value.values_read(), vec![&replacement, &original]);
    }

    #[test]
    fn custom_traverse_implementations_keep_legacy_children() {
        struct Wrapper(RValue);
        impl Traverse for Wrapper {
            fn rvalues(&self) -> Vec<&RValue> { vec![&self.0] }
            fn rvalues_mut(&mut self) -> Vec<&mut RValue> { vec![&mut self.0] }
        }
        let mut wrapped = Wrapper(Global::from("root").into());
        let mut seen = 0;
        wrapped.traverse_rvalues_ref(&mut |_| seen += 1);
        assert_eq!(seen, 1);
        wrapped.traverse_rvalues(&mut |_| seen += 1);
        assert_eq!(seen, 2);
    }
}
