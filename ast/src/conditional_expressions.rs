use rustc_hash::FxHashMap;

use crate::{
    Binary, BinaryOperation, Block, If, IfExpression, Index, LValue, Literal, LocalRw, RValue,
    RcLocal, Select, SideEffects, Statement, Traverse, Unary, UnaryOperation,
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
    reconstruct_nested_blocks(block);
    while reconstruct_once(block) {}
}

fn reconstruct_nested_blocks(block: &mut Block) {
    for statement in &mut block.0 {
        reconstruct_nested_in_statement(statement);
    }
}

fn reconstruct_nested_in_statement(statement: &mut Statement) {
    reconstruct_closures_in_statement(statement);
    match statement {
        Statement::If(r#if) => {
            reconstruct_conditional_expressions(&mut r#if.then_block.lock());
            reconstruct_conditional_expressions(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => reconstruct_conditional_expressions(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => reconstruct_conditional_expressions(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => {
            reconstruct_conditional_expressions(&mut numeric_for.block.lock())
        }
        Statement::GenericFor(generic_for) => {
            reconstruct_conditional_expressions(&mut generic_for.block.lock())
        }
        _ => {}
    }
}

fn reconstruct_closures_in_statement(statement: &mut Statement) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        reconstruct_conditional_expressions(&mut function.lock().body);
    }
}

fn reconstruct_once(block: &mut Block) -> bool {
    if block.0.len() < 3 {
        return false;
    }

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

        let replacement = build_if_expression(condition, then_value, else_value);
        if !replace_direct_rvalue_use(&mut block.0[use_index], &local, replacement, &usage) {
            continue;
        }

        block.0.remove(if_index);
        block.0.remove(decl_index);
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
    Some(local.clone())
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
        _ => value.rvalues().into_iter().any(contains_unsupported_value),
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
    for local in statement.values_read() {
        usage.entry(local.clone()).or_default().reads += 1;
    }
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
    for rvalue in statement.rvalues() {
        collect_closures_in_rvalue(rvalue, f);
    }
}

fn collect_closures_in_rvalue(rvalue: &RValue, f: &mut impl FnMut(&crate::Closure)) {
    if let RValue::Closure(closure) = rvalue {
        f(closure);
        return;
    }
    for child in rvalue.rvalues() {
        collect_closures_in_rvalue(child, f);
    }
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
    usage: &FxHashMap<RcLocal, Usage>,
) -> bool {
    match statement {
        Statement::Assign(assign) => {
            let mut before_unsafe = assign
                .left
                .iter()
                .any(|left| lvalue_prior_unsafe(left, usage));
            replace_in_rvalue_list(
                &mut assign.right,
                local,
                replacement,
                usage,
                &mut before_unsafe,
            )
        }
        Statement::Return(return_) => {
            let mut before_unsafe = false;
            replace_in_rvalue_list(
                &mut return_.values,
                local,
                replacement,
                usage,
                &mut before_unsafe,
            )
        }
        Statement::Call(call) => {
            let mut before_unsafe = rvalue_prior_unsafe(&call.value, usage);
            replace_in_rvalue_list(
                &mut call.arguments,
                local,
                replacement,
                usage,
                &mut before_unsafe,
            )
        }
        Statement::MethodCall(method_call) => {
            let mut before_unsafe = rvalue_prior_unsafe(&method_call.value, usage);
            replace_in_rvalue_list(
                &mut method_call.arguments,
                local,
                replacement,
                usage,
                &mut before_unsafe,
            )
        }
        Statement::SetList(set_list) => {
            let mut before_unsafe = false;
            if replace_in_rvalue_list(
                &mut set_list.values,
                local,
                replacement.clone(),
                usage,
                &mut before_unsafe,
            ) {
                return true;
            }
            if let Some(tail) = &mut set_list.tail {
                return replace_first_rvalue_use(
                    tail,
                    local,
                    replacement,
                    usage,
                    &mut before_unsafe,
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
    replacement: RValue,
    usage: &FxHashMap<RcLocal, Usage>,
    before_unsafe: &mut bool,
) -> bool {
    for value in values {
        if replace_first_rvalue_use(value, local, replacement.clone(), usage, before_unsafe) {
            return true;
        }
        if rvalue_prior_unsafe(value, usage) {
            *before_unsafe = true;
        }
    }
    false
}

fn replace_first_rvalue_use(
    value: &mut RValue,
    local: &RcLocal,
    replacement: RValue,
    usage: &FxHashMap<RcLocal, Usage>,
    before_unsafe: &mut bool,
) -> bool {
    if matches!(value, RValue::Local(read) if read == local) {
        if !can_replace_after_prior_eval(&replacement, *before_unsafe, usage) {
            return false;
        }
        *value = replacement;
        return true;
    }

    match value {
        RValue::Binary(binary) => {
            if replace_first_rvalue_use(
                &mut binary.left,
                local,
                replacement.clone(),
                usage,
                before_unsafe,
            ) {
                return true;
            }
            if rvalue_prior_unsafe(&binary.left, usage) {
                *before_unsafe = true;
            }
            replace_first_rvalue_use(&mut binary.right, local, replacement, usage, before_unsafe)
        }
        RValue::Unary(unary) => {
            replace_first_rvalue_use(&mut unary.value, local, replacement, usage, before_unsafe)
        }
        RValue::Index(index) => {
            if replace_first_rvalue_use(
                &mut index.left,
                local,
                replacement.clone(),
                usage,
                before_unsafe,
            ) {
                return true;
            }
            if rvalue_prior_unsafe(&index.left, usage) {
                *before_unsafe = true;
            }
            replace_first_rvalue_use(&mut index.right, local, replacement, usage, before_unsafe)
        }
        RValue::Call(call) => {
            if rvalue_prior_unsafe(&call.value, usage) {
                *before_unsafe = true;
            }
            replace_in_rvalue_list(
                &mut call.arguments,
                local,
                replacement,
                usage,
                before_unsafe,
            )
        }
        RValue::MethodCall(method_call) => {
            if rvalue_prior_unsafe(&method_call.value, usage) {
                *before_unsafe = true;
            }
            replace_in_rvalue_list(
                &mut method_call.arguments,
                local,
                replacement,
                usage,
                before_unsafe,
            )
        }
        RValue::Table(table) => {
            for (key, table_value) in &mut table.0 {
                if key
                    .as_ref()
                    .is_some_and(|key| rvalue_prior_unsafe(key, usage))
                {
                    *before_unsafe = true;
                }
                if replace_first_rvalue_use(
                    table_value,
                    local,
                    replacement.clone(),
                    usage,
                    before_unsafe,
                ) {
                    return true;
                }
                if rvalue_prior_unsafe(table_value, usage) {
                    *before_unsafe = true;
                }
            }
            false
        }
        _ => false,
    }
}

fn can_replace_after_prior_eval(
    replacement: &RValue,
    before_unsafe: bool,
    usage: &FxHashMap<RcLocal, Usage>,
) -> bool {
    !before_unsafe
        || !(replacement.has_side_effects()
            || contains_global(replacement)
            || reads_captured_local(replacement, usage))
}

fn rvalue_prior_unsafe(value: &RValue, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    value.has_side_effects() || contains_global(value) || reads_captured_local(value, usage)
}

fn lvalue_prior_unsafe(lvalue: &LValue, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    match lvalue {
        LValue::Local(_) => false,
        LValue::Global(_) => true,
        LValue::Index(index) => !stable_index(index, usage),
    }
}

fn stable_index(index: &Index, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    stable_index_component(&index.left, usage) && stable_index_component(&index.right, usage)
}

fn stable_index_component(value: &RValue, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    match value {
        RValue::Local(local) => !usage.get(local).is_some_and(|usage| usage.captured),
        RValue::Literal(_) => true,
        RValue::Index(index) => stable_index(index, usage),
        _ => false,
    }
}

fn reads_captured_local(value: &RValue, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    value
        .values_read()
        .into_iter()
        .any(|local| usage.get(local).is_some_and(|usage| usage.captured))
}

fn contains_global(value: &RValue) -> bool {
    if matches!(value, RValue::Global(_)) {
        return true;
    }
    value.rvalues().into_iter().any(contains_global)
}

fn is_generated_temp(local: &RcLocal) -> bool {
    let Some(name) = local.0 .0.lock().0.clone() else {
        return false;
    };
    name == "v"
        || name
            .strip_prefix('v')
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::reconstruct_conditional_expressions;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Global, If, Index, LValue, Literal, Local,
        RValue, RcLocal, Return, Select,
    };

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
            "children.Label = if cond then (createElement(\"TextLabel\")) else nil"
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
            RValue::Table(crate::Table(
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
                global("use"),
                vec![
                    Call::new(global("before"), vec![]).into(),
                    local_value(&temp),
                ],
            )
            .into(),
        ]);

        reconstruct_conditional_expressions(&mut block);

        assert_eq!(block.0.len(), 3);
    }
}
