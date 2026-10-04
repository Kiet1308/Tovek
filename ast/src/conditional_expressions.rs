use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

use crate::{
    deinline_safety::CaptureSafety, Binary, BinaryOperation, Block, If, IfExpression, Index, LValue, Literal, LocalRw, RValue,
    RcLocal, Select, Statement, Traverse, Unary, UnaryOperation,
};

#[derive(Default)]
struct Usage {
    reads: usize,
    writes: usize,
    captured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UseContext {
    Direct,
    IndexReceiver,
    Nested,
}

const MAX_NON_OPTIONAL_EXPRESSION_COST: usize = 80;

/// Reconstruct Luau conditional expressions from branch-assigned temporary locals.
///
/// This pass intentionally starts with the bytecode shape emitted for
/// single-value branch assignment:
///
/// ```lua
/// local v
/// if c then
///     v = a
/// else
///     v = b
/// end
/// use(v)
/// ```
///
/// and rewrites only the immediately following single use. That preserves the
/// branch RHS evaluation point except for expression-order details that are
/// guarded below.
pub fn reconstruct_conditional_expressions(block: &mut Block) {
    let safety = CaptureSafety::new(block);
    reconstruct_with_style(block, true, Scope { safety: &safety, function: None });
}

/// Reconstruct only value-exact boolean short-circuit idioms. Branch values
/// outside this subset stay as assignments; this never emits IfExpression.
pub fn reconstruct_short_circuit_expressions(block: &mut Block) {
    let safety = CaptureSafety::new(block);
    reconstruct_with_style(block, false, Scope { safety: &safety, function: None });
}

/// Where a block runs: the module's capture census and the function owning
/// the block (`None`: the chunk). A local a closure anywhere captures,
/// including an upvalue of that function, may change during any call.
#[derive(Clone, Copy)]
struct Scope<'a> {
    safety: &'a CaptureSafety,
    function: Option<usize>,
}

fn reconstruct_with_style(block: &mut Block, allow_if_expression: bool, scope: Scope) {
    reconstruct_nested_blocks(block, allow_if_expression, scope);
    reconstruct_current_block(block, allow_if_expression, scope);
}

fn reconstruct_nested_blocks(block: &mut Block, allow_if_expression: bool, scope: Scope) {
    for statement in &mut block.0 {
        reconstruct_nested_in_statement(statement, allow_if_expression, scope);
    }
}

fn reconstruct_nested_in_statement(statement: &mut Statement, allow_if_expression: bool, scope: Scope) {
    reconstruct_closures_in_statement(statement, allow_if_expression, scope);
    match statement {
        Statement::If(r#if) => {
            reconstruct_with_style(&mut r#if.then_block.lock(), allow_if_expression, scope);
            reconstruct_with_style(&mut r#if.else_block.lock(), allow_if_expression, scope);
        }
        Statement::While(r#while) => reconstruct_with_style(&mut r#while.block.lock(), allow_if_expression, scope),
        Statement::Repeat(repeat) => reconstruct_with_style(&mut repeat.block.lock(), allow_if_expression, scope),
        Statement::NumericFor(numeric_for) => {
            reconstruct_with_style(&mut numeric_for.block.lock(), allow_if_expression, scope)
        }
        Statement::GenericFor(generic_for) => {
            reconstruct_with_style(&mut generic_for.block.lock(), allow_if_expression, scope)
        }
        _ => {}
    }
}

fn reconstruct_closures_in_statement(statement: &mut Statement, allow_if_expression: bool, scope: Scope) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        let scope = Scope { function: Some(triomphe::Arc::as_ptr(&function.0) as usize), ..scope };
        reconstruct_with_style(&mut function.lock().body, allow_if_expression, scope);
    }
}

/// Successful rewrites remove only the candidate binder's one read and three
/// writes. Every other local occurrence moves intact, including condition
/// captures; short-circuit construction only discards literal boolean arms.
/// Thus one usage census remains valid throughout this block's fixed point.
fn reconstruct_current_block(block: &mut Block, allow_if_expression: bool, scope: Scope) {
    let len = block.len();
    if len < 3 { return; }
    let mut pending: BTreeSet<_> = block.iter().enumerate()
        .filter_map(|(index, statement)| candidate_decl(statement).map(|_| index)).collect();
    if pending.is_empty() { return; }
    let usage = collect_usage(block);
    let mut next: Vec<_> = (0..len).map(|index| (index + 1 < len).then_some(index + 1)).collect();
    let mut previous: Vec<_> = (0..len).map(|index| index.checked_sub(1)).collect();
    let mut removed = vec![false; len];
    while let Some(declaration) = pending.pop_first() {
        if removed[declaration] { continue; }
        let Some(branch) = next[declaration] else { continue; };
        let Some(use_index) = next[branch] else { continue; };
        let predecessor = match reconstruct_at(block, declaration, branch, use_index, &usage, allow_if_expression, scope) {
            Fold::None => continue,
            Fold::Use => {
                removed[declaration] = true;
                block[declaration] = crate::Empty {}.into();
                previous[declaration]
            }
            // The declaration now holds the value and is no candidate.
            Fold::Declaration => Some(declaration),
        };
        removed[branch] = true;
        block[branch] = crate::Empty {}.into();
        if let Some(predecessor) = predecessor { next[predecessor] = Some(use_index); }
        previous[use_index] = predecessor;
        // Only triples that touch the new adjacency or edited use can change.
        // Keep the former restart scheduler's earliest-declaration priority.
        let mut affected = Some(use_index);
        for _ in 0..3 {
            let Some(index) = affected else { break; };
            if candidate_decl(&block[index]).is_some() { pending.insert(index); }
            affected = previous[index];
        }
    }
    let mut index = 0;
    block.0.retain(|_| { let keep = !removed[index]; index += 1; keep });
}

/// Where [`reconstruct_at`] put a branch-assigned value.
enum Fold {
    None,
    /// Into its one use; the declaration and the branch are gone.
    Use,
    /// Into the declaration (`local v = c and f()`); the branch is gone.
    Declaration,
}

fn reconstruct_at(block: &mut Block, decl_index: usize, if_index: usize, use_index: usize,
    usage: &FxHashMap<RcLocal, Usage>, allow_if_expression: bool, scope: Scope) -> Fold {
    let Some(local) = candidate_decl(&block.0[decl_index]) else {
        return Fold::None;
    };

    let Some(local_usage) = usage.get(&local) else {
        return Fold::None;
    };
    if local_usage.reads != 1 || local_usage.writes != 3 || local_usage.captured {
        return Fold::None;
    }

    let Statement::If(r#if) = &block.0[if_index] else {
        return Fold::None;
    };
    let Some((condition, then_value, else_value)) = branch_assignments(r#if, &local) else {
        return Fold::None;
    };
    if contains_unsupported_value(&then_value) || contains_unsupported_value(&else_value) {
        return Fold::None;
    }

    if replaceable_direct_rvalue_read_count(&block.0[use_index], &local) != 1 {
        return Fold::None;
    }
    let Some(use_context) = classify_replaceable_use(&block.0[use_index], &local) else {
        return Fold::None;
    };
    if !is_generated_temp(&local) && use_context != UseContext::IndexReceiver {
        return Fold::None;
    }
    if !complexity_allowed(&condition, &then_value, &else_value) {
        return Fold::None;
    }

    let replacement = if allow_if_expression {
        select_expression(condition, then_value, else_value)
    } else if let Some(value) = build_short_circuit(condition, then_value, else_value) {
        value
    } else {
        return Fold::None;
    };
    let captured = |read: &RcLocal| !scope.safety.uncaptured(read);
    let register = |read: &RcLocal| scope.safety.register_of(read, scope.function);
    match replace_direct_rvalue_use(&mut block.0[use_index], &local, replacement, &captured, &register) {
        Ok(()) => Fold::Use,
        // In the use it would run after code it ran before, or be skipped
        // (`v3 or v4`): it stays where the branch computed it.
        Err(replacement) => {
            let mut declaration = crate::Assign::new(vec![local.into()], vec![replacement]);
            declaration.prefix = true;
            block.0[decl_index] = declaration.into();
            Fold::Declaration
        }
    }
}

#[cfg(test)]
fn reconstruct_once(block: &mut Block, allow_if_expression: bool) -> bool {
    if block.0.len() < 3 {
        return false;
    }
    let safety = CaptureSafety::new(block);

    let usage = collect_usage(block);
    for decl_index in 0..block.0.len() - 2 {
        let Some(local) = candidate_decl(&block.0[decl_index]) else {
            continue;
        };

        let Some(local_usage) = usage.get(&local) else {
            continue;
        };
        if local_usage.reads != 1 || local_usage.writes != 3 || local_usage.captured {
            continue;
        }

        let if_index = decl_index + 1;
        let Statement::If(r#if) = &block.0[if_index] else {
            continue;
        };
        let Some((condition, then_value, else_value)) = branch_assignments(r#if, &local) else {
            continue;
        };
        if contains_unsupported_value(&then_value) || contains_unsupported_value(&else_value) {
            continue;
        }

        let use_index = if_index + 1;
        if replaceable_direct_rvalue_read_count(&block.0[use_index], &local) != 1 {
            continue;
        }
        let Some(use_context) = classify_replaceable_use(&block.0[use_index], &local) else {
            continue;
        };
        if !is_generated_temp(&local) && use_context != UseContext::IndexReceiver {
            continue;
        }
        if !complexity_allowed(&condition, &then_value, &else_value) {
            continue;
        }

        let replacement = if allow_if_expression {
            select_expression(condition, then_value, else_value)
        } else if let Some(value) = build_short_circuit(condition, then_value, else_value) {
            value
        } else {
            continue;
        };
        match replace_direct_rvalue_use(&mut block.0[use_index], &local, replacement, &|read| !safety.uncaptured(read), &|_| false) {
            Ok(()) => {
                block.0.remove(if_index);
                block.0.remove(decl_index);
            }
            Err(replacement) => {
                let mut declaration = crate::Assign::new(vec![local.into()], vec![replacement]);
                declaration.prefix = true;
                block.0[decl_index] = declaration.into();
                block.0.remove(if_index);
            }
        }
        return true;
    }

    false
}

fn candidate_decl(statement: &Statement) -> Option<RcLocal> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 {
        return None;
    }
    if !(assign.right.is_empty()
        || matches!(assign.right.as_slice(), [RValue::Literal(Literal::Nil)]))
    {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    (!local.preserve_binding()).then(|| local.clone())
}

fn branch_assignments(r#if: &If, local: &RcLocal) -> Option<(RValue, RValue, RValue)> {
    let then_value = single_local_assignment_value(&r#if.then_block.lock(), local)?;
    let else_value = single_local_assignment_value(&r#if.else_block.lock(), local)?;
    Some((r#if.condition.clone(), then_value, else_value))
}

fn single_local_assignment_value(block: &Block, local: &RcLocal) -> Option<RValue> {
    let [Statement::Assign(assign)] = block.0.as_slice() else {
        return None;
    };
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(assigned) = &assign.left[0] else {
        return None;
    };
    if assigned != local {
        return None;
    }
    Some(assign.right[0].clone())
}

/// The value-exact boolean idiom a select can be written as.
enum ShortCircuit {
    /// `if c then false else true` is `not c`.
    Not,
    /// `if c then false else e` is `not c and e`.
    NotAnd,
    /// `if c then t else true` is `not c or t`.
    NotOr,
    /// `if c then true else false` is `c` for a boolean `c`.
    Condition,
    /// `if c then t else false` is `c and t` for a boolean `c`.
    And,
    /// `if c then true else e` is `c or e` for a boolean `c`.
    Or,
}

fn short_circuit_shape(condition: &RValue, then_value: &RValue, else_value: &RValue) -> Option<ShortCircuit> {
    let then_true = matches!(then_value, RValue::Literal(Literal::Boolean(true)));
    let then_false = matches!(then_value, RValue::Literal(Literal::Boolean(false)));
    let else_true = matches!(else_value, RValue::Literal(Literal::Boolean(true)));
    let else_false = matches!(else_value, RValue::Literal(Literal::Boolean(false)));
    if then_false && else_true {
        return Some(ShortCircuit::Not);
    }
    // `not C` is always boolean, even when C is nil or a non-boolean value.
    if then_false {
        return Some(ShortCircuit::NotAnd);
    }
    if else_true {
        return Some(ShortCircuit::NotOr);
    }
    if !crate::binary::is_boolean(condition) {
        return None;
    }
    if then_true && else_false {
        Some(ShortCircuit::Condition)
    } else if else_false {
        Some(ShortCircuit::And)
    } else if then_true {
        Some(ShortCircuit::Or)
    } else {
        None
    }
}

fn build_shape(shape: ShortCircuit, condition: RValue, then_value: RValue, else_value: RValue) -> RValue {
    let not = |condition| -> RValue { Unary::new(condition, UnaryOperation::Not).into() };
    match shape {
        ShortCircuit::Not => not(condition),
        ShortCircuit::NotAnd => Binary::new(not(condition), else_value, BinaryOperation::And).into(),
        ShortCircuit::NotOr => Binary::new(not(condition), then_value, BinaryOperation::Or).into(),
        ShortCircuit::Condition => condition,
        ShortCircuit::And => Binary::new(condition, then_value, BinaryOperation::And).into(),
        ShortCircuit::Or => Binary::new(condition, else_value, BinaryOperation::Or).into(),
    }
}

fn build_short_circuit(condition: RValue, then_value: RValue, else_value: RValue) -> Option<RValue> {
    let shape = short_circuit_shape(&condition, &then_value, &else_value)?;
    Some(build_shape(shape, condition, then_value, else_value))
}

/// A select written as its value-exact boolean idiom when one exists
/// (`c and t`, `not c or t`, ...), otherwise as an if-expression. An idiom
/// whose remaining operand is `nil` (`not c and nil`) says less than the
/// if-expression it replaces, so that select stays an if-expression.
pub(crate) fn select_expression(condition: RValue, then_value: RValue, else_value: RValue) -> RValue {
    let shape = short_circuit_shape(&condition, &then_value, &else_value).filter(|shape| match shape {
        ShortCircuit::NotAnd => !is_nil(&else_value),
        ShortCircuit::NotOr => !is_nil(&then_value),
        _ => true,
    });
    match shape {
        Some(shape) => build_shape(shape, condition, then_value, else_value),
        None => build_if_expression(condition, then_value, else_value),
    }
}

fn build_if_expression(condition: RValue, then_value: RValue, else_value: RValue) -> RValue {
    let (condition, then_value, else_value) = if is_nil(&then_value) && !is_nil(&else_value) {
        (negate_condition(condition), else_value, then_value)
    } else {
        (condition, then_value, else_value)
    };
    IfExpression::new(condition, then_value, else_value).into()
}

fn is_nil(value: &RValue) -> bool {
    matches!(value, RValue::Literal(Literal::Nil))
}

fn negate_condition(condition: RValue) -> RValue {
    match condition {
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => *unary.value,
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            let operation = match binary.operation {
                BinaryOperation::Equal => BinaryOperation::NotEqual,
                BinaryOperation::NotEqual => BinaryOperation::Equal,
                _ => unreachable!(),
            };
            Binary {
                node_origin: Default::default(),
                left: binary.left,
                right: binary.right,
                operation,
            }
            .into()
        }
        other => Unary::new(other, UnaryOperation::Not).into(),
    }
}

fn contains_unsupported_value(value: &RValue) -> bool {
    match value {
        RValue::VarArg(_) | RValue::Select(Select::VarArg(_)) | RValue::Closure(_) => true,
        _ => !value.visit_rvalues(&mut |child| !contains_unsupported_value(child)),
    }
}

fn complexity_allowed(condition: &RValue, then_value: &RValue, else_value: &RValue) -> bool {
    is_nil(then_value)
        || is_nil(else_value)
        || 1 + crate::expression_budget::expression_cost(condition)
            + crate::expression_budget::expression_cost(then_value)
            + crate::expression_budget::expression_cost(else_value)
            <= MAX_NON_OPTIONAL_EXPRESSION_COST
}

fn collect_usage(block: &Block) -> FxHashMap<RcLocal, Usage> {
    let mut usage = FxHashMap::default();
    collect_usage_in_block(block, &mut usage);
    usage
}

fn collect_usage_in_block(block: &Block, usage: &mut FxHashMap<RcLocal, Usage>) {
    for statement in &block.0 {
        collect_usage_in_statement(statement, usage);
    }
}

fn collect_usage_in_statement(statement: &Statement, usage: &mut FxHashMap<RcLocal, Usage>) {
    statement.visit_local_reads(&mut |local| { usage.entry(local.clone()).or_default().reads += 1; true });
    for local in statement.values_written() {
        usage.entry(local.clone()).or_default().writes += 1;
    }

    let mut functions = Vec::new();
    collect_closures_in_statement(statement, &mut |closure| {
        for upvalue in &closure.upvalues {
            let local = match upvalue {
                crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local,
            };
            usage.entry(local.clone()).or_default().captured = true;
        }
        functions.push(closure.function.clone());
    });
    for function in functions {
        collect_usage_in_block(&function.lock().body, usage);
    }

    match statement {
        Statement::If(r#if) => {
            collect_usage_in_block(&r#if.then_block.lock(), usage);
            collect_usage_in_block(&r#if.else_block.lock(), usage);
        }
        Statement::While(r#while) => collect_usage_in_block(&r#while.block.lock(), usage),
        Statement::Repeat(repeat) => collect_usage_in_block(&repeat.block.lock(), usage),
        Statement::NumericFor(numeric_for) => {
            collect_usage_in_block(&numeric_for.block.lock(), usage)
        }
        Statement::GenericFor(generic_for) => {
            collect_usage_in_block(&generic_for.block.lock(), usage)
        }
        _ => {}
    }
}

fn collect_closures_in_statement(statement: &Statement, f: &mut impl FnMut(&crate::Closure)) {
    statement.visit_rvalues(&mut |rvalue| { collect_closures_in_rvalue(rvalue, f); true });
    statement.visit_lvalues(&mut |lvalue| lvalue.visit_rvalues(&mut |rvalue| { collect_closures_in_rvalue(rvalue, f); true }));
}

fn collect_closures_in_rvalue(rvalue: &RValue, f: &mut impl FnMut(&crate::Closure)) {
    if let RValue::Closure(closure) = rvalue {
        f(closure);
        return;
    }
    rvalue.visit_rvalues(&mut |child| { collect_closures_in_rvalue(child, f); true });
}

fn replaceable_direct_rvalue_read_count(statement: &Statement, local: &RcLocal) -> usize {
    match statement {
        Statement::Assign(assign) => assign
            .right
            .iter()
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        Statement::Return(return_) => return_
            .values
            .iter()
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        Statement::Call(call) => call
            .arguments
            .iter()
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        Statement::MethodCall(method_call) => method_call
            .arguments
            .iter()
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        Statement::SetList(set_list) => set_list
            .values
            .iter()
            .chain(set_list.tail.iter())
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        _ => 0,
    }
}

fn classify_replaceable_use(statement: &Statement, local: &RcLocal) -> Option<UseContext> {
    match statement {
        Statement::Assign(assign) => assign
            .right
            .iter()
            .find_map(|value| classify_rvalue_use(value, local)),
        Statement::Return(return_) => return_
            .values
            .iter()
            .find_map(|value| classify_rvalue_use(value, local)),
        Statement::Call(call) => call
            .arguments
            .iter()
            .find_map(|value| classify_rvalue_use(value, local)),
        Statement::MethodCall(method_call) => method_call
            .arguments
            .iter()
            .find_map(|value| classify_rvalue_use(value, local)),
        Statement::SetList(set_list) => set_list
            .values
            .iter()
            .chain(set_list.tail.iter())
            .find_map(|value| classify_rvalue_use(value, local)),
        _ => None,
    }
}

fn classify_rvalue_use(value: &RValue, local: &RcLocal) -> Option<UseContext> {
    if matches!(value, RValue::Local(read) if read == local) {
        return Some(UseContext::Direct);
    }

    match value {
        RValue::Binary(binary) => classify_rvalue_use(&binary.left, local)
            .or_else(|| classify_rvalue_use(&binary.right, local))
            .map(nest_direct_use),
        RValue::Unary(unary) => classify_rvalue_use(&unary.value, local).map(nest_direct_use),
        RValue::Index(index) => {
            if matches!(index.left.as_ref(), RValue::Local(read) if read == local) {
                Some(UseContext::IndexReceiver)
            } else {
                classify_rvalue_use(&index.left, local)
                    .or_else(|| classify_rvalue_use(&index.right, local))
                    .map(nest_direct_use)
            }
        }
        RValue::Call(call) => call
            .arguments
            .iter()
            .find_map(|value| classify_rvalue_use(value, local))
            .map(nest_direct_use),
        RValue::MethodCall(method_call) => method_call
            .arguments
            .iter()
            .find_map(|value| classify_rvalue_use(value, local))
            .map(nest_direct_use),
        RValue::Table(table) => table
            .0
            .iter()
            .find_map(|(_, value)| classify_rvalue_use(value, local))
            .map(nest_direct_use),
        _ => None,
    }
}

fn nest_direct_use(use_context: UseContext) -> UseContext {
    match use_context {
        UseContext::Direct => UseContext::Nested,
        other => other,
    }
}

fn replaceable_rvalue_read_count(value: &RValue, local: &RcLocal) -> usize {
    if matches!(value, RValue::Local(read) if read == local) {
        return 1;
    }

    match value {
        RValue::Binary(binary) => {
            replaceable_rvalue_read_count(&binary.left, local)
                + replaceable_rvalue_read_count(&binary.right, local)
        }
        RValue::Unary(unary) => replaceable_rvalue_read_count(&unary.value, local),
        RValue::Index(index) => {
            replaceable_rvalue_read_count(&index.left, local)
                + replaceable_rvalue_read_count(&index.right, local)
        }
        RValue::Call(call) => call
            .arguments
            .iter()
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        RValue::MethodCall(method_call) => method_call
            .arguments
            .iter()
            .map(|value| replaceable_rvalue_read_count(value, local))
            .sum(),
        RValue::Table(table) => table
            .0
            .iter()
            .map(|(_, value)| replaceable_rvalue_read_count(value, local))
            .sum(),
        _ => 0,
    }
}

fn replace_direct_rvalue_use(
    statement: &mut Statement,
    local: &RcLocal,
    replacement: RValue,
    captured: &dyn Fn(&RcLocal) -> bool,
    register: &dyn Fn(&RcLocal) -> bool,
) -> Result<(), RValue> {
    // Keep one owned replacement while searching. A failed position only
    // borrows it; the accepted position either moves it or makes the single
    // real copy required by the legacy path's diagnostic origin policy.
    let mut replacement = Some(replacement);
    let replaced = replace_in_statement(statement, local, &mut replacement, captured, register);
    match replacement {
        Some(replacement) if !replaced => Err(replacement),
        _ => Ok(()),
    }
}

fn replace_in_statement(
    statement: &mut Statement,
    local: &RcLocal,
    replacement: &mut Option<RValue>,
    captured: &dyn Fn(&RcLocal) -> bool,
    register: &dyn Fn(&RcLocal) -> bool,
) -> bool {
    match statement {
        Statement::Assign(assign) => {
            let mut before_unsafe = assign
                .left
                .iter()
                .any(|left| lvalue_prior_unsafe(left, captured, register));
            replace_in_rvalue_list(
                &mut assign.right,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::Return(return_) => {
            let mut before_unsafe = false;
            replace_in_rvalue_list(
                &mut return_.values,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::Call(call) => {
            let mut before_unsafe = rvalue_prior_unsafe(&call.value, captured);
            replace_in_rvalue_list(
                &mut call.arguments,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::MethodCall(method_call) => {
            let mut before_unsafe = rvalue_prior_unsafe(&method_call.value, captured);
            replace_in_rvalue_list(
                &mut method_call.arguments,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::SetList(set_list) => {
            let mut before_unsafe = false;
            if replace_in_rvalue_list(
                &mut set_list.values,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            ) {
                return true;
            }
            if let Some(tail) = &mut set_list.tail {
                return replace_first_rvalue_use(
                    tail,
                    local,
                    replacement,
                    captured,
                    &mut before_unsafe,
                    false,
                );
            }
            false
        }
        _ => false,
    }
}

fn replace_in_rvalue_list(
    values: &mut [RValue],
    local: &RcLocal,
    replacement: &mut Option<RValue>,
    captured: &dyn Fn(&RcLocal) -> bool,
    before_unsafe: &mut bool,
) -> bool {
    for value in values {
        if replace_first_rvalue_use(value, local, replacement, captured, before_unsafe, true) {
            return true;
        }
        if rvalue_prior_unsafe(value, captured) {
            *before_unsafe = true;
        }
    }
    false
}

fn replace_first_rvalue_use(
    value: &mut RValue,
    local: &RcLocal,
    replacement: &mut Option<RValue>,
    captured: &dyn Fn(&RcLocal) -> bool,
    before_unsafe: &mut bool,
    copied: bool,
) -> bool {
    if matches!(value, RValue::Local(read) if read == local) {
        if !can_replace_after_prior_eval(replacement.as_ref().unwrap(), *before_unsafe, captured) {
            return false;
        }
        let replacement = replacement.take().unwrap();
        *value = if copied { clone_replacement(&replacement) } else { replacement };
        return true;
    }

    match value {
        RValue::Binary(binary) => {
            if replace_first_rvalue_use(
                &mut binary.left,
                local,
                replacement,
                captured,
                before_unsafe,
                true,
            ) {
                return true;
            }
            if rvalue_prior_unsafe(&binary.left, captured) {
                *before_unsafe = true;
            }
            // The right operand of `and`/`or` may be skipped: a value that
            // can run code must not move from its own statement into it.
            if skippable_operand(binary.operation) && crate::is_observable(replacement.as_ref().unwrap()) {
                return false;
            }
            replace_first_rvalue_use(&mut binary.right, local, replacement, captured, before_unsafe, copied)
        }
        RValue::Unary(unary) => {
            replace_first_rvalue_use(&mut unary.value, local, replacement, captured, before_unsafe, copied)
        }
        RValue::Index(index) => {
            if replace_first_rvalue_use(
                &mut index.left,
                local,
                replacement,
                captured,
                before_unsafe,
                true,
            ) {
                return true;
            }
            if rvalue_prior_unsafe(&index.left, captured) {
                *before_unsafe = true;
            }
            replace_first_rvalue_use(&mut index.right, local, replacement, captured, before_unsafe, copied)
        }
        RValue::Call(call) => {
            if rvalue_prior_unsafe(&call.value, captured) {
                *before_unsafe = true;
            }
            replace_in_rvalue_list(
                &mut call.arguments,
                local,
                replacement,
                captured,
                before_unsafe,
            )
        }
        RValue::MethodCall(method_call) => {
            if rvalue_prior_unsafe(&method_call.value, captured) {
                *before_unsafe = true;
            }
            replace_in_rvalue_list(
                &mut method_call.arguments,
                local,
                replacement,
                captured,
                before_unsafe,
            )
        }
        RValue::Table(table) => {
            for (key, table_value) in &mut table.0 {
                if key
                    .as_ref()
                    .is_some_and(|key| rvalue_prior_unsafe(key, captured))
                {
                    *before_unsafe = true;
                }
                if replace_first_rvalue_use(
                    table_value,
                    local,
                    replacement,
                    captured,
                    before_unsafe,
                    true,
                ) {
                    return true;
                }
                if entry_prior_unsafe(key.as_ref(), table_value, captured) {
                    *before_unsafe = true;
                }
            }
            false
        }
        _ => false,
    }
}

fn clone_replacement(value: &RValue) -> RValue {
    #[cfg(test)]
    REPLACEMENT_CLONES.with(|clones| clones.set(clones.get() + 1));
    value.clone()
}

#[cfg(test)]
thread_local! { static REPLACEMENT_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
fn replace_direct_rvalue_use_reference(
    statement: &mut Statement,
    local: &RcLocal,
    replacement: RValue,
    captured: &dyn Fn(&RcLocal) -> bool,
    register: &dyn Fn(&RcLocal) -> bool,
) -> bool {
    match statement {
        Statement::Assign(assign) => {
            let mut before_unsafe = assign
                .left
                .iter()
                .any(|left| lvalue_prior_unsafe(left, captured, register));
            replace_in_rvalue_list_reference(
                &mut assign.right,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::Return(return_) => {
            let mut before_unsafe = false;
            replace_in_rvalue_list_reference(
                &mut return_.values,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::Call(call) => {
            let mut before_unsafe = rvalue_prior_unsafe(&call.value, captured);
            replace_in_rvalue_list_reference(
                &mut call.arguments,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::MethodCall(method_call) => {
            let mut before_unsafe = rvalue_prior_unsafe(&method_call.value, captured);
            replace_in_rvalue_list_reference(
                &mut method_call.arguments,
                local,
                replacement,
                captured,
                &mut before_unsafe,
            )
        }
        Statement::SetList(set_list) => {
            let mut before_unsafe = false;
            if replace_in_rvalue_list_reference(
                &mut set_list.values,
                local,
                clone_replacement(&replacement),
                captured,
                &mut before_unsafe,
            ) {
                return true;
            }
            if let Some(tail) = &mut set_list.tail {
                return replace_first_rvalue_use_reference(
                    tail,
                    local,
                    replacement,
                    captured,
                    &mut before_unsafe,
                );
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
fn replace_in_rvalue_list_reference(
    values: &mut [RValue],
    local: &RcLocal,
    replacement: RValue,
    captured: &dyn Fn(&RcLocal) -> bool,
    before_unsafe: &mut bool,
) -> bool {
    for value in values {
        if replace_first_rvalue_use_reference(value, local, clone_replacement(&replacement), captured, before_unsafe) {
            return true;
        }
        if rvalue_prior_unsafe(value, captured) {
            *before_unsafe = true;
        }
    }
    false
}

#[cfg(test)]
fn replace_first_rvalue_use_reference(
    value: &mut RValue,
    local: &RcLocal,
    replacement: RValue,
    captured: &dyn Fn(&RcLocal) -> bool,
    before_unsafe: &mut bool,
) -> bool {
    if matches!(value, RValue::Local(read) if read == local) {
        if !can_replace_after_prior_eval(&replacement, *before_unsafe, captured) {
            return false;
        }
        *value = replacement;
        return true;
    }

    match value {
        RValue::Binary(binary) => {
            if replace_first_rvalue_use_reference(
                &mut binary.left,
                local,
                clone_replacement(&replacement),
                captured,
                before_unsafe,
            ) {
                return true;
            }
            if rvalue_prior_unsafe(&binary.left, captured) {
                *before_unsafe = true;
            }
            if skippable_operand(binary.operation) && crate::is_observable(&replacement) {
                return false;
            }
            replace_first_rvalue_use_reference(&mut binary.right, local, replacement, captured, before_unsafe)
        }
        RValue::Unary(unary) => {
            replace_first_rvalue_use_reference(&mut unary.value, local, replacement, captured, before_unsafe)
        }
        RValue::Index(index) => {
            if replace_first_rvalue_use_reference(
                &mut index.left,
                local,
                clone_replacement(&replacement),
                captured,
                before_unsafe,
            ) {
                return true;
            }
            if rvalue_prior_unsafe(&index.left, captured) {
                *before_unsafe = true;
            }
            replace_first_rvalue_use_reference(&mut index.right, local, replacement, captured, before_unsafe)
        }
        RValue::Call(call) => {
            if rvalue_prior_unsafe(&call.value, captured) {
                *before_unsafe = true;
            }
            replace_in_rvalue_list_reference(
                &mut call.arguments,
                local,
                replacement,
                captured,
                before_unsafe,
            )
        }
        RValue::MethodCall(method_call) => {
            if rvalue_prior_unsafe(&method_call.value, captured) {
                *before_unsafe = true;
            }
            replace_in_rvalue_list_reference(
                &mut method_call.arguments,
                local,
                replacement,
                captured,
                before_unsafe,
            )
        }
        RValue::Table(table) => {
            for (key, table_value) in &mut table.0 {
                if key
                    .as_ref()
                    .is_some_and(|key| rvalue_prior_unsafe(key, captured))
                {
                    *before_unsafe = true;
                }
                if replace_first_rvalue_use_reference(
                    table_value,
                    local,
                    clone_replacement(&replacement),
                    captured,
                    before_unsafe,
                ) {
                    return true;
                }
                if entry_prior_unsafe(key.as_ref(), table_value, captured) {
                    *before_unsafe = true;
                }
            }
            false
        }
        _ => false,
    }
}


fn skippable_operand(operation: BinaryOperation) -> bool {
    matches!(operation, BinaryOperation::And | BinaryOperation::Or)
}

fn can_replace_after_prior_eval(
    replacement: &RValue,
    before_unsafe: bool,
    captured: &dyn Fn(&RcLocal) -> bool,
) -> bool {
    !before_unsafe
        || !(crate::is_observable(replacement)
            || contains_global(replacement)
            || reads_captured_local(replacement, captured))
}

/// A constructor entry, once evaluated and stored: storing under a key that
/// may be nil or NaN raises (`{[key] = 1, value}` with `key` nil never
/// evaluates `value`).
fn entry_prior_unsafe(key: Option<&RValue>, value: &RValue, captured: &dyn Fn(&RcLocal) -> bool) -> bool {
    rvalue_prior_unsafe(value, captured) || key.is_some_and(|key| !crate::is_total_table_key(key))
}

fn rvalue_prior_unsafe(value: &RValue, captured: &dyn Fn(&RcLocal) -> bool) -> bool {
    crate::is_observable(value) || contains_global(value) || reads_captured_local(value, captured)
}

/// `register`: the locals the statement's function holds in registers.
fn lvalue_prior_unsafe(lvalue: &LValue, captured: &dyn Fn(&RcLocal) -> bool, register: &dyn Fn(&RcLocal) -> bool) -> bool {
    match lvalue {
        LValue::Local(_) => false,
        LValue::Global(_) => true,
        LValue::Index(index) => !stable_address(index, captured, register),
    }
}

/// Whether a store address reads the same whether the assigned value is
/// computed before or inside the statement: a literal; a register local,
/// which SETTABLE reads after the value either way; a local nothing can
/// change. An upvalue base is fetched (GETUPVAL) before the value, and a
/// nested base (`object.sub[1] = v`) is a GETTABLE there, which may run
/// `__index`.
fn stable_address(index: &Index, captured: &dyn Fn(&RcLocal) -> bool, register: &dyn Fn(&RcLocal) -> bool) -> bool {
    [&*index.left, &*index.right].into_iter().all(|component| match component {
        RValue::Local(local) => register(local) || !captured(local),
        RValue::Literal(_) => true,
        _ => false,
    })
}

fn reads_captured_local(value: &RValue, captured: &dyn Fn(&RcLocal) -> bool) -> bool {
    value.any_local_read(&mut |local| captured(local))
}

fn contains_global(value: &RValue) -> bool {
    if matches!(value, RValue::Global(_)) {
        return true;
    }
    !value.visit_rvalues(&mut |child| !contains_global(child))
}

fn is_generated_temp(local: &RcLocal) -> bool {
    local.is_inferred_temporary()
}

#[cfg(test)]
mod tests {
    use super::reconstruct_conditional_expressions;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Global, If, Index, LValue, Literal, Local,
        RValue, RcLocal, Return, Select,
    };

    fn tagged_replacement(mode: usize, captured: &RcLocal) -> RValue {
        use crate::{node_origins, Traverse};
        let value = match mode {
            0 => Literal::Number(7.0).into(),
            1 => Call::new(global("evaluate"), vec![]).into(),
            _ => local_value(captured),
        };
        let mut value = crate::IfExpression::new(Literal::Boolean(true).into(), value,
            Binary::new(Literal::Number(1.0).into(), Literal::Number(2.0).into(), BinaryOperation::Equal).into()).into();
        fn stamp(value: &mut RValue, ordinal: &mut usize) {
            *ordinal += 1;
            if let Some(origin) = node_origins::value_mut(value) {
                *origin = node_origins::Origin::input(node_origins::Input {
                    function: "conditional_replacement".into(), block: 1, statement: *ordinal, value: None,
                });
                let data = origin.0.as_mut().unwrap();
                data.inlined = *ordinal % 2 == 0;
                data.incomplete = *ordinal % 3 == 0;
                data.synthesized = Some("conditional_test");
                // Keep some origins uncloned to detect accidental cloning of
                // the moved SetList-tail path as well as missed copy markers.
                data.cloned = *ordinal % 5 == 0;
            }
            value.visit_rvalues_mut(&mut |child| { stamp(child, ordinal); true });
        }
        stamp(&mut value, &mut 0);
        value
    }

    #[test]
    fn borrowed_replacement_search_matches_legacy_barriers_and_origins() {
        use crate::{node_origins, Statement, Traverse};
        type Snapshot = Option<(Vec<std::sync::Arc<node_origins::Input>>, bool, bool, bool, Option<&'static str>)>;
        fn origins(statement: &Statement) -> Vec<Snapshot> {
            let mut out = Vec::new();
            statement.traverse_rvalues_ref(&mut |value| {
                if let Some(origin) = node_origins::value(value) {
                    out.push(origin.0.as_ref().map(|data| (data.inputs.clone(), data.inlined,
                        data.cloned, data.incomplete, data.synthesized)));
                }
            });
            out
        }
        let target = local("v0");
        let captured = local("captured");
        let object = local("object");
        let callable = local("callable");
        for shape in 0..12 {
            let target_value = local_value(&target);
            let expression: RValue = match shape {
                0 => target_value,
                1 => Binary::new(target_value, nil(), BinaryOperation::Equal).into(),
                2 => Binary::new(nil(), target_value, BinaryOperation::Equal).into(),
                3 => crate::Unary::new(target_value, crate::UnaryOperation::Not).into(),
                4 => Index::new(target_value, string("key")).into(),
                5 => Index::new(local_value(&object), target_value).into(),
                6 => Call::new(local_value(&callable), vec![nil(), target_value]).into(),
                7 => crate::MethodCall::new(local_value(&object), "method".into(), vec![nil(), target_value]).into(),
                8 => crate::Table::new(vec![(Some(string("first")), nil()), (None, target_value)]).into(),
                9 => crate::Table::new(vec![(Some(Call::new(global("key"), vec![]).into()), target_value)]).into(),
                10 => Binary::new(global("condition"), target_value, BinaryOperation::And).into(),
                _ => global("no_target"),
            };
            for statement_kind in 0..7 {
                for prior_effect in [false, true] {
                    for mode in 0..3 {
                        let prior = if prior_effect { Call::new(global("before"), vec![]).into() } else { nil() };
                        let statement: Statement = match statement_kind {
                            0 => Return::new(vec![prior, expression.clone()]).into(),
                            1 => Assign::new(vec![object.clone().into()], vec![prior, expression.clone()]).into(),
                            2 => Assign::new(vec![Index::new(local_value(&object), string("key")).into()], vec![prior, expression.clone()]).into(),
                            3 => Call::new(local_value(&callable), vec![prior, expression.clone()]).into(),
                            4 => crate::MethodCall::new(local_value(&object), "method".into(), vec![prior, expression.clone()]).into(),
                            5 => crate::SetList::new(object.clone(), 1, vec![prior, expression.clone()], None).into(),
                            _ => crate::SetList::new(object.clone(), 1, vec![prior], Some(expression.clone())).into(),
                        };
                        let usage = |local: &RcLocal| local == &captured;
                        let mut expected = statement.clone();
                        let mut actual = statement;
                        let changed = super::replace_direct_rvalue_use_reference(&mut expected, &target,
                            tagged_replacement(mode, &captured), &usage, &|_| false);
                        assert_eq!(super::replace_direct_rvalue_use(&mut actual, &target,
                            tagged_replacement(mode, &captured), &usage, &|_| false).is_ok(), changed);
                        assert_eq!(actual, expected, "shape {shape}, statement {statement_kind}, mode {mode}, effect {prior_effect}");
                        assert_eq!(origins(&actual), origins(&expected), "shape {shape}, statement {statement_kind}, origins");
                    }
                }
            }
        }
    }

    #[test]
    fn failed_replacement_positions_never_clone_the_candidate_tree() {
        let target = local("v0");
        let object = local("object");
        for width in [64, 256, 1024] {
            for use_site in 0..3 {
                let values = vec![nil(); width];
                let statement: crate::Statement = if use_site == 2 {
                    crate::SetList::new(object.clone(), 1, values, Some(local_value(&target))).into()
                } else {
                    let mut values = values;
                    if use_site == 1 { values.push(local_value(&target)); }
                    Return::new(values).into()
                };
                let mut expected = statement.clone();
                let mut actual = statement;
                let usage = |_: &RcLocal| false;
                super::REPLACEMENT_CLONES.with(|clones| clones.set(0));
                let changed = super::replace_direct_rvalue_use_reference(&mut expected, &target,
                    tagged_replacement(0, &object), &usage, &|_| false);
                let legacy_clones = super::REPLACEMENT_CLONES.with(|clones| clones.get());
                super::REPLACEMENT_CLONES.with(|clones| clones.set(0));
                assert_eq!(super::replace_direct_rvalue_use(&mut actual, &target,
                    tagged_replacement(0, &object), &usage, &|_| false).is_ok(), changed);
                let clones = super::REPLACEMENT_CLONES.with(|clones| clones.get());
                assert_eq!(actual, expected);
                assert_eq!(changed, use_site != 0);
                assert_eq!(clones, usize::from(use_site == 1));
                // SetList's original fixed-values trial also cloned the root
                // before trying each list element; its tail then moved it.
                assert_eq!(legacy_clones, width + usize::from(use_site != 0));
            }
        }
    }

    #[test]
    fn adjacency_worklist_matches_legacy_restarts() {
        fn input(seed: u64) -> Block {
            let mut random = seed + 1;
            let mut next = || { random ^= random << 13; random ^= random >> 7; random ^= random << 17; random as usize };
            let mut statements = Vec::new();
            let consume = local("consume");
            for index in 0..20 {
                let outer = local(&format!("v{}", index * 2));
                let inner = local(&format!("v{}", index * 2 + 1));
                for (offset, binding) in [&outer, &inner].into_iter().enumerate() {
                    let mut value = if next() % 3 == 0 { Literal::Nil.into() }
                        else { Literal::Boolean(next() % 2 == 0).into() };
                    let condition = if next() % 2 == 0 { global("flag") }
                        else { Binary::new(global("left"), global("right"), BinaryOperation::Equal).into() };
                    // Guarantee a successful pair in both styles, while retaining
                    // the seeded mixture of accepted and refused later candidates.
                    if index == 0 { value = Literal::Boolean(false).into(); }
                    statements.push(declare_empty(binding));
                    statements.push(If::new(condition,
                        Block(vec![assign_local(binding, value)]),
                        Block(vec![assign_local(binding, Literal::Boolean(offset == 0).into())])).into());
                }
                let mut reads = vec![local_value(&outer), local_value(&inner)];
                let duplicate_read = next() % 4 == 0;
                let prior_effect = next() % 5 == 0;
                if index != 0 && duplicate_read { reads.push(local_value(&outer)); }
                if index != 0 && prior_effect { reads.insert(0, Call::new(global("effect"), vec![]).into()); }
                // A global callee would reject every moved global condition.
                statements.push(Call::new(local_value(&consume), reads).into());
            }
            Block(statements)
        }
        for seed in 0..100 {
            for allow_if_expression in [false, true] {
                let mut expected = input(seed);
                let mut actual = input(seed);
                let original_len = actual.len();
                let mut rewrites = 0;
                while super::reconstruct_once(&mut expected, allow_if_expression) { rewrites += 1; }
                assert!(rewrites >= 2, "seed {seed}, allow if {allow_if_expression}: no successful pair");
                { let safety = super::CaptureSafety::new(&actual); super::reconstruct_current_block(&mut actual, allow_if_expression, super::Scope { safety: &safety, function: None }); }
                assert!(actual.len() <= original_len - rewrites);
                assert_eq!(actual.len(), expected.len());
                assert_eq!(actual.to_string(), expected.to_string(), "seed {seed}, allow if {allow_if_expression}");
            }
        }
    }

    #[test]
    fn adjacency_worklist_reactivates_an_earlier_refused_candidate() {
        fn input() -> Block {
            let outer = local("v0");
            let inner = local("v1");
            let mut statements = Vec::new();
            for binding in [&outer, &inner] {
                statements.push(declare_empty(binding));
                statements.push(If::new(global("flag"),
                    Block(vec![assign_local(binding, Literal::Boolean(false).into())]),
                    Block(vec![assign_local(binding, Literal::Boolean(true).into())])).into());
            }
            statements.push(Call::new(local_value(&local("consume")),
                vec![local_value(&outer), local_value(&inner)]).into());
            Block(statements)
        }
        for allow_if_expression in [false, true] {
            let mut expected = input();
            let first_declaration = expected[0].to_string();
            // The outer declaration is visited first but cannot reach its use.
            assert!(super::reconstruct_once(&mut expected, allow_if_expression));
            assert_eq!(expected.len(), 3);
            assert_eq!(expected[0].to_string(), first_declaration);
            // Erasing the inner pair makes the earlier outer candidate adjacent.
            assert!(super::reconstruct_once(&mut expected, allow_if_expression));
            assert_eq!(expected.len(), 1);
            assert!(!super::reconstruct_once(&mut expected, allow_if_expression));
            let mut actual = input();
            { let safety = super::CaptureSafety::new(&actual); super::reconstruct_current_block(&mut actual, allow_if_expression, super::Scope { safety: &safety, function: None }); }
            assert_eq!(actual.len(), 1);
            assert_eq!(actual.to_string(), expected.to_string());
        }
    }

    #[test]
    fn keeps_global_callee_before_a_moved_global_condition() {
        for allow_if_expression in [false, true] {
            let temp = local("v");
            let mut block = Block(vec![declare_empty(&temp),
                If::new(global("flag"),
                    Block(vec![assign_local(&temp, Literal::Boolean(false).into())]),
                    Block(vec![assign_local(&temp, Literal::Boolean(true).into())])).into(),
                Call::new(global("consume"), vec![local_value(&temp)]).into()]);
            let use_statement = block[2].to_string();
            { let safety = super::CaptureSafety::new(&block); super::reconstruct_current_block(&mut block, allow_if_expression, super::Scope { safety: &safety, function: None }); }
            // `flag` stays before the lookup of `consume`: the value folds
            // into the declaration, not the use.
            assert_eq!(block.len(), 2);
            assert_eq!(block[0].to_string(), "local v = not flag");
            assert_eq!(block[1].to_string(), use_statement);
        }
    }

    #[test]
    fn keeps_source_preserved_generated_binding() {
        for allow_if_expression in [false, true] {
            let temp = local("v9");
            temp.0.lock().add_source_binding(crate::SourceBinding {
                origin: crate::BindingOrigin::DebugLocal {
                    prototype: 0, register: 0, start_pc: 0, end_pc: 8,
                },
                name: "v9".into(),
            });
            let mut block = Block(vec![declare_empty(&temp),
                If::new(global("flag"),
                    Block(vec![assign_local(&temp, Literal::Boolean(false).into())]),
                    Block(vec![assign_local(&temp, Literal::Boolean(true).into())])).into(),
                Call::new(local_value(&local("consume")), vec![local_value(&temp)]).into()]);
            let original = block.to_string();
            assert!(!super::reconstruct_once(&mut block, allow_if_expression));
            { let safety = super::CaptureSafety::new(&block); super::reconstruct_current_block(&mut block, allow_if_expression, super::Scope { safety: &safety, function: None }); }
            assert_eq!(block.to_string(), original);
        }
    }

    #[test]
    fn keeps_captured_callee_before_a_moved_global_condition() {
        for by_ref in [false, true] {
            for allow_if_expression in [false, true] {
                let temp = local("v");
                let consume = local("consume");
                let capture = if by_ref { crate::Upvalue::Ref(consume.clone()) }
                    else { crate::Upvalue::Copy(consume.clone()) };
                let closure = crate::Closure {
                    node_origin: Default::default(),
                    function: Default::default(),
                    upvalues: vec![capture],
                };
                let mut block = Block(vec![declare_empty(&temp),
                    If::new(global("flag"),
                        Block(vec![assign_local(&temp, Literal::Boolean(false).into())]),
                        Block(vec![assign_local(&temp, Literal::Boolean(true).into())])).into(),
                    Call::new(local_value(&consume), vec![local_value(&temp)]).into(),
                    Return::new(vec![closure.into()]).into()]);
                let use_statement = block[2].to_string();
                { let safety = super::CaptureSafety::new(&block); super::reconstruct_current_block(&mut block, allow_if_expression, super::Scope { safety: &safety, function: None }); }
                assert_eq!(block.len(), 3);
                assert_eq!(block[0].to_string(), "local v = not flag");
                assert_eq!(block[1].to_string(), use_statement);
            }
        }
    }

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn nil() -> RValue {
        RValue::Literal(Literal::Nil)
    }

    fn declare_empty(local: &RcLocal) -> crate::Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![]);
        assign.prefix = true;
        assign.into()
    }

    fn assign_local(local: &RcLocal, value: RValue) -> crate::Statement {
        Assign::new(vec![LValue::Local(local.clone())], vec![value]).into()
    }

    fn assign(left: LValue, value: RValue) -> crate::Statement {
        Assign::new(vec![left], vec![value]).into()
    }

    #[test]
    fn statement_style_keeps_general_select_and_false_nil_semantics() {
        let boolean = |b| RValue::Literal(Literal::Boolean(b));
        let condition = local("condition");
        let value = local("value");
        assert!(super::build_short_circuit(local_value(&condition), local_value(&value), nil()).is_none());
        // A nil false-condition cannot be replaced with `condition and value`:
        // that would return nil instead of the branch's literal false.
        assert!(super::build_short_circuit(local_value(&condition), local_value(&value), boolean(false)).is_none());
        let shortened = super::build_short_circuit(local_value(&condition), boolean(false), local_value(&value)).unwrap();
        assert!(matches!(shortened, RValue::Binary(_)));

        let temp = local("v");
        let mut block = Block(vec![declare_empty(&temp),
            If::new(local_value(&condition), Block(vec![assign_local(&temp, local_value(&value))]),
                Block(vec![assign_local(&temp, nil())])).into(),
            Return::new(vec![local_value(&temp)]).into()]);
        super::reconstruct_short_circuit_expressions(&mut block);
        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn statement_style_shortens_boolean_branch_without_if_expression() {
        let temp = local("v");
        let condition = Binary::new(local_value(&local("input")), nil(), BinaryOperation::Equal).into();
        let mut block = Block(vec![declare_empty(&temp),
            If::new(condition, Block(vec![assign_local(&temp, global("value"))]),
                Block(vec![assign_local(&temp, RValue::Literal(Literal::Boolean(false)))])).into(),
            Return::new(vec![local_value(&temp)]).into()]);
        super::reconstruct_short_circuit_expressions(&mut block);
        assert_eq!(block.0.len(), 1);
        assert!(!format!("{block}").contains("if "));
    }

    #[test]
    fn reconstructs_returned_branch_temp() {
        let temp = local("v");
        let cond = local("cond");
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&temp, local_value(&a))]),
                Block(vec![assign_local(&temp, local_value(&b))]),
            )
            .into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.to_string(), "return if cond then a else b");
    }

    #[test]
    fn reconstructs_optional_table_field_and_prefers_non_nil_then_arm() {
        let temp = local("v7");
        let label = local("leftLabel");
        let children = local("children");
        let create = local("createElement");
        let condition = Binary::new(local_value(&label), nil(), BinaryOperation::Equal).into();
        let element = Call::new(local_value(&create), vec![string("TextLabel")]).into();
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                condition,
                Block(vec![assign_local(&temp, nil())]),
                Block(vec![assign_local(&temp, element)]),
            )
            .into(),
            assign(
                LValue::Index(Index::new(local_value(&children), string("LeftLabel"))),
                local_value(&temp),
            ),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(
            block.to_string(),
            "children.LeftLabel = if leftLabel ~= nil then createElement(\"TextLabel\") else nil"
        );
    }

    #[test]
    fn reconstructs_single_value_selected_call_arm() {
        let temp = local("v9");
        let cond = local("cond");
        let children = local("children");
        let create = local("createElement");
        let element = RValue::Select(Select::Call(Call::new(
            local_value(&create),
            vec![string("TextLabel")],
        )));
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&temp, element)]),
                Block(vec![assign_local(&temp, nil())]),
            )
            .into(),
            assign(
                LValue::Index(Index::new(local_value(&children), string("Label"))),
                local_value(&temp),
            ),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(
            block.to_string(),
            "children.Label = if cond then createElement(\"TextLabel\") else nil"
        );
    }

    #[test]
    fn reconstructs_index_receiver_use() {
        let temp = local("selectedRect");
        let cond = local("cond");
        let active = local("activeRect");
        let inactive = local("inactiveRect");
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&temp, local_value(&active))]),
                Block(vec![assign_local(&temp, local_value(&inactive))]),
            )
            .into(),
            Return::new(vec![RValue::Index(Index::new(
                local_value(&temp),
                string("Offset"),
            ))])
            .into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(
            block.to_string(),
            "return (if cond then activeRect else inactiveRect).Offset"
        );
    }

    #[test]
    fn preserves_named_local_return_value() {
        let result = local("result");
        let cond = local("cond");
        let mut block = Block(vec![
            declare_empty(&result),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&result, string("a"))]),
                Block(vec![assign_local(&result, string("b"))]),
            )
            .into(),
            Return::new(vec![local_value(&result)]).into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn rejects_large_non_optional_expression() {
        let temp = local("v");
        let cond = local("cond");
        let large_table = || {
            RValue::Table(crate::Table::new(
                (0..60)
                    .map(|i| (Some(string(&format!("Field{i}"))), string("value")))
                    .collect(),
            ))
        };
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&temp, large_table())]),
                Block(vec![assign_local(&temp, large_table())]),
            )
            .into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn rejects_local_used_twice() {
        let temp = local("v");
        let cond = local("cond");
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&temp, string("a"))]),
                Block(vec![assign_local(&temp, string("b"))]),
            )
            .into(),
            Return::new(vec![local_value(&temp), local_value(&temp)]).into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn rejects_extra_branch_statement() {
        let temp = local("v");
        let cond = local("cond");
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![
                    Call::new(global("print"), vec![string("side")]).into(),
                    assign_local(&temp, string("a")),
                ]),
                Block(vec![assign_local(&temp, string("b"))]),
            )
            .into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn rejects_intervening_statement_before_use() {
        let temp = local("v");
        let cond = local("cond");
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(&temp, string("a"))]),
                Block(vec![assign_local(&temp, string("b"))]),
            )
            .into(),
            Call::new(global("print"), vec![string("between")]).into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.0.len(), 4);
    }

    #[test]
    fn rejects_side_effectful_replacement_after_prior_call_argument() {
        let temp = local("v");
        let cond = local("cond");
        let make = local("make");
        let mut block = Block(vec![
            declare_empty(&temp),
            If::new(
                local_value(&cond),
                Block(vec![assign_local(
                    &temp,
                    Call::new(local_value(&make), vec![string("a")]).into(),
                )]),
                Block(vec![assign_local(&temp, nil())]),
            )
            .into(),
            Call::new(
                local_value(&local("consume")),
                vec![
                    Call::new(global("before"), vec![]).into(),
                    local_value(&temp),
                ],
            )
            .into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        // `make` keeps running before `before`: in the declaration.
        assert_eq!(block.0.len(), 2);
        assert!(block.0[0].to_string().starts_with("local v = "), "{}", block.0[0]);
        assert_eq!(block.0[1].to_string(), "consume(before(), v)");
    }
}
