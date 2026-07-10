use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Block, Call, LValue, LocalRw, MethodCall, RValue, RcLocal, Select, SideEffects, Statement,
    Traverse,
};

#[derive(Default)]
pub(crate) struct Usage {
    pub(crate) reads: usize,
    pub(crate) writes: usize,
    pub(crate) captured: bool,
}

struct MotionFacts {
    captured: FxHashSet<RcLocal>,
    stable_captured: FxHashSet<RcLocal>,
}

/// Inline generated, single-use local temporaries back into their use sites.
///
/// This pass is intentionally conservative. It only removes locals named like
/// `v`, `v2`, ... and only moves expressions that are single-value and cheap to
/// relocate. Calls, method calls, varargs, selects, indexes, and closures are
/// left alone because inlining them can alter multi-return behavior, evaluation
/// order, capture semantics, or error behavior.
pub fn inline_single_use_temps(block: &mut Block) -> bool {
    // Set of locals captured by ANY closure, computed ONCE over the whole tree
    // (`collect_usage` already recurses into every nested block + closure). A
    // snapshot of a captured local must not be moved past a side-effecting
    // statement — the call may invoke a closure that mutates the cell (C10:
    // `local captured = source; bump(); return captured` -> `bump(); return
    // source` returned 99 not 1, because `bump` mutates the upvalue `source`).
    // The per-block usage recomputed during recursion is BLIND to a capturing
    // closure in a sibling/enclosing scope, so the whole-program set is threaded
    // down (mirrors `eliminate_nil`). The existing `does_not_move_captured_*`
    // tests pin this captured-dependency protection.
    let facts = collect_motion_facts(block);
    inline_in_block(block, &facts)
}

/// Joint leaf-to-root fixed point for declarative UI trees. The capture and
/// single-write facts are invariant under these monotone statement removals, so
/// compute them once instead of rescanning the whole function between every
/// table-rebuild layer.
pub fn rebuild_ui_expression_trees(block: &mut Block) -> bool {
    let facts = collect_motion_facts(block);
    let mut any_changed = false;
    loop {
        let rebuilt = crate::rebuild_table_literals::rebuild_with_captured(block, &facts.captured);
        let inlined = inline_in_block(block, &facts);
        any_changed |= rebuilt | inlined;
        if !rebuilt && !inlined {
            return any_changed;
        }
    }
}

fn collect_motion_facts(block: &Block) -> MotionFacts {
    let usage = collect_usage(block);
    let captured = usage
        .iter()
        .filter(|(_, usage)| usage.captured)
        .map(|(local, _)| local.clone())
        .collect();
    let mut stable_captured = FxHashSet::default();
    collect_stable_declared_locals(block, &usage, &mut stable_captured);
    MotionFacts {
        captured,
        stable_captured,
    }
}

fn collect_stable_declared_locals(
    block: &Block,
    usage: &FxHashMap<RcLocal, Usage>,
    stable: &mut FxHashSet<RcLocal>,
) {
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement
            && assign.prefix
        {
            stable.extend(assign.left.iter().filter_map(|left| {
                let LValue::Local(local) = left else {
                    return None;
                };
                usage
                    .get(local)
                    .is_some_and(|usage| usage.writes == 1)
                    .then(|| local.clone())
            }));
        }

        let mut functions = Vec::new();
        collect_closures_in_statement(statement, &mut |closure| {
            functions.push(closure.function.clone())
        });
        for function in functions {
            collect_stable_declared_locals(&function.lock().body, usage, stable);
        }

        match statement {
            Statement::If(node) => {
                collect_stable_declared_locals(&node.then_block.lock(), usage, stable);
                collect_stable_declared_locals(&node.else_block.lock(), usage, stable);
            }
            Statement::While(node) => {
                collect_stable_declared_locals(&node.block.lock(), usage, stable)
            }
            Statement::Repeat(node) => {
                collect_stable_declared_locals(&node.block.lock(), usage, stable)
            }
            Statement::NumericFor(node) => {
                collect_stable_declared_locals(&node.block.lock(), usage, stable)
            }
            Statement::GenericFor(node) => {
                collect_stable_declared_locals(&node.block.lock(), usage, stable)
            }
            _ => {}
        }
    }
}

fn inline_in_block(block: &mut Block, facts: &MotionFacts) -> bool {
    let mut changed = inline_nested_blocks(block, facts);
    while inline_once(block, facts) {
        changed = true;
    }
    changed
}

fn inline_nested_blocks(block: &mut Block, facts: &MotionFacts) -> bool {
    let mut changed = false;
    for statement in &mut block.0 {
        changed |= inline_nested_in_statement(statement, facts);
    }
    changed
}

fn inline_nested_in_statement(statement: &mut Statement, facts: &MotionFacts) -> bool {
    let closures_changed = inline_closures_in_statement(statement, facts);
    let blocks_changed = match statement {
        Statement::If(r#if) => {
            inline_in_block(&mut r#if.then_block.lock(), facts)
                | inline_in_block(&mut r#if.else_block.lock(), facts)
        }
        Statement::While(r#while) => inline_in_block(&mut r#while.block.lock(), facts),
        Statement::Repeat(repeat) => inline_in_block(&mut repeat.block.lock(), facts),
        Statement::NumericFor(numeric_for) => inline_in_block(&mut numeric_for.block.lock(), facts),
        Statement::GenericFor(generic_for) => inline_in_block(&mut generic_for.block.lock(), facts),
        _ => false,
    };
    closures_changed | blocks_changed
}

fn inline_closures_in_statement(statement: &mut Statement, facts: &MotionFacts) -> bool {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    functions.into_iter().fold(false, |changed, function| {
        inline_in_block(&mut function.lock().body, facts) | changed
    })
}

fn inline_once(block: &mut Block, facts: &MotionFacts) -> bool {
    let usage = collect_usage(block);
    for index in 0..block.0.len() {
        let Some((local, replacement)) = candidate_decl(&block.0[index]) else {
            continue;
        };
        let Some(local_usage) = usage.get(&local) else {
            continue;
        };
        // Whole-program capture set (not the per-block `usage.captured`, which
        // misses a capturing closure in a sibling/enclosing scope).
        if local_usage.reads != 1 || local_usage.writes != 1 || facts.captured.contains(&local) {
            continue;
        }
        let generated = is_generated_temp(&local);
        let named_table = !generated && matches!(&replacement, RValue::Table(_));
        if (!generated && !named_table) || !is_movable_single_value(&replacement) {
            continue;
        }
        if replacement.values_read().iter().any(|read| **read == local) {
            continue;
        }

        let Some(use_index) = direct_use_after(block, index, &local) else {
            continue;
        };
        if named_table && !is_declarative_table_use(&block.0[use_index], &local) {
            continue;
        }
        if !can_move_between(&replacement, &block.0[index + 1..use_index], facts) {
            continue;
        }
        if replace_direct_rvalue_use(&mut block.0[use_index], &local, replacement, facts) {
            block.0.remove(index);
            return true;
        }
    }
    false
}

fn is_declarative_table_use(statement: &Statement, local: &RcLocal) -> bool {
    match statement {
        Statement::Return(return_) => return_.values.iter().any(|value| {
            matches!(value, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(value, local)
        }),
        Statement::Call(call) => call.arguments.iter().any(|value| {
            matches!(value, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(value, local)
        }),
        Statement::MethodCall(call) => call.arguments.iter().any(|value| {
            matches!(value, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(value, local)
        }),
        Statement::Assign(assign) => assign
            .right
            .iter()
            .any(|value| declarative_table_use_in_value(value, local)),
        Statement::SetList(set_list) => {
            set_list
                .values
                .iter()
                .chain(set_list.tail.iter())
                .any(|value| {
                    matches!(value, RValue::Local(read) if read == local)
                        || declarative_table_use_in_value(value, local)
                })
        }
        _ => false,
    }
}

fn declarative_table_use_in_value(value: &RValue, local: &RcLocal) -> bool {
    match value {
        RValue::Call(call) => call.arguments.iter().any(|argument| {
            matches!(argument, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(argument, local)
        }),
        RValue::MethodCall(call) => call.arguments.iter().any(|argument| {
            matches!(argument, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(argument, local)
        }),
        RValue::Table(table) => table.0.iter().any(|(key, table_value)| {
            key.as_ref().is_some_and(|key| {
                matches!(key, RValue::Local(read) if read == local)
                    || declarative_table_use_in_value(key, local)
            }) || matches!(table_value, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(table_value, local)
        }),
        RValue::IfExpression(if_expression) => {
            declarative_table_use_in_value(&if_expression.then_value, local)
                || declarative_table_use_in_value(&if_expression.else_value, local)
        }
        RValue::Select(Select::Call(call)) => call.arguments.iter().any(|argument| {
            matches!(argument, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(argument, local)
        }),
        RValue::Select(Select::MethodCall(call)) => call.arguments.iter().any(|argument| {
            matches!(argument, RValue::Local(read) if read == local)
                || declarative_table_use_in_value(argument, local)
        }),
        _ => false,
    }
}

fn candidate_decl(statement: &Statement) -> Option<(RcLocal, RValue)> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    Some((local.clone(), assign.right[0].clone()))
}

fn direct_use_after(block: &Block, decl_index: usize, local: &RcLocal) -> Option<usize> {
    (decl_index + 1..block.0.len())
        .find(|&index| inlineable_direct_rvalue_read_count(&block.0[index], local) > 0)
}

pub(crate) fn collect_usage(block: &Block) -> FxHashMap<RcLocal, Usage> {
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

fn inlineable_direct_rvalue_read_count(statement: &Statement, local: &RcLocal) -> usize {
    let mut count = 0;
    for_each_inlineable_direct_rvalue(statement, &mut |rvalue| {
        count += rvalue_read_count(rvalue, local);
    });
    count
}

fn rvalue_read_count(rvalue: &RValue, local: &RcLocal) -> usize {
    let mut count = usize::from(matches!(rvalue, RValue::Local(read) if read == local));
    for child in rvalue.rvalues() {
        count += rvalue_read_count(child, local);
    }
    count
}

fn collect_closures_in_statement(statement: &Statement, f: &mut impl FnMut(&crate::Closure)) {
    for_each_direct_rvalue(statement, &mut |rvalue| {
        collect_closures_in_rvalue(rvalue, f)
    });
}

fn collect_closures_in_rvalue(rvalue: &RValue, f: &mut impl FnMut(&crate::Closure)) {
    if let RValue::Closure(closure) = rvalue {
        f(closure);
    }
    for child in rvalue.rvalues() {
        collect_closures_in_rvalue(child, f);
    }
}

fn replace_direct_rvalue_use(
    statement: &mut Statement,
    local: &RcLocal,
    replacement: RValue,
    facts: &MotionFacts,
) -> bool {
    let mut before_side_effects = match &*statement {
        Statement::Assign(assign) => assign
            .left
            .iter()
            .any(|left| lvalue_evaluation_order_barrier(left, facts)),
        _ => false,
    };
    let mut replaced = false;
    for_each_inlineable_direct_rvalue_mut(statement, &mut |rvalue| {
        if replaced {
            return;
        }
        if replace_first_rvalue_use(
            rvalue,
            local,
            replacement.clone(),
            facts,
            &mut before_side_effects,
            false,
        ) {
            replaced = true;
        } else if rvalue_evaluation_order_barrier(rvalue, facts) {
            before_side_effects = true;
        }
    });
    replaced
}

fn replace_first_rvalue_use(
    rvalue: &mut RValue,
    local: &RcLocal,
    replacement: RValue,
    facts: &MotionFacts,
    before_side_effects: &mut bool,
    conditionally_evaluated: bool,
) -> bool {
    if matches!(rvalue, RValue::Local(read) if read == local) {
        if !can_replace_after_prior_effects(&replacement, *before_side_effects, facts)
            || (conditionally_evaluated && rvalue_evaluation_order_barrier(&replacement, facts))
        {
            return false;
        }
        *rvalue = replacement;
        return true;
    }

    // A declaration initializer is evaluated unconditionally. Moving it into
    // the right arm of `and`/`or`, or either value arm of an if-expression,
    // must not make calls/global reads/captured-cell reads conditional. Keep the
    // ordinary left-to-right barrier accounting, but explicitly carry whether
    // the current subtree may be skipped.
    if let RValue::Binary(binary) = rvalue
        && matches!(
            binary.operation,
            crate::BinaryOperation::And | crate::BinaryOperation::Or
        )
    {
        if replace_first_rvalue_use(
            &mut binary.left,
            local,
            replacement.clone(),
            facts,
            before_side_effects,
            conditionally_evaluated,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(&binary.left, facts) {
            *before_side_effects = true;
        }
        return replace_first_rvalue_use(
            &mut binary.right,
            local,
            replacement,
            facts,
            before_side_effects,
            true,
        );
    }

    if let RValue::IfExpression(if_expression) = rvalue {
        if replace_first_rvalue_use(
            &mut if_expression.condition,
            local,
            replacement.clone(),
            facts,
            before_side_effects,
            conditionally_evaluated,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(&if_expression.condition, facts) {
            *before_side_effects = true;
        }

        // At most one read exists globally for an inline candidate. Scanning
        // the then arm first can therefore only make the else-arm check more
        // conservative; it cannot move two copies of the initializer.
        if replace_first_rvalue_use(
            &mut if_expression.then_value,
            local,
            replacement.clone(),
            facts,
            before_side_effects,
            true,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(&if_expression.then_value, facts) {
            *before_side_effects = true;
        }
        return replace_first_rvalue_use(
            &mut if_expression.else_value,
            local,
            replacement,
            facts,
            before_side_effects,
            true,
        );
    }

    for child in rvalue.rvalues_mut() {
        if replace_first_rvalue_use(
            child,
            local,
            replacement.clone(),
            facts,
            before_side_effects,
            conditionally_evaluated,
        ) {
            return true;
        }
        if rvalue_evaluation_order_barrier(child, facts) {
            *before_side_effects = true;
        }
    }
    false
}

fn for_each_direct_rvalue(statement: &Statement, f: &mut impl FnMut(&RValue)) {
    match statement {
        Statement::Call(call) => for_each_call_rvalue(call, f),
        Statement::MethodCall(method_call) => for_each_method_call_rvalue(method_call, f),
        Statement::Assign(assign) => assign.right.iter().for_each(f),
        Statement::If(r#if) => f(&r#if.condition),
        Statement::While(r#while) => f(&r#while.condition),
        Statement::Repeat(repeat) => f(&repeat.condition),
        Statement::NumForInit(init) => {
            f(&init.counter.1);
            f(&init.limit.1);
            f(&init.step.1);
        }
        Statement::NumForNext(next) => {
            f(&next.counter.1);
            f(&next.limit);
            f(&next.step);
        }
        Statement::NumericFor(numeric_for) => {
            f(&numeric_for.initial);
            f(&numeric_for.limit);
            f(&numeric_for.step);
        }
        Statement::GenericForInit(init) => init.0.right.iter().for_each(f),
        Statement::GenericForNext(next) => {
            f(&next.generator);
            f(&next.state);
        }
        Statement::GenericFor(generic_for) => generic_for.right.iter().for_each(f),
        Statement::Return(return_) => return_.values.iter().for_each(f),
        Statement::SetList(set_list) => {
            set_list.values.iter().for_each(&mut *f);
            if let Some(tail) = &set_list.tail {
                f(tail);
            }
        }
        Statement::Empty(_)
        | Statement::Goto(_)
        | Statement::Label(_)
        | Statement::Continue(_)
        | Statement::Break(_)
        | Statement::Close(_)
        | Statement::Comment(_) => {}
    }
}

fn for_each_direct_rvalue_mut(statement: &mut Statement, f: &mut impl FnMut(&mut RValue)) {
    match statement {
        Statement::Call(call) => for_each_call_rvalue_mut(call, f),
        Statement::MethodCall(method_call) => for_each_method_call_rvalue_mut(method_call, f),
        Statement::Assign(assign) => assign.right.iter_mut().for_each(f),
        Statement::If(r#if) => f(&mut r#if.condition),
        Statement::While(r#while) => f(&mut r#while.condition),
        Statement::Repeat(repeat) => f(&mut repeat.condition),
        Statement::NumForInit(init) => {
            f(&mut init.counter.1);
            f(&mut init.limit.1);
            f(&mut init.step.1);
        }
        Statement::NumForNext(next) => {
            f(&mut next.counter.1);
            f(&mut next.limit);
            f(&mut next.step);
        }
        Statement::NumericFor(numeric_for) => {
            f(&mut numeric_for.initial);
            f(&mut numeric_for.limit);
            f(&mut numeric_for.step);
        }
        Statement::GenericForInit(init) => init.0.right.iter_mut().for_each(f),
        Statement::GenericForNext(next) => {
            f(&mut next.generator);
            f(&mut next.state);
        }
        Statement::GenericFor(generic_for) => generic_for.right.iter_mut().for_each(f),
        Statement::Return(return_) => return_.values.iter_mut().for_each(f),
        Statement::SetList(set_list) => {
            set_list.values.iter_mut().for_each(&mut *f);
            if let Some(tail) = &mut set_list.tail {
                f(tail);
            }
        }
        Statement::Empty(_)
        | Statement::Goto(_)
        | Statement::Label(_)
        | Statement::Continue(_)
        | Statement::Break(_)
        | Statement::Close(_)
        | Statement::Comment(_) => {}
    }
}

fn for_each_inlineable_direct_rvalue(statement: &Statement, f: &mut impl FnMut(&RValue)) {
    match statement {
        Statement::While(_) | Statement::Repeat(_) => {}
        _ => for_each_direct_rvalue(statement, f),
    }
}

fn for_each_inlineable_direct_rvalue_mut(
    statement: &mut Statement,
    f: &mut impl FnMut(&mut RValue),
) {
    match statement {
        Statement::While(_) | Statement::Repeat(_) => {}
        _ => for_each_direct_rvalue_mut(statement, f),
    }
}

fn for_each_call_rvalue(call: &Call, f: &mut impl FnMut(&RValue)) {
    f(&call.value);
    call.arguments.iter().for_each(f);
}

fn for_each_call_rvalue_mut(call: &mut Call, f: &mut impl FnMut(&mut RValue)) {
    f(&mut call.value);
    call.arguments.iter_mut().for_each(f);
}

fn for_each_method_call_rvalue(method_call: &MethodCall, f: &mut impl FnMut(&RValue)) {
    f(&method_call.value);
    method_call.arguments.iter().for_each(f);
}

fn for_each_method_call_rvalue_mut(method_call: &mut MethodCall, f: &mut impl FnMut(&mut RValue)) {
    f(&mut method_call.value);
    method_call.arguments.iter_mut().for_each(f);
}

pub(crate) fn is_generated_temp(local: &RcLocal) -> bool {
    let Some(name) = local.0 .0.lock().0.clone() else {
        return false;
    };
    name == "v"
        || name
            .strip_prefix('v')
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn is_movable_single_value(rvalue: &RValue) -> bool {
    match rvalue {
        RValue::Local(_) | RValue::Literal(_) => true,
        // A constructor or closure always produces exactly one value. Its
        // children/body do not need to be individually "movable": the complete
        // object is relocated as one evaluation, with ordering and captured-cell
        // dependencies checked by the gates below.
        RValue::Table(_) | RValue::Closure(_) => true,
        RValue::Unary(unary) => !rvalue.has_side_effects() && is_movable_single_value(&unary.value),
        RValue::IfExpression(if_expression) => {
            !rvalue.has_side_effects()
                && is_movable_single_value(&if_expression.condition)
                && is_movable_single_value(&if_expression.then_value)
                && is_movable_single_value(&if_expression.else_value)
        }
        RValue::Global(_)
        | RValue::Index(_)
        | RValue::Binary(_)
        | RValue::Call(_)
        | RValue::MethodCall(_)
        | RValue::VarArg(_)
        | RValue::Select(_) => false,
    }
}

fn can_move_between(replacement: &RValue, statements: &[Statement], facts: &MotionFacts) -> bool {
    let read_locals = replacement
        .values_read()
        .into_iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    let reads_global = contains_global(replacement);
    let reads_captured_local = reads_captured_local(replacement, &facts.captured);
    let has_effects = replacement.has_side_effects();
    for statement in statements {
        if statement_writes_any_local(statement, &read_locals) {
            return false;
        }
        if reads_global && statement_may_mutate_global_or_environment(statement) {
            return false;
        }
        if reads_captured_local && statement.has_side_effects() {
            return false;
        }
        if has_effects && statement_evaluation_order_barrier(statement, facts) {
            return false;
        }
    }
    true
}

fn can_replace_after_prior_effects(
    replacement: &RValue,
    before_side_effects: bool,
    facts: &MotionFacts,
) -> bool {
    !before_side_effects
        || !(replacement.has_side_effects()
            || contains_global(replacement)
            || reads_captured_local(replacement, &facts.captured))
}

fn reads_captured_local(rvalue: &RValue, captured: &FxHashSet<RcLocal>) -> bool {
    rvalue
        .values_read()
        .into_iter()
        .any(|local| captured.contains(local))
}

fn contains_global(rvalue: &RValue) -> bool {
    if matches!(rvalue, RValue::Global(_)) {
        return true;
    }
    rvalue.rvalues().into_iter().any(contains_global)
}

fn rvalue_evaluation_order_barrier(rvalue: &RValue, facts: &MotionFacts) -> bool {
    rvalue.has_side_effects()
        || contains_global(rvalue)
        || rvalue
            .values_read()
            .into_iter()
            .any(|local| facts.captured.contains(local) && !facts.stable_captured.contains(local))
}

fn lvalue_evaluation_order_barrier(lvalue: &LValue, facts: &MotionFacts) -> bool {
    match lvalue {
        LValue::Local(_) => false,
        LValue::Global(_) => true,
        LValue::Index(index) => {
            index_component_order_barrier(&index.left, facts)
                || index_component_order_barrier(&index.right, facts)
        }
    }
}

fn index_component_order_barrier(value: &RValue, facts: &MotionFacts) -> bool {
    match value {
        RValue::Local(local) => {
            facts.captured.contains(local) && !facts.stable_captured.contains(local)
        }
        RValue::Literal(_) => false,
        RValue::Index(index) => {
            index_component_order_barrier(&index.left, facts)
                || index_component_order_barrier(&index.right, facts)
        }
        _ => true,
    }
}

fn statement_evaluation_order_barrier(statement: &Statement, facts: &MotionFacts) -> bool {
    let mut barrier = false;
    for_each_direct_rvalue(statement, &mut |rvalue| {
        barrier |= rvalue_evaluation_order_barrier(rvalue, facts);
    });
    barrier
        || statement_may_mutate_global_or_environment(statement)
        || match statement {
            Statement::If(r#if) => {
                block_evaluation_order_barrier(&r#if.then_block.lock(), facts)
                    || block_evaluation_order_barrier(&r#if.else_block.lock(), facts)
            }
            Statement::While(r#while) => {
                block_evaluation_order_barrier(&r#while.block.lock(), facts)
            }
            Statement::Repeat(repeat) => {
                block_evaluation_order_barrier(&repeat.block.lock(), facts)
            }
            Statement::NumericFor(numeric_for) => {
                block_evaluation_order_barrier(&numeric_for.block.lock(), facts)
            }
            Statement::GenericFor(generic_for) => {
                block_evaluation_order_barrier(&generic_for.block.lock(), facts)
            }
            _ => false,
        }
}

fn block_evaluation_order_barrier(block: &Block, facts: &MotionFacts) -> bool {
    block
        .0
        .iter()
        .any(|statement| statement_evaluation_order_barrier(statement, facts))
}

pub(crate) fn statement_writes_any_local(
    statement: &Statement,
    locals: &FxHashSet<RcLocal>,
) -> bool {
    statement
        .values_written()
        .into_iter()
        .any(|written| locals.contains(written))
        || match statement {
            Statement::If(r#if) => {
                block_writes_any_local(&r#if.then_block.lock(), locals)
                    || block_writes_any_local(&r#if.else_block.lock(), locals)
            }
            Statement::While(r#while) => block_writes_any_local(&r#while.block.lock(), locals),
            Statement::Repeat(repeat) => block_writes_any_local(&repeat.block.lock(), locals),
            Statement::NumericFor(numeric_for) => {
                block_writes_any_local(&numeric_for.block.lock(), locals)
            }
            Statement::GenericFor(generic_for) => {
                block_writes_any_local(&generic_for.block.lock(), locals)
            }
            _ => false,
        }
}

fn block_writes_any_local(block: &Block, locals: &FxHashSet<RcLocal>) -> bool {
    block
        .0
        .iter()
        .any(|statement| statement_writes_any_local(statement, locals))
}

fn statement_may_mutate_global_or_environment(statement: &Statement) -> bool {
    if statement.has_side_effects() {
        return true;
    }
    match statement {
        Statement::Assign(assign) => assign.left.iter().any(lvalue_may_mutate_global_or_index),
        Statement::If(r#if) => {
            block_may_mutate_global_or_environment(&r#if.then_block.lock())
                || block_may_mutate_global_or_environment(&r#if.else_block.lock())
        }
        Statement::While(r#while) => block_may_mutate_global_or_environment(&r#while.block.lock()),
        Statement::Repeat(repeat) => block_may_mutate_global_or_environment(&repeat.block.lock()),
        Statement::NumericFor(numeric_for) => {
            block_may_mutate_global_or_environment(&numeric_for.block.lock())
        }
        Statement::GenericFor(generic_for) => {
            block_may_mutate_global_or_environment(&generic_for.block.lock())
        }
        _ => false,
    }
}

fn block_may_mutate_global_or_environment(block: &Block) -> bool {
    block
        .0
        .iter()
        .any(statement_may_mutate_global_or_environment)
}

fn lvalue_may_mutate_global_or_index(lvalue: &LValue) -> bool {
    matches!(lvalue, LValue::Global(_) | LValue::Index(_))
}

#[cfg(test)]
mod tests {
    use super::inline_single_use_temps;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Closure, Function, Global, If, Index, LValue,
        Literal, Local, RValue, RcLocal, Repeat, Return, Select, Table, Upvalue, While,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn declare(local: &RcLocal, value: RValue) -> crate::Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    fn assign(left: LValue, value: RValue) -> crate::Statement {
        Assign::new(vec![left], vec![value]).into()
    }

    fn print(value: RValue) -> crate::Statement {
        Call::new(global("print"), vec![value]).into()
    }

    fn closure_capturing(local: &RcLocal) -> RValue {
        RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
    }

    #[test]
    fn inlines_generated_local_and_literal_temps() {
        let create = local("createElement");
        let temp_fn = local("v");
        let temp_name = local("v2");
        let mut block = Block(vec![
            declare(&temp_fn, local_value(&create)),
            declare(&temp_name, string("Frame")),
            Return::new(vec![Call::new(
                local_value(&temp_fn),
                vec![local_value(&temp_name)],
            )
            .into()])
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.to_string(), "return createElement(\"Frame\")");
    }

    #[test]
    fn inlines_pure_table_temps() {
        let temp = local("v3");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table(vec![
                    (Some(string("Name")), string("ProgressBar")),
                    (Some(string("LayoutOrder")), number(1.0)),
                ])),
            ),
            print(local_value(&temp)),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(
            block.to_string(),
            "print({\n\tName = \"ProgressBar\",\n\tLayoutOrder = 1\n})"
        );
    }

    #[test]
    fn inlines_named_single_use_table_at_declarative_return() {
        let children = local("children");
        let mut block = Block(vec![
            declare(
                &children,
                RValue::Table(Table(vec![(Some(string("Name")), string("Child"))])),
            ),
            Return::new(vec![local_value(&children)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.to_string(), "return {\n\tName = \"Child\"\n}");
    }

    #[test]
    fn named_table_is_not_inlined_into_plain_alias() {
        let config = local("config");
        let destination = local("destination");
        let mut block = Block(vec![
            declare(&config, RValue::Table(Table::default())),
            declare(&destination, local_value(&config)),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
    }

    #[test]
    fn inlines_effectful_table_when_use_preserves_evaluation_order() {
        let create = local("createElement");
        let temp = local("v4");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table(vec![(
                    Some(string("Child")),
                    Call::new(global("makeChild"), vec![]).into(),
                )])),
            ),
            Return::new(vec![Call::new(
                local_value(&create),
                vec![string("Frame"), local_value(&temp)],
            )
            .into()])
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 1);
        assert_eq!(
            block.to_string(),
            "return createElement(\"Frame\", {\n\tChild = makeChild()\n})"
        );
    }

    #[test]
    fn effectful_table_is_not_made_conditional_by_short_circuit_inline() {
        let temp = local("v4");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table(vec![(
                    Some(string("Child")),
                    Call::new(global("makeChild"), vec![]).into(),
                )])),
            ),
            Return::new(vec![Binary::new(
                global("enabled"),
                local_value(&temp),
                BinaryOperation::And,
            )
            .into()])
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
        assert!(block.to_string().contains("local v4 = {"));
        assert!(block.to_string().contains("return enabled and v4"));
    }

    #[test]
    fn stable_captured_callee_does_not_block_ui_table_inline() {
        let create = local("createElement");
        let temp = local("v4");
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![
                declare(
                    &temp,
                    RValue::Table(Table(vec![(
                        Some(string("Child")),
                        Call::new(global("makeChild"), vec![]).into(),
                    )])),
                ),
                Return::new(vec![Call::new(
                    local_value(&create),
                    vec![string("Frame"), local_value(&temp)],
                )
                .into()])
                .into(),
            ]),
            ..Function::default()
        }));
        let mut block = Block(vec![
            declare(&create, global("factory")),
            print(RValue::Closure(Closure {
                function: ByAddress(function.clone()),
                upvalues: vec![Upvalue::Ref(create)],
            })),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(
            function.lock().body.to_string(),
            "return createElement(\"Frame\", {\n\tChild = makeChild()\n})"
        );
    }

    #[test]
    fn stable_captured_callee_allows_adjacent_props_children_field_call() {
        let create = local("createElement");
        let target = local("children");
        let props = local("v7");
        let children = local("children2");
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![
                declare(&target, RValue::Table(Table::default())),
                declare(
                    &props,
                    RValue::Table(Table(vec![(Some(string("Name")), string("Title"))])),
                ),
                declare(
                    &children,
                    RValue::Table(Table(vec![(
                        Some(string("Constraint")),
                        Call::new(global("makeConstraint"), vec![]).into(),
                    )])),
                ),
                assign(
                    LValue::Index(Index::new(local_value(&target), string("Title"))),
                    RValue::Select(Select::Call(Call::new(
                        local_value(&create),
                        vec![
                            string("TextLabel"),
                            local_value(&props),
                            local_value(&children),
                        ],
                    ))),
                ),
            ]),
            ..Function::default()
        }));
        let mut block = Block(vec![
            declare(&create, global("factory")),
            print(RValue::Closure(Closure {
                function: ByAddress(function.clone()),
                upvalues: vec![Upvalue::Ref(create)],
            })),
        ]);

        inline_single_use_temps(&mut block);

        let output = function.lock().body.to_string();
        assert!(!output.contains("local v7"), "{output}");
        assert!(!output.contains("local children2"), "{output}");
        assert!(
            output.contains("children.Title = createElement("),
            "{output}"
        );
    }

    #[test]
    fn effectful_table_does_not_cross_intervening_effect() {
        let temp = local("v4");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table(vec![(
                    Some(string("Child")),
                    Call::new(global("makeChild"), vec![]).into(),
                )])),
            ),
            Call::new(global("between"), vec![]).into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn effectful_table_does_not_move_after_prior_call_argument() {
        let temp = local("v4");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table(vec![(
                    Some(string("Child")),
                    Call::new(global("makeChild"), vec![]).into(),
                )])),
            ),
            Return::new(vec![Call::new(
                global("consume"),
                vec![
                    Call::new(global("before"), vec![]).into(),
                    local_value(&temp),
                ],
            )
            .into()])
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
    }

    #[test]
    fn does_not_inline_call_rhs() {
        let temp = local("v4");
        let mut block = Block(vec![
            declare(&temp, Call::new(global("makeValue"), vec![]).into()),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
        assert!(matches!(&block.0[0], crate::Statement::Assign(_)));
    }

    #[test]
    fn does_not_create_call_receiver_property_assignment() {
        let temp = local("v4");
        let parent = local("parent");
        let mut block = Block(vec![
            declare(&temp, Call::new(global("makeInstance"), vec![]).into()),
            assign(
                LValue::Index(Index::new(local_value(&temp), string("Parent"))),
                local_value(&parent),
            ),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
        assert_eq!(
            block.to_string(),
            "local v4 = makeInstance()\nv4.Parent = parent"
        );
    }

    #[test]
    fn does_not_inline_mutated_table_temp() {
        let temp = local("v5");
        let mut block = Block(vec![
            declare(&temp, RValue::Table(Table(vec![]))),
            assign(
                LValue::Index(Index::new(local_value(&temp), string("Name"))),
                string("Value"),
            ),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn does_not_inline_meaningfully_named_local() {
        let named = local("result");
        let mut block = Block(vec![
            declare(&named, string("Frame")),
            Return::new(vec![local_value(&named)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
    }

    #[test]
    fn does_not_move_local_read_past_write_to_dependency() {
        let source = local("source");
        let temp = local("v6");
        let mut block = Block(vec![
            declare(&temp, local_value(&source)),
            assign(LValue::Local(source.clone()), string("changed")),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn sees_writes_inside_nested_blocks() {
        let source = local("source");
        let temp = local("v7");
        let mut block = Block(vec![
            declare(&temp, local_value(&source)),
            If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![assign(
                    LValue::Local(source.clone()),
                    string("changed"),
                )]),
                Block(vec![]),
            )
            .into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn does_not_inline_into_while_condition() {
        let source = local("source");
        let temp = local("v8");
        let mut block = Block(vec![
            declare(&temp, local_value(&source)),
            While::new(
                local_value(&temp),
                Block(vec![assign(
                    LValue::Local(source.clone()),
                    RValue::Literal(Literal::Boolean(false)),
                )]),
            )
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
    }

    #[test]
    fn does_not_inline_into_repeat_condition() {
        let source = local("source");
        let temp = local("v9");
        let mut block = Block(vec![
            declare(&temp, local_value(&source)),
            Repeat::new(
                local_value(&temp),
                Block(vec![assign(
                    LValue::Local(source.clone()),
                    RValue::Literal(Literal::Boolean(false)),
                )]),
            )
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
    }

    #[test]
    fn does_not_move_captured_dependency_past_intervening_call() {
        let source = local("source");
        let handler = local("handler");
        let temp = local("v10");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&source)),
            declare(&temp, local_value(&source)),
            Call::new(global("mutate"), vec![]).into(),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 4);
    }

    #[test]
    fn does_not_move_captured_dependency_past_prior_return_value_call() {
        let source = local("source");
        let handler = local("handler");
        let temp = local("v11");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&source)),
            declare(&temp, local_value(&source)),
            Return::new(vec![
                Call::new(global("mutate"), vec![]).into(),
                local_value(&temp),
            ])
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn does_not_inline_binary_rhs() {
        let source = local("source");
        let temp = local("v12");
        let mut block = Block(vec![
            declare(
                &temp,
                Binary::new(local_value(&source), number(1.0), BinaryOperation::Add).into(),
            ),
            Return::new(vec![local_value(&temp)]).into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.0.len(), 2);
    }

    #[test]
    fn inlines_chain_until_fixed_point() {
        let source = local("source");
        let first = local("v13");
        let second = local("v14");
        let mut block = Block(vec![
            declare(&first, local_value(&source)),
            declare(&second, local_value(&first)),
            Return::new(vec![Binary::new(
                local_value(&second),
                number(1.0),
                BinaryOperation::Add,
            )
            .into()])
            .into(),
        ]);

        inline_single_use_temps(&mut block);

        assert_eq!(block.to_string(), "return source + 1");
    }
}
