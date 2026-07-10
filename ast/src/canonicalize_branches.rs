//! Late, semantics-exact branch and loop shape canonicalization.
//!
//! This pass only changes nesting: literal-dispatch return arms become `elseif`,
//! wait-only repeat lowerings flatten, and an infinite `while` whose sole
//! outer-loop break is its final statement becomes `repeat ... until`. It never
//! negates or reduces a condition.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    deinline::rvalue_exact_eq, flatten_guards::block_size, BinaryOperation, Block, Global, If,
    Index, LValue, Literal, Local, NumericFor, RValue, RcLocal, Repeat, SideEffects, Statement,
    Traverse, Unary, UnaryOperation,
};

pub fn canonicalize_branches(block: &mut Block) {
    crate::factor_common_tails::unshare_blocks(block);
    let facts = FunctionFacts::collect(block, &[]);
    canonicalize_block(block, &facts);
}

struct FunctionFacts {
    declared: FxHashSet<RcLocal>,
    captured: FxHashSet<RcLocal>,
}

impl FunctionFacts {
    fn collect(block: &Block, parameters: &[RcLocal]) -> Self {
        let usage = crate::inline_temps::collect_usage(block);
        let captured = usage
            .into_iter()
            .filter_map(|(local, usage)| usage.captured.then_some(local))
            .collect();
        let mut declared = parameters.iter().cloned().collect();
        collect_declared_locals(block, &mut declared);
        Self { declared, captured }
    }
}

fn collect_declared_locals(block: &Block, declared: &mut FxHashSet<RcLocal>) {
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement
            && assign.prefix
        {
            declared.extend(assign.left.iter().filter_map(|left| match left {
                LValue::Local(local) => Some(local.clone()),
                _ => None,
            }));
        }
        match statement {
            Statement::If(node) => {
                collect_declared_locals(&node.then_block.lock(), declared);
                collect_declared_locals(&node.else_block.lock(), declared);
            }
            Statement::While(node) => collect_declared_locals(&node.block.lock(), declared),
            Statement::Repeat(node) => collect_declared_locals(&node.block.lock(), declared),
            Statement::NumericFor(node) => {
                declared.insert(node.counter.clone());
                collect_declared_locals(&node.block.lock(), declared);
            }
            Statement::GenericFor(node) => {
                declared.extend(node.res_locals.iter().cloned());
                collect_declared_locals(&node.block.lock(), declared);
            }
            _ => {}
        }
    }
}

fn canonicalize_block(block: &mut Block, facts: &FunctionFacts) {
    for statement in &mut block.0 {
        let mut functions = Vec::new();
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            collect_functions(value, &mut functions);
        }
        for function in functions {
            let mut function = function.lock();
            let nested_facts = FunctionFacts::collect(&function.body, &function.parameters);
            canonicalize_block(&mut function.body, &nested_facts);
        }

        match statement {
            Statement::If(node) => {
                canonicalize_block(&mut node.then_block.lock(), facts);
                canonicalize_block(&mut node.else_block.lock(), facts);
            }
            Statement::While(node) => canonicalize_block(&mut node.block.lock(), facts),
            Statement::Repeat(node) => canonicalize_block(&mut node.block.lock(), facts),
            Statement::NumericFor(node) => canonicalize_block(&mut node.block.lock(), facts),
            Statement::GenericFor(node) => canonicalize_block(&mut node.block.lock(), facts),
            _ => {}
        }
        flatten_leading_repeat_in_infinite_while(statement);
        reroll_terminal_while(statement);
    }
    recover_parent_walks(block, facts);
    reroll_two_index_blocks(block);
    chain_adjacent_return_ifs(&mut block.0);
}

/// Recover the characteristic ancestor iterator
/// `while true; if not cur then FAIL; break; ...; cur = cur.Parent` as
/// `while cur ... end; if not cur then FAIL end`. Moving FAIL just after the
/// loop preserves it on normal exhaustion while keeping success-break paths
/// untouched.
fn recover_parent_walks(block: &mut Block, facts: &FunctionFacts) {
    let mut index = 0;
    while index < block.0.len() {
        let Some((cursor, failure)) = parent_walk_candidate(&block.0[index]) else {
            index += 1;
            continue;
        };
        if !facts.declared.contains(&cursor) || facts.captured.contains(&cursor) {
            index += 1;
            continue;
        }
        let Statement::While(node) = &mut block.0[index] else {
            unreachable!()
        };
        node.condition = RValue::Local(cursor.clone());
        node.block.lock().0.remove(0);
        let exhausted = If::new(
            Unary::new(RValue::Local(cursor), UnaryOperation::Not).into(),
            Block(vec![failure]),
            Block::default(),
        )
        .into();
        block.0.insert(index + 1, exhausted);
        index += 2;
    }
}

fn parent_walk_candidate(statement: &Statement) -> Option<(RcLocal, Statement)> {
    let Statement::While(node) = statement else {
        return None;
    };
    if !is_true(&node.condition) {
        return None;
    }
    let body = node.block.lock();
    let [Statement::If(failure), Statement::If(success), advance] = body.0.as_slice() else {
        return None;
    };
    if !failure.else_block.lock().0.is_empty() || !success.else_block.lock().0.is_empty() {
        return None;
    }
    let RValue::Unary(not) = &failure.condition else {
        return None;
    };
    let RValue::Local(cursor) = &*not.value else {
        return None;
    };
    if not.operation != UnaryOperation::Not || !is_parent_advance(advance, cursor) {
        return None;
    }
    let failure_body = failure.then_block.lock();
    let [failure_statement @ Statement::Assign(_), Statement::Break(_)] = failure_body.0.as_slice()
    else {
        return None;
    };
    if !matches!(
        success.then_block.lock().0.last(),
        Some(Statement::Break(_))
    ) {
        return None;
    }
    let cursor_set = FxHashSet::from_iter([cursor.clone()]);
    if crate::inline_temps::statement_writes_any_local(&Statement::If(success.clone()), &cursor_set)
        || contains_goto_or_label(&success.then_block.lock())
    {
        return None;
    }
    Some((cursor.clone(), failure_statement.clone()))
}

fn is_parent_advance(statement: &Statement, cursor: &RcLocal) -> bool {
    let Statement::Assign(assign) = statement else {
        return false;
    };
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return false;
    }
    let [LValue::Local(destination)] = assign.left.as_slice() else {
        return false;
    };
    let [RValue::Index(Index { left, right })] = assign.right.as_slice() else {
        return false;
    };
    destination == cursor
        && matches!(&**left, RValue::Local(source) if source == cursor)
        && matches!(&**right, RValue::Literal(Literal::String(name)) if name.as_slice() == b"Parent")
}

/// Re-roll the exact two-copy lowering `local a = xs[1]; BODY(a); local b =
/// xs[2]; BODY(b)` into a two-iteration numeric loop. The alpha matcher permits
/// only the element-local rename; the sole expression hole is therefore the
/// proven consecutive index literal.
fn reroll_two_index_blocks(block: &mut Block) {
    if block.0.len() < 4 {
        return;
    }
    let usage = crate::inline_temps::collect_usage(block);
    let mut reserved = FxHashSet::default();
    crate::rehoist_constants::collect_reserved_identifiers(block, &mut reserved);
    let mut index = 0;
    while index + 3 < block.0.len() {
        let Some((element, base)) = reroll_pair_candidate(block, index, &usage) else {
            index += 1;
            continue;
        };

        let counter_name = crate::rehoist_constants::unique_name("i", &mut reserved);
        let counter = RcLocal::new(Local::new(Some(counter_name)));
        if crate::inline_temps::is_generated_temp(&element) {
            let item_base = indexed_item_name(&base);
            let item_name = crate::rehoist_constants::unique_name(&item_base, &mut reserved);
            element.0 .0.lock().0 = Some(item_name);
        }

        let mut declaration = block.0.remove(index).into_assign().unwrap();
        let body_statement = block.0.remove(index);
        block.0.drain(index..index + 2);
        let RValue::Index(index_value) = &mut declaration.right[0] else {
            unreachable!()
        };
        index_value.right = Box::new(RValue::Local(counter.clone()));
        let loop_statement: Statement = NumericFor::new(
            RValue::Literal(Literal::Number(1.0)),
            RValue::Literal(Literal::Number(2.0)),
            RValue::Literal(Literal::Number(1.0)),
            counter,
            Block(vec![declaration.into(), body_statement]),
        )
        .into();
        block.0.insert(index, loop_statement);
        index += 1;
    }
}

fn reroll_pair_candidate(
    block: &Block,
    index: usize,
    usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>,
) -> Option<(RcLocal, RValue)> {
    let (first_local, first_base, first_index) = indexed_local_declaration(&block.0[index])?;
    let (second_local, second_base, second_index) = indexed_local_declaration(&block.0[index + 2])?;
    if first_index != 1
        || second_index != 2
        || !rvalue_exact_eq(first_base, second_base)
        || block_size(&block.0[index..index + 2]) < 3
    {
        return None;
    }

    let first_tail = &block.0[index + 1];
    let second_tail = &block.0[index + 3];
    let first_tail_block = Block(vec![first_tail.clone()]);
    let second_tail_block = Block(vec![second_tail.clone()]);
    if contains_goto_or_label(&first_tail_block)
        || contains_goto_or_label(&second_tail_block)
        || contains_loop_control(&first_tail_block)
        || contains_loop_control(&second_tail_block)
        || !local_is_confined(&block.0[index..index + 2], first_local, usage)
        || !local_is_confined(&block.0[index + 2..index + 4], second_local, usage)
    {
        return None;
    }

    let localizable = FxHashSet::from_iter([first_local.clone()]);
    let bindings = crate::factor_common_tails::block_alpha_bindings_with_locals(
        std::slice::from_ref(first_tail),
        std::slice::from_ref(second_tail),
        &localizable,
    )?;
    (bindings.local_binding(first_local) == Some(second_local))
        .then(|| (first_local.clone(), first_base.clone()))
}

fn indexed_local_declaration(statement: &Statement) -> Option<(&RcLocal, &RValue, i64)> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    let RValue::Index(index) = &assign.right[0] else {
        return None;
    };
    let RValue::Literal(Literal::Number(number)) = &*index.right else {
        return None;
    };
    if !number.is_finite() || number.fract() != 0.0 {
        return None;
    }
    Some((local, &index.left, *number as i64))
}

fn local_is_confined(
    region: &[Statement],
    local: &RcLocal,
    usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>,
) -> bool {
    let Some(whole) = usage.get(local) else {
        return false;
    };
    let region_usage = crate::inline_temps::collect_usage(&Block(region.to_vec()));
    let Some(region) = region_usage.get(local) else {
        return false;
    };
    !whole.captured
        && whole.reads == region.reads
        && whole.writes == region.writes
        && whole.writes == 1
}

fn indexed_item_name(base: &RValue) -> String {
    let RValue::Local(local) = base else {
        return "item".into();
    };
    let Some(name) = local.0 .0.lock().0.clone() else {
        return "item".into();
    };
    name.strip_suffix('s')
        .filter(|singular| !singular.is_empty())
        .unwrap_or("item")
        .to_string()
}

fn collect_functions(
    value: &mut RValue,
    functions: &mut Vec<by_address::ByAddress<triomphe::Arc<parking_lot::Mutex<crate::Function>>>>,
) {
    if let RValue::Closure(closure) = value {
        functions.push(closure.function.clone());
        return;
    }
    for child in value.rvalues_mut() {
        collect_functions(child, functions);
    }
}

fn ends_in_return(block: &Block) -> bool {
    matches!(block.0.last(), Some(Statement::Return(_)))
}

fn contains_goto_or_label(block: &Block) -> bool {
    block.0.iter().any(|statement| match statement {
        Statement::Goto(_) | Statement::Label(_) => true,
        Statement::If(node) => {
            contains_goto_or_label(&node.then_block.lock())
                || contains_goto_or_label(&node.else_block.lock())
        }
        Statement::While(node) => contains_goto_or_label(&node.block.lock()),
        Statement::Repeat(node) => contains_goto_or_label(&node.block.lock()),
        Statement::NumericFor(node) => contains_goto_or_label(&node.block.lock()),
        Statement::GenericFor(node) => contains_goto_or_label(&node.block.lock()),
        _ => false,
    })
}

/// Adjacent `if scrutinee == CONST then ... return end` arms become one
/// `if/elseif` chain. Requiring the same pure scrutinee and distinct literal
/// constants prevents unrelated early-return guards from being nested merely
/// because they happen to be adjacent.
fn chain_adjacent_return_ifs(statements: &mut Vec<Statement>) {
    let mut index = 0;
    while index + 1 < statements.len() {
        let Some((scrutinee, first_constant)) = return_arm_discriminator(&statements[index]) else {
            index += 1;
            continue;
        };
        let mut constants = vec![first_constant];
        let mut end = index + 1;
        while end < statements.len() {
            let Some((next_scrutinee, next_constant)) = return_arm_discriminator(&statements[end])
            else {
                break;
            };
            if !rvalue_exact_eq(&scrutinee, &next_scrutinee)
                || constants
                    .iter()
                    .any(|constant| rvalue_exact_eq(constant, &next_constant))
            {
                break;
            }
            constants.push(next_constant);
            end += 1;
        }

        if end == index + 1 {
            index += 1;
            continue;
        }

        let mut run: Vec<_> = statements.drain(index..end).collect();
        let mut chain = run.pop().unwrap();
        while let Some(mut previous) = run.pop() {
            let Statement::If(previous_if) = &mut previous else {
                unreachable!()
            };
            previous_if.else_block.lock().0.push(chain);
            chain = previous;
        }
        statements.insert(index, chain);
        index += 1;
    }
}

fn return_arm_discriminator(statement: &Statement) -> Option<(RValue, RValue)> {
    let Statement::If(node) = statement else {
        return None;
    };
    if !node.else_block.lock().0.is_empty()
        || !ends_in_return(&node.then_block.lock())
        || contains_goto_or_label(&node.then_block.lock())
    {
        return None;
    }
    let RValue::Binary(condition) = &node.condition else {
        return None;
    };
    if condition.operation != BinaryOperation::Equal {
        return None;
    }

    let (scrutinee, constant) = match (&*condition.left, &*condition.right) {
        (RValue::Literal(_), right) if !matches!(right, RValue::Literal(_)) => {
            (right, &*condition.left)
        }
        (left, RValue::Literal(_)) if !matches!(left, RValue::Literal(_)) => {
            (left, &*condition.right)
        }
        _ => return None,
    };
    if scrutinee.has_side_effects() {
        return None;
    }
    Some((scrutinee.clone(), constant.clone()))
}

fn is_true(value: &RValue) -> bool {
    matches!(value, RValue::Literal(Literal::Boolean(true)))
}

/// Recover the compiler-specific `repeat task.wait(...) until C` lowering. The
/// wait-only gate is intentionally narrow: moving arbitrary repeat statements
/// into the outer while could extend a body-local's lexical scope across REST.
/// With one call and no declaration, `while true do repeat WAIT until C; REST
/// end` is exactly `while true do WAIT; if C then REST end end`.
fn flatten_leading_repeat_in_infinite_while(statement: &mut Statement) {
    let Statement::While(node) = statement else {
        return;
    };
    if !is_true(&node.condition) {
        return;
    }

    let mut outer = node.block.lock();
    let Some(Statement::Repeat(repeat)) = outer.0.first() else {
        return;
    };
    if outer.0.len() < 2 || !is_single_task_wait(&repeat.block.lock()) {
        return;
    }

    let Statement::Repeat(repeat) = outer.0.remove(0) else {
        unreachable!()
    };
    let condition = repeat.condition;
    let mut statements = std::mem::take(&mut repeat.block.lock().0);
    let rest = std::mem::take(&mut outer.0);
    statements.push(crate::If::new(condition, Block(rest), Block::default()).into());
    outer.0 = statements;
}

fn is_single_task_wait(block: &Block) -> bool {
    let [Statement::Call(call)] = block.0.as_slice() else {
        return false;
    };
    let RValue::Index(Index { left, right }) = &*call.value else {
        return false;
    };
    matches!(&**left, RValue::Global(Global(name)) if name.as_slice() == b"task")
        && matches!(&**right, RValue::Literal(Literal::String(name)) if name.as_slice() == b"wait")
}

fn contains_loop_control(block: &Block) -> bool {
    contains_loop_control_statements(&block.0)
}

fn contains_loop_control_statements(statements: &[Statement]) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::Break(_) | Statement::Continue(_) => true,
        // A nested loop owns its own break/continue; moving the outer loop shape
        // does not retarget those controls.
        Statement::While(_)
        | Statement::Repeat(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_) => false,
        Statement::If(node) => {
            contains_loop_control(&node.then_block.lock())
                || contains_loop_control(&node.else_block.lock())
        }
        _ => false,
    })
}

fn terminal_break_condition(statement: &Statement) -> Option<RValue> {
    let Statement::If(node) = statement else {
        return None;
    };
    if !node.else_block.lock().0.is_empty() {
        return None;
    }
    let then_block = node.then_block.lock();
    matches!(then_block.0.as_slice(), [Statement::Break(_)]).then(|| node.condition.clone())
}

/// `while true do BODY; if C then break end end` -> `repeat BODY until C` when
/// no other control can target the outer loop. Luau repeat conditions can read
/// locals declared in BODY, preserving the original final-guard scope.
fn reroll_terminal_while(statement: &mut Statement) {
    let Statement::While(node) = statement else {
        return;
    };
    if !is_true(&node.condition) {
        return;
    }
    let mut body = node.block.lock();
    let Some(condition) = body.0.last().and_then(terminal_break_condition) else {
        return;
    };
    if contains_goto_or_label(&body)
        || contains_loop_control_statements(&body.0[..body.0.len() - 1])
    {
        return;
    }
    body.0.pop();
    let statements = std::mem::take(&mut body.0);
    drop(body);
    *statement = Statement::Repeat(Repeat::new(condition, Block(statements)));
}

#[cfg(test)]
mod tests {
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    use super::canonicalize_branches;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Break, Call, Closure, Function, Global, If, Index,
        LValue, Literal, Local, RValue, RcLocal, Repeat, Return, Statement, Upvalue, While,
    };

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn call(name: &str) -> Statement {
        Statement::Call(Call::new(global(name), vec![]))
    }

    fn task_wait() -> Statement {
        Statement::Call(Call::new(
            RValue::Index(Index::new(
                global("task"),
                RValue::Literal(Literal::String(b"wait".to_vec())),
            )),
            vec![RValue::Literal(Literal::Number(1.0))],
        ))
    }

    fn indexed_declaration(local: &RcLocal, collection: &RcLocal, index: f64) -> Statement {
        let mut assignment = Assign::new(
            vec![LValue::Local(local.clone())],
            vec![RValue::Index(Index::new(
                RValue::Local(collection.clone()),
                RValue::Literal(Literal::Number(index)),
            ))],
        );
        assignment.prefix = true;
        assignment.into()
    }

    fn return_local_if_truthy(local: &RcLocal) -> Statement {
        If::new(
            RValue::Local(local.clone()),
            Block(vec![Return::new(vec![RValue::Local(local.clone())]).into()]),
            Block::default(),
        )
        .into()
    }

    fn assign_local(local: &RcLocal, value: RValue) -> Statement {
        Assign::new(vec![LValue::Local(local.clone())], vec![value]).into()
    }

    fn declare_local(local: &RcLocal, value: RValue) -> Statement {
        let mut assignment = Assign::new(vec![LValue::Local(local.clone())], vec![value]);
        assignment.prefix = true;
        assignment.into()
    }

    fn ancestor_flag_walk(parent: &RcLocal, found: RValue, flag: &RcLocal) -> Statement {
        let failure = If::new(
            crate::Unary::new(RValue::Local(parent.clone()), crate::UnaryOperation::Not).into(),
            Block(vec![
                assign_local(flag, RValue::Literal(Literal::Boolean(false))),
                Break {}.into(),
            ]),
            Block::default(),
        )
        .into();
        let success = If::new(
            found,
            Block(vec![
                assign_local(flag, RValue::Literal(Literal::Boolean(true))),
                Break {}.into(),
            ]),
            Block::default(),
        )
        .into();
        let advance = Assign::new(
            vec![LValue::Local(parent.clone())],
            vec![RValue::Index(Index::new(
                RValue::Local(parent.clone()),
                RValue::Literal(Literal::String(b"Parent".to_vec())),
            ))],
        )
        .into();
        While::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![failure, success, advance]),
        )
        .into()
    }

    fn return_if(scrutinee: &RcLocal, condition: &str, body: &str) -> Statement {
        Statement::If(If::new(
            Binary::new(
                RValue::Local(scrutinee.clone()),
                RValue::Literal(Literal::String(condition.as_bytes().to_vec())),
                BinaryOperation::Equal,
            )
            .into(),
            Block(vec![call(body), Statement::Return(Return::default())]),
            Block::default(),
        ))
    }

    #[test]
    fn adjacent_return_ifs_become_elseif_chain() {
        let time_of_day = RcLocal::new(Local::new(Some("timeOfDay".into())));
        let mut block = Block(vec![
            return_if(&time_of_day, "Day", "day"),
            return_if(&time_of_day, "Night", "night"),
            call("fallback"),
        ]);
        canonicalize_branches(&mut block);
        let output = block.to_string();
        assert!(
            output.contains("elseif timeOfDay == \"Night\" then"),
            "{output}"
        );
        assert!(output.ends_with("fallback()"), "{output}");
    }

    #[test]
    fn unrelated_return_guards_stay_separate() {
        let mut block = Block(vec![
            Statement::If(If::new(
                global("invalidInput"),
                Block(vec![Statement::Return(Return::default())]),
                Block::default(),
            )),
            Statement::If(If::new(
                global("notReady"),
                Block(vec![Statement::Return(Return::default())]),
                Block::default(),
            )),
        ]);

        canonicalize_branches(&mut block);

        assert_eq!(block.0.len(), 2, "unrelated guards must not become elseif");
    }

    #[test]
    fn two_consecutive_index_copies_reroll_to_numeric_for() {
        let players = RcLocal::new(Local::new(Some("players".into())));
        let first = RcLocal::new(Local::new(Some("v5".into())));
        let second = RcLocal::new(Local::new(Some("v6".into())));
        let mut block = Block(vec![
            indexed_declaration(&first, &players, 1.0),
            return_local_if_truthy(&first),
            indexed_declaration(&second, &players, 2.0),
            return_local_if_truthy(&second),
            call("fallback"),
        ]);

        canonicalize_branches(&mut block);

        assert!(matches!(&block.0[0], Statement::NumericFor(_)));
        let output = block.to_string();
        assert!(output.contains("for i = 1, 2 do"), "{output}");
        assert!(output.contains("local player = players[i]"), "{output}");
        assert!(output.ends_with("fallback()"), "{output}");
    }

    #[test]
    fn index_copy_reroll_refuses_loop_control_retargeting() {
        let players = RcLocal::new(Local::new(Some("players".into())));
        let first = RcLocal::new(Local::new(Some("v5".into())));
        let second = RcLocal::new(Local::new(Some("v6".into())));
        let break_if = |local: &RcLocal| {
            If::new(
                RValue::Local(local.clone()),
                Block(vec![Break {}.into()]),
                Block::default(),
            )
            .into()
        };
        let mut block = Block(vec![
            indexed_declaration(&first, &players, 1.0),
            break_if(&first),
            indexed_declaration(&second, &players, 2.0),
            break_if(&second),
        ]);

        canonicalize_branches(&mut block);

        assert_eq!(block.0.len(), 4);
    }

    #[test]
    fn ancestor_flag_walk_recovers_while_cursor_shape() {
        let parent = RcLocal::new(Local::new(Some("parent".into())));
        let found = RcLocal::new(Local::new(Some("found".into())));
        let flag = RcLocal::new(Local::new(Some("isInside".into())));
        let mut block = Block(vec![
            declare_local(&parent, global("startParent")),
            ancestor_flag_walk(&parent, RValue::Local(found), &flag),
        ]);

        canonicalize_branches(&mut block);

        assert_eq!(block.0.len(), 3);
        let Statement::While(walk) = &block.0[1] else {
            panic!("expected while")
        };
        assert!(matches!(&walk.condition, RValue::Local(local) if local == &parent));
        assert_eq!(walk.block.lock().0.len(), 2);
        let Statement::If(exhausted) = &block.0[2] else {
            panic!("expected exhaustion assignment")
        };
        assert!(matches!(&exhausted.condition,
            RValue::Unary(unary)
                if unary.operation == crate::UnaryOperation::Not
                    && matches!(&*unary.value, RValue::Local(local) if local == &parent)));
    }

    #[test]
    fn ancestor_flag_walk_refuses_captured_cursor() {
        let parent = RcLocal::new(Local::new(Some("parent".into())));
        let found = RcLocal::new(Local::new(Some("found".into())));
        let flag = RcLocal::new(Local::new(Some("isInside".into())));
        let mut block = Block(vec![
            declare_local(&parent, global("startParent")),
            declare_local(
                &found,
                RValue::Closure(Closure {
                    function: ByAddress(Arc::new(Mutex::new(Function::default()))),
                    upvalues: vec![Upvalue::Ref(parent.clone())],
                }),
            ),
            ancestor_flag_walk(
                &parent,
                RValue::Call(Call::new(RValue::Local(found), vec![])),
                &flag,
            ),
        ]);

        canonicalize_branches(&mut block);

        assert_eq!(block.0.len(), 3);
        let Statement::While(walk) = &block.0[2] else {
            panic!("captured cursor walk must remain a while")
        };
        assert!(matches!(
            &walk.condition,
            RValue::Literal(Literal::Boolean(true))
        ));
        assert_eq!(walk.block.lock().0.len(), 3);
    }

    #[test]
    fn terminal_break_while_becomes_repeat() {
        let guard = Statement::If(If::new(
            global("done"),
            Block(vec![Statement::Break(Break {})]),
            Block::default(),
        ));
        let mut block = Block(vec![Statement::While(While::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![call("step"), guard]),
        ))]);
        canonicalize_branches(&mut block);
        assert!(matches!(block.0[0], Statement::Repeat(Repeat { .. })));
        assert_eq!(block.to_string(), "repeat\n\tstep()\nuntil done");
    }

    #[test]
    fn other_outer_break_refuses_repeat_reroll() {
        let inner_break = Statement::If(If::new(
            global("abort"),
            Block(vec![Statement::Break(Break {})]),
            Block::default(),
        ));
        let final_break = Statement::If(If::new(
            global("done"),
            Block(vec![Statement::Break(Break {})]),
            Block::default(),
        ));
        let mut block = Block(vec![Statement::While(While::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![inner_break, final_break]),
        ))]);
        canonicalize_branches(&mut block);
        assert!(matches!(block.0[0], Statement::While(_)));
    }

    #[test]
    fn leading_repeat_in_infinite_while_flattens_to_if() {
        let nested = Statement::Repeat(Repeat::new(global("ready"), Block(vec![task_wait()])));
        let mut block = Block(vec![Statement::While(While::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![nested, call("refresh")]),
        ))]);

        canonicalize_branches(&mut block);

        assert_eq!(
            block.to_string(),
            "while true do\n\ttask.wait(1)\n\n\tif ready then\n\t\trefresh()\n\tend\nend"
        );
    }

    #[test]
    fn leading_repeat_with_local_declaration_is_not_flattened() {
        let temporary = RcLocal::new(Local::new(Some("temporary".into())));
        let mut declaration = Assign::new(
            vec![LValue::Local(temporary)],
            vec![RValue::Literal(Literal::Number(1.0))],
        );
        declaration.prefix = true;
        let nested = Statement::Repeat(Repeat::new(
            global("ready"),
            Block(vec![declaration.into(), task_wait()]),
        ));
        let mut block = Block(vec![Statement::While(While::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![nested, call("refresh")]),
        ))]);

        canonicalize_branches(&mut block);

        let Statement::While(outer) = &block.0[0] else {
            panic!("expected while")
        };
        assert!(matches!(
            outer.block.lock().0.first(),
            Some(Statement::Repeat(_))
        ));
    }
}
