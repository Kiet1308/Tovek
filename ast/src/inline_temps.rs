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
    numbers: FxHashSet<RcLocal>,
    rebuild_call_chains: bool,
}

impl MotionFacts {
    fn total_numeric(&self, value: &RValue) -> bool {
        self.rebuild_call_chains && crate::numeric_facts::total(value, &self.numbers)
    }

    fn candidate_effects(&self, value: &RValue) -> crate::effects::Summary {
        let capture = |local: &RcLocal| self.captured.contains(local) && !self.stable_captured.contains(local);
        if self.total_numeric(value) {
            crate::effects::Summary {
                effects: if value.values_read().into_iter().any(capture) { crate::effects::Effects::CAPTURE_READ }
                    else { crate::effects::Effects::default() },
                nodes: 0, exhausted: false,
            }
        } else { crate::effects::summarize(value, &capture) }
    }
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
    let mut facts = collect_motion_facts(block);
    facts.rebuild_call_chains = true;
    let mut any_changed = false;
    loop {
        let rebuilt = crate::rebuild_table_literals::rebuild_with_captured(block, &facts.captured, &facts.stable_captured);
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
        numbers: crate::numeric_facts::collect(block, &usage),
        rebuild_call_chains: false,
    }
}

fn collect_stable_declared_locals(
    block: &Block,
    usage: &FxHashMap<RcLocal, Usage>,
    stable: &mut FxHashSet<RcLocal>,
) {
    // A recursive function has a nil predeclaration followed immediately by
    // closure installation. There is no executed expression between the two
    // writes, and constructing the closure does not run its body. With no
    // further writes, every later read sees that installed function. Keep
    // non-adjacent, conditional, loop-carried and reassigned cells unknown.
    if !crate::simplify_gotos::function_tree_has_goto_or_label(block) {
        for pair in block.0.windows(2) {
            let (Statement::Assign(decl), Statement::Assign(init)) = (&pair[0], &pair[1]) else { continue; };
            if !decl.prefix || decl.parallel || decl.left.len() != 1
                || !(decl.right.is_empty() || matches!(decl.right.as_slice(), [RValue::Literal(crate::Literal::Nil)]))
                || init.prefix || init.parallel || init.left.len() != 1
                || !matches!(init.right.as_slice(), [RValue::Closure(_)]) { continue; }
            let LValue::Local(local) = &decl.left[0] else { continue; };
            if init.left[0].as_local() == Some(local) && usage.get(local).is_some_and(|u| u.writes == 2) {
                stable.insert(local.clone());
            }
        }
    }
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
        if replacement.values_read().iter().any(|read| **read == local) {
            continue;
        }

        let Some(use_index) = (index + 1..block.0.len()).find(|&use_index| {
            inlineable_direct_rvalue_read_count(&block.0[use_index], &local) > 0
                || (facts.rebuild_call_chains
                    && is_single_index_key_use(&block.0[use_index], &local))
        }) else {
            continue;
        };
        // A call used as another call's callee is always adjusted to one
        // result, just like its original single-local initializer. Reuse the
        // ordinary motion/order/conditional guards; never inline it into a
        // multret argument or a branch that may not execute.
        let call_callee = facts.rebuild_call_chains
            && !is_service_or_require_handle(&replacement)
            && matches!(
                &replacement,
                RValue::Call(_)
                    | RValue::MethodCall(_)
                    | RValue::Select(Select::Call(_) | Select::MethodCall(_))
            )
            && is_call_callee_use(&block.0[use_index], &local);
        // Field aliases and scalar calls need the same evaluation-position
        // proof as curried callees. Recorded bindings stay protected.
        let ordered_alias = facts.rebuild_call_chains && !is_service_or_require_handle(&replacement) && matches!(&replacement,
            RValue::Index(_) | RValue::Select(Select::Call(_) | Select::MethodCall(_)));
        // Operators may throw or dispatch metamethods. A generated snapshot
        // can still fold into its original evaluation position, using the same
        // motion and conditional proofs as call/field aliases below. Meaningful
        // names and recorded source bindings remain declarations.
        let ordered_operator = facts.rebuild_call_chains && generated
            && matches!(&replacement, RValue::Binary(_) | RValue::Unary(_));
        // A sole-use import forwarded into a named table field gains no
        // readable role from an extra alias: the destination already names it.
        // This only admits a candidate; the ordinary source-binding, capture,
        // motion and evaluation-position checks below still decide legality.
        let import_field_store = facts.rebuild_call_chains
            && is_service_or_require_handle(&replacement)
            && is_named_field_store_use(&block.0[use_index], &local);
        let named_function = crate::assignment_preserves_function_name(&block.0[use_index], &local)
            || (matches!(&replacement, RValue::Closure(_))
                && crate::local::constructor_preserves_function_name(&block.0[use_index], &local));
        if local.preserve_binding() && !named_function {
            crate::telemetry::count("inline_refused_source_binding", 1);
            continue;
        }
        if !call_callee && !ordered_alias && !ordered_operator && !import_field_store && ((!generated && !named_table && !named_function) || !is_movable_single_value(&replacement))
        {
            crate::telemetry::count("inline_refused_expression_role", 1);
            continue;
        }
        if named_table && !is_declarative_table_use(&block.0[use_index], &local) {
            continue;
        }
        if !can_move_between(&replacement, &block.0[index + 1..use_index], facts) {
            crate::telemetry::count("inline_refused_intervening_statement", 1);
            continue;
        }
        if !crate::evaluation_order::can_sink_with_summary(&block.0[use_index], &local, &replacement, &|l| {
            facts.captured.contains(l) && !facts.stable_captured.contains(l)
        }, facts.candidate_effects(&replacement)) {
            crate::telemetry::count("inline_refused_evaluation_position", 1);
            continue;
        }
        if facts.rebuild_call_chains
            && matches!(&replacement, RValue::Local(_) | RValue::Literal(_))
            && replace_single_index_key_use(&mut block.0[use_index], &local, &replacement, facts)
        {
            block.0.remove(index);
            return true;
        }
        let numeric = facts.total_numeric(&replacement);
        if replace_direct_rvalue_use(&mut block.0[use_index], &local, replacement, facts) {
            crate::telemetry::count("inline_accepted", 1);
            if numeric { crate::telemetry::count("inline_accepted_numeric_proof", 1); }
            block.0.remove(index);
            return true;
        }
        crate::telemetry::count("inline_refused_arity_or_context", 1);
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

fn is_named_field_store_use(statement: &Statement, local: &RcLocal) -> bool {
    matches!(statement, Statement::Assign(assign)
        if !assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1
            && matches!(&assign.right[0], RValue::Local(read) if read == local)
            && matches!(&assign.left[0], LValue::Index(index)
                if matches!(index.right.as_ref(), RValue::Literal(crate::Literal::String(key))
                    if std::str::from_utf8(key).ok().is_some_and(crate::valid_source_name))))
}

fn is_call_callee_use(statement: &Statement, local: &RcLocal) -> bool {
    fn in_value(value: &RValue, local: &RcLocal) -> bool {
        let is_callee = match value {
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                matches!(call.value.as_ref(), RValue::Local(read) if read == local)
            }
            _ => false,
        };
        is_callee
            || value
                .rvalues()
                .into_iter()
                .any(|child| in_value(child, local))
    }
    let mut found = matches!(statement, Statement::Call(call)
        if matches!(call.value.as_ref(), RValue::Local(read) if read == local));
    for_each_inlineable_direct_rvalue(statement, &mut |value| found |= in_value(value, local));
    found
}

fn is_single_index_key_use(statement: &Statement, local: &RcLocal) -> bool {
    matches!(statement, Statement::Assign(assign)
        if !assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1
            && matches!(&assign.left[0], LValue::Index(index)
                if matches!(index.right.as_ref(), RValue::Local(read) if read == local)))
}

fn replace_single_index_key_use(
    statement: &mut Statement,
    local: &RcLocal,
    replacement: &RValue,
    facts: &MotionFacts,
) -> bool {
    if !is_single_index_key_use(statement, local) {
        return false;
    }
    let Statement::Assign(assign) = statement else {
        unreachable!()
    };
    let LValue::Index(index) = &mut assign.left[0] else {
        unreachable!()
    };
    if !can_replace_after_prior_effects(
        replacement,
        rvalue_evaluation_order_barrier(&index.left, facts),
        facts,
    ) {
        return false;
    }
    *index.right = replacement.clone();
    true
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

pub(crate) fn collect_closures_in_statement(statement: &Statement, f: &mut impl FnMut(&crate::Closure)) {
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
        // The caller has already proved this exact use with can_sink_with_summary.
        // Closure construction does not execute its body: captures can commute
        // with earlier capture reads. The older boolean barrier below cannot
        // distinguish those reads from callbacks/writes, which the position
        // proof still rejects. Keep the independent conditional-use refusal.
        if (!matches!(&replacement, RValue::Closure(_))
            && !can_replace_after_prior_effects(&replacement, *before_side_effects, facts))
            || (conditionally_evaluated && rvalue_evaluation_order_barrier(&replacement, facts))
        {
            return false;
        }
        *rvalue = replacement;
        crate::node_origins::inlined(rvalue);
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
    if local.preserve_binding() { return false; }
    let Some(name) = local.0 .0.lock().0.clone() else {
        return false;
    };
    name == "v"
        || name
            .strip_prefix('v')
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

pub(crate) fn is_movable_single_value(rvalue: &RValue) -> bool {
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

/// Readability protection shared with SSA: module/service handles retain their
/// named header declaration. This is a refusal, never an API purity assumption.
pub fn is_service_or_require_handle(rvalue: &RValue) -> bool {
    match rvalue {
        RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) =>
            call.method == "GetService" && matches!(call.arguments.first(), Some(RValue::Literal(crate::Literal::String(_)))),
        RValue::Call(call) | RValue::Select(Select::Call(call)) =>
            matches!(&*call.value, RValue::Global(global) if global.0.as_slice() == b"require"),
        _ => false,
    }
}

fn can_move_between(replacement: &RValue, statements: &[Statement], facts: &MotionFacts) -> bool {
    let read_locals = replacement
        .values_read()
        .into_iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    let reads_global = contains_global(replacement);
    let reads_captured_local = reads_motion_sensitive_capture(replacement, facts);
    // A replacement can be observable without being marked as side-effecting:
    // for example, `{[nil] = 1}` raises while evaluating the constructor. Such
    // expressions must not cross another evaluation barrier or their error is
    // reordered (and may be swallowed by a later control-flow path).
    let has_effects = !facts.total_numeric(replacement) && crate::is_observable(replacement);
    for statement in statements {
        if statement_writes_any_local(statement, &read_locals) {
            return false;
        }
        if reads_global && statement_may_mutate_global_or_environment(statement) {
            return false;
        }
        if reads_captured_local && crate::statement_is_observable(statement) {
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
        || !((!facts.total_numeric(replacement) && crate::is_observable(replacement))
            || contains_global(replacement)
            || reads_motion_sensitive_capture(replacement, facts))
}

fn reads_motion_sensitive_capture(rvalue: &RValue, facts: &MotionFacts) -> bool {
    rvalue.values_read().into_iter().any(|local| {
        facts.captured.contains(local)
            && !(facts.rebuild_call_chains && facts.stable_captured.contains(local))
    })
}

fn contains_global(rvalue: &RValue) -> bool {
    if matches!(rvalue, RValue::Global(_)) {
        return true;
    }
    rvalue.rvalues().into_iter().any(contains_global)
}

fn rvalue_evaluation_order_barrier(rvalue: &RValue, facts: &MotionFacts) -> bool {
    (!facts.total_numeric(rvalue) && crate::is_observable(rvalue))
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
        // The outer LValue index stores after the RHS, but an index inside
        // its base/key executes before it. Even local/literal children may
        // invoke __index, mutate captured state, yield or raise here.
        RValue::Index(_) => true,
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
    if crate::statement_is_observable(statement) {
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
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
    }

    #[test]
    fn named_constructor_helper_crosses_capture_reads_but_not_observers_or_source_bindings() {
        for barrier in 0..10 {
            let state = local("state");
            let helper = local("fire");
            helper.0.lock().add_source_binding(crate::SourceBinding {
                origin: crate::BindingOrigin::Function { prototype: 1 }, name: "fire".into(),
            });
            if barrier == 4 { helper.0.lock().add_source_binding(crate::SourceBinding {
                origin: crate::BindingOrigin::DebugLocal { prototype: 0, register: 1, start_pc: 0, end_pc: 10 },
                name: "fire".into(),
            }); }
            let function = Arc::new(Mutex::new(Function {
                body: Block(vec![assign(state.clone().into(), number(1.0)), Return::new(vec![local_value(&state)]).into()]),
                ..Function::default()
            }));
            let mut upvalues = vec![Upvalue::Ref(state.clone())];
            if barrier == 7 { upvalues.push(Upvalue::Ref(helper.clone())); }
            let closure = RValue::Closure(Closure {
                node_origin: Default::default(), function: ByAddress(function.clone()), upvalues,
            });
            let first = if barrier == 1 { Call::new(global("observe"), vec![]).into() }
                else { closure_capturing(&state) };
            let mut fields = vec![(Some(string("subscribe")), first),
                (Some(string(if barrier == 5 { "other" } else { "fire" })), local_value(&helper))];
            if barrier == 1 { fields.push((Some(string("other")), closure_capturing(&state))); }
            if barrier == 6 { fields.push((Some(string("again")), local_value(&helper))); }
            let table = RValue::Table(Table::new(fields));
            let returned = if barrier == 9 {
                Binary::new(global("enabled"), table, BinaryOperation::And).into()
            } else { table };
            let tail = Return::new(vec![returned]).into();
            let mut block = Block(vec![declare(&state, number(0.0)), declare(&helper, closure)]);
            if barrier == 2 { block.0.push(Call::new(global("observe"), vec![]).into()); }
            if barrier == 3 { block.0.push(assign(state.clone().into(), number(2.0))); }
            if barrier == 8 {
                block.0.push(While::new(global("enabled"), Block(vec![tail])).into());
            } else { block.0.push(tail); }
            let before = block.to_string();
            if barrier == 0 {
                let facts = super::collect_motion_facts(&block);
                let candidate = &block.0[1].as_assign().unwrap().right[0];
                assert!(!super::can_replace_after_prior_effects(candidate, true, &facts));
                assert!(crate::evaluation_order::can_sink_with_summary(&block.0[2], &helper,
                    candidate, &|l| facts.captured.contains(l) && !facts.stable_captured.contains(l),
                    facts.candidate_effects(candidate)));
            }
            let changed = super::rebuild_ui_expression_trees(&mut block);
            assert_eq!(changed, barrier == 0, "barrier {barrier}: {}", block);
            if barrier == 0 {
                assert!(!block.to_string().contains("local function fire"));
                assert!(block.to_string().contains("fire = function()"));
                let table = block.0.last().unwrap().as_return().unwrap().values[0].as_table().unwrap();
                let installed = table.0[1].1.as_closure().unwrap();
                assert!(Arc::ptr_eq(&installed.function.0, &function));
            } else { assert_eq!(block.to_string(), before, "barrier {barrier}"); }
        }
    }

    #[test]
    fn constructor_function_name_exception_is_late_and_requires_recorded_matching_name() {
        for source in 0..5 {
            let helper = local("fire");
            if source != 0 { helper.0.lock().add_source_binding(crate::SourceBinding {
                origin: if source == 2 { crate::BindingOrigin::DebugUpvalue { prototype: 1, slot: 0 } }
                    else { crate::BindingOrigin::Function { prototype: 1 } },
                name: if source == 3 { "other".into() } else { "fire".into() },
            }); }
            let mut fields = vec![(Some(string("fire")), local_value(&helper))];
            if source != 4 { fields.push((Some(string("subscribe")), closure_capturing(&local("state")))); }
            let statement = Return::new(vec![Table::new(fields).into()]).into();
            assert_eq!(crate::local::constructor_preserves_function_name(&statement, &helper), source == 1);
            assert!(!crate::assignment_preserves_function_name(&statement, &helper));
        }
    }

    #[test]
    fn ui_callee_chain_keeps_calls_and_single_value_context() {
        let factory = local("factory");
        let handle = local("frame");
        let mut block = Block(vec![
            declare(
                &handle,
                Call::new(local_value(&factory), vec![string("Frame")]).into(),
            ),
            Return::new(vec![
                Call::new(local_value(&handle), vec![number(1.0)]).into(),
            ])
            .into(),
        ]);
        assert!(!inline_single_use_temps(&mut block));
        assert!(super::rebuild_ui_expression_trees(&mut block));
        assert_eq!(block.to_string(), "return (factory(\"Frame\"))(1)");
    }

    #[test]
    fn field_aliases_inline_at_proven_slots_and_keep_snapshots() {
        let object = local("object"); let alias = local("_field"); let callback = local("callback");
        for barrier in 0..4 {
            let mut block = Block(vec![declare(&alias, Index::new(local_value(&object), string("field")).into())]);
            if barrier == 1 { block.0.push(Call::new(local_value(&callback), vec![]).into()); }
            let ret = if barrier == 2 { vec![global("environment"), local_value(&alias)] }
                else { vec![local_value(&alias)] };
            block.0.push(Return::new(ret).into());
            if barrier == 3 { alias.0.lock().add_source_binding(crate::SourceBinding {
                name: "field".into(), origin: crate::BindingOrigin::DebugLocal { prototype: 1, register: 0, start_pc: 0, end_pc: 5 }
            }); }
            let before = block.to_string();
            assert_eq!(super::rebuild_ui_expression_trees(&mut block), barrier == 0);
            if barrier == 0 { assert_eq!(block.to_string(), "return object.field"); }
            else { assert_eq!(before, block.to_string()); }
        }
    }

    #[test]
    fn scalar_call_alias_keeps_single_result_in_last_return_slot() {
        let factory = local("factory"); let value = local("result");
        let mut block = Block(vec![
            declare(&value, RValue::Select(Select::Call(Call::new(local_value(&factory), vec![])))),
            Return::new(vec![number(1.0), local_value(&value)]).into(),
        ]);
        assert!(super::rebuild_ui_expression_trees(&mut block));
        let crate::Statement::Return(ret) = &block.0[0] else { panic!(); };
        assert!(matches!(ret.values[1], RValue::Select(Select::Call(_))));
    }

    #[test]
    fn ordered_alias_cleanup_retains_named_module_imports() {
        let module = local("colors");
        let mut block = Block(vec![
            declare(&module, RValue::Select(Select::Call(Call::new(global("require"), vec![string("Colors")])))),
            Return::new(vec![Index::new(local_value(&module), string("Black")).into()]).into(),
        ]);
        let before = block.to_string();
        assert!(!super::rebuild_ui_expression_trees(&mut block));
        assert_eq!(before, block.to_string());
    }

    #[test]
    fn callee_cleanup_retains_import_and_service_headers() {
        for scalar in [false, true] {
            for service in [false, true] {
                for statement_call in [false, true] {
                    let handle = local("factory");
                    let initializer = if service {
                        let call = crate::MethodCall::new(global("game"), "GetService".into(), vec![string("Factory")]);
                        if scalar { RValue::Select(Select::MethodCall(call)) } else { call.into() }
                    } else {
                        let call = Call::new(global("require"), vec![string("Factory")]);
                        if scalar { RValue::Select(Select::Call(call)) } else { call.into() }
                    };
                    let call = Call::new(local_value(&handle), vec![number(1.0)]);
                    let use_ = if statement_call { call.into() }
                        else { Return::new(vec![call.into()]).into() };
                    let mut block = Block(vec![declare(&handle, initializer), use_]);
                    let before = block.to_string();
                    assert!(!super::rebuild_ui_expression_trees(&mut block));
                    assert_eq!(block.to_string(), before);
                }
            }
        }
    }

    #[test]
    fn named_import_field_store_folds_only_at_safe_evaluation_positions() {
        for barrier in 0..6 {
            let module = local("component");
            let object = local("components");
            let mut block = Block(vec![declare(&module,
                RValue::Select(Select::Call(Call::new(global("require"), vec![string("Component")]))))]);
            if barrier == 1 { block.0.push(Call::new(global("between"), vec![]).into()); }
            if barrier == 4 { module.0.lock().add_source_binding(crate::SourceBinding {
                name: "component".into(), origin: crate::BindingOrigin::DebugLocal {
                    prototype: 1, register: 0, start_pc: 0, end_pc: 5,
                },
            }); }
            let receiver = if barrier == 2 { Index::new(local_value(&object), string("Nested")).into() }
                else { local_value(&object) };
            let key = if barrier == 3 { global("key") } else { string("Component") };
            block.0.push(assign(Index::new(receiver, key).into(), local_value(&module)));
            if barrier == 5 { block.0.push(Return::new(vec![local_value(&module)]).into()); }
            let before = block.to_string();
            assert_eq!(super::rebuild_ui_expression_trees(&mut block), barrier == 0);
            if barrier == 0 { assert_eq!(block.to_string(), "components.Component = require(\"Component\")"); }
            else { assert_eq!(block.to_string(), before); }
        }
    }

    #[test]
    fn import_field_store_preserves_a_mutable_captured_receiver() {
        let module = local("component");
        let object = local("components");
        let mut block = Block(vec![
            declare(&object, global("initial")),
            Call::new(global("publish"), vec![closure_capturing(&object)]).into(),
            assign(object.clone().into(), global("replacement")),
            declare(&module, RValue::Select(Select::Call(Call::new(global("require"), vec![string("Component")])))),
            assign(Index::new(local_value(&object), string("Component")).into(), local_value(&module)),
        ]);
        let before = block.to_string();
        super::rebuild_ui_expression_trees(&mut block);
        assert_eq!(block.to_string(), before);
    }

    #[test]
    fn ordered_operator_cleanup_keeps_effects_branches_and_binding_roles() {
        for barrier in 0..7 {
            let source = local("source");
            let temp = local(if barrier == 6 { "distance" } else { "v12" });
            let mut block = Block(vec![declare(&temp,
                Binary::new(local_value(&source), number(2.0), BinaryOperation::Mul).into())]);
            if barrier == 1 {
                block.0.push(Call::new(global("mutate"), vec![]).into());
            }
            if barrier == 4 {
                block.0.push(assign(source.clone().into(), number(3.0)));
            }
            if barrier == 5 {
                temp.0.lock().add_source_binding(crate::SourceBinding {
                    name: "distance".into(), origin: crate::BindingOrigin::DebugLocal {
                        prototype: 1, register: 0, start_pc: 0, end_pc: 5,
                    },
                });
            }
            let result = match barrier {
                2 => Binary::new(Call::new(global("first"), vec![]).into(),
                    local_value(&temp), BinaryOperation::Add).into(),
                3 => Binary::new(local_value(&source), local_value(&temp), BinaryOperation::And).into(),
                _ => Binary::new(local_value(&temp), number(4.0), BinaryOperation::Add).into(),
            };
            block.0.push(Return::new(vec![result]).into());
            let before = block.to_string();
            assert_eq!(super::rebuild_ui_expression_trees(&mut block), barrier == 0);
            if barrier == 0 { assert_eq!(block.to_string(), "return source * 2 + 4"); }
            else { assert_eq!(block.to_string(), before); }
        }
    }

    #[test]
    fn checked_numeric_counter_arithmetic_crosses_calls_but_not_counter_writes() {
        for mutate in [false, true] {
            let counter = local("i");
            let temp = local("v9");
            let mut body = Block(vec![
                declare(&temp, Binary::new(local_value(&counter), number(1.0), BinaryOperation::Add).into()),
                Call::new(global("between"), vec![]).into(),
            ]);
            if mutate { body.0.push(assign(counter.clone().into(), global("replacement"))); }
            body.0.push(Call::new(global("consume"), vec![local_value(&temp)]).into());
            let mut block = Block(vec![crate::NumericFor::new(number(1.0), number(3.0), number(1.0), counter, body).into()]);
            let before = block.to_string();
            assert_eq!(super::rebuild_ui_expression_trees(&mut block), !mutate);
            let output = block.to_string();
            if mutate { assert_eq!(output, before); }
            else { assert!(!output.contains("local v9"), "{output}"); assert!(output.contains("consume(i + 1)"), "{output}"); }
        }
    }

    #[test]
    fn completed_length_snapshot_proves_arithmetic_but_keeps_length_effects() {
        for operation in [BinaryOperation::Add, BinaryOperation::IDiv, BinaryOperation::Mod, BinaryOperation::Pow] {
            for overwritten in [false, true] {
                let count = local("count");
                let temp = local("v9");
                let length: RValue = crate::Unary::new(global("input"), crate::UnaryOperation::Length).into();
                let mut block = Block(vec![declare(&count, length.clone())]);
                if overwritten { block.0.push(assign(count.clone().into(), global("replacement"))); }
                block.0.push(declare(&temp, Binary::new(local_value(&count), number(2.0), operation).into()));
                block.0.push(Call::new(global("consume"), vec![local_value(&temp)]).into());
                let facts = super::collect_motion_facts(&block);
                assert!(!crate::numeric_facts::total(&length, &facts.numbers));
                assert_eq!(facts.numbers.contains(&count), !overwritten);
                let before = block.to_string();
                assert_eq!(super::rebuild_ui_expression_trees(&mut block), !overwritten);
                if overwritten { assert_eq!(before, block.to_string()); }
                else { assert!(block.to_string().contains("local count = #input")); assert!(!block.to_string().contains("local v9")); }
            }
        }
    }

    #[test]
    fn recursive_callee_snapshot_requires_adjacent_unconditional_final_installation() {
        for barrier in 0..5 {
            let helper = local("helper");
            let temp = local("v9");
            let function = Arc::new(Mutex::new(Function {
                body: Block(vec![declare(&temp, local_value(&helper)),
                    Call::new(global("consume"), vec![local_value(&temp)]).into()]),
                ..Function::default()
            }));
            let closure = RValue::Closure(Closure {
                node_origin: Default::default(), function: ByAddress(function.clone()), upvalues: vec![Upvalue::Ref(helper.clone())],
            });
            let mut declaration = Assign::new(vec![helper.clone().into()], vec![]); declaration.prefix = true;
            let mut block = Block(vec![declaration.into()]);
            if barrier == 1 { block.0.push(Call::new(global("observe"), vec![]).into()); }
            let install = assign(helper.clone().into(), closure);
            if barrier == 2 { block.0.push(If::new(global("enabled"), Block(vec![install]), Block::default()).into()); }
            else { block.0.push(install); }
            if barrier == 3 { block.0.push(assign(helper.clone().into(), global("replacement"))); }
            if barrier == 4 {
                block.0.push(declare(&local("setter"), RValue::Closure(Closure {
                    node_origin: Default::default(), upvalues: vec![Upvalue::Ref(helper.clone())],
                    function: ByAddress(Arc::new(Mutex::new(Function {
                        body: Block(vec![assign(helper.clone().into(), global("replacement"))]), ..Function::default()
                    }))),
                })));
            }
            block.0.push(Return::new(vec![local_value(&helper)]).into());
            let facts = super::collect_motion_facts(&block);
            assert_eq!(facts.stable_captured.contains(&helper), barrier == 0);
            super::rebuild_ui_expression_trees(&mut block);
            assert_eq!(function.lock().body.to_string().contains("local v9"), barrier != 0);
        }
    }

    #[test]
    fn numeric_motion_requires_runtime_facts_not_type_hints_or_environment_constants() {
        for kind in 0..4 {
            let input = local("input");
            input.0.lock().1 = Some("number".into());
            let temp = local("v9");
            let value = if kind == 1 { number(std::f64::consts::PI) }
                else if kind == 2 { number(f64::INFINITY) }
                else { local_value(&input) };
            let mut block = Block(vec![]);
            if kind == 3 { block.0.push(declare(&input, number(2.0))); }
            block.0.push(declare(&temp, Binary::new(value, number(1.0), BinaryOperation::Add).into()));
            block.0.push(Call::new(global("consume"), vec![local_value(&temp)]).into());
            let before = block.to_string();
            assert_eq!(super::rebuild_ui_expression_trees(&mut block), kind == 3);
            if kind == 3 { assert!(block.to_string().contains("consume(input + 1)")); }
            else { assert_eq!(block.to_string(), before); }
        }
    }

    #[test]
    fn ui_callee_chain_preserves_nested_assignment_evaluation() {
        for shape in 0..3 {
            let factory = local("factory");
            let handle = local("v1");
            let target = local("target");
            let keys = local("keys");
            let base = if shape != 1 {
                Index::new(local_value(&target), string("Child")).into()
            } else {
                local_value(&target)
            };
            let key = if shape != 0 {
                Index::new(local_value(&keys), string("Key")).into()
            } else {
                string("Value")
            };
            let mut block = Block(vec![
                declare(&handle, Call::new(local_value(&factory), vec![]).into()),
                assign(Index::new(base, key).into(), Call::new(local_value(&handle), vec![]).into()),
            ]);
            let before = block.to_string();
            assert!(!super::rebuild_ui_expression_trees(&mut block));
            assert_eq!(block.to_string(), before);
        }
    }

    #[test]
    fn ui_callee_chain_can_cross_only_the_terminal_index_store() {
        let factory = local("factory");
        let handle = local("v1");
        let target = local("target");
        let mut block = Block(vec![
            declare(&handle, Call::new(local_value(&factory), vec![]).into()),
            assign(
                Index::new(local_value(&target), string("Value")).into(),
                Call::new(local_value(&handle), vec![]).into(),
            ),
        ]);
        assert!(super::rebuild_ui_expression_trees(&mut block));
        assert_eq!(block.0.len(), 1);
        assert_eq!(block.to_string(), "target.Value = (factory())()");
    }

    #[test]
    fn captured_snapshot_preserves_nested_assignment_reads() {
        let source = local("source");
        let handler = local("handler");
        let temp = local("v1");
        let target = local("target");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&source)),
            declare(&temp, local_value(&source)),
            assign(
                Index::new(Index::new(local_value(&target), string("Child")).into(), string("Value")).into(),
                local_value(&temp),
            ),
        ]);
        let before = block.to_string();
        assert!(!inline_single_use_temps(&mut block));
        assert_eq!(block.to_string(), before);
    }

    #[test]
    fn ui_callee_chain_does_not_cross_effects_or_become_conditional() {
        for kind in 0..4 {
            let handle = local("frame");
            let mut statements = vec![declare(&handle, Call::new(global("new"), vec![]).into())];
            let call: RValue = Call::new(local_value(&handle), vec![]).into();
            let use_value = match kind {
                0 => {
                    statements.push(Call::new(global("effect"), vec![]).into());
                    call
                }
                1 => Binary::new(global("enabled"), call, BinaryOperation::And).into(),
                2 => Call::new(global("consume"), vec![
                    Call::new(global("effect"), vec![]).into(),
                    call,
                ])
                .into(),
                _ => Call::new(global("consume"), vec![local_value(&handle)]).into(),
            };
            statements.push(Return::new(vec![use_value]).into());
            let mut block = Block(statements);
            let before = block.to_string();
            super::rebuild_ui_expression_trees(&mut block);
            assert_eq!(block.to_string(), before, "case {kind}");
        }
    }

    #[test]
    fn ui_key_snapshot_stays_before_mutating_callback() {
        let source = local("key");
        let snapshot = local("v2");
        let props = local("props");
        let callback = local("mutate");
        let mut block = Block(vec![
            declare(&callback, closure_capturing(&source)),
            declare(&snapshot, local_value(&source)),
            Call::new(local_value(&callback), vec![]).into(),
            assign(
                Index::new(local_value(&props), local_value(&snapshot)).into(),
                number(2.0),
            ),
        ]);
        super::rebuild_ui_expression_trees(&mut block);
        assert!(block.to_string().contains("local v2 = key"));
        assert!(block.to_string().contains("props[v2] = 2"));
    }

    #[test]
    fn ui_key_alias_rebuilds_fresh_props() {
        let key = local("children");
        let snapshot = local("v2");
        let props = local("props");
        let mut block = Block(vec![
            declare(&props, Table::default().into()),
            declare(&snapshot, local_value(&key)),
            assign(
                Index::new(local_value(&props), local_value(&snapshot)).into(),
                number(2.0),
            ),
            Return::new(vec![local_value(&props)]).into(),
        ]);
        super::rebuild_ui_expression_trees(&mut block);
        assert_eq!(block.to_string(), "return {\n\t[children] = 2\n}");
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
                RValue::Table(Table::new(vec![
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
                RValue::Table(Table::new(vec![(Some(string("Name")), string("Child"))])),
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
                RValue::Table(Table::new(vec![(
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
                RValue::Table(Table::new(vec![(
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
                    RValue::Table(Table::new(vec![(
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
                node_origin: Default::default(),
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
                    RValue::Table(Table::new(vec![(Some(string("Name")), string("Title"))])),
                ),
                declare(
                    &children,
                    RValue::Table(Table::new(vec![(
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
                node_origin: Default::default(),
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
                RValue::Table(Table::new(vec![(
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
    fn raising_table_key_does_not_cross_intervening_effect() {
        let key = local("key");
        let temp = local("v4");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table::new(vec![(Some(local_value(&key)), number(1.0))])),
            ),
            Call::new(global("between"), vec![]).into(),
            print(local_value(&temp)),
        ]);

        inline_single_use_temps(&mut block);

        // The constructor raises for a nil/NaN key before `between` runs; moving
        // it below the call would change both the observable output and error.
        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn effectful_table_does_not_move_after_prior_call_argument() {
        let temp = local("v4");
        let mut block = Block(vec![
            declare(
                &temp,
                RValue::Table(Table::new(vec![(
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
            declare(&temp, RValue::Table(Table::new(vec![]))),
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
