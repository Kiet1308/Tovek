//! Allocation-free counterparts of the de-inliner's shallow statement lists.
//! Assignment RHS comes before indexed LHS operands. Internal loop markers,
//! nested blocks and closure bodies deliberately remain outside this selector.
use crate::{LValue, RValue, Statement, Traverse};

pub(crate) fn visit_stmt_rvalues<'a>(
    statement: &'a Statement,
    visit: &mut dyn FnMut(&'a RValue) -> bool,
) -> bool {
    match statement {
        Statement::Assign(assign) => {
            assign.right.iter().all(&mut *visit)
                && assign.left.iter().all(|left| match left {
                    LValue::Index(index) => visit(&index.left) && visit(&index.right),
                    _ => true,
                })
        }
        Statement::Call(_)
        | Statement::MethodCall(_)
        | Statement::Return(_)
        | Statement::If(_)
        | Statement::While(_)
        | Statement::Repeat(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_)
        | Statement::SetList(_) => statement.visit_rvalues(visit),
        _ => true,
    }
}

pub(crate) fn visit_stmt_rvalues_mut<'a>(
    statement: &'a mut Statement,
    visit: &mut dyn FnMut(&'a mut RValue) -> bool,
) -> bool {
    match statement {
        Statement::Assign(assign) => {
            assign.right.iter_mut().all(&mut *visit)
                && assign.left.iter_mut().all(|left| match left {
                    LValue::Index(index) => visit(&mut index.left) && visit(&mut index.right),
                    _ => true,
                })
        }
        Statement::Call(_)
        | Statement::MethodCall(_)
        | Statement::Return(_)
        | Statement::If(_)
        | Statement::While(_)
        | Statement::Repeat(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_)
        | Statement::SetList(_) => statement.visit_rvalues_mut(visit),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Block, Call, Closure, Function, GenericFor, GenericForInit,
        GenericForNext, Global, If, Index, Literal, MethodCall, NumForInit, NumForNext,
        NumericFor, RcLocal, Repeat, Return, SetList, While};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn number(value: u32) -> RValue { Literal::Number(value as f64).into() }

    fn fixtures() -> Vec<Statement> {
        let local = RcLocal::default();
        let nested = || Block(vec![Return::new(vec![number(99)]).into()]);
        let closure = Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function { body: nested(), ..Function::default() }))),
            upvalues: vec![],
        };
        vec![
            Assign::new(vec![Index::new(number(4), number(5)).into(), local.clone().into(),
                Index::new(number(6), number(7)).into(), Global::from("target").into()],
                vec![number(1), number(2), closure.into()]).into(),
            Assign { prefix: true, ..Assign::new(vec![], vec![]) }.into(),
            Call::new(number(1), vec![number(2), number(3)]).into(),
            MethodCall::new(number(1), "method".into(), vec![number(2), number(3)]).into(),
            Return::new(vec![number(1), number(2)]).into(),
            If::new(number(1), nested(), nested()).into(),
            While::new(number(1), nested()).into(),
            Repeat::new(number(1), nested()).into(),
            NumericFor::new(number(1), number(2), number(3), local.clone(), nested()).into(),
            GenericFor::new(vec![local.clone()], vec![number(1), number(2)], nested()).into(),
            SetList::new(local.clone(), 1, vec![number(1), number(2)], Some(number(3))).into(),
            SetList::new(local.clone(), 1, vec![], None).into(),
            NumForInit { counter: (local.clone().into(), number(1)),
                limit: (local.clone().into(), number(2)), step: (local.clone().into(), number(3)) }.into(),
            NumForNext::new(local.clone(), number(1), number(2)).into(),
            GenericForInit::new(local.clone(), local.clone(), local.clone()).into(),
            GenericForNext::new(vec![local.clone()], number(1), local.clone(), local.clone()).into(),
            crate::Empty {}.into(), crate::Continue {}.into(), crate::Break {}.into(),
            crate::Close { locals: vec![local] }.into(),
            crate::Label::from("label").into(), crate::Goto::new(crate::Label::from("label")).into(),
            crate::Comment::new("comment".into()).into(),
        ]
    }

    #[test]
    fn shallow_statement_visitors_match_independent_legacy_lists_and_early_stop() {
        // The original Vec helpers are retained unchanged and do not call these
        // visitors, so pointer order and omitted variants have an independent oracle.
        for statement in fixtures() {
            let expected: Vec<_> = super::super::stmt_rvalues(&statement).into_iter()
                .map(|value| value as *const RValue).collect();
            for stop in 1..=expected.len() + 1 {
                let mut actual = Vec::new();
                let completed = visit_stmt_rvalues(&statement, &mut |value| {
                    actual.push(value as *const RValue);
                    actual.len() != stop
                });
                assert_eq!(actual, expected[..expected.len().min(stop)]);
                assert_eq!(completed, stop > expected.len());

                let mut old = statement.clone();
                let mut new = statement.clone();
                let mut old_count = 0;
                let old_completed = super::super::stmt_rvalues_mut(&mut old).into_iter().all(|value| {
                    old_count += 1;
                    *value = Call::new(Global::from("replacement").into(), vec![number(old_count)]).into();
                    old_count as usize != stop
                });
                let mut new_count = 0;
                let new_completed = visit_stmt_rvalues_mut(&mut new, &mut |value| {
                    new_count += 1;
                    *value = Call::new(Global::from("replacement").into(), vec![number(new_count)]).into();
                    new_count as usize != stop
                });
                assert_eq!((new_count, new_completed), (old_count, old_completed));
                assert_eq!(new.to_string(), old.to_string());
            }
        }
    }
}
