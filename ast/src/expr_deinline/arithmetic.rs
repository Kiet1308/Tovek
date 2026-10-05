//! Exact scalar reconstruction for a named bytecode prototype. The match is
//! evidence for an equivalent call, not proof that the source contained a call.
//! No algebra, type-based purity assumption, or partial arithmetic evaluator.

use std::cell::Cell;

use rustc_hash::FxHashSet;

use crate::deinline::{Bindings, MatchCtx, unify_rvalue};
use crate::{
    BinaryOperation, Function, IfExpression, Literal, RValue, RcLocal, Statement, Traverse,
    UnaryOperation, LocalRw,
};

pub(super) const MARKER: &str =
    "equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown";
pub(super) const MAX_TARGETS: usize = 32;
const MAX_NODES: usize = 64;
const MAX_ATTEMPTS: usize = 8192;

// CaptureSafety is the shared complete capture proof for both expression
// families. Arithmetic additionally limits attempted matches, independently
// of the global search budget and without retaining duplicate local owners.
pub(super) struct AttemptBudget {
    attempts_left: Cell<usize>,
}

impl Default for AttemptBudget {
    fn default() -> Self { Self { attempts_left: Cell::new(MAX_ATTEMPTS) } }
}

impl AttemptBudget {
    pub(super) fn spend_attempt(&self) -> bool {
        let remaining = self.attempts_left.get();
        self.attempts_left.set(remaining.saturating_sub(1));
        remaining != 0
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
    // The helper's own locals are registers no closure of its body shares.
    let result = return_tree(&function.body.0, Some(&params), &mut budget, 0, &|_| false)?;
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

/// `captured`: whether code a moved operation runs (a metamethod) may change
/// a local, so a let may not sink past a read of it.
fn return_tree(
    stmts: &[Statement],
    params: Option<&FxHashSet<RcLocal>>,
    budget: &mut usize,
    depth: usize,
    captured: &dyn Fn(&RcLocal) -> bool,
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
                || branch.condition.any_local_read(&mut |local| local == result) { return None; }
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
                || assign.right[0].any_local_read(&mut |read| read == local) { return None; }
            let mut extended = params.cloned();
            if let Some(p) = &mut extended { p.insert(local.clone()); }
            let mut result = return_tree(rest, extended.as_ref(), budget, depth + 1, captured)?;
            let destination = Statement::Return(crate::Return::new(vec![result.clone()]));
            // A let is substituted exactly once and only at an evaluation slot
            // it can reach. This preserves metamethod order and skipped arms.
            if !crate::evaluation_order::can_sink(&destination, local, &assign.right[0], &|read: &RcLocal| captured(read)) {
                return None;
            }
            fn substitute(value: &mut RValue, local: &RcLocal, replacement: &RValue) {
                if matches!(value, RValue::Local(l) if l == local) { *value = replacement.clone(); }
                else { value.visit_rvalues_mut(&mut |child| { substitute(child, local, replacement); true }); }
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
            let yes = return_tree(&branch.then_block.lock().0, params, budget, depth + 1, captured)?;
            let no = return_tree(&branch.else_block.lock().0, params, budget, depth + 1, captured)?;
            *budget = budget.checked_sub(1)?;
            Some(IfExpression::new(branch.condition.clone(), yes, no).into())
        }
        [Statement::If(branch), rest @ ..]
            if !rest.is_empty() && branch.else_block.lock().0.is_empty() =>
        {
            if !allowed(&branch.condition, params, budget) {
                return None;
            }
            let yes = return_tree(&branch.then_block.lock().0, params, budget, depth + 1, captured)?;
            let no = return_tree(rest, params, budget, depth + 1, captured)?;
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
                || assign.right[0].any_local_read(&mut |local| local == result) { return None; }
            Some(assign.right[0].clone())
        }
        [Statement::If(branch)] => {
            if !allowed(&branch.condition, params, budget) || branch.condition.any_local_read(&mut |local| local == result) { return None; }
            let yes = assigned_result(&branch.then_block.lock().0, result, params, budget, depth + 1)?;
            let no = assigned_result(&branch.else_block.lock().0, result, params, budget, depth + 1)?;
            *budget = budget.checked_sub(1)?;
            Some(IfExpression::new(branch.condition.clone(), yes, no).into())
        }
        _ => None,
    }
}

/// A caller's region as one expression. Its locals may be cells a closure
/// shares (`captures`): a let does not sink past a read of one that code may
/// change.
pub(super) fn region(statements: &[Statement], captures: &crate::deinline_safety::CaptureSafety) -> Option<RValue> {
    if statements.is_empty() || statements.len() > 8 { return None; }
    let mut budget = MAX_NODES;
    return_tree(statements, None, &mut budget, 0, &|local| !captures.stable(&RValue::Local(local.clone())))
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
    let mut children = 0;
    value.visit_rvalues(&mut |child| { children += operators(child); true });
    usize::from(matches!(value, RValue::Unary(_) | RValue::Binary(_) | RValue::IfExpression(_))) + children
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
                // `c and y or n` is `if c then y else n` only for a truthy `y`.
                if and.operation != BinaryOperation::And
                    || !matches!(
                        &*and.right,
                        RValue::Literal(
                            Literal::Number(_)
                                | Literal::String(_)
                                | Literal::Vector(..)
                                | Literal::VectorD(..)
                                | Literal::Boolean(true)
                        )
                    )
                {
                    return Err(());
                }
                (&*and.left, &*and.right, &*candidate.right)
            }
            _ => return Err(()),
        };
        // `if not c then a else b` is `if c then b else a` for any `c`: the
        // helper's guard and its inlined select may disagree on polarity
        // (`if x > 0.2 then return f(x) end return 0` against
        // `not (v > 0.2) and 0 or f(v)`).
        let (pattern_condition, pattern_negated) = strip_not(&pattern.condition);
        let (condition, negated) = strip_not(condition);
        let (yes, no) = if pattern_negated == negated { (yes, no) } else { (no, yes) };
        unify_tree(ctx, pattern_condition, condition, bindings)?;
        unify_tree(ctx, &pattern.then_value, yes, bindings)?;
        unify_tree(ctx, &pattern.else_value, no, bindings)
    } else {
        // A selection may sit inside arithmetic or beneath a unary operator.
        // Keep every operator and operand position exact, but give each child
        // the same selection proof as the root. Delegating the whole wrapper
        // to `unify_rvalue` would miss an equivalent nested select. Leaves still
        // use its bind-once parameters and bit-exact literal checks; there is
        // no alternative search, reassociation or algebra here. `allowed`
        // has already bounded the complete candidate to MAX_NODES.
        match (pattern, candidate) {
            (RValue::Unary(pattern), RValue::Unary(candidate))
                if pattern.operation == candidate.operation =>
            {
                unify_tree(ctx, &pattern.value, &candidate.value, bindings)
            }
            (RValue::Binary(pattern), RValue::Binary(candidate))
                if pattern.operation == candidate.operation =>
            {
                unify_tree(ctx, &pattern.left, &candidate.left, bindings)?;
                unify_tree(ctx, &pattern.right, &candidate.right, bindings)
            }
            _ => unify_rvalue(ctx, pattern, candidate, bindings),
        }
    }
}

/// A condition without its outer `not`, and whether one was removed.
fn strip_not(condition: &RValue) -> (&RValue, bool) {
    match condition {
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => (&unary.value, true),
        _ => (condition, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, Block, Call, Closure, Local, Return, Unary};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.into())))
    }

    fn number(value: f64) -> RValue { Literal::Number(value).into() }

    fn binary(left: RValue, operation: BinaryOperation, right: RValue) -> RValue {
        Binary::new(left, right, operation).into()
    }

    fn negate(value: RValue) -> RValue { Unary::new(value, UnaryOperation::Not).into() }

    /// Both selections sit below exact arithmetic and unary wrappers. The
    /// lowered tree uses a truthy `and/or` arm and reverses an inner if guard.
    fn nested_curve(value: RValue, factor: RValue, lowered: bool) -> RValue {
        let inner_condition = binary(value.clone(), BinaryOperation::LessThan, number(5.0));
        let inner = if lowered {
            IfExpression::new(negate(inner_condition), number(4.0), number(3.0)).into()
        } else {
            IfExpression::new(inner_condition, number(3.0), number(4.0)).into()
        };
        let high = binary(binary(factor, BinaryOperation::Mul, number(2.0)), BinaryOperation::Add, inner);
        let condition = binary(value, BinaryOperation::GreaterThan, number(0.0));
        let selected = if lowered {
            binary(binary(negate(condition), BinaryOperation::And, number(1.0)), BinaryOperation::Or, high)
        } else {
            IfExpression::new(condition, high, number(1.0)).into()
        };
        Unary::new(binary(selected, BinaryOperation::Sub, number(7.0)), UnaryOperation::Negate).into()
    }

    fn matches(pattern: &RValue, candidate: &RValue, params: &[RcLocal]) -> bool {
        let parameters = params.iter().cloned().collect();
        let locals = FxHashSet::default();
        unify(&MatchCtx { params: &parameters, locals: &locals }, pattern, candidate, &mut Bindings::default())
    }

    #[test]
    fn nested_selects_match_below_exact_arithmetic_and_unary_operators() {
        let (value, factor, argument, multiplier) = (local("value"), local("factor"), local("argument"), local("multiplier"));
        let pattern = nested_curve(value.clone().into(), factor.clone().into(), false);
        let candidate = nested_curve(argument.clone().into(), multiplier.clone().into(), true);
        let params = FxHashSet::from_iter([value.clone(), factor.clone()]);
        let locals = FxHashSet::default();
        let context = MatchCtx { params: &params, locals: &locals };
        let mut bindings = Bindings::default();
        assert!(unify(&context, &pattern, &candidate, &mut bindings));
        assert_eq!(bindings.params.len(), 2);
        assert!(matches!(bindings.params.get(&value), Some(RValue::Local(local)) if local == &argument));
        assert!(matches!(bindings.params.get(&factor), Some(RValue::Local(local)) if local == &multiplier));
        // This is an intentional coverage extension. Keep the independent
        // legacy oracle frozen rather than making its result agree with us.
        assert!(!crate::expr_deinline::reference::arithmetic::unify(
            &context, &pattern, &candidate, &mut Bindings::default(),
        ));
    }

    #[test]
    fn nested_selects_preserve_operators_operand_order_and_repeated_bindings() {
        let (value, factor, argument, multiplier) = (local("value"), local("factor"), local("argument"), local("multiplier"));
        let pattern = nested_curve(value.clone().into(), factor.clone().into(), false);
        for changed in 0..4 {
            let mut candidate = nested_curve(argument.clone().into(), multiplier.clone().into(), true);
            let RValue::Unary(unary) = &mut candidate else { unreachable!() };
            let RValue::Binary(outer) = &mut *unary.value else { unreachable!() };
            match changed {
                0 => unary.operation = UnaryOperation::Not,
                1 => outer.operation = BinaryOperation::Add,
                2 => std::mem::swap(&mut outer.left, &mut outer.right),
                _ => {
                    let RValue::Binary(selection) = &mut *outer.left else { unreachable!() };
                    let RValue::Binary(high) = &mut *selection.right else { unreachable!() };
                    let RValue::IfExpression(inner) = &mut *high.right else { unreachable!() };
                    let RValue::Unary(not) = &mut *inner.condition else { unreachable!() };
                    let RValue::Binary(condition) = &mut *not.value else { unreachable!() };
                    condition.left = Box::new(local("different_argument").into());
                }
            }
            assert!(!matches(&pattern, &candidate, &[value.clone(), factor.clone()]), "changed case {changed}");
        }
    }

    #[test]
    fn nested_selects_require_truthy_arms_and_preserve_float_bits() {
        let (value, bias, argument) = (local("value"), local("bias"), local("argument"));
        let wrapper = |selected| binary(selected, BinaryOperation::Add, number(4.0));
        let condition = |value| binary(value, BinaryOperation::LessThan, number(0.0));
        let pattern = wrapper(IfExpression::new(condition(value.clone().into()), bias.clone().into(), value.clone().into()).into());
        let lowered = |arm| wrapper(binary(
            binary(condition(argument.clone().into()), BinaryOperation::And, arm),
            BinaryOperation::Or, argument.clone().into(),
        ));
        // Zero (including -0) is truthy in Luau. False, nil and an unknown
        // local can fall through the `or`, so cannot stand for an if arm.
        for arm in [number(0.0), number(-0.0), Literal::Boolean(true).into()] {
            assert!(matches(&pattern, &lowered(arm), &[value.clone(), bias.clone()]));
        }
        for arm in [Literal::Boolean(false).into(), Literal::Nil.into(), local("unknown").into()] {
            assert!(!matches(&pattern, &lowered(arm), &[value.clone(), bias.clone()]));
        }
        let exact_zero = wrapper(IfExpression::new(condition(value.clone().into()), number(-0.0), value.clone().into()).into());
        assert!(matches(&exact_zero, &lowered(number(-0.0)), &[value.clone()]));
        assert!(!matches(&exact_zero, &lowered(number(0.0)), &[value]));
    }

    #[test]
    fn nested_selects_do_not_invert_ordered_comparisons_or_reassociate_arithmetic() {
        let (value, argument) = (local("value"), local("argument"));
        let condition = binary(value.clone().into(), BinaryOperation::LessThan, number(0.0));
        let pattern_select: RValue = IfExpression::new(condition, number(2.0), value.clone().into()).into();
        let pattern = binary(binary(pattern_select, BinaryOperation::Add, number(3.0)), BinaryOperation::Add, number(4.0));
        // For NaN, `x < 0` and `not (x >= 0)` have different answers.
        let inverse = negate(binary(argument.clone().into(), BinaryOperation::GreaterThanOrEqual, number(0.0)));
        let wrong_select: RValue = IfExpression::new(inverse, number(2.0), argument.clone().into()).into();
        let wrong_comparison = binary(binary(wrong_select, BinaryOperation::Add, number(3.0)), BinaryOperation::Add, number(4.0));
        assert!(!matches(&pattern, &wrong_comparison, &[value.clone()]));
        let selected = binary(
            binary(binary(argument.clone().into(), BinaryOperation::LessThan, number(0.0)), BinaryOperation::And, number(2.0)),
            BinaryOperation::Or, argument.into(),
        );
        let regrouped = binary(selected, BinaryOperation::Add, binary(number(3.0), BinaryOperation::Add, number(4.0)));
        assert!(!matches(&pattern, &regrouped, &[value]));
    }

    fn declaration(name: &RcLocal, params: Vec<RcLocal>, expression: RValue) -> Statement {
        let function = Function {
            name: Some("nestedCurve".into()), bytecode_proto_id: Some(7), parameters: params,
            body: Block(vec![Return::new(vec![expression]).into()]), ..Default::default()
        };
        let mut assign = Assign::new(vec![name.clone().into()], vec![Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(function))),
            upvalues: vec![],
        }.into()]);
        assign.prefix = true;
        assign.into()
    }

    #[test]
    fn nested_selection_reconstruction_preserves_ambiguity_and_argument_safety() {
        for scenario in 0..5 {
            let (helper, value, factor, argument, multiplier) = (
                local("nestedCurve"), local("value"), local("factor"), local("argument"), local("multiplier"),
            );
            let expression = nested_curve(value.clone().into(), factor.clone().into(), false);
            let mut block = Block(vec![]);
            // A failed candidate may bind parameters before its final literal
            // differs; none of those bindings may contaminate the next helper.
            if scenario == 1 {
                let mut wrong = nested_curve(factor.clone().into(), value.clone().into(), false);
                let RValue::Unary(unary) = &mut wrong else { unreachable!() };
                let RValue::Binary(outer) = &mut *unary.value else { unreachable!() };
                outer.right = Box::new(number(8.0));
                block.0.push(declaration(&local("nearMiss"), vec![value.clone(), factor.clone()], wrong));
            }
            block.0.push(declaration(&helper, vec![value.clone(), factor.clone()], expression.clone()));
            if scenario == 2 {
                block.0.push(declaration(&local("equallyGood"), vec![value.clone(), factor.clone()], expression));
            }
            if scenario == 3 {
                let mut writer = declaration(&local("writer"), vec![], number(0.0));
                let Statement::Assign(assign) = &mut writer else { unreachable!() };
                let RValue::Closure(closure) = &mut assign.right[0] else { unreachable!() };
                closure.upvalues.push(crate::Upvalue::Ref(multiplier.clone()));
                closure.function.0.lock().body = Block(vec![Assign::new(vec![multiplier.clone().into()], vec![number(9.0)]).into()]);
                block.0.push(writer);
            }
            let argument_factor = if scenario == 4 {
                // Evaluating this potentially throwing operation eagerly would
                // change the path on which value <= 0 skips the factor entirely.
                binary(multiplier.clone().into(), BinaryOperation::Add, number(1.0))
            } else { multiplier.clone().into() };
            block.0.push(Return::new(vec![nested_curve(argument.clone().into(), argument_factor, true)]).into());
            let before = block.to_string();
            crate::expr_deinline::expr_deinline(&mut block);
            let Statement::Return(result) = block.0.last().unwrap() else { unreachable!() };
            if scenario < 2 {
                let RValue::Call(Call { value: callee, arguments, .. }) = &result.values[0] else { panic!("{block}") };
                assert!(matches!(callee.as_ref(), RValue::Local(local) if local == &helper));
                assert_eq!(arguments, &vec![RValue::Local(argument), RValue::Local(multiplier)]);
                assert!(block.0.iter().any(|statement| matches!(statement, Statement::Comment(comment) if comment.text == MARKER)));
            } else {
                assert_eq!(block.to_string(), before, "unsafe/ambiguous scenario {scenario}");
            }
        }
    }

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
        let safety = AttemptBudget::default();
        for _ in 0..MAX_ATTEMPTS {
            assert!(safety.spend_attempt());
        }
        assert!(!safety.spend_attempt());
        assert!(!safety.spend_attempt());
        let mut value = RValue::Literal(Literal::Number(1.0));
        for _ in 0..MAX_NODES {
            value = crate::Unary {
                node_origin: Default::default(),
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
