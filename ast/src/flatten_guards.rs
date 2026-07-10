//! Turns deeply right-nested `if`s whose minority branch merely bails out
//! (e.g. `if c then <body> else return nil end`) into guard clauses
//! (`if not c then return nil end <body>`), matching idiomatic hand-written
//! Lua and undoing the staircase nesting the control-flow structurer leaves.
//! Pure restructuring: control flow is preserved exactly.

use std::collections::VecDeque;

use crate::{
    binary::is_boolean, Binary, BinaryOperation, Block, If, RValue, Statement, Unary,
    UnaryOperation,
};

// Statements that divert control out of the current linear flow. `goto` is
// deliberately excluded: it would entangle this pass with the label/goto
// structure produced by `simplify_gotos` (and we additionally skip any `if`
// whose branches mention a goto/label, see `guard_split`).
fn is_guard_terminator(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Return(_) | Statement::Break(_) | Statement::Continue(_)
    )
}

fn ends_in_terminator(stmts: &[Statement]) -> bool {
    matches!(stmts.last(), Some(s) if is_guard_terminator(s))
}

pub(crate) fn contains_goto_or_label(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Goto(_) | Statement::Label(_) => true,
        Statement::If(f) => {
            contains_goto_or_label(&f.then_block.lock().0)
                || contains_goto_or_label(&f.else_block.lock().0)
        }
        Statement::While(w) => contains_goto_or_label(&w.block.lock().0),
        Statement::Repeat(r) => contains_goto_or_label(&r.block.lock().0),
        Statement::NumericFor(nf) => contains_goto_or_label(&nf.block.lock().0),
        Statement::GenericFor(gf) => contains_goto_or_label(&gf.block.lock().0),
        _ => false,
    })
}

// Statement count, descending into nested control structures. Used here to pick
// the smaller branch to lift when both terminate, and by `recover_guard_continue`
// as a body-weight heuristic ("is this body non-trivial / worth de-nesting?").
pub(crate) fn block_size(stmts: &[Statement]) -> usize {
    stmts
        .iter()
        .map(|s| {
            1 + match s {
                Statement::If(f) => {
                    block_size(&f.then_block.lock().0) + block_size(&f.else_block.lock().0)
                }
                Statement::While(w) => block_size(&w.block.lock().0),
                Statement::Repeat(r) => block_size(&r.block.lock().0),
                Statement::NumericFor(nf) => block_size(&nf.block.lock().0),
                Statement::GenericFor(gf) => block_size(&gf.block.lock().0),
                _ => 0,
            }
        })
        .sum()
}

// Logical negation, kept readable:
//   not (not X)  ->  X
//   a == b       ->  a ~= b   (and vice-versa; exact in Lua, even for NaN)
//   otherwise    ->  not (cond)   (the formatter parenthesises by precedence)
// Relational operators are deliberately NOT flipped: `not (a < b)` differs from
// `a >= b` when an operand is NaN, so we keep an explicit `not`.
pub(crate) fn negate(cond: RValue) -> RValue {
    match cond {
        RValue::Unary(u) if u.operation == UnaryOperation::Not => *u.value,
        RValue::Binary(b)
            if matches!(
                b.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            let operation = match b.operation {
                BinaryOperation::Equal => BinaryOperation::NotEqual,
                _ => BinaryOperation::Equal,
            };
            RValue::Binary(Binary {
                left: b.left,
                right: b.right,
                operation,
            })
        }
        other => RValue::Unary(Unary {
            value: Box::new(other),
            operation: UnaryOperation::Not,
        }),
    }
}

enum Pull {
    Else,
    Then,
}

/// Once the main branch is this large, removing one indentation level is a
/// larger readability win than the small cost of an explicit `not (...)`.
const COMPLEX_NEGATION_DENEST_MIN_BODY_SIZE: usize = 4;
const MAX_LIFTED_GUARD_BODY_SIZE: usize = 2;

/// A lifted else-guard is worthwhile only when its negation is already compact
/// (`not flag`, `a ~= b`, double-not removal), or a later condition-normalization
/// step can De-Morgan it into at least one simpler operand. Refuse arithmetic,
/// relational and plain boolean spines that would merely gain `not (...)`.
fn negation_is_readable(condition: &RValue) -> bool {
    match condition {
        RValue::Local(_)
        | RValue::Global(_)
        | RValue::Literal(_)
        | RValue::Index(_)
        | RValue::Call(_)
        | RValue::MethodCall(_)
        | RValue::VarArg(_)
        | RValue::Select(_) => true,
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => true,
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            true
        }
        RValue::Binary(binary)
            if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) =>
        {
            negation_has_simplifiable_operand(&binary.left)
                || negation_has_simplifiable_operand(&binary.right)
        }
        _ => false,
    }
}

fn negation_has_simplifiable_operand(value: &RValue) -> bool {
    match value {
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => is_boolean(&unary.value),
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            true
        }
        RValue::Binary(binary)
            if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) =>
        {
            negation_has_simplifiable_operand(&binary.left)
                || negation_has_simplifiable_operand(&binary.right)
        }
        _ => false,
    }
}

// If `f` has a terminating branch worth lifting into a guard clause, returns the
// guard `if` plus the statements to inline after it. Otherwise hands `f` back.
// `has_rest` says whether more statements follow this `if` in its block.
fn guard_split(f: If, has_rest: bool) -> Result<(Statement, Vec<Statement>), If> {
    // Never disturb goto/label structure (see `is_guard_terminator`).
    let then_has_goto = contains_goto_or_label(&f.then_block.lock().0);
    let else_has_goto = contains_goto_or_label(&f.else_block.lock().0);
    if then_has_goto || else_has_goto {
        return Err(f);
    }

    let then_term = {
        let t = f.then_block.lock();
        !t.0.is_empty() && ends_in_terminator(&t.0)
    };
    let else_term = {
        let e = f.else_block.lock();
        !e.0.is_empty() && ends_in_terminator(&e.0)
    };
    let then_size = block_size(&f.then_block.lock().0);
    let else_size = block_size(&f.else_block.lock().0);

    let pull = match (else_term, then_term) {
        (true, false) if else_size <= MAX_LIFTED_GUARD_BODY_SIZE && then_size > else_size => {
            Pull::Else
        }
        (false, true) if then_size <= MAX_LIFTED_GUARD_BODY_SIZE && else_size > then_size => {
            Pull::Then
        }
        (true, true) => {
            // When both branches terminate, whichever one we inline ends in a
            // terminator. Splicing it ahead of trailing statements would leave a
            // `return`/`break`/`continue` mid-block (invalid Lua), so only
            // transform when nothing follows this `if`. (Those trailing statements
            // are dead code, but dropping them is out of scope for this pass.)
            if has_rest {
                return Err(f);
            }
            let smaller = then_size.min(else_size);
            let larger = then_size.max(else_size);
            if smaller > MAX_LIFTED_GUARD_BODY_SIZE || larger <= smaller {
                return Err(f);
            }
            // Lift the smaller branch as the guard; keep the larger as main flow.
            if else_size <= then_size {
                Pull::Else
            } else {
                Pull::Then
            }
        }
        _ => return Err(f),
    };

    if matches!(pull, Pull::Else) && !negation_is_readable(&f.condition) {
        let main_size = block_size(&f.then_block.lock().0);
        if main_size < COMPLEX_NEGATION_DENEST_MIN_BODY_SIZE {
            return Err(f);
        }
    }

    let If {
        condition,
        then_block,
        else_block,
    } = f;
    let then_stmts = std::mem::take(&mut then_block.lock().0);
    let else_stmts = std::mem::take(&mut else_block.lock().0);

    Ok(match pull {
        // if not C then <else> end ; <then...>
        Pull::Else => (
            If::new(negate(condition), Block(else_stmts), Block::default()).into(),
            then_stmts,
        ),
        // if C then <then> end ; <else...>
        Pull::Then => (
            If::new(condition, Block(then_stmts), Block::default()).into(),
            else_stmts,
        ),
    })
}

/// See module docs. Bottom-up: nested blocks are flattened first, then each
/// terminating branch is lifted into a guard clause at the current level, with
/// the inlined branch re-examined so a whole `if/else return` staircase collapses
/// in one pass.
pub fn flatten_guards(block: &mut Block) {
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                flatten_guards(&mut f.then_block.lock());
                flatten_guards(&mut f.else_block.lock());
            }
            Statement::While(w) => flatten_guards(&mut w.block.lock()),
            Statement::Repeat(r) => flatten_guards(&mut r.block.lock()),
            Statement::NumericFor(nf) => flatten_guards(&mut nf.block.lock()),
            Statement::GenericFor(gf) => flatten_guards(&mut gf.block.lock()),
            _ => {}
        }
    }

    let mut work: VecDeque<Statement> = std::mem::take(&mut block.0).into();
    let mut out: Vec<Statement> = Vec::with_capacity(work.len());
    while let Some(s) = work.pop_front() {
        match s {
            Statement::If(f) => match guard_split(f, !work.is_empty()) {
                Ok((guard, inline)) => {
                    out.push(guard);
                    // Re-process the inlined branch so further `... else return`
                    // levels lift too.
                    for st in inline.into_iter().rev() {
                        work.push_front(st);
                    }
                }
                Err(f) => out.push(Statement::If(f)),
            },
            other => out.push(other),
        }
    }
    block.0 = out;
}

#[cfg(test)]
mod tests {
    use super::flatten_guards;
    use crate::{
        Binary, BinaryOperation, Block, Call, Global, If, Literal, Local, RValue, RcLocal, Return,
        Statement, UnaryOperation,
    };

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.into())))
    }

    fn lv(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn call(name: &str) -> Statement {
        Call::new(RValue::Global(Global::from(name)), vec![]).into()
    }

    fn returning_branch() -> Block {
        Block(vec![call("prepare"), Return::default().into()])
    }

    #[test]
    fn refuses_guard_when_complex_negation_is_uglier() {
        let a = local("a");
        let b = local("b");
        let condition = Binary::new(lv(&a), lv(&b), BinaryOperation::And).into();
        let mut block = Block(vec![If::new(
            condition,
            returning_branch(),
            Block(vec![Return::new(vec![Literal::Nil.into()]).into()]),
        )
        .into()]);

        flatten_guards(&mut block);

        assert_eq!(block.0.len(), 1);
        assert!(matches!(&block.0[0], Statement::If(r#if)
            if !r#if.else_block.lock().0.is_empty()));
    }

    #[test]
    fn lifts_else_guard_when_equality_negation_simplifies() {
        let a = local("a");
        let b = local("b");
        let condition = Binary::new(lv(&a), lv(&b), BinaryOperation::Equal).into();
        let mut block = Block(vec![If::new(
            condition,
            returning_branch(),
            Block(vec![Return::new(vec![Literal::Nil.into()]).into()]),
        )
        .into()]);

        flatten_guards(&mut block);

        assert_eq!(block.0.len(), 3);
        let Statement::If(guard) = &block.0[0] else {
            panic!("expected guard")
        };
        assert!(guard.else_block.lock().0.is_empty());
        assert!(matches!(&guard.condition,
            RValue::Binary(binary) if binary.operation == BinaryOperation::NotEqual));
    }

    #[test]
    fn lifts_else_guard_for_atomic_condition() {
        let ready = local("ready");
        let mut block = Block(vec![If::new(
            lv(&ready),
            returning_branch(),
            Block(vec![Return::new(vec![Literal::Nil.into()]).into()]),
        )
        .into()]);

        flatten_guards(&mut block);

        let Statement::If(guard) = &block.0[0] else {
            panic!("expected guard")
        };
        assert!(matches!(&guard.condition,
            RValue::Unary(unary) if unary.operation == UnaryOperation::Not));
    }

    #[test]
    fn refuses_equal_large_terminal_branches() {
        let ready = local("ready");
        let branch = || {
            Block(vec![
                call("first"),
                call("second"),
                call("third"),
                Return::default().into(),
            ])
        };
        let mut block = Block(vec![If::new(lv(&ready), branch(), branch()).into()]);

        flatten_guards(&mut block);

        assert_eq!(block.0.len(), 1);
        assert!(matches!(&block.0[0], Statement::If(r#if)
            if !r#if.else_block.lock().0.is_empty()));
    }

    #[test]
    fn refuses_large_terminal_guard_branch() {
        let ready = local("ready");
        let mut block = Block(vec![If::new(
            lv(&ready),
            Block(vec![
                call("main1"),
                call("main2"),
                call("main3"),
                call("main4"),
            ]),
            Block(vec![
                call("cleanup1"),
                call("cleanup2"),
                call("cleanup3"),
                Return::default().into(),
            ]),
        )
        .into()]);

        flatten_guards(&mut block);

        assert_eq!(block.0.len(), 1);
    }
}
