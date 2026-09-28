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

use crate::{Block, LValue, Literal, RValue, RcLocal, Statement, Traverse};

/// Longest one-line if-expression (condition and arms) this style writes.
const MAX_WIDTH: usize = 90;
/// Longest `if ... elseif ...` chain folded into one expression.
const MAX_ARMS: usize = 4;

pub fn compact_conditionals(block: &mut Block) {
    for statement in &mut block.0 {
        compact_nested(statement);
    }
    compact_block(block);
}

fn compact_nested(statement: &Statement) {
    let mut functions = Vec::new();
    crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
        functions.push(closure.function.clone());
    });
    for function in functions {
        compact_conditionals(&mut function.lock().body);
    }
    match statement {
        Statement::If(r#if) => {
            compact_conditionals(&mut r#if.then_block.lock());
            compact_conditionals(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => compact_conditionals(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => compact_conditionals(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => compact_conditionals(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => compact_conditionals(&mut generic_for.block.lock()),
        _ => {}
    }
}

fn compact_block(block: &mut Block) {
    let mut index = 0;
    while index < block.0.len() {
        let Statement::If(_) = &block.0[index] else {
            index += 1;
            continue;
        };
        // `local x` directly before the chain: the missing arm is its `nil`.
        let declared = index
            .checked_sub(1)
            .and_then(|previous| bare_declaration(&block.0[previous]));
        let target = declared.clone().or_else(|| first_assigned_local(&block.0[index]));
        let Some(target) = target else {
            index += 1;
            continue;
        };
        let Some(value) = select_value(&block.0[index], &target, declared.is_some(), 0) else {
            index += 1;
            continue;
        };
        if !fits_one_line(&value) {
            index += 1;
            continue;
        }
        if declared.is_some() {
            let declaration = block.0[index - 1].as_assign_mut().unwrap();
            declaration.right = vec![value];
            block.0.remove(index);
        } else {
            let mut assign = crate::Assign::new(vec![LValue::Local(target)], vec![value]);
            assign.node_origin = crate::node_origins::Origin::default();
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
fn select_value(statement: &Statement, target: &RcLocal, else_is_nil: bool, depth: usize) -> Option<RValue> {
    let Statement::If(r#if) = statement else { return None };
    if depth >= MAX_ARMS {
        return None;
    }
    let then_value = arm_value(&r#if.then_block.lock(), target)?;
    let else_block = r#if.else_block.lock();
    let else_value = match else_block.0.as_slice() {
        [] if else_is_nil => RValue::Literal(Literal::Nil),
        [] => return None,
        [nested @ Statement::If(_)] => select_value(nested, target, else_is_nil, depth + 1)?,
        _ => arm_value(&else_block, target)?,
    };
    Some(crate::conditional_expressions::select_expression(r#if.condition.clone(), then_value, else_value))
}

fn arm_value(block: &Block, target: &RcLocal) -> Option<RValue> {
    let [Statement::Assign(assign)] = block.0.as_slice() else { return None };
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    if assign.left[0].as_local() != Some(target) {
        return None;
    }
    let value = &assign.right[0];
    (!contains_multiline_literal(value)).then(|| value.clone())
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
}
