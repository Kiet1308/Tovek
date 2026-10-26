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

/// Longest one-line if-expression (condition and arms) this style writes.
const MAX_WIDTH: usize = 90;
/// Longest `if ... elseif ...` chain folded into one expression.
const MAX_ARMS: usize = 4;

pub fn compact_conditionals(block: &mut Block) {
    rewrite(block, true, 0);
}

/// Default presentation: two atomic arms in a value position, at most 72
/// columns including the assignment/return and four columns per indent.
pub fn readable_conditionals(block: &mut Block) {
    rewrite(block, false, 0);
}

fn rewrite(block: &mut Block, compact: bool, depth: usize) {
    for statement in &mut block.0 {
        compact_nested(statement, compact, depth);
    }
    compact_block(block, compact, depth);
}

fn compact_nested(statement: &Statement, compact: bool, depth: usize) {
    let mut functions = Vec::new();
    crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
        functions.push(closure.function.clone());
    });
    for function in functions {
        rewrite(&mut function.lock().body, compact, depth + 1);
    }
    match statement {
        Statement::If(r#if) => {
            rewrite(&mut r#if.then_block.lock(), compact, depth + 1);
            rewrite(&mut r#if.else_block.lock(), compact, depth + 1);
        }
        Statement::While(r#while) => rewrite(&mut r#while.block.lock(), compact, depth + 1),
        Statement::Repeat(repeat) => rewrite(&mut repeat.block.lock(), compact, depth + 1),
        Statement::NumericFor(numeric_for) => rewrite(&mut numeric_for.block.lock(), compact, depth + 1),
        Statement::GenericFor(generic_for) => rewrite(&mut generic_for.block.lock(), compact, depth + 1),
        _ => {}
    }
}

fn compact_block(block: &mut Block, compact: bool, depth: usize) {
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
        super::rewrite(&mut nested, false, 2);
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
            super::rewrite(&mut block, compact, 0);
            assert_eq!(block.0.len(), 2, "folding would bind the condition to an outer mode");
        }
    }
}
