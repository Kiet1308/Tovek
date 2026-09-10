//! Exact scalar reconstruction for a named bytecode prototype. The match is
//! evidence for an equivalent call, not proof that the source contained a call.
//! No algebra, type-based purity assumption, or partial arithmetic evaluator.

use std::cell::Cell;

use rustc_hash::FxHashSet;

use crate::deinline::{Bindings, MatchCtx, stmt_rvalues, unify_rvalue};
use crate::{
    BinaryOperation, Block, Function, IfExpression, Literal, RValue, RcLocal, Statement, Traverse,
    UnaryOperation, Upvalue,
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

pub(super) fn pattern(function: &Function) -> Option<RValue> {
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
    let result = return_tree(&function.body.0, &params, &mut budget, 0)?;
    // At least three operators/selection nodes: a simple x * 2 is too generic.
    if operators(&result) < 3 {
        return None;
    }
    Some(result)
}

fn return_tree(
    stmts: &[Statement],
    params: &FxHashSet<RcLocal>,
    budget: &mut usize,
    depth: usize,
) -> Option<RValue> {
    if depth > 4 {
        return None;
    }
    match stmts {
        [Statement::Return(ret)] if ret.values.len() == 1 => {
            allowed(&ret.values[0], Some(params), budget).then(|| ret.values[0].clone())
        }
        [Statement::If(branch)] => {
            if !allowed(&branch.condition, Some(params), budget) {
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
            if !allowed(&branch.condition, Some(params), budget) {
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
