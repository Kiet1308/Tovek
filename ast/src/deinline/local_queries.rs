//! Presence queries used by matching. Counting every occurrence or collecting
//! all locals allocates even when the first operand answers the question.
//! These borrow the tree and stop at that operand. Closure bodies participate,
//! just as in `count_local_reads` and `collect_written`, including closures in
//! assignment addresses. In particular, SETLIST counts as a write here.

use crate::{LValue, LocalRw, RValue, RcLocal, Statement, Traverse};

pub(super) fn reads(statements: &[Statement], local: &RcLocal) -> bool {
    statements.iter().any(|statement| {
        statement.any_local_read(&mut |read| read == local)
            || child_blocks(statement, local, reads)
            || !super::visit_stmt_rvalues(statement, &mut |value| !closures(value, local, reads))
    })
}

pub(super) fn value_reads(value: &RValue, local: &RcLocal) -> bool {
    value.any_local_read(&mut |read| read == local) || closures(value, local, reads)
}

pub(super) fn writes(statements: &[Statement], local: &RcLocal) -> bool {
    statements.iter().any(|statement| {
        let own = match statement {
            Statement::Assign(assign) => assign.left.iter().any(|left| matches!(left, LValue::Local(written) if written == local)),
            Statement::NumericFor(node) => &node.counter == local,
            Statement::GenericFor(node) => node.res_locals.contains(local),
            Statement::SetList(node) => &node.object_local == local,
            _ => false,
        };
        own || child_blocks(statement, local, writes)
            || !super::visit_stmt_rvalues(statement, &mut |value| !closures(value, local, writes))
    })
}

fn child_blocks(statement: &Statement, local: &RcLocal, query: fn(&[Statement], &RcLocal) -> bool) -> bool {
    match statement {
        Statement::If(node) => query(&node.then_block.lock().0, local) || query(&node.else_block.lock().0, local),
        Statement::While(node) => query(&node.block.lock().0, local),
        Statement::Repeat(node) => query(&node.block.lock().0, local),
        Statement::NumericFor(node) => query(&node.block.lock().0, local),
        Statement::GenericFor(node) => query(&node.block.lock().0, local),
        _ => false,
    }
}

fn closures(value: &RValue, local: &RcLocal, query: fn(&[Statement], &RcLocal) -> bool) -> bool {
    match value {
        RValue::Closure(closure) => query(&closure.function.lock().body.0, local),
        _ => !value.visit_rvalues(&mut |child| !closures(child, local, query)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, BinaryOperation, Block, Call, Closure, Function, GenericFor,
        If, IfExpression, Index, Literal, MethodCall, NumericFor, Repeat, Return, Select,
        SetList, Table, Unary, UnaryOperation, Upvalue, While};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use rustc_hash::FxHashSet;
    use triomphe::Arc;

    #[test]
    fn borrowed_queries_match_full_censuses_in_nested_blocks_and_operand_closures() {
        let locals: Vec<RcLocal> = (0..6).map(|_| RcLocal::default()).collect();
        let nil = || RValue::Literal(Literal::Nil);
        let closure: RValue = Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                body: Block(vec![
                    Assign::new(vec![locals[3].clone().into()], vec![locals[1].clone().into()]).into(),
                    Return::new(vec![locals[1].clone().into()]).into(),
                ]),
                ..Default::default()
            }))),
            upvalues: vec![Upvalue::Copy(locals[2].clone())],
        }.into();
        let values = vec![
            locals[0].clone().into(), closure.clone(),
            Index::new(closure.clone(), nil()).into(),
            Index::new(nil(), closure.clone()).into(),
            Unary::new(closure.clone(), UnaryOperation::Not).into(),
            Binary::new(nil(), closure.clone(), BinaryOperation::Or).into(),
            Table::new(vec![(Some(closure.clone()), nil()), (None, closure.clone())]).into(),
            Call::new(closure.clone(), vec![locals[0].clone().into()]).into(),
            Select::Call(Call::new(nil(), vec![closure.clone()])).into(),
            MethodCall::new(closure.clone(), "run".into(), vec![nil()]).into(),
            Select::MethodCall(MethodCall::new(nil(), "run".into(), vec![closure.clone()])).into(),
            IfExpression::new(nil(), closure.clone(), locals[0].clone().into()).into(),
        ];
        for value in values {
            let legacy_return = [Statement::Return(Return::new(vec![value.clone()]))];
            for local in &locals {
                assert_eq!(value_reads(&value, local), crate::deinline::count_local_reads(&legacy_return, local) > 0);
            }
            let leaves = vec![
                Return::new(vec![value.clone()]).into(),
                // The closure is in an indexed assignment's address, not its RHS.
                Assign::new(vec![LValue::Index(Index::new(value.clone(), nil()))], vec![nil()]).into(),
                SetList::new(locals[4].clone(), 1, vec![value.clone()], Some(value)).into(),
            ];
            let mut cases = vec![Block(leaves)];
            for depth in 0..12 {
                let body = cases.last().unwrap().clone();
                let nested = match depth % 6 {
                    0 => If::new(nil(), body, Block::default()).into(),
                    1 => If::new(nil(), Block::default(), body).into(),
                    2 => While::new(nil(), body).into(),
                    3 => Repeat::new(nil(), body).into(),
                    4 => Statement::NumericFor(Box::new(NumericFor::new(nil(), nil(), nil(), locals[4].clone(), body))),
                    _ => GenericFor::new(vec![locals[4].clone()], vec![nil()], body).into(),
                };
                cases.push(Block(vec![nested]));
            }
            for case in cases {
                let mut written = FxHashSet::default();
                crate::deinline::collect_written(&case.0, &mut written);
                for local in &locals {
                    assert_eq!(reads(&case.0, local), crate::deinline::count_local_reads(&case.0, local) > 0);
                    assert_eq!(writes(&case.0, local), written.contains(local));
                }
            }
        }
    }
}
