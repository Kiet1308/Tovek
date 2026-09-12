//! Exact scalar reconstruction for a named bytecode prototype. The match is
//! evidence for an equivalent call, not proof that the source contained a call.
//! No algebra, type-based purity assumption, or partial arithmetic evaluator.

use std::cell::Cell;

use rustc_hash::FxHashSet;

use crate::deinline::{Bindings, MatchCtx, stmt_rvalues, unify_rvalue};
use crate::{
    BinaryOperation, Block, Function, IfExpression, Literal, RValue, RcLocal, Statement, Traverse,
    UnaryOperation, Upvalue, LocalRw,
};

pub(super) const MARKER: &str =
    " equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown";
pub(super) const MAX_TARGETS: usize = 32;
const MAX_NODES: usize = 64;
const MAX_ATTEMPTS: usize = 8192;

pub(super) struct Safety {
    reference_captures: FxHashSet<RcLocal>,
    attempts_left: Cell<usize>,
}

impl Safety {
    pub(super) fn new(body: &Block) -> Self {
        let mut reference_captures = FxHashSet::default();
        captures(&body.0, &mut reference_captures);
        Self {
            reference_captures,
            attempts_left: Cell::new(MAX_ATTEMPTS),
        }
    }

    pub(super) fn spend_attempt(&self) -> bool {
        let remaining = self.attempts_left.get();
        self.attempts_left.set(remaining.saturating_sub(1));
        remaining != 0
    }

    pub(super) fn stable(&self, arg: &RValue) -> bool {
        match arg {
            RValue::Local(local) => !self.reference_captures.contains(local),
            RValue::Literal(_) => true,
            _ => false,
        }
    }
}

fn captures(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for stmt in stmts {
        match stmt {
            Statement::If(s) => {
                captures(&s.then_block.lock().0, out);
                captures(&s.else_block.lock().0, out);
            }
            Statement::While(s) => captures(&s.block.lock().0, out),
            Statement::Repeat(s) => captures(&s.block.lock().0, out),
            Statement::NumericFor(s) => captures(&s.block.lock().0, out),
            Statement::GenericFor(s) => captures(&s.block.lock().0, out),
            _ => {}
        }
        for value in stmt_rvalues(stmt) {
            captures_in_value(value, out);
        }
    }
}

fn captures_in_value(value: &RValue, out: &mut FxHashSet<RcLocal>) {
    if let RValue::Closure(closure) = value {
        for capture in &closure.upvalues {
            if let Upvalue::Ref(local) = capture {
                out.insert(local.clone());
            }
        }
        captures(&closure.function.0.lock().body.0, out);
    } else {
        for child in value.rvalues() {
            captures_in_value(child, out);
        }
    }
}

pub(crate) fn pattern(function: &Function) -> Option<RValue> {
    if function.bytecode_proto_id.is_none()
        || !function
            .name
            .as_deref()
            .is_some_and(crate::valid_source_name)
        || function.is_variadic
        || function.parameters.is_empty()
        || function.parameters.len() > 8
    {
        return None;
    }
    let params = function.parameters.iter().cloned().collect();
    let mut budget = MAX_NODES;
    let result = return_tree(&function.body.0, Some(&params), &mut budget, 0)?;
    // Reconstruction requires an argument for every parameter. An unused
    // parameter cannot bind from this pattern, so this is not a callable
    // candidate and must not veto the optional loop-synthesis pass either.
    let reads = result.values_read();
    if !params.iter().all(|parameter| reads.contains(&parameter)) { return None; }
    // At least three operators/selection nodes: a simple x * 2 is too generic.
    if operators(&result) < 3 {
        return None;
    }
    Some(result)
}

fn return_tree(
    stmts: &[Statement],
    params: Option<&FxHashSet<RcLocal>>,
    budget: &mut usize,
    depth: usize,
) -> Option<RValue> {
    if depth > 8 {
        return None;
    }
    match stmts {
        [Statement::Assign(decl), Statement::If(branch), Statement::Return(ret)]
            if decl.prefix && !decl.parallel && decl.left.len() == 1
                && (decl.right.is_empty() || matches!(decl.right.as_slice(), [RValue::Literal(Literal::Nil)]))
                && ret.values.len() == 1 => {
            let crate::LValue::Local(result) = &decl.left[0] else { return None; };
            if !matches!(&ret.values[0], RValue::Local(l) if l == result)
                || params.is_some_and(|p| p.contains(result))
                || !allowed(&branch.condition, params, budget)
                || branch.condition.values_read().contains(&result) { return None; }
            let yes = assigned_result(&branch.then_block.lock().0, result, params, budget, depth + 1)?;
            let no = assigned_result(&branch.else_block.lock().0, result, params, budget, depth + 1)?;
            *budget = budget.checked_sub(1)?;
            Some(IfExpression::new(branch.condition.clone(), yes, no).into())
        }
        [Statement::Assign(assign), rest @ ..]
            if assign.prefix && !assign.parallel && assign.left.len() == 1
                && assign.right.len() == 1 && !rest.is_empty() => {
            let crate::LValue::Local(local) = &assign.left[0] else { return None; };
            if params.is_some_and(|p| p.contains(local))
                || !allowed(&assign.right[0], params, budget)
                || assign.right[0].values_read().contains(&local) { return None; }
            let mut extended = params.cloned();
            if let Some(p) = &mut extended { p.insert(local.clone()); }
            let mut result = return_tree(rest, extended.as_ref(), budget, depth + 1)?;
            let destination = Statement::Return(crate::Return::new(vec![result.clone()]));
            // A let is substituted exactly once and only at an evaluation slot
            // it can reach. This preserves metamethod order and skipped arms.
            if !crate::evaluation_order::can_sink(&destination, local, &assign.right[0], &|_| false) {
                return None;
            }
            fn substitute(value: &mut RValue, local: &RcLocal, replacement: &RValue) {
                if matches!(value, RValue::Local(l) if l == local) { *value = replacement.clone(); }
                else { for child in value.rvalues_mut() { substitute(child, local, replacement); } }
            }
            substitute(&mut result, local, &assign.right[0]);
            Some(result)
        }
        [Statement::Return(ret)] if ret.values.len() == 1 => {
            allowed(&ret.values[0], params, budget).then(|| ret.values[0].clone())
        }
        [Statement::If(branch)] => {
            if !allowed(&branch.condition, params, budget) {
                return None;
            }
            let yes = return_tree(&branch.then_block.lock().0, params, budget, depth + 1)?;
            let no = return_tree(&branch.else_block.lock().0, params, budget, depth + 1)?;
            *budget = budget.checked_sub(1)?;
            Some(IfExpression::new(branch.condition.clone(), yes, no).into())
        }
        [Statement::If(branch), rest @ ..]
            if !rest.is_empty() && branch.else_block.lock().0.is_empty() =>
        {
            if !allowed(&branch.condition, params, budget) {
                return None;
            }
            let yes = return_tree(&branch.then_block.lock().0, params, budget, depth + 1)?;
            let no = return_tree(rest, params, budget, depth + 1)?;
            *budget = budget.checked_sub(1)?;
            Some(IfExpression::new(branch.condition.clone(), yes, no).into())
        }
        _ => None,
    }
}

// A private phi/select result has one terminal scalar store per path. An empty
// arm keeps the declaration's nil. No control/statement follows an arm store,
// so converting it to an internal return cannot skip a later evaluation.
fn assigned_result(stmts: &[Statement], result: &RcLocal, params: Option<&FxHashSet<RcLocal>>, budget: &mut usize, depth: usize) -> Option<RValue> {
    if depth > 8 { return None; }
    match stmts {
        [] => { *budget = budget.checked_sub(1)?; Some(Literal::Nil.into()) }
        [Statement::Assign(assign)] if !assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1 => {
            if !matches!(&assign.left[0], crate::LValue::Local(l) if l == result)
                || !allowed(&assign.right[0], params, budget)
                || assign.right[0].values_read().contains(&result) { return None; }
            Some(assign.right[0].clone())
        }
        [Statement::If(branch)] => {
            if !allowed(&branch.condition, params, budget) || branch.condition.values_read().contains(&result) { return None; }
            let yes = assigned_result(&branch.then_block.lock().0, result, params, budget, depth + 1)?;
            let no = assigned_result(&branch.else_block.lock().0, result, params, budget, depth + 1)?;
            *budget = budget.checked_sub(1)?;
            Some(IfExpression::new(branch.condition.clone(), yes, no).into())
        }
        _ => None,
    }
}

pub(super) fn region(statements: &[Statement]) -> Option<RValue> {
    if statements.is_empty() || statements.len() > 8 { return None; }
    let mut budget = MAX_NODES;
    return_tree(statements, None, &mut budget, 0)
}

fn allowed(value: &RValue, params: Option<&FxHashSet<RcLocal>>, budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    match value {
        RValue::Local(local) => params.is_none_or(|params| params.contains(local)),
        // The formatter renders +/-pi and infinity via math globals. Moving
        // those apparent literals into another function could change its
        // environment lookup; they are not closed scalar syntax. NaN literals
        // are also left out (runtime NaN values in local arguments are fine).
        RValue::Literal(Literal::Number(number)) => {
            number.is_finite() && number.abs().to_bits() != std::f64::consts::PI.to_bits()
        }
        RValue::Literal(Literal::Boolean(_) | Literal::Nil) => true,
        RValue::Unary(unary)
            if matches!(
                unary.operation,
                UnaryOperation::Negate | UnaryOperation::Not
            ) =>
        {
            allowed(&unary.value, params, budget)
        }
        RValue::Binary(binary) if binary.operation != BinaryOperation::Concat => {
            allowed(&binary.left, params, budget) && allowed(&binary.right, params, budget)
        }
        RValue::IfExpression(expr) => {
            allowed(&expr.condition, params, budget)
                && allowed(&expr.then_value, params, budget)
                && allowed(&expr.else_value, params, budget)
        }
        _ => false, // no free captures, globals, calls, constructors, indices or result packs
    }
}

fn operators(value: &RValue) -> usize {
    usize::from(matches!(
        value,
        RValue::Unary(_) | RValue::Binary(_) | RValue::IfExpression(_)
    )) + value
        .rvalues()
        .iter()
        .map(|value| operators(value))
        .sum::<usize>()
}

pub(super) fn unify(
    ctx: &MatchCtx,
    pattern: &RValue,
    candidate: &RValue,
    bindings: &mut Bindings,
) -> bool {
    let mut budget = MAX_NODES;
    if !allowed(candidate, None, &mut budget) {
        return false;
    }
    unify_tree(ctx, pattern, candidate, bindings).is_ok()
}

fn unify_tree(
    ctx: &MatchCtx,
    pattern: &RValue,
    candidate: &RValue,
    bindings: &mut Bindings,
) -> Result<(), ()> {
    if let RValue::IfExpression(pattern) = pattern {
        let (condition, yes, no) = match candidate {
            RValue::IfExpression(candidate) => (
                &*candidate.condition,
                &*candidate.then_value,
                &*candidate.else_value,
            ),
            RValue::Binary(candidate) if candidate.operation == BinaryOperation::Or => {
                let RValue::Binary(and) = &*candidate.left else {
                    return Err(());
                };
                if and.operation != BinaryOperation::And
                    || !matches!(
                        &*and.right,
                        RValue::Literal(Literal::Number(_) | Literal::Boolean(true))
                    )
                {
                    return Err(());
                }
                (&*and.left, &*and.right, &*candidate.right)
            }
            _ => return Err(()),
        };
        unify_tree(ctx, &pattern.condition, condition, bindings)?;
        unify_tree(ctx, &pattern.then_value, yes, bindings)?;
        unify_tree(ctx, &pattern.else_value, no, bindings)
    } else {
        unify_rvalue(ctx, pattern, candidate, bindings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_parameters_do_not_create_unmatchable_helper_candidates() {
        let value = crate::RcLocal::new(crate::Local::new(Some("value".into())));
        let unused = crate::RcLocal::new(crate::Local::new(Some("unused".into())));
        let mut expression: RValue = value.clone().into();
        for factor in [2.0, 3.0, 4.0] {
            expression = crate::Binary::new(expression, Literal::Number(factor).into(), BinaryOperation::Mul).into();
        }
        let mut function = Function {
            bytecode_proto_id: Some(0), name: Some("calculate".into()),
            parameters: vec![value, unused],
            body: Block(vec![crate::Return::new(vec![expression]).into()]),
            ..Default::default()
        };
        assert!(pattern(&function).is_none());
        function.parameters.pop();
        assert!(pattern(&function).is_some());
    }

    #[test]
    fn node_and_attempt_budgets_fail_closed() {
        let safety = Safety::new(&Block(vec![]));
        for _ in 0..MAX_ATTEMPTS {
            assert!(safety.spend_attempt());
        }
        assert!(!safety.spend_attempt());
        assert!(!safety.spend_attempt());
        let mut value = RValue::Literal(Literal::Number(1.0));
        for _ in 0..MAX_NODES {
            value = crate::Unary {
                value: Box::new(value),
                operation: UnaryOperation::Negate,
            }
            .into();
        }
        let mut budget = MAX_NODES;
        assert!(!allowed(&value, None, &mut budget));
        assert_eq!(budget, 0);
    }

    #[test]
    fn refuses_literals_formatted_as_environment_lookups() {
        for number in [
            std::f64::consts::PI,
            -std::f64::consts::PI,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ] {
            let mut budget = MAX_NODES;
            assert!(!allowed(&Literal::Number(number).into(), None, &mut budget));
        }
    }
}
