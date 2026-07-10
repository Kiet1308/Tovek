//! Late, low-risk cleanup shared by the final decompiler pipeline.
//!
//! The individual rewrites are deliberately narrow: straight-line boolean
//! propagation never crosses captured cells or loop back-edges; dead stores are
//! removed only when their evaluation is disposable; API canonicalization is a
//! literal lookup table; and module-table recovery accepts only static fields.

use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Assign, Block, Break, Global, Index, LValue, Literal, Local, LocalRw, RValue, RcLocal, Reduce,
    Select, Statement, Table, Traverse,
};

const MAX_ACTIVE_LOCALS: usize = 200;

pub fn cleanup_final(block: &mut Block, script_name: Option<&str>) {
    canonicalize_api_in_block(block);
    let root_upvalues = FxHashSet::default();
    simplify_constants_in_tree(block, &root_upvalues);
    rewrite_tail_loop_returns_in_tree(block);
    remove_dead_discards_in_tree(block, &root_upvalues);
    recover_named_module_return(block, script_name);
}

// ---------------------------------------------------------------------------
// 5.2: straight-line boolean propagation and dead branches

type ConstantState = FxHashMap<RcLocal, bool>;

fn simplify_constants_in_tree(block: &mut Block, function_upvalues: &FxHashSet<RcLocal>) {
    for (function, upvalues) in closure_functions(block) {
        simplify_constants_in_tree(&mut function.lock().body, &upvalues);
    }
    if function_has_goto(block) {
        return;
    }
    let mut captured = crate::inline_temps::collect_usage(block)
        .into_iter()
        .filter(|(_, usage)| usage.captured)
        .map(|(local, _)| local)
        .collect::<FxHashSet<_>>();
    captured.extend(function_upvalues.iter().cloned());
    let mut state = ConstantState::default();
    simplify_constant_block(block, &captured, &mut state);
}

fn simplify_constant_block(
    block: &mut Block,
    captured: &FxHashSet<RcLocal>,
    state: &mut ConstantState,
) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index < block.0.len() {
        if matches!(&block.0[index], Statement::If(_)) {
            let constant = {
                let Statement::If(node) = &mut block.0[index] else {
                    unreachable!()
                };
                changed |= replace_constants(&mut node.condition, state);
                let old = std::mem::replace(
                    &mut node.condition,
                    RValue::Literal(Literal::Boolean(false)),
                );
                node.condition = old.reduce_condition();
                match node.condition {
                    RValue::Literal(Literal::Boolean(value)) => Some(value),
                    _ => None,
                }
            };

            if let Some(take_then) = constant {
                let mut selected_state = state.clone();
                let (can_flatten, selected_statements) = {
                    let Statement::If(node) = &mut block.0[index] else {
                        unreachable!()
                    };
                    let selected = if take_then {
                        &mut node.then_block
                    } else {
                        &mut node.else_block
                    };
                    let mut selected = selected.lock();
                    changed |=
                        simplify_constant_block(&mut selected, captured, &mut selected_state);
                    let can_flatten = block_can_flatten_into_parent(&selected);
                    let statements = can_flatten.then(|| std::mem::take(&mut selected.0));
                    (can_flatten, statements)
                };
                *state = selected_state;
                if can_flatten {
                    let statements = selected_statements.unwrap();
                    let inserted = statements.len();
                    block.0.splice(index..=index, statements);
                    changed = true;
                    index += inserted;
                    continue;
                }
                index += 1;
                continue;
            }

            let mut then_state = state.clone();
            let mut else_state = state.clone();
            {
                let Statement::If(node) = &mut block.0[index] else {
                    unreachable!()
                };
                changed |=
                    simplify_constant_block(&mut node.then_block.lock(), captured, &mut then_state);
                changed |=
                    simplify_constant_block(&mut node.else_block.lock(), captured, &mut else_state);
            }
            *state = intersect_states(&then_state, &else_state);
            index += 1;
            continue;
        }

        if is_loop_statement(&block.0[index]) {
            let written = statement_written_locals_deep(&block.0[index]);
            // `while`/`repeat` conditions are re-evaluated across the back-edge.
            // A constant entering the loop is not valid there when the body can
            // write that local. Numeric/generic-for header expressions, by
            // contrast, are evaluated once before the body and may use entry
            // facts.
            let mut header_state = state.clone();
            if matches!(block.0[index], Statement::While(_) | Statement::Repeat(_)) {
                for local in &written {
                    header_state.remove(local);
                }
            }
            changed |= rewrite_direct_statement_values(&mut block.0[index], &header_state);
            for local in written {
                state.remove(&local);
            }
            let mut loop_state = ConstantState::default();
            changed |= match &mut block.0[index] {
                Statement::While(node) => {
                    simplify_constant_block(&mut node.block.lock(), captured, &mut loop_state)
                }
                Statement::Repeat(node) => {
                    simplify_constant_block(&mut node.block.lock(), captured, &mut loop_state)
                }
                Statement::NumericFor(node) => {
                    simplify_constant_block(&mut node.block.lock(), captured, &mut loop_state)
                }
                Statement::GenericFor(node) => {
                    simplify_constant_block(&mut node.block.lock(), captured, &mut loop_state)
                }
                _ => false,
            };
            index += 1;
            continue;
        }

        changed |= rewrite_direct_statement_values(&mut block.0[index], state);
        update_state_after_statement(&block.0[index], captured, state);
        if matches!(
            block.0[index],
            Statement::Return(_) | Statement::Break(_) | Statement::Continue(_)
        ) {
            state.clear();
        }
        index += 1;
    }
    changed
}

fn replace_constants(value: &mut RValue, state: &ConstantState) -> bool {
    if let RValue::Local(local) = value
        && let Some(&constant) = state.get(local)
    {
        *value = RValue::Literal(Literal::Boolean(constant));
        return true;
    }
    if matches!(value, RValue::Closure(_)) {
        return false;
    }
    value
        .rvalues_mut()
        .into_iter()
        .fold(false, |changed, child| {
            replace_constants(child, state) | changed
        })
}

fn rewrite_direct_statement_values(statement: &mut Statement, state: &ConstantState) -> bool {
    crate::deinline::stmt_rvalues_mut(statement)
        .into_iter()
        .fold(false, |changed, value| {
            replace_constants(value, state) | changed
        })
}

fn update_state_after_statement(
    statement: &Statement,
    captured: &FxHashSet<RcLocal>,
    state: &mut ConstantState,
) {
    for local in statement.values_written() {
        state.remove(local);
    }
    let Statement::Assign(assign) = statement else {
        return;
    };
    if assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return;
    };
    let RValue::Literal(Literal::Boolean(value)) = assign.right[0] else {
        return;
    };
    if !captured.contains(local) {
        state.insert(local.clone(), value);
    }
}

fn intersect_states(left: &ConstantState, right: &ConstantState) -> ConstantState {
    left.iter()
        .filter(|(local, value)| right.get(*local) == Some(*value))
        .map(|(local, value)| (local.clone(), *value))
        .collect()
}

fn block_can_flatten_into_parent(block: &Block) -> bool {
    !block.0.iter().any(|statement| {
        matches!(statement, Statement::Assign(assign) if assign.prefix)
            || matches!(statement, Statement::Label(_) | Statement::Goto(_))
    })
}

fn is_loop_statement(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_)
    )
}

fn statement_written_locals_deep(statement: &Statement) -> FxHashSet<RcLocal> {
    let mut written = statement
        .values_written()
        .into_iter()
        .cloned()
        .collect::<FxHashSet<_>>();
    match statement {
        Statement::While(node) => collect_block_writes(&node.block.lock(), &mut written),
        Statement::Repeat(node) => collect_block_writes(&node.block.lock(), &mut written),
        Statement::NumericFor(node) => {
            written.insert(node.counter.clone());
            collect_block_writes(&node.block.lock(), &mut written);
        }
        Statement::GenericFor(node) => {
            written.extend(node.res_locals.iter().cloned());
            collect_block_writes(&node.block.lock(), &mut written);
        }
        _ => {}
    }
    written
}

fn collect_block_writes(block: &Block, written: &mut FxHashSet<RcLocal>) {
    for statement in &block.0 {
        written.extend(statement.values_written().into_iter().cloned());
        match statement {
            Statement::If(node) => {
                collect_block_writes(&node.then_block.lock(), written);
                collect_block_writes(&node.else_block.lock(), written);
            }
            Statement::While(node) => collect_block_writes(&node.block.lock(), written),
            Statement::Repeat(node) => collect_block_writes(&node.block.lock(), written),
            Statement::NumericFor(node) => {
                written.insert(node.counter.clone());
                collect_block_writes(&node.block.lock(), written);
            }
            Statement::GenericFor(node) => {
                written.extend(node.res_locals.iter().cloned());
                collect_block_writes(&node.block.lock(), written);
            }
            _ => {}
        }
    }
}

fn function_has_goto(block: &Block) -> bool {
    block.0.iter().any(|statement| {
        matches!(statement, Statement::Goto(_) | Statement::Label(_))
            || match statement {
                Statement::If(node) => {
                    function_has_goto(&node.then_block.lock())
                        || function_has_goto(&node.else_block.lock())
                }
                Statement::While(node) => function_has_goto(&node.block.lock()),
                Statement::Repeat(node) => function_has_goto(&node.block.lock()),
                Statement::NumericFor(node) => function_has_goto(&node.block.lock()),
                Statement::GenericFor(node) => function_has_goto(&node.block.lock()),
                _ => false,
            }
    })
}

// ---------------------------------------------------------------------------
// 5.3 / 5.4: unused stores and redundant aliases

fn remove_dead_discards_in_tree(block: &mut Block, function_upvalues: &FxHashSet<RcLocal>) {
    for (function, upvalues) in closure_functions(block) {
        remove_dead_discards_in_tree(&mut function.lock().body, &upvalues);
    }
    remove_dead_discards_with_worklist(block, function_upvalues);
}

fn collect_current_function_usage(block: &Block) -> FxHashMap<RcLocal, crate::inline_temps::Usage> {
    let mut usage = FxHashMap::default();
    collect_current_function_usage_in_block(block, &mut usage);
    usage
}

fn collect_current_function_usage_in_block(
    block: &Block,
    usage: &mut FxHashMap<RcLocal, crate::inline_temps::Usage>,
) {
    for statement in &block.0 {
        for local in statement.values_read() {
            usage.entry(local.clone()).or_default().reads += 1;
        }
        for local in statement.values_written() {
            usage.entry(local.clone()).or_default().writes += 1;
        }
        for value in crate::deinline::stmt_rvalues(statement) {
            record_direct_closure_captures(value, usage);
        }
        match statement {
            Statement::If(node) => {
                collect_current_function_usage_in_block(&node.then_block.lock(), usage);
                collect_current_function_usage_in_block(&node.else_block.lock(), usage);
            }
            Statement::While(node) => {
                collect_current_function_usage_in_block(&node.block.lock(), usage)
            }
            Statement::Repeat(node) => {
                collect_current_function_usage_in_block(&node.block.lock(), usage)
            }
            Statement::NumericFor(node) => {
                collect_current_function_usage_in_block(&node.block.lock(), usage)
            }
            Statement::GenericFor(node) => {
                collect_current_function_usage_in_block(&node.block.lock(), usage)
            }
            _ => {}
        }
    }
}

fn record_direct_closure_captures(
    value: &RValue,
    usage: &mut FxHashMap<RcLocal, crate::inline_temps::Usage>,
) {
    if let RValue::Closure(closure) = value {
        for upvalue in &closure.upvalues {
            let local = match upvalue {
                crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local,
            };
            usage.entry(local.clone()).or_default().captured = true;
        }
        // The child body is a different function. Its own cleanup invocation
        // accounts for its locals; parent bindings are already represented by
        // the closure's explicit upvalue list above.
        return;
    }
    for child in value.rvalues() {
        record_direct_closure_captures(child, usage);
    }
}

#[derive(Debug)]
struct DeadStoreCandidate {
    statement_id: usize,
    destination: RcLocal,
    prefix: bool,
    reads: Vec<RcLocal>,
}

fn remove_dead_discards_with_worklist(block: &mut Block, function_upvalues: &FxHashSet<RcLocal>) {
    let mut candidates = Vec::new();
    let mut next_statement_id = 0;
    collect_dead_store_candidates(
        block,
        function_upvalues,
        &mut next_statement_id,
        &mut candidates,
    );
    if candidates.is_empty() {
        return;
    }

    let mut usage = collect_current_function_usage(block);
    let mut candidates_by_destination = FxHashMap::<RcLocal, Vec<usize>>::default();
    for (index, candidate) in candidates.iter().enumerate() {
        candidates_by_destination
            .entry(candidate.destination.clone())
            .or_default()
            .push(index);
    }

    let mut queue = (0..candidates.len()).collect::<VecDeque<_>>();
    let mut removed = vec![false; candidates.len()];
    let mut removed_statements = FxHashSet::default();
    while let Some(index) = queue.pop_front() {
        if removed[index] || !dead_store_is_unused(&candidates[index], &usage) {
            continue;
        }
        removed[index] = true;
        let candidate = &candidates[index];
        removed_statements.insert(candidate.statement_id);

        let destination_became_single =
            if let Some(destination_usage) = usage.get_mut(&candidate.destination) {
                destination_usage.writes = destination_usage.writes.saturating_sub(1);
                destination_usage.writes == 1
            } else {
                false
            };
        if destination_became_single {
            enqueue_destination_candidates(
                &candidate.destination,
                &candidates_by_destination,
                &mut queue,
            );
        }
        for read in &candidate.reads {
            let read_became_unused = if let Some(read_usage) = usage.get_mut(read) {
                read_usage.reads = read_usage.reads.saturating_sub(1);
                read_usage.reads == 0
            } else {
                false
            };
            if read_became_unused {
                enqueue_destination_candidates(read, &candidates_by_destination, &mut queue);
            }
        }
    }

    if !removed_statements.is_empty() {
        let mut next_statement_id = 0;
        remove_marked_statements(block, &removed_statements, &mut next_statement_id);
    }
}

fn collect_dead_store_candidates(
    block: &Block,
    function_upvalues: &FxHashSet<RcLocal>,
    next_statement_id: &mut usize,
    candidates: &mut Vec<DeadStoreCandidate>,
) {
    for statement in &block.0 {
        let statement_id = *next_statement_id;
        *next_statement_id += 1;
        if let Some(candidate) = dead_store_candidate(statement, statement_id, function_upvalues) {
            candidates.push(candidate);
        }
        match statement {
            Statement::If(node) => {
                collect_dead_store_candidates(
                    &node.then_block.lock(),
                    function_upvalues,
                    next_statement_id,
                    candidates,
                );
                collect_dead_store_candidates(
                    &node.else_block.lock(),
                    function_upvalues,
                    next_statement_id,
                    candidates,
                );
            }
            Statement::While(node) => collect_dead_store_candidates(
                &node.block.lock(),
                function_upvalues,
                next_statement_id,
                candidates,
            ),
            Statement::Repeat(node) => collect_dead_store_candidates(
                &node.block.lock(),
                function_upvalues,
                next_statement_id,
                candidates,
            ),
            Statement::NumericFor(node) => collect_dead_store_candidates(
                &node.block.lock(),
                function_upvalues,
                next_statement_id,
                candidates,
            ),
            Statement::GenericFor(node) => collect_dead_store_candidates(
                &node.block.lock(),
                function_upvalues,
                next_statement_id,
                candidates,
            ),
            _ => {}
        }
    }
}

fn dead_store_candidate(
    statement: &Statement,
    statement_id: usize,
    function_upvalues: &FxHashSet<RcLocal>,
) -> Option<DeadStoreCandidate> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if assign.parallel || assign.left.len() != 1 || assign.right.len() > 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    if function_upvalues.contains(local) {
        return None;
    }
    if !assign.right.is_empty() && !assign.right.first().is_some_and(disposable_unused_value) {
        return None;
    }
    Some(DeadStoreCandidate {
        statement_id,
        destination: local.clone(),
        prefix: assign.prefix,
        reads: statement.values_read().into_iter().cloned().collect(),
    })
}

fn dead_store_is_unused(
    candidate: &DeadStoreCandidate,
    usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>,
) -> bool {
    usage.get(&candidate.destination).is_none_or(|local_usage| {
        local_usage.reads == 0
            && !local_usage.captured
            // A prefix declaration owns the lexical binding for later writes.
            // Keep it until those writes have themselves disappeared; otherwise
            // `local v; v, x = call()` would silently turn `v` into a global.
            && (!candidate.prefix || local_usage.writes == 1)
    })
}

fn enqueue_destination_candidates(
    local: &RcLocal,
    candidates_by_destination: &FxHashMap<RcLocal, Vec<usize>>,
    queue: &mut VecDeque<usize>,
) {
    if let Some(indices) = candidates_by_destination.get(local) {
        queue.extend(indices.iter().copied());
    }
}

fn remove_marked_statements(
    block: &mut Block,
    removed: &FxHashSet<usize>,
    next_statement_id: &mut usize,
) {
    let statements = std::mem::take(&mut block.0);
    block.0.reserve(statements.len());
    for mut statement in statements {
        let statement_id = *next_statement_id;
        *next_statement_id += 1;
        if removed.contains(&statement_id) {
            continue;
        }
        match &mut statement {
            Statement::If(node) => {
                remove_marked_statements(&mut node.then_block.lock(), removed, next_statement_id);
                remove_marked_statements(&mut node.else_block.lock(), removed, next_statement_id);
            }
            Statement::While(node) => {
                remove_marked_statements(&mut node.block.lock(), removed, next_statement_id)
            }
            Statement::Repeat(node) => {
                remove_marked_statements(&mut node.block.lock(), removed, next_statement_id)
            }
            Statement::NumericFor(node) => {
                remove_marked_statements(&mut node.block.lock(), removed, next_statement_id)
            }
            Statement::GenericFor(node) => {
                remove_marked_statements(&mut node.block.lock(), removed, next_statement_id)
            }
            _ => {}
        }
        block.0.push(statement);
    }
}

fn disposable_unused_value(value: &RValue) -> bool {
    // Keep recovered helper definitions and named tables even when all of their
    // optimized call sites disappeared; they are valuable source structure and
    // are not the scalar/lookup junk this cleanup targets.
    !contains_structural_definition(value) && crate::side_effects::is_total_pure(value)
}

fn contains_structural_definition(value: &RValue) -> bool {
    matches!(value, RValue::Closure(_) | RValue::Table(_))
        || value
            .rvalues()
            .into_iter()
            .any(contains_structural_definition)
}

// ---------------------------------------------------------------------------
// 5.7: canonical Roblox literal APIs

fn canonicalize_api_in_block(block: &mut Block) {
    for statement in &mut block.0 {
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            canonicalize_api_value(value);
        }
        match statement {
            Statement::If(node) => {
                canonicalize_api_in_block(&mut node.then_block.lock());
                canonicalize_api_in_block(&mut node.else_block.lock());
            }
            Statement::While(node) => canonicalize_api_in_block(&mut node.block.lock()),
            Statement::Repeat(node) => canonicalize_api_in_block(&mut node.block.lock()),
            Statement::NumericFor(node) => canonicalize_api_in_block(&mut node.block.lock()),
            Statement::GenericFor(node) => canonicalize_api_in_block(&mut node.block.lock()),
            _ => {}
        }
    }
}

fn canonicalize_api_value(value: &mut RValue) {
    if let RValue::Closure(closure) = value {
        canonicalize_api_in_block(&mut closure.function.lock().body);
        return;
    }
    for child in value.rvalues_mut() {
        canonicalize_api_value(child);
    }
    if let Some(property) = canonical_api_property(value) {
        *value = property;
    }
}

fn canonical_api_property(value: &RValue) -> Option<RValue> {
    let call = match value {
        RValue::Call(call) => call,
        RValue::Select(Select::Call(call)) => call,
        _ => return None,
    };
    let RValue::Index(callee) = call.value.as_ref() else {
        return None;
    };
    let RValue::Global(class) = callee.left.as_ref() else {
        return None;
    };
    if !matches!(callee.right.as_ref(), RValue::Literal(Literal::String(key)) if key == b"new") {
        return None;
    }

    let property = if class.0 == b"Vector3" && all_exact_numbers(&call.arguments, 3, 0.0) {
        "zero"
    } else if class.0 == b"Vector3" && all_exact_numbers(&call.arguments, 3, 1.0) {
        "one"
    } else if class.0 == b"Vector2" && all_exact_numbers(&call.arguments, 2, 0.0) {
        "zero"
    } else if class.0 == b"Vector2" && all_exact_numbers(&call.arguments, 2, 1.0) {
        "one"
    } else if class.0 == b"CFrame" && call.arguments.is_empty() {
        "identity"
    } else {
        return None;
    };
    Some(
        Index::new(
            RValue::Global(Global(class.0.clone())),
            RValue::Literal(Literal::String(property.as_bytes().to_vec())),
        )
        .into(),
    )
}

fn all_exact_numbers(values: &[RValue], count: usize, expected: f64) -> bool {
    values.len() == count
        && values.iter().all(
            |value| matches!(value, RValue::Literal(Literal::Number(number)) if number.to_bits() == expected.to_bits()),
        )
}

// ---------------------------------------------------------------------------
// 5.9: a void return inside a function-tail loop is a break

fn rewrite_tail_loop_returns_in_tree(block: &mut Block) {
    for (function, _) in closure_functions(block) {
        rewrite_tail_loop_returns_in_tree(&mut function.lock().body);
    }
    let runtime = block
        .0
        .iter()
        .enumerate()
        .filter(|(_, statement)| !matches!(statement, Statement::Comment(_) | Statement::Empty(_)))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let loop_index = match runtime.as_slice() {
        [.., index] if is_loop_statement(&block.0[*index]) => Some(*index),
        [.., loop_index, return_index]
            if is_loop_statement(&block.0[*loop_index])
                && matches!(&block.0[*return_index], Statement::Return(ret) if ret.values.is_empty()) =>
        {
            Some(*loop_index)
        }
        _ => None,
    };
    let Some(loop_index) = loop_index else {
        return;
    };
    let loop_body = match &mut block.0[loop_index] {
        Statement::While(node) => &mut node.block,
        Statement::Repeat(node) => &mut node.block,
        Statement::NumericFor(node) => &mut node.block,
        Statement::GenericFor(node) => &mut node.block,
        _ => unreachable!(),
    };
    replace_void_returns_with_break(&mut loop_body.lock());
}

fn replace_void_returns_with_break(block: &mut Block) {
    for statement in &mut block.0 {
        if matches!(statement, Statement::Return(ret) if ret.values.is_empty()) {
            *statement = Statement::Break(Break {});
            continue;
        }
        match statement {
            Statement::If(node) => {
                replace_void_returns_with_break(&mut node.then_block.lock());
                replace_void_returns_with_break(&mut node.else_block.lock());
            }
            // A break inside a nested loop would target the wrong loop.
            Statement::While(_)
            | Statement::Repeat(_)
            | Statement::NumericFor(_)
            | Statement::GenericFor(_) => {}
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 5.7: preserve a named module table when it owns several functions

fn recover_named_module_return(block: &mut Block, script_name: Option<&str>) {
    if count_declared_locals(block) >= MAX_ACTIVE_LOCALS {
        return;
    }
    let table = match block.0.last() {
        Some(Statement::Return(return_)) => match return_.values.as_slice() {
            [RValue::Table(table)] => table.clone(),
            _ => return,
        },
        _ => return,
    };
    if table
        .0
        .iter()
        .filter(|(_, value)| matches!(value, RValue::Closure(_)))
        .count()
        < 2
        || !table.0.iter().all(|(key, _)| {
            matches!(key, Some(RValue::Literal(Literal::String(bytes))) if valid_identifier(bytes))
        })
    {
        return;
    }

    let mut reserved = FxHashSet::default();
    crate::rehoist_constants::collect_reserved_identifiers(block, &mut reserved);
    let base = script_name
        .and_then(crate::name_locals::script_module_hint)
        .unwrap_or_else(|| "Module".to_string());
    let module = RcLocal::new(Local::new(Some(crate::rehoist_constants::unique_name(
        &base,
        &mut reserved,
    ))));
    let mut statements = Vec::with_capacity(table.0.len() + 2);
    let mut declaration = Assign::new(
        vec![LValue::Local(module.clone())],
        vec![RValue::Table(Table::default())],
    );
    declaration.prefix = true;
    statements.push(declaration.into());
    statements.extend(table.0.into_iter().map(|(key, value)| {
        Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Local(module.clone()),
                key.unwrap(),
            ))],
            vec![value],
        )
        .into()
    }));
    statements.push(crate::Return::new(vec![RValue::Local(module)]).into());
    block.0.splice(block.0.len() - 1.., statements);
}

fn valid_identifier(bytes: &[u8]) -> bool {
    let Ok(name) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn count_declared_locals(block: &Block) -> usize {
    block
        .0
        .iter()
        .map(|statement| {
            let direct = match statement {
                Statement::Assign(assign) if assign.prefix => assign
                    .left
                    .iter()
                    .filter(|left| matches!(left, LValue::Local(_)))
                    .count(),
                Statement::NumericFor(_) => 1,
                Statement::GenericFor(node) => node.res_locals.len(),
                _ => 0,
            };
            direct
                + match statement {
                    Statement::If(node) => {
                        count_declared_locals(&node.then_block.lock())
                            + count_declared_locals(&node.else_block.lock())
                    }
                    Statement::While(node) => count_declared_locals(&node.block.lock()),
                    Statement::Repeat(node) => count_declared_locals(&node.block.lock()),
                    Statement::NumericFor(node) => count_declared_locals(&node.block.lock()),
                    Statement::GenericFor(node) => count_declared_locals(&node.block.lock()),
                    _ => 0,
                }
        })
        .sum()
}

type FunctionRef = by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>;
type ClosureFunction = (FunctionRef, FxHashSet<RcLocal>);

fn closure_functions(block: &Block) -> Vec<ClosureFunction> {
    let mut functions = Vec::new();
    collect_closure_functions(block, &mut functions);
    functions
}

fn collect_closure_functions(block: &Block, functions: &mut Vec<ClosureFunction>) {
    for statement in &block.0 {
        for value in crate::deinline::stmt_rvalues(statement) {
            collect_closures_in_value(value, functions);
        }
        match statement {
            Statement::If(node) => {
                collect_closure_functions(&node.then_block.lock(), functions);
                collect_closure_functions(&node.else_block.lock(), functions);
            }
            Statement::While(node) => collect_closure_functions(&node.block.lock(), functions),
            Statement::Repeat(node) => collect_closure_functions(&node.block.lock(), functions),
            Statement::NumericFor(node) => collect_closure_functions(&node.block.lock(), functions),
            Statement::GenericFor(node) => collect_closure_functions(&node.block.lock(), functions),
            _ => {}
        }
    }
}

fn collect_closures_in_value(value: &RValue, functions: &mut Vec<ClosureFunction>) {
    if let RValue::Closure(closure) = value {
        let upvalues = closure
            .upvalues
            .iter()
            .map(|upvalue| match upvalue {
                crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local) => local.clone(),
            })
            .collect();
        functions.push((closure.function.clone(), upvalues));
        return;
    }
    for child in value.rvalues() {
        collect_closures_in_value(child, functions);
    }
}

#[cfg(test)]
mod tests {
    use super::cleanup_final;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Closure, Function, Global, If, Index, LValue,
        Literal, Local, RValue, RcLocal, Return, Table, Upvalue, While,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn boolean(value: bool) -> RValue {
        RValue::Literal(Literal::Boolean(value))
    }

    fn declare(local: &RcLocal, values: Vec<RValue>) -> crate::Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], values);
        assign.prefix = true;
        assign.into()
    }

    fn call(name: &str) -> crate::Statement {
        Call::new(global(name), vec![]).into()
    }

    fn closure() -> RValue {
        RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![],
        })
    }

    #[test]
    fn removes_dead_false_branch_and_boolean_stores() {
        let flag = local("v4");
        let mut block = Block(vec![
            declare(&flag, vec![]),
            Assign::new(vec![LValue::Local(flag.clone())], vec![boolean(true)]).into(),
            If::new(
                crate::Unary::new(RValue::Local(flag), crate::UnaryOperation::Not).into(),
                Block(vec![call("dead")]),
                Block::default(),
            )
            .into(),
            call("live"),
        ]);

        cleanup_final(&mut block, None);

        assert_eq!(block.to_string(), "live()");
    }

    #[test]
    fn captured_boolean_is_not_propagated_across_calls() {
        let flag = local("flag");
        let holder = local("holder");
        let captured = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(flag.clone())],
        });
        let mut block = Block(vec![
            declare(&flag, vec![boolean(true)]),
            declare(&holder, vec![captured]),
            call("invoke"),
            If::new(
                RValue::Local(flag),
                Block(vec![call("kept")]),
                Block::default(),
            )
            .into(),
        ]);

        cleanup_final(&mut block, None);

        assert!(block.to_string().contains("if flag then"));
    }

    #[test]
    fn current_function_usage_keeps_binding_captured_only_by_child() {
        let flag = local("flag");
        let holder = local("holder");
        let captured = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(flag.clone())],
        });
        let mut block = Block(vec![
            declare(&flag, vec![boolean(false)]),
            declare(&holder, vec![captured]),
        ]);

        cleanup_final(&mut block, None);

        let output = block.to_string();
        assert!(output.contains("local flag = false"), "{output}");
        assert!(output.contains("local function holder"), "{output}");
    }

    #[test]
    fn current_function_upvalue_writes_are_never_treated_as_dead_constants() {
        let flag = local("flag");
        let holder = local("holder");
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![
                Assign::new(vec![LValue::Local(flag.clone())], vec![boolean(false)]).into(),
                call("invoke"),
                If::new(
                    RValue::Local(flag.clone()),
                    Block(vec![call("kept")]),
                    Block::default(),
                )
                .into(),
            ]),
            ..Function::default()
        }));
        let mut block = Block(vec![declare(
            &holder,
            vec![RValue::Closure(Closure {
                function: ByAddress(function.clone()),
                upvalues: vec![Upvalue::Ref(flag)],
            })],
        )]);

        cleanup_final(&mut block, None);

        let output = function.lock().body.to_string();
        assert!(output.contains("flag = false"), "{output}");
        assert!(output.contains("if flag then"), "{output}");
    }

    #[test]
    fn entry_constant_is_not_propagated_through_while_back_edge() {
        let flag = local("flag");
        let mut block = Block(vec![
            declare(&flag, vec![boolean(true)]),
            While::new(
                RValue::Local(flag.clone()),
                Block(vec![Assign::new(
                    vec![LValue::Local(flag)],
                    vec![boolean(false)],
                )
                .into()]),
            )
            .into(),
        ]);

        cleanup_final(&mut block, None);

        let output = block.to_string();
        assert!(output.contains("while flag do"), "{output}");
        assert!(!output.contains("while true do"), "{output}");
    }

    #[test]
    fn keeps_discarded_calls_that_can_raise_or_be_overridden() {
        let format = RValue::Index(Index::new(global("string"), string("format")));
        let mut block = Block(vec![
            Call::new(
                format,
                vec![
                    string("[BugsReplicator] [%.2f] %s | Plant: %s | Reason: %s"),
                    RValue::Literal(Literal::Number(1.0)),
                    string("STOP"),
                    string("Plant"),
                    string("Reason"),
                ],
            )
            .into(),
            Call::new(global("require"), vec![global("Module")]).into(),
        ]);

        cleanup_final(&mut block, None);

        let output = block.to_string();
        assert!(output.contains("string.format"), "{output}");
        assert!(output.contains("require(Module)"), "{output}");
    }

    #[test]
    fn keeps_untrusted_format_and_potentially_raising_dead_values() {
        let format = RValue::Index(Index::new(global("string"), string("format")));
        let invalid_format: crate::Statement =
            Call::new(format, vec![string("%d"), RValue::Table(Table::default())]).into();
        let arithmetic = local("arithmetic");
        let lookup = local("lookup");
        let mut block = Block(vec![
            invalid_format,
            declare(
                &arithmetic,
                vec![Binary::new(
                    RValue::Literal(Literal::Number(1.0)),
                    string("x"),
                    BinaryOperation::Add,
                )
                .into()],
            ),
            declare(
                &lookup,
                vec![RValue::Index(Index::new(
                    RValue::Literal(Literal::Nil),
                    string("field"),
                ))],
            ),
        ]);

        cleanup_final(&mut block, None);

        let output = block.to_string();
        assert!(output.contains("string.format(\"%d\", {})"), "{output}");
        assert!(output.contains("local arithmetic = 1 + \"x\""), "{output}");
        assert!(output.contains("local lookup = (nil).field"), "{output}");
    }

    #[test]
    fn keeps_predeclaration_that_scopes_later_effectful_multi_write() {
        let unused = local("v2");
        let result = local("result");
        let mut block = Block(vec![
            declare(&unused, vec![]),
            Assign::new(
                vec![LValue::Local(unused.clone()), LValue::Local(result.clone())],
                vec![Call::new(global("render"), vec![]).into()],
            )
            .into(),
            Return::new(vec![RValue::Local(result)]).into(),
        ]);

        cleanup_final(&mut block, None);

        assert!(block.to_string().starts_with("local v2"));
        assert!(block.to_string().contains("v2, result = render()"));
    }

    #[test]
    fn worklist_removes_transitive_dead_store_chain() {
        let first = local("first");
        let second = local("second");
        let third = local("third");
        let mut block = Block(vec![
            declare(&first, vec![RValue::Literal(Literal::Number(1.0))]),
            declare(&second, vec![RValue::Local(first)]),
            declare(&third, vec![RValue::Local(second)]),
        ]);

        cleanup_final(&mut block, None);

        assert!(block.0.is_empty(), "{}", block);
    }

    #[test]
    fn canonicalizes_vector_and_cframe_literals() {
        let constructor = |class: &str, values: Vec<RValue>| {
            Call::new(
                RValue::Index(Index::new(global(class), string("new"))),
                values,
            )
            .into()
        };
        let mut block = Block(vec![Return::new(vec![
            constructor(
                "Vector3",
                vec![
                    RValue::Literal(Literal::Number(0.0)),
                    RValue::Literal(Literal::Number(0.0)),
                    RValue::Literal(Literal::Number(0.0)),
                ],
            ),
            constructor("CFrame", vec![]),
        ])
        .into()]);

        cleanup_final(&mut block, None);

        assert_eq!(block.to_string(), "return Vector3.zero, CFrame.identity");
    }

    #[test]
    fn return_in_function_tail_loop_becomes_break() {
        let mut block = Block(vec![While::new(
            boolean(true),
            Block(vec![If::new(
                global("timedOut"),
                Block(vec![Return::new(vec![]).into()]),
                Block::default(),
            )
            .into()]),
        )
        .into()]);

        cleanup_final(&mut block, None);

        assert!(block.to_string().contains("if timedOut then\n\t\tbreak"));
    }

    #[test]
    fn multi_function_return_table_recovers_named_module() {
        let mut block = Block(vec![Return::new(vec![RValue::Table(Table(vec![
            (Some(string("Play")), closure()),
            (Some(string("Stop")), closure()),
        ]))])
        .into()]);

        cleanup_final(&mut block, Some("effects/exp-orb.luau"));

        assert_eq!(
            block.to_string(),
            "local ExpOrb = {}\n\nfunction ExpOrb.Play() end\n\nfunction ExpOrb.Stop() end\n\nreturn ExpOrb"
        );
    }

    #[test]
    fn second_run_is_idempotent() {
        let unused = local("_");
        let mut block = Block(vec![
            declare(
                &unused,
                vec![Binary::new(
                    RValue::Literal(Literal::Number(1.0)),
                    RValue::Literal(Literal::Number(2.0)),
                    BinaryOperation::Add,
                )
                .into()],
            ),
            call("live"),
        ]);

        cleanup_final(&mut block, None);
        let once = block.to_string();
        cleanup_final(&mut block, None);

        assert_eq!(block.to_string(), once);
    }
}
