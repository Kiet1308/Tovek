//! Turns deeply right-nested `if`s whose minority branch merely bails out
//! (e.g. `if c then <body> else return nil end`) into guard clauses
//! (`if not c then return nil end <body>`), matching idiomatic hand-written
//! Lua and undoing the staircase nesting the control-flow structurer leaves.
//! Pure restructuring: control flow is preserved exactly.

use std::collections::VecDeque;

use crate::{
    binary::is_boolean, Binary, BinaryOperation, Block, If, RValue, Statement, Traverse, Unary,
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
                node_origin: Default::default(),
                left: b.left,
                right: b.right,
                operation,
            })
        }
        other => RValue::Unary(Unary {
            node_origin: Default::default(),
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
// `has_rest` says whether more statements follow this `if` in its block;
// `is_elseif` whether it is the sole statement of an `else` (an `elseif`).
fn guard_split(f: If, has_rest: bool, is_elseif: bool) -> Result<(Statement, Vec<Statement>), If> {
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

    // A bare `return`/`break`/`continue` else arm bails out whatever the size
    // of the main arm: `if not c then return end <main>` is the idiomatic
    // guard (and the order the compiler laid the code out in). Splitting an
    // `elseif` that way would break its chain for no nesting gained.
    let else_bails = !is_elseif && {
        let e = f.else_block.lock();
        matches!(e.0.as_slice(), [statement] if is_guard_terminator(statement))
    };
    let pull = match (else_term, then_term) {
        (true, false)
            if else_size <= MAX_LIFTED_GUARD_BODY_SIZE
                && (then_size > else_size || else_bails) =>
        {
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
            if smaller > MAX_LIFTED_GUARD_BODY_SIZE {
                return Err(f);
            }
            if larger == smaller {
                // Equal arms: the then arm is the guard, in the order the
                // compiler laid them out (`if c then return a end return b`).
                // Only value returns: a bare `return` here is still the
                // function's implicit end, not a source exit. An `elseif`
                // keeps its chain.
                let returns_value = |block: &crate::Block| {
                    matches!(block.0.last(), Some(Statement::Return(r)) if !r.values.is_empty())
                };
                if is_elseif
                    || !returns_value(&f.then_block.lock())
                    || !returns_value(&f.else_block.lock())
                {
                    return Err(f);
                }
                Pull::Then
            } else if else_size < then_size {
                // Lift the smaller branch as the guard; keep the larger as
                // main flow.
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
        ..
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
    flatten_guards_in(block, false);
}

fn flatten_guards_in(block: &mut Block, is_else: bool) {
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                flatten_guards_in(&mut f.then_block.lock(), false);
                flatten_guards_in(&mut f.else_block.lock(), true);
            }
            Statement::While(w) => flatten_guards_in(&mut w.block.lock(), false),
            Statement::Repeat(r) => flatten_guards_in(&mut r.block.lock(), false),
            Statement::NumericFor(nf) => flatten_guards_in(&mut nf.block.lock(), false),
            Statement::GenericFor(gf) => flatten_guards_in(&mut gf.block.lock(), false),
            _ => {}
        }
    }
    let is_elseif = is_else && block.0.len() == 1;

    let mut work: VecDeque<Statement> = std::mem::take(&mut block.0).into();
    let mut out: Vec<Statement> = Vec::with_capacity(work.len());
    while let Some(s) = work.pop_front() {
        match s {
            Statement::If(f) => match guard_split(f, !work.is_empty(), is_elseif && out.is_empty()) {
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

/// Late guard clauses for a shared terminal tail.
///
/// The structurer emits a tail that several arms flow into exactly once, after
/// the `if`.  When that tail is a single `return`/`break`/`continue` and the
/// `if` has no `else`, the nested form
///
/// ```text
/// if c then <body> end
/// return nil
/// ```
///
/// reads better as the guard `if not c then return nil end <body> return nil`
/// (the trailing terminator is kept only when `<body>` can fall through).
/// Runs after the whole-chunk tail factoring and de-inlining passes so the
/// duplicated one-line terminator can no longer disturb their matching.
pub fn flatten_terminal_tail_guards(block: &mut Block) {
    for s in block.0.iter_mut() {
        match s {
            Statement::If(f) => {
                flatten_terminal_tail_guards(&mut f.then_block.lock());
                flatten_terminal_tail_guards(&mut f.else_block.lock());
            }
            Statement::While(w) => flatten_terminal_tail_guards(&mut w.block.lock()),
            Statement::Repeat(r) => flatten_terminal_tail_guards(&mut r.block.lock()),
            Statement::NumericFor(nf) => flatten_terminal_tail_guards(&mut nf.block.lock()),
            Statement::GenericFor(gf) => flatten_terminal_tail_guards(&mut gf.block.lock()),
            _ => {}
        }
        s.visit_rvalues_mut(&mut |value| {
            flatten_terminal_tail_guards_in_rvalue(value);
            true
        });
    }

    let mut index = 0;
    while index + 1 < block.0.len() {
        let eligible = match (&block.0[index], &block.0[index + 1]) {
            (Statement::If(f), tail) if small_guard_tail(tail) => {
                let then = f.then_block.lock();
                let else_empty = f.else_block.lock().0.is_empty();
                else_empty
                    && !then.0.is_empty()
                    && !contains_goto_or_label(&then.0)
                    && block_size(&then.0) >= COMPLEX_NEGATION_DENEST_MIN_BODY_SIZE
                    && (negation_is_readable(&f.condition)
                        || block_size(&then.0) >= 2 * COMPLEX_NEGATION_DENEST_MIN_BODY_SIZE)
            }
            _ => false,
        };
        if !eligible {
            index += 1;
            continue;
        }
        let Statement::If(f) = block.0.remove(index) else {
            unreachable!()
        };
        let tail = block.0[index].clone();
        let If {
            condition,
            then_block,
            ..
        } = f;
        let body = std::mem::take(&mut then_block.lock().0);
        let falls_through = !ends_in_terminator(&body);
        let mut replacement = vec![If::new(negate(condition), Block(vec![tail]), Block::default()).into()];
        replacement.extend(body);
        if !falls_through {
            // `<body>` never reaches the shared terminator: drop it.
            block.0.remove(index);
        }
        let count = replacement.len();
        block.0.splice(index..index, replacement);
        index += count;
    }
    invert_empty_then_arms(block);
}

/// `if c then else B end` becomes `if not c then B end`. Coalescing a common
/// tail out of both arms can leave the then arm empty; the negation is the
/// NaN-safe [`negate`] (only `not` and `==`/`~=` are folded). Nested blocks
/// were already visited by [`flatten_terminal_tail_guards`].
fn invert_empty_then_arms(block: &mut Block) {
    for statement in &mut block.0 {
        let Statement::If(r#if) = statement else { continue };
        if r#if.then_block.lock().0.is_empty() && !r#if.else_block.lock().0.is_empty() {
            let condition = std::mem::replace(&mut r#if.condition, crate::Literal::Nil.into());
            r#if.condition = negate(condition);
            std::mem::swap(&mut *r#if.then_block.lock(), &mut *r#if.else_block.lock());
        }
    }
}

fn small_guard_tail(statement: &Statement) -> bool {
    fn value_cost(value: &RValue) -> usize {
        // A single return statement can contain an entire UI tree or several
        // closures after late inlining. Never duplicate those as a guard.
        if matches!(value, RValue::Table(_) | RValue::Closure(_)) {
            return 13;
        }
        let mut children = 0;
        value.visit_rvalues(&mut |child| { children += value_cost(child); true });
        1 + children
    }
    if !is_guard_terminator(statement) { return false; }
    let mut cost = 0;
    statement.visit_rvalues(&mut |value| { cost += value_cost(value); true });
    cost <= 12
}

fn flatten_terminal_tail_guards_in_rvalue(value: &mut RValue) {
    if let RValue::Closure(closure) = value {
        flatten_terminal_tail_guards(&mut closure.function.lock().body);
        return;
    }
    value.visit_rvalues_mut(&mut |nested| {
        flatten_terminal_tail_guards_in_rvalue(nested);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::{flatten_guards, flatten_terminal_tail_guards};
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
    fn terminal_guard_does_not_duplicate_inlined_ui_constructor() {
        let ready = local("ready");
        let mut block = Block(vec![
            If::new(
                lv(&ready),
                Block(vec![call("a"), call("b"), call("c"), call("d")]),
                Block::default(),
            )
            .into(),
            Return::new(vec![
                Call::new(RValue::Global(Global::from("render")), vec![
                    crate::Table::new(vec![(None, Literal::Number(1.0).into())]).into(),
                ])
                .into(),
            ])
            .into(),
        ]);
        let before = block.to_string();
        super::flatten_terminal_tail_guards(&mut block);
        assert_eq!(block.to_string(), before);
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
    fn lifts_bare_return_else_over_an_equal_main_arm() {
        // `for ... do if ok then n += 1 else return end end` reads as the
        // guard `if not ok then return end n += 1`.
        let ok = local("ok");
        let mut block = Block(vec![If::new(
            lv(&ok),
            Block(vec![call("count")]),
            Block(vec![Return::default().into()]),
        )
        .into()]);

        flatten_guards(&mut block);

        assert_eq!(block.to_string(), "if not ok then\n\treturn\nend\n\ncount()");
    }

    #[test]
    fn equal_value_returns_become_a_guard_and_a_tail() {
        let ok = local("ok");
        let mut block = Block(vec![If::new(
            lv(&ok),
            Block(vec![Return::new(vec![Literal::Number(1.0).into()]).into()]),
            Block(vec![Return::new(vec![Literal::Number(2.0).into()]).into()]),
        )
        .into()]);

        flatten_guards(&mut block);

        assert_eq!(block.to_string(), "if ok then\n\treturn 1\nend\n\nreturn 2");
    }

    #[test]
    fn equal_arms_ending_the_function_keep_their_else() {
        // Bare returns here are the implicit end of the function, stripped
        // later: `if c then a() else b() end` must not become a guard.
        let ok = local("ok");
        let arm = |name| Block(vec![call(name), Return::default().into()]);
        let mut block = Block(vec![If::new(lv(&ok), arm("a"), arm("b")).into()]);
        let before = block.to_string();

        flatten_guards(&mut block);

        assert_eq!(block.to_string(), before);
    }

    #[test]
    fn keeps_elseif_chain_with_bare_return_else() {
        let (a, b) = (local("a"), local("b"));
        let chain = If::new(lv(&b), Block(vec![call("second")]), Block(vec![Return::default().into()]));
        let mut block = Block(vec![If::new(
            lv(&a),
            Block(vec![call("first")]),
            Block(vec![chain.into()]),
        )
        .into()]);
        let before = block.to_string();

        flatten_guards(&mut block);

        assert_eq!(block.to_string(), before);
        assert!(before.contains("elseif b then"), "{before}");
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

    #[test]
    fn empty_then_arm_becomes_a_negated_condition() {
        let flag = local("flag");
        let relational = Binary::new(lv(&flag), RValue::Literal(Literal::Number(1.0)), BinaryOperation::LessThan);
        let call = |name: &str| Statement::Call(Call::new(RValue::Global(Global(name.as_bytes().to_vec())), vec![]));
        let mut block = Block(vec![
            If::new(lv(&flag), Block::default(), Block(vec![call("a")])).into(),
            If::new(relational.into(), Block::default(), Block(vec![call("b")])).into(),
        ]);
        flatten_terminal_tail_guards(&mut block);
        // A relational comparison keeps its explicit `not` (NaN-safe).
        assert_eq!(block.to_string(), "if not flag then\n\ta()\nend\n\nif not (flag < 1) then\n\tb()\nend");
    }
}
