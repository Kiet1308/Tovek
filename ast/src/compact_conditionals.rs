//! `--style compact`: write a scalar select as a Luau if-expression.
//!
//! ```lua
//! local mode                          local mode = if flag then "a" else "b"
//! if flag then mode = "a" else    =>
//!     mode = "b" end
//! ```
//!
//! Every arm of the `if` chain must be exactly one assignment of one value to
//! the same local. A select with a value-exact boolean idiom is written as that
//! idiom (`c and t`, `not c or t`) instead. The expression evaluates the conditions in the same order
//! and then exactly one arm, which the assignment truncates to one value just
//! like each arm's own assignment did, so `false`/`nil` and every effect keep
//! their order. A chain without a final `else` only folds into the local's
//! declaration, where the missing arm is the declared `nil`.
//!
//! Arms stay statements when a value is a function or table literal, or when
//! the expression would not fit on one readable line.

use crate::{Block, LValue, Literal, LocalRw, RValue, RcLocal, Statement, Traverse};
use crate::deinline_safety::CaptureSafety;

/// Longest one-line if-expression (condition and arms) this style writes.
const MAX_WIDTH: usize = 90;
/// Longest `if ... elseif ...` chain folded into one expression.
const MAX_ARMS: usize = 4;

pub fn compact_conditionals(block: &mut Block) {
    rewrite(block, true, 0, None);
}

/// Default presentation: two atomic arms in a value position, at most 72
/// columns including the assignment/return and four columns per indent.
pub fn readable_conditionals(block: &mut Block) {
    let safety = CaptureSafety::captures(block);
    rewrite(block, false, 0, Some(&safety));
}

fn rewrite(block: &mut Block, compact: bool, depth: usize, safety: Option<&CaptureSafety>) {
    for statement in &mut block.0 {
        compact_nested(statement, compact, depth, safety);
    }
    compact_block(block, compact, depth, safety);
}

fn compact_nested(statement: &Statement, compact: bool, depth: usize, safety: Option<&CaptureSafety>) {
    // shortcut: charge possible multiline groups even when they stay flat;
    // use the formatter's exact layout if these conservative skips matter.
    let value_depth = depth + usize::from(!compact && match statement {
        Statement::Call(call) => call.arguments.len() > 1,
        Statement::MethodCall(call) => call.arguments.len() > 1,
        Statement::Return(ret) => ret.values.len() > 1,
        _ => false,
    });
    statement.visit_rvalues(&mut |value| { compact_nested_value(value, compact, value_depth, safety); true });
    statement.visit_lvalues(&mut |left| left.visit_rvalues(&mut |value| {
        compact_nested_value(value, compact, value_depth, safety); true
    }));
    // Shared blocks can print at different depths, just like shared function bodies.
    let rewrite_child = |child: &triomphe::Arc<parking_lot::Mutex<Block>>| {
        if compact || triomphe::Arc::strong_count(child) == 1 {
            rewrite(&mut child.lock(), compact, depth + 1, safety);
        }
    };
    match statement {
        Statement::If(r#if) => {
            rewrite_child(&r#if.then_block);
            rewrite_child(&r#if.else_block);
        }
        Statement::While(r#while) => rewrite_child(&r#while.block),
        Statement::Repeat(repeat) => rewrite_child(&repeat.block),
        Statement::NumericFor(numeric_for) => rewrite_child(&numeric_for.block),
        Statement::GenericFor(generic_for) => rewrite_child(&generic_for.block),
        _ => {}
    }
}

fn compact_nested_value(value: &RValue, compact: bool, depth: usize, safety: Option<&CaptureSafety>) {
    if let RValue::Closure(closure) = value {
        // shortcut: shared bodies keep statements; use a maximum-depth census if more compaction is needed.
        if !compact && triomphe::Arc::strong_count(&closure.function.0) > 1 { return; }
        rewrite(&mut closure.function.lock().body, compact, depth + 1, safety);
        return;
    }
    let child_depth = depth + usize::from(!compact && match value {
        // Tables increase the formatter's indentation even when their fields stay flat.
        RValue::Table(_) => true,
        RValue::Call(call) | RValue::Select(crate::Select::Call(call)) => call.arguments.len() > 1,
        RValue::MethodCall(call) | RValue::Select(crate::Select::MethodCall(call)) => call.arguments.len() > 1,
        RValue::Binary(binary) => matches!(binary.operation, crate::BinaryOperation::And | crate::BinaryOperation::Or),
        _ => false,
    });
    value.visit_rvalues(&mut |child| { compact_nested_value(child, compact, child_depth, safety); true });
}

fn compact_block(block: &mut Block, compact: bool, depth: usize, safety: Option<&CaptureSafety>) {
    let mut index = 0;
    while index < block.0.len() {
        let Statement::If(_) = &block.0[index] else {
            index += 1;
            continue;
        };
        if !compact && let Some((value, consumes_next)) = returned_select(&block.0[index], block.0.get(index + 1)) {
            let replacement: Statement = crate::Return::new(vec![value]).into();
            if readable_width(&replacement, depth) {
                block.0[index] = replacement;
                if consumes_next { block.0.remove(index + 1); }
                index += 1;
                continue;
            }
        }
        // `local x` directly before the chain: the missing arm is its `nil`.
        let declared = index
            .checked_sub(1)
            .and_then(|previous| bare_declaration(&block.0[previous]));
        // After freshness repair, removing a captured local's writes could make its closure shared.
        if declared.as_ref().is_some_and(|target| safety.is_some_and(|safety| !safety.uncaptured(target))) {
            index += 1;
            continue;
        }
        let target = declared.clone().or_else(|| first_assigned_local(&block.0[index]));
        let Some(target) = target else {
            index += 1;
            continue;
        };
        let Some(value) = select_value(&block.0[index], &target, declared.is_some(), 0, compact) else {
            index += 1;
            continue;
        };
        let mut assign = crate::Assign::new(vec![LValue::Local(target)], vec![value]);
        assign.prefix = declared.is_some();
        if if compact { !fits_one_line(&assign.right[0]) }
            else { !readable_width(&assign.clone().into(), depth) }
        {
            index += 1;
            continue;
        }
        if declared.is_some() {
            let declaration = block.0[index - 1].as_assign_mut().unwrap();
            declaration.right = assign.right;
            block.0.remove(index);
        } else {
            block.0[index] = assign.into();
            index += 1;
        }
    }
}

/// `local x` or `local x = nil`, one target.
fn bare_declaration(statement: &Statement) -> Option<RcLocal> {
    let Statement::Assign(assign) = statement else { return None };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 {
        return None;
    }
    if !(assign.right.is_empty() || matches!(assign.right.as_slice(), [RValue::Literal(Literal::Nil)])) {
        return None;
    }
    assign.left[0].as_local().cloned()
}

fn first_assigned_local(statement: &Statement) -> Option<RcLocal> {
    let Statement::If(r#if) = statement else { return None };
    let then_block = r#if.then_block.lock();
    let [Statement::Assign(assign)] = then_block.0.as_slice() else { return None };
    assign.left.first().and_then(LValue::as_local).cloned()
}

/// The if-expression equivalent to this `if` chain assigning `target`, or
/// `None` when any arm is not exactly `target = value`.
fn select_value(statement: &Statement, target: &RcLocal, else_is_nil: bool, depth: usize, compact: bool) -> Option<RValue> {
    let Statement::If(r#if) = statement else { return None };
    if depth >= if compact { MAX_ARMS } else { 1 }
        || else_is_nil && r#if.condition.values_read().contains(&target)
    {
        return None;
    }
    // `else_is_nil`: the chain folds into the target's declaration.
    let then_value = arm_value(&r#if.then_block.lock(), target, else_is_nil, compact)?;
    let else_block = r#if.else_block.lock();
    let else_value = match else_block.0.as_slice() {
        [] if else_is_nil && compact => RValue::Literal(Literal::Nil),
        [] => return None,
        [nested @ Statement::If(_)] => select_value(nested, target, else_is_nil, depth + 1, compact)?,
        _ => arm_value(&else_block, target, else_is_nil, compact)?,
    };
    Some(crate::conditional_expressions::select_expression(r#if.condition.clone(), then_value, else_value))
}

/// The value of an arm that is exactly `target = value`. Folded into the
/// declaration `local target = ...`, a value reading `target` would read an
/// outer binding instead (`Assign::reads_own_target`).
fn arm_value(block: &Block, target: &RcLocal, into_declaration: bool, compact: bool) -> Option<RValue> {
    let [Statement::Assign(assign)] = block.0.as_slice() else { return None };
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    if assign.left[0].as_local() != Some(target) || into_declaration && assign.reads_own_target() {
        return None;
    }
    let value = &assign.right[0];
    (if compact { !contains_multiline_literal(value) } else { atomic(value) }).then(|| value.clone())
}

fn atomic(value: &RValue) -> bool {
    match value {
        RValue::Literal(_) | RValue::Local(_) | RValue::Global(_) => true,
        RValue::Index(index) => atomic(&index.left) && matches!(*index.right, RValue::Literal(_)),
        _ => false,
    }
}

fn returned_select(statement: &Statement, following: Option<&Statement>) -> Option<(RValue, bool)> {
    let Statement::If(branch) = statement else { return None; };
    let arm = |block: &Block| {
        let [Statement::Return(ret)] = block.0.as_slice() else { return None; };
        let [value] = ret.values.as_slice() else { return None; };
        atomic(value).then(|| value.clone())
    };
    let yes = arm(&branch.then_block.lock())?;
    let otherwise = branch.else_block.lock();
    let (no, consumes_next) = if otherwise.0.is_empty() {
        let Statement::Return(ret) = following? else { return None; };
        let [value] = ret.values.as_slice() else { return None; };
        if !atomic(value) { return None; }
        (value.clone(), true)
    } else { (arm(&otherwise)?, false) };
    Some((crate::conditional_expressions::select_expression(branch.condition.clone(), yes, no), consumes_next))
}

fn readable_width(statement: &Statement, depth: usize) -> bool {
    // A block preview also emits the mandatory inferred-call site comment.
    let text = Block(vec![statement.clone()]).to_string();
    !text.contains('\n') && text.chars().count() + depth * 4 <= 72
}

fn contains_multiline_literal(value: &RValue) -> bool {
    match value {
        RValue::Closure(_) => true,
        RValue::Table(table) if !table.0.is_empty() => true,
        _ => !value.visit_rvalues(&mut |child| !contains_multiline_literal(child)),
    }
}

fn fits_one_line(value: &RValue) -> bool {
    let text = value.to_string();
    text.len() <= MAX_WIDTH && !text.contains('\n')
}

#[cfg(test)]
mod tests {
    use super::compact_conditionals;
    use crate::{Assign, Binary, BinaryOperation, Block, Call, Global, If, LValue, Literal, Local, RValue, RcLocal};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn string(text: &str) -> RValue {
        RValue::Literal(Literal::String(text.as_bytes().to_vec()))
    }

    fn assign(target: &RcLocal, value: RValue) -> crate::Statement {
        Assign::new(vec![LValue::Local(target.clone())], vec![value]).into()
    }

    fn declare(target: &RcLocal) -> crate::Statement {
        let mut declaration = Assign::new(vec![LValue::Local(target.clone())], vec![]);
        declaration.prefix = true;
        declaration.into()
    }

    fn selector_callback(field_width: usize) -> RValue {
        let parameter = local("c");
        let condition = crate::Index::new(value(&parameter), string(&"x".repeat(field_width))).into();
        crate::Closure { node_origin: Default::default(), upvalues: vec![],
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
                parameters: vec![parameter],
                body: Block(vec![If::new(condition,
                    Block(vec![crate::Return::new(vec![Literal::Boolean(false).into()]).into()]),
                    Block(vec![crate::Return::new(vec![Literal::Nil.into()]).into()])).into()]),
                ..Default::default()
            }))) }.into()
    }

    #[test]
    fn declaration_and_chain_become_one_if_expression() {
        let flag = local("flag");
        let other = local("other");
        let mode = local("mode");
        let mut block = Block(vec![
            declare(&mode),
            If::new(value(&flag), Block(vec![assign(&mode, string("a"))]), Block(vec![
                If::new(value(&other), Block(vec![assign(&mode, string("b"))]), Block::default()).into(),
            ])).into(),
        ]);
        compact_conditionals(&mut block);
        assert_eq!(block.to_string(), "local mode = if flag then \"a\" elseif other then \"b\" else nil");
    }

    #[test]
    fn reassignment_with_calls_keeps_order_in_one_expression() {
        let flag = local("flag");
        let total = local("total");
        let call = |name: &str| -> RValue {
            Call::new(RValue::Global(Global(name.as_bytes().to_vec())),
                vec![Binary::new(value(&total), RValue::Literal(Literal::Number(2.0)), BinaryOperation::Mul).into()]).into()
        };
        let mut block = Block(vec![
            If::new(value(&flag), Block(vec![assign(&total, call("double"))]),
                Block(vec![assign(&total, call("halve"))])).into(),
        ]);
        compact_conditionals(&mut block);
        assert_eq!(block.to_string(), "total = if flag then double(total * 2) else halve(total * 2)");
    }

    /// `local mode; if flag then mode = describe(mode) ...`: in `local mode =
    /// if flag then describe(mode) ...` the read would name an outer `mode`.
    /// As a reassignment the read keeps its variable and the chain folds.
    #[test]
    fn value_reading_the_declared_target_stays_a_statement() {
        let flag = local("flag");
        let mode = local("mode");
        let describe = || -> RValue { Call::new(RValue::Global(Global(b"describe".to_vec())), vec![value(&mode)]).into() };
        let chain = || -> crate::Statement {
            If::new(value(&flag), Block(vec![assign(&mode, describe())]), Block(vec![assign(&mode, string("b"))])).into()
        };
        let mut block = Block(vec![declare(&mode), chain()]);
        compact_conditionals(&mut block);
        assert_eq!(block.0.len(), 2, "{block}");
        let mut block = Block(vec![chain()]);
        compact_conditionals(&mut block);
        assert_eq!(block.to_string(), "mode = if flag then describe(mode) else \"b\"");
    }

    #[test]
    fn missing_else_without_declaration_stays_a_statement() {
        let flag = local("flag");
        let mode = local("mode");
        let mut block = Block(vec![
            If::new(value(&flag), Block(vec![assign(&mode, string("a"))]), Block::default()).into(),
        ]);
        compact_conditionals(&mut block);
        assert!(block.to_string().starts_with("if flag then"));
    }

    #[test]
    fn default_selects_only_two_atomic_arms() {
        let flag = local("flag");
        let mode = local("mode");
        let chain = |right| If::new(value(&flag), Block(vec![assign(&mode, string("a"))]),
            Block(vec![assign(&mode, right)])).into();
        let mut block = Block(vec![declare(&mode), chain(string("b"))]);
        super::readable_conditionals(&mut block);
        assert_eq!(block.to_string(), "local mode = if flag then \"a\" else \"b\"");
        let mut block = Block(vec![chain(Call::new(Global::from("effect").into(), vec![]).into())]);
        super::readable_conditionals(&mut block);
        assert!(matches!(block.0[0], crate::Statement::If(_)));
    }

    #[test]
    fn default_width_includes_target_and_indentation() {
        let flag = local("flag");
        let mode = local("mode");
        let chain = || If::new(value(&flag), Block(vec![assign(&mode, string("a"))]),
            Block(vec![assign(&mode, string(&"b".repeat(34)))])).into();
        let mut root = Block(vec![declare(&mode), chain()]);
        super::readable_conditionals(&mut root);
        assert!(root.to_string().chars().count() <= 72);
        assert_eq!(root.0.len(), 1);
        let mut nested = Block(vec![declare(&mode), chain()]);
        let safety = super::CaptureSafety::captures(&nested);
        super::rewrite(&mut nested, false, 2, Some(&safety));
        assert_eq!(nested.0.len(), 2, "indentation counts toward the line budget");
    }

    #[test]
    fn default_width_includes_rebuilt_call_comments() {
        let mut condition = Call::new(Global::from("predicate").into(), vec![]);
        condition.rebuilt = Some(crate::call_origins::Kind::StatementDeinline);
        let mut block = Block(vec![If::new(condition.into(),
            Block(vec![crate::Return::new(vec![string("accepted")]).into()]),
            Block(vec![crate::Return::new(vec![string("refused")]).into()])).into()]);
        super::readable_conditionals(&mut block);
        assert!(matches!(block.0[0], crate::Statement::If(_)));
        assert!(block.to_string().contains("-- inferred equivalent call"));
    }

    #[test]
    fn default_width_includes_expression_indentation_around_closures() {
        for context in 0..8 {
            for short in [false, true] {
                let width = if short { 1 } else if context == 0 { 29 } else { 33 };
                let callback = selector_callback(width);
                let long = || string(&"p".repeat(150));
                let table = |name: &str, value| crate::Table::new(vec![(Some(string(name)), value)]).into();
                let statement = match context {
                    0 => crate::Return::new(vec![table("outer", table("callback", callback))]).into(),
                    1 => Call::new(Global::from("consume").into(), vec![long(), callback]).into(),
                    2 => crate::MethodCall::new(Global::from("receiver").into(), "consume".into(), vec![long(), callback]).into(),
                    3 => assign(&local("result"), crate::Select::Call(Call::new(Global::from("consume").into(), vec![long(), callback])).into()),
                    4 => assign(&local("result"), crate::Select::MethodCall(crate::MethodCall::new(
                        Global::from("receiver").into(), "consume".into(), vec![long(), callback])).into()),
                    5 => crate::Return::new(vec![long(), callback]).into(),
                    6 => Assign::new(vec![crate::Index::new(Global::from("registry").into(), table("callback", callback)).into()],
                        vec![Literal::Nil.into()]).into(),
                    _ => crate::Return::new(vec![Binary::new(Global::from("p".repeat(150).as_str()).into(),
                        callback, BinaryOperation::Or).into()]).into(),
                };
                let mut block = Block(vec![statement]);
                super::readable_conditionals(&mut block);
                crate::inline_temps::collect_closures_in_statement(&block.0[0], &mut |closure| {
                    let folded = matches!(closure.function.lock().body.0[0], crate::Statement::Return(_));
                    assert_eq!(folded, short, "context {context}: {block}");
                });
                let text = block.to_string();
                for line in text.lines().filter(|line| line.contains("return if ")) {
                    let columns: usize = line.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
                    assert!(columns <= 72, "context {context}: {columns} columns: {line}");
                }
            }
        }
    }

    #[test]
    fn default_shared_callback_does_not_commit_to_its_shallowest_indentation() {
        for shared in [false, true] {
            let shallow = selector_callback(29);
            let deep = if shared { shallow.clone() } else { selector_callback(29) };
            let table = |value| crate::Table::new(vec![(Some(string("nested")), value)]).into();
            let mut block = Block(vec![crate::Return::new(vec![
                Call::new(shallow, vec![]).into(), table(table(deep)),
            ]).into()]);
            super::readable_conditionals(&mut block);
            let mut folded = Vec::new();
            crate::inline_temps::collect_closures_in_statement(&block.0[0], &mut |closure| {
                folded.push(matches!(closure.function.lock().body.0[0], crate::Statement::Return(_)));
            });
            assert_eq!(folded, [!shared, false], "{block}");
        }
    }

    #[test]
    fn default_shared_block_does_not_commit_to_its_shallowest_indentation() {
        for shared in [false, true] {
            let selector = || Block(vec![If::new(
                crate::Index::new(Global::from("c").into(), string(&"x".repeat(33))).into(),
                Block(vec![crate::Return::new(vec![Literal::Boolean(false).into()]).into()]),
                Block(vec![crate::Return::new(vec![Literal::Nil.into()]).into()])).into()]);
            let shallow = If::new(Global::from("first").into(), selector(), Block::default());
            let mut deep = If::new(Global::from("second").into(), Block::default(), Block::default());
            deep.then_block = if shared { shallow.then_block.clone() }
                else { triomphe::Arc::new(parking_lot::Mutex::new(selector())) };
            let mut block = Block(vec![shallow.into(), crate::While::new(Global::from("loop").into(),
                Block(vec![deep.into(), crate::Break {}.into()])).into()]);
            super::readable_conditionals(&mut block);
            let shallow_folded = matches!(block.0[0].as_if().unwrap().then_block.lock().0[0], crate::Statement::Return(_));
            assert_eq!(shallow_folded, !shared, "{block}");
            let deep_folded = matches!(block.0[1].as_while().unwrap().block.lock().0[0]
                .as_if().unwrap().then_block.lock().0[0], crate::Statement::Return(_));
            assert!(!deep_folded, "{block}");
            for line in block.to_string().lines().filter(|line| line.contains("return if ")) {
                let columns: usize = line.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum();
                assert!(columns <= 72, "{columns} columns: {line}");
            }
        }
    }

    #[test]
    fn default_returns_preserve_false_nil_and_multret_arity() {
        let ret = |value| crate::Return::new(vec![value]).into();
        let mut block = Block(vec![If::new(Global::from("flag").into(),
            Block(vec![ret(Literal::Boolean(false).into())]), Block::default()).into(), ret(Literal::Nil.into())]);
        super::readable_conditionals(&mut block);
        assert_eq!(block.to_string(), "return if flag then false else nil");
        let mut block = Block(vec![If::new(Global::from("flag").into(),
            Block(vec![ret(Call::new(Global::from("many").into(), vec![]).into())]),
            Block(vec![ret(Literal::Nil.into())])).into()]);
        super::readable_conditionals(&mut block);
        assert!(matches!(block.0[0], crate::Statement::If(_)), "a call can return multiple values");
    }

    #[test]
    fn declaration_condition_must_not_read_the_new_binding() {
        for compact in [false, true] {
            let mode = local("mode");
            let mut block = Block(vec![declare(&mode), If::new(value(&mode),
                Block(vec![assign(&mode, string("a"))]), Block(vec![assign(&mode, string("b"))])).into()]);
            let safety = (!compact).then(|| super::CaptureSafety::captures(&block));
            super::rewrite(&mut block, compact, 0, safety.as_ref());
            assert_eq!(block.0.len(), 2, "folding would bind the condition to an outer mode");
        }
    }

    #[test]
    fn default_keeps_captured_declaration_writes_after_freshness_repair() {
        for by_reference in [false, true] {
            for captured_target in [false, true] {
                let mode = local("mode");
                let captured = if captured_target { mode.clone() } else { local("other") };
                let closure = crate::Closure {
                    node_origin: Default::default(),
                    function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function {
                        body: Block(vec![crate::Return::new(vec![value(&captured)]).into()]),
                        ..Default::default()
                    }))),
                    upvalues: vec![if by_reference { crate::Upvalue::Ref(captured) } else { crate::Upvalue::Copy(captured) }],
                };
                let mut block = Block(vec![declare(&mode), If::new(Global::from("flag").into(),
                    Block(vec![assign(&mode, string("a"))]), Block(vec![assign(&mode, string("b"))])).into(),
                    crate::Return::new(vec![closure.into()]).into()]);
                super::readable_conditionals(&mut block);
                assert_eq!(block.0.len(), if captured_target { 3 } else { 2 });
                assert_eq!(block.0[0].as_assign().unwrap().right.is_empty(), captured_target,
                    "a captured branch result must keep its writes: {block}");
            }
        }
    }

    #[test]
    fn incomplete_capture_census_keeps_declaration_writes() {
        let mode = local("mode");
        let mut block = Block(vec![declare(&mode), If::new(Global::from("flag").into(),
            Block(vec![assign(&mode, string("a"))]), Block(vec![assign(&mode, string("b"))])).into()]);
        let safety = super::CaptureSafety::exhausted_for_tests();
        super::rewrite(&mut block, false, 0, Some(&safety));
        assert_eq!(block.0.len(), 2);
        assert!(block.0[0].as_assign().unwrap().right.is_empty());
    }
}
