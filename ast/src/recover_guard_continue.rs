//! Recovers idiomatic guard `continue` clauses inside `for`/`while` loop bodies.
//!
//! The DUAL of `flatten_guards`: where that pass lifts an *existing* terminating
//! branch (`if c then BODY else <bail> end` -> `if not c then <bail> end; BODY`),
//! this pass *manufactures* a synthetic `continue` for a trailing one-sided
//! `if cond then BODY end` (empty else) that is the last statement of a loop body:
//!
//! ```text
//! for _, x in t do                for _, x in t do
//!     if a and b then     ===>        if not a then continue end
//!         BODY                        if not b then continue end
//!     end                             BODY
//! end                             end
//! ```
//!
//! Each top-level `and` conjunct becomes its own guard. A maximal run of adjacent
//! negated conjuncts (`not f1 and not f2`, the De-Morgan'd form the structurer
//! produces from a source `if f1 or f2 then continue end`) re-groups into a single
//! `if f1 or f2 then continue end`, matching hand-written source instead of
//! exploding into many lines.
//!
//! ## Why this is exact (no semantic change)
//!
//! * `continue` is, by definition, "stop the rest of this iteration and go to the
//!   loop's iteration step" — identical to falling off the end of the loop body.
//!   So `if cond then BODY end` *as the last statement* ≡ `if not cond then
//!   continue end; BODY`. The last-statement precondition is mandatory: otherwise
//!   the introduced `continue` would skip statements that follow the `if`.
//! * Splitting `a and b` into two sequential guards reproduces Lua's left-to-right
//!   short-circuit exactly (each conjunct is evaluated once, only if all earlier
//!   guards passed), so it is safe even when the conjuncts have side effects. We
//!   never split `or` and never reorder/duplicate operands.
//! * Negation goes through [`negate`], which flips `==`/`~=` and strips double
//!   `not`, but deliberately does NOT flip relational operators (`not (a < b)` is
//!   kept verbatim) because `a >= b` differs from `not (a < b)` for NaN.
//!
//! ## Placement (HARD invariant)
//!
//! This MUST run as the last AST transform, with only the formatter after it. The
//! manufactured `not (a < b)` would be silently turned into the NaN-unsafe
//! `a >= b` if any later `reduce`/`reduce_condition` pass touched it (see
//! `Unary::reduce`). Turning `not (a < b)` into the source-faithful `a >= b` is the
//! separate `normalize_conditions` job (proposal §10), out of scope here.

use crate::{
    conditional_expressions::expression_cost,
    flatten_guards::{block_size, contains_goto_or_label, negate},
    Binary, BinaryOperation, Block, Continue, If, RValue, Statement, Traverse, UnaryOperation,
};

/// A single heavy predicate (e.g. a `string.find(...)` call) over an otherwise
/// trivial body is still worth turning into a guard. Calibrated against
/// `expression_cost` (a `Call`/`MethodCall` costs 8): ~24 ≈ "more than one call,
/// or a call plus a comparison", so a lone cheap `if a.b then ...` stays untouched
/// while a genuinely heavy single condition fires.
const COND_COST_THRESHOLD: usize = 24;

/// See module docs.
pub fn recover_guard_continue(block: &mut Block) {
    process_block(block, false);
}

/// Recurse into every nested structure to (a) process nested loops/closures and
/// (b), when this block is a loop body, peel its trailing guard chain. Children
/// are processed before the current block is peeled, so a hoisted body's nested
/// loops have already had their own guards recovered.
fn process_block(block: &mut Block, is_loop_body: bool) {
    for statement in &mut block.0 {
        descend_into(statement);
    }
    if is_loop_body {
        peel_trailing_guards(&mut block.0);
    }
}

fn descend_into(statement: &mut Statement) {
    descend_closures(statement);
    match statement {
        // An `if` is not a loop body; recurse only to reach nested loops/closures.
        Statement::If(r#if) => {
            process_block(&mut r#if.then_block.lock(), false);
            process_block(&mut r#if.else_block.lock(), false);
        }
        Statement::While(r#while) => process_block(&mut r#while.block.lock(), true),
        Statement::NumericFor(numeric_for) => process_block(&mut numeric_for.block.lock(), true),
        Statement::GenericFor(generic_for) => process_block(&mut generic_for.block.lock(), true),
        // `repeat`: a `continue` that jumps over a body-local read by the `until`
        // condition is a Luau *compile* error (not caught by the parser-based test
        // gate), so we never synthesize a `continue` targeting a repeat. We still
        // descend so nested `for`/`while` loops inside the repeat are recovered.
        Statement::Repeat(repeat) => process_block(&mut repeat.block.lock(), false),
        _ => {}
    }
}

/// Closures are stitched back into the module body by `link_upvalues`, so this
/// whole-module pass must descend into them explicitly. A closure boundary resets
/// the loop context (a `continue` cannot cross a function boundary), which the
/// fresh `recover_guard_continue` entry guarantees.
///
/// This reaches only closures embedded in the statement's *expressions* (e.g. an
/// assignment RHS or call argument); `post_traverse_rvalues` stops at block
/// boundaries (`Closure` has an empty `Traverse` impl). Closures sitting as
/// *statements* inside nested blocks are reached by the structural recursion in
/// `descend_into`/`process_block`, so the two mechanisms cover disjoint positions.
fn descend_closures(statement: &mut Statement) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        recover_guard_continue(&mut function.lock().body);
    }
}

/// Peel every trailing `if cond then BODY end` from a loop body into guard
/// clauses. Only the *last* statement can ever be a guard (otherwise the
/// introduced `continue` would skip the statements after it), so after each hoist
/// we re-examine the newly exposed tail, collapsing a whole nested staircase.
fn peel_trailing_guards(body: &mut Vec<Statement>) {
    loop {
        let fire = match body.last() {
            Some(Statement::If(r#if)) => {
                if !r#if.else_block.lock().0.is_empty() {
                    false // one-sided guards only; an else branch would be lost
                } else {
                    let then = r#if.then_block.lock();
                    !then.0.is_empty()
                        // a lone `continue`/`break`/`return` is already the
                        // idiomatic form — nothing to hoist, keep it
                        && !is_bare_terminator(&then.0)
                        // never entangle with goto/label structure
                        && !contains_goto_or_label(&then.0)
                        && heuristics_fire(&r#if.condition, &then.0)
                }
            }
            _ => false,
        };
        if !fire {
            return;
        }

        let Some(Statement::If(r#if)) = body.pop() else {
            unreachable!()
        };
        let If {
            condition,
            then_block,
            ..
        } = r#if;
        let body_stmts = std::mem::take(&mut then_block.lock().0);
        body.extend(build_guards(condition));
        body.extend(body_stmts);
        // loop: the appended BODY tail may itself be a trailing guard
    }
}

/// Refusal-by-default: fire only when the rewrite buys readability.
///
/// * Gate A (`block_size > 1`) counts AST nodes recursively, so a single but
///   heavily nested body (one `if/elseif` block of many lines) still qualifies —
///   de-nesting a real body is always a win.
/// * Gate B catches a lone heavy predicate (e.g. a `string.find(...)` filter)
///   even over a tiny body.
/// * Gate C splits a multi-predicate filter over a trivial body only when the
///   condition carries an explicit negation (see [`has_negated_conjunct`]). A
///   conjunction with `not`s is what hand-written source expresses as guard
///   `continue`s; an all-positive `a and b and c` reads cleanly as one compact
///   `if`, so those are left as the source author wrote them.
fn heuristics_fire(condition: &RValue, body: &[Statement]) -> bool {
    block_size(body) > 1
        || expression_cost(condition) > COND_COST_THRESHOLD
        || (conjunct_count(condition) >= 2 && has_negated_conjunct(condition))
}

/// True when any top-level `and` conjunct is a `not X`. Such a conjunction
/// (`... and not x and not y`) reads awkwardly positively and is the De-Morgan'd
/// shape the structurer leaves from a source guard, so it is worth recovering;
/// an all-positive conjunction is not (over a trivial body — gates A/B still apply).
fn has_negated_conjunct(condition: &RValue) -> bool {
    match condition {
        RValue::Binary(binary) if binary.operation == BinaryOperation::And => {
            has_negated_conjunct(&binary.left) || has_negated_conjunct(&binary.right)
        }
        other => is_not(other),
    }
}

/// Build the guard clauses for a hoisted condition. Splits the top-level `and`
/// spine; a maximal run of adjacent `not X` conjuncts collapses to one
/// `if X1 or X2 ... then continue end` (De Morgan); every other conjunct becomes
/// `if negate(c) then continue end`.
fn build_guards(condition: RValue) -> Vec<Statement> {
    let mut conjuncts = Vec::new();
    flatten_and(condition, &mut conjuncts);

    let mut guards = Vec::new();
    let mut iter = conjuncts.into_iter().peekable();
    while let Some(conjunct) = iter.next() {
        match strip_not(conjunct) {
            // `not X` starts a run; greedily absorb following `not Y` conjuncts and
            // emit a single positive `if X or Y or ... then continue end`.
            Ok(mut disjunction) => {
                while iter.peek().is_some_and(is_not) {
                    let next = strip_not(iter.next().unwrap()).unwrap();
                    disjunction = Binary::new(disjunction, next, BinaryOperation::Or).into();
                }
                guards.push(guard(disjunction));
            }
            Err(other) => guards.push(guard(negate(other))),
        }
    }
    guards
}

fn guard(condition: RValue) -> Statement {
    If::new(condition, Block(vec![Continue {}.into()]), Block::default()).into()
}

/// Flatten the top-level `and` spine into conjuncts in left-to-right (i.e.
/// short-circuit) order, regardless of associativity. `or` is never split.
fn flatten_and(condition: RValue, out: &mut Vec<RValue>) {
    match condition {
        RValue::Binary(binary) if binary.operation == BinaryOperation::And => {
            flatten_and(*binary.left, out);
            flatten_and(*binary.right, out);
        }
        other => out.push(other),
    }
}

fn conjunct_count(condition: &RValue) -> usize {
    match condition {
        RValue::Binary(binary) if binary.operation == BinaryOperation::And => {
            conjunct_count(&binary.left) + conjunct_count(&binary.right)
        }
        _ => 1,
    }
}

fn is_not(value: &RValue) -> bool {
    matches!(value, RValue::Unary(unary) if unary.operation == UnaryOperation::Not)
}

/// `Ok(inner)` when `value` is `not inner` (the caller may start/extend an
/// `or`-run with `inner`), otherwise `Err(value)` unchanged (the caller emits a
/// single `negate(value)` guard). Not an error channel — just "was it negated?".
fn strip_not(value: RValue) -> Result<RValue, RValue> {
    match value {
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => Ok(*unary.value),
        other => Err(other),
    }
}

fn is_bare_terminator(stmts: &[Statement]) -> bool {
    matches!(
        stmts,
        [Statement::Continue(_)] | [Statement::Break(_)] | [Statement::Return(_)]
    )
}

#[cfg(test)]
mod tests {
    use super::recover_guard_continue;
    use crate::{
        Binary, BinaryOperation, Block, Break, Call, Continue, GenericFor, Global, Goto, If, Local,
        RValue, RcLocal, Repeat, Return, Statement, Unary, UnaryOperation, While,
    };

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn lv(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn call_stmt(name: &str) -> Statement {
        Call::new(global(name), vec![]).into()
    }

    fn call_val(name: &str) -> RValue {
        RValue::Call(Call::new(global(name), vec![]))
    }

    fn and(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::And).into()
    }

    fn or(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::Or).into()
    }

    fn not(a: RValue) -> RValue {
        Unary::new(a, UnaryOperation::Not).into()
    }

    fn eq(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::Equal).into()
    }

    fn gen_for(body: Vec<Statement>) -> Statement {
        GenericFor::new(vec![local("x")], vec![global("pairs")], Block(body)).into()
    }

    fn if_then(condition: RValue, body: Vec<Statement>) -> Statement {
        If::new(condition, Block(body), Block::default()).into()
    }

    fn if_then_else(condition: RValue, then: Vec<Statement>, els: Vec<Statement>) -> Statement {
        If::new(condition, Block(then), Block(els)).into()
    }

    // --- assertion helpers ---

    fn loop_body(block: &Block) -> Vec<Statement> {
        match &block.0[0] {
            Statement::GenericFor(gf) => gf.block.lock().0.clone(),
            Statement::Repeat(r) => r.block.lock().0.clone(),
            other => panic!("expected loop, got {:?}", other),
        }
    }

    /// True iff `stmt` is `if <cond> then continue end` (empty else).
    fn is_continue_guard(stmt: &Statement) -> bool {
        let Statement::If(r#if) = stmt else {
            return false;
        };
        r#if.else_block.lock().0.is_empty()
            && matches!(r#if.then_block.lock().0.as_slice(), [Statement::Continue(_)])
    }

    fn guard_condition(stmt: &Statement) -> RValue {
        let Statement::If(r#if) = stmt else {
            panic!("not an if");
        };
        r#if.condition.clone()
    }

    #[test]
    fn splits_top_level_and() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("f"), call_stmt("g")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 4, "two guards + two body statements:\n{}", block);
        assert!(is_continue_guard(&body[0]) && is_continue_guard(&body[1]));
        // each guard negates one conjunct
        assert!(matches!(guard_condition(&body[0]),
            RValue::Unary(u) if u.operation == UnaryOperation::Not));
        assert!(matches!(&body[2], Statement::Call(_)));
        assert!(matches!(&body[3], Statement::Call(_)));
    }

    #[test]
    fn demorgan_run_groups_into_single_or_guard() {
        // if (not f1) and (not f2) then big1(); big2() end  ->  if f1 or f2 then continue end; ...
        let mut block = Block(vec![gen_for(vec![if_then(
            and(not(call_val("f1")), not(call_val("f2"))),
            vec![call_stmt("big1"), call_stmt("big2")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 3, "one or-guard + two body statements:\n{}", block);
        assert!(is_continue_guard(&body[0]));
        assert!(
            matches!(guard_condition(&body[0]),
                RValue::Binary(b) if b.operation == BinaryOperation::Or),
            "negated run must collapse to a single `or` guard:\n{}",
            block
        );
    }

    #[test]
    fn mixed_condition_groups_negations_and_splits_rest() {
        // A and (not f1) and (not f2) and (c == d)
        let a = local("a");
        let c = local("c");
        let d = local("d");
        let condition = and(
            and(and(lv(&a), not(call_val("f1"))), not(call_val("f2"))),
            eq(lv(&c), lv(&d)),
        );
        let mut block = Block(vec![gen_for(vec![if_then(
            condition,
            vec![call_stmt("body")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        // guards: [if not A], [if f1 or f2], [if c ~= d]  + body call
        assert_eq!(body.len(), 4, "three guards + body:\n{}", block);
        assert!(is_continue_guard(&body[0]) && is_continue_guard(&body[1]) && is_continue_guard(&body[2]));
        assert!(matches!(guard_condition(&body[1]),
            RValue::Binary(b) if b.operation == BinaryOperation::Or));
        // equality conjunct negates to `~=`
        assert!(matches!(guard_condition(&body[2]),
            RValue::Binary(b) if b.operation == BinaryOperation::NotEqual));
    }

    #[test]
    fn top_level_or_stays_single_guard() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            or(lv(&a), lv(&b)),
            vec![call_stmt("big1"), call_stmt("big2")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 3, "single guard (no or-split) + body:\n{}", block);
        assert!(is_continue_guard(&body[0]));
        // condition is `not (a or b)`
        assert!(matches!(guard_condition(&body[0]),
            RValue::Unary(u) if u.operation == UnaryOperation::Not));
    }

    #[test]
    fn staircase_collapses_in_one_pass() {
        let c1 = local("c1");
        let c2 = local("c2");
        let mut block = Block(vec![gen_for(vec![if_then(
            lv(&c1),
            vec![if_then(lv(&c2), vec![call_stmt("big1"), call_stmt("big2")])],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 4, "two guards + two body statements:\n{}", block);
        assert!(is_continue_guard(&body[0]) && is_continue_guard(&body[1]));
        assert!(matches!(&body[2], Statement::Call(_)) && matches!(&body[3], Statement::Call(_)));
    }

    #[test]
    fn equality_negates_to_not_equal() {
        let a = local("a");
        let b = local("b");
        let c = local("c");
        // non-trivial body so gate A fires regardless of the negation gate
        let mut block = Block(vec![gen_for(vec![if_then(
            and(eq(lv(&a), lv(&b)), lv(&c)),
            vec![call_stmt("body1"), call_stmt("body2")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert!(matches!(guard_condition(&body[0]),
            RValue::Binary(b) if b.operation == BinaryOperation::NotEqual));
    }

    #[test]
    fn refuses_positive_conjunction_over_trivial_body() {
        // `if a and b then oneThing() end` — all-positive, cheap, single-statement
        // body: source keeps this compact, so we must not churn it into guards.
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("one")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 1, "compact positive conjunction must stay untouched:\n{}", block);
        assert!(!is_continue_guard(&body[0]));
    }

    #[test]
    fn fires_negated_conjunction_over_trivial_body() {
        // `if not f1 and not f2 then oneThing() end` — the De-Morgan'd guard shape;
        // worth recovering even over a one-statement body.
        let mut block = Block(vec![gen_for(vec![if_then(
            and(not(call_val("f1")), not(call_val("f2"))),
            vec![call_stmt("one")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 2, "negated conjunction collapses to one or-guard + body:\n{}", block);
        assert!(is_continue_guard(&body[0]));
        assert!(matches!(guard_condition(&body[0]),
            RValue::Binary(b) if b.operation == BinaryOperation::Or));
    }

    #[test]
    fn fires_in_while_loop() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![While::new(
            global("running"),
            Block(vec![if_then(
                and(lv(&a), lv(&b)),
                vec![call_stmt("f"), call_stmt("g")],
            )]),
        )
        .into()]);

        recover_guard_continue(&mut block);

        let Statement::While(w) = &block.0[0] else {
            panic!("expected while");
        };
        let body = w.block.lock();
        assert_eq!(body.0.len(), 4, "while-loop guard recovery:\n{}", block);
        assert!(is_continue_guard(&body.0[0]) && is_continue_guard(&body.0[1]));
    }

    #[test]
    fn deep_staircase_collapses_in_one_pass() {
        let c1 = local("c1");
        let c2 = local("c2");
        let c3 = local("c3");
        let mut block = Block(vec![gen_for(vec![if_then(
            lv(&c1),
            vec![if_then(
                lv(&c2),
                vec![if_then(lv(&c3), vec![call_stmt("a"), call_stmt("b")])],
            )],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 5, "three guards + two body statements:\n{}", block);
        assert!(
            is_continue_guard(&body[0]) && is_continue_guard(&body[1]) && is_continue_guard(&body[2])
        );
    }

    #[test]
    fn idempotent_on_second_run() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("f"), call_stmt("g")],
        )])]);

        recover_guard_continue(&mut block);
        let once = block.to_string();
        recover_guard_continue(&mut block);
        assert_eq!(once, block.to_string(), "second run must be a no-op");
    }

    #[test]
    fn multi_statement_body_ending_in_return_converts() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("prep"), Return::default().into()],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 4, "two guards + prep + return:\n{}", block);
        assert!(is_continue_guard(&body[0]) && is_continue_guard(&body[1]));
        assert!(matches!(&body[3], Statement::Return(_)));
    }

    #[test]
    fn continue_is_last_in_its_then_block() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("body1"), call_stmt("body2")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        for guard in body.iter().take(2) {
            let Statement::If(r#if) = guard else {
                panic!("expected guard if");
            };
            assert!(
                matches!(r#if.then_block.lock().0.as_slice(), [Statement::Continue(_)]),
                "continue must be the sole/last statement of its block (Luau rule):\n{}",
                block
            );
        }
    }

    // --- refusal cases ---

    #[test]
    fn refuses_trivial_one_liner() {
        let c = local("c");
        let mut block = Block(vec![gen_for(vec![if_then(lv(&c), vec![call_stmt("one")])])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 1, "trivial guard must be left untouched:\n{}", block);
        assert!(!is_continue_guard(&body[0]));
    }

    #[test]
    fn refuses_non_empty_else() {
        let c = local("c");
        let mut block = Block(vec![gen_for(vec![if_then_else(
            lv(&c),
            vec![call_stmt("big1"), call_stmt("big2")],
            vec![call_stmt("other")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 1);
        let Statement::If(r#if) = &body[0] else {
            panic!("expected if to remain");
        };
        assert!(!r#if.else_block.lock().0.is_empty(), "else branch must be preserved");
    }

    #[test]
    fn refuses_when_statement_follows_the_if() {
        let c = local("c");
        let mut block = Block(vec![gen_for(vec![
            if_then(lv(&c), vec![call_stmt("big1"), call_stmt("big2")]),
            call_stmt("after"),
        ])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 2);
        assert!(!is_continue_guard(&body[0]), "non-trailing if must be left intact:\n{}", block);
    }

    #[test]
    fn refuses_outside_loop() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("big1"), call_stmt("big2")],
        )]);

        recover_guard_continue(&mut block);

        // no enclosing loop -> nothing peeled
        assert_eq!(block.0.len(), 1);
        assert!(!is_continue_guard(&block.0[0]), "must not synthesize continue outside a loop:\n{}", block);
    }

    #[test]
    fn refuses_inside_repeat_but_fires_nested_for() {
        let c = local("c");
        let d = local("d");
        let e = local("e");
        let inner_for = gen_for(vec![if_then(
            and(lv(&d), lv(&e)),
            vec![call_stmt("thing1"), call_stmt("thing2")],
        )]);
        let mut block = Block(vec![Repeat::new(
            global("done"),
            Block(vec![
                if_then(lv(&c), vec![call_stmt("big1"), call_stmt("big2")]),
                inner_for,
            ]),
        )
        .into()]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        // the repeat's own trailing-ish if must be untouched...
        assert!(!is_continue_guard(&body[0]), "must not synthesize continue targeting a repeat:\n{}", block);
        // ...but the nested for must have its guards recovered
        let Statement::GenericFor(gf) = &body[1] else {
            panic!("expected nested for");
        };
        let inner = gf.block.lock();
        assert_eq!(inner.0.len(), 4, "nested for inside repeat should still fire:\n{}", block);
        assert!(is_continue_guard(&inner.0[0]) && is_continue_guard(&inner.0[1]));
    }

    #[test]
    fn keeps_bare_continue_guard() {
        let bad = local("bad");
        let mut block = Block(vec![gen_for(vec![If::new(
            lv(&bad),
            Block(vec![Continue {}.into()]),
            Block::default(),
        )
        .into()])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 1, "existing `if bad then continue end` must be kept as-is:\n{}", block);
        assert!(is_continue_guard(&body[0]));
        // still guarded by `bad`, not `not bad`
        assert!(matches!(guard_condition(&body[0]), RValue::Local(l) if l == bad));
    }

    #[test]
    fn keeps_bare_break_guard() {
        let bad = local("bad");
        let mut block = Block(vec![gen_for(vec![If::new(
            lv(&bad),
            Block(vec![Break {}.into()]),
            Block::default(),
        )
        .into()])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 1, "trailing `if bad then break end` must be kept:\n{}", block);
        assert!(matches!(body[0].as_if().unwrap().then_block.lock().0.as_slice(), [Statement::Break(_)]));
    }

    #[test]
    fn refuses_goto_in_body() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("big"), Goto::new("L".into()).into()],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 1, "goto in body must block the transform:\n{}", block);
        assert!(!is_continue_guard(&body[0]));
    }

    #[test]
    fn fires_inside_nested_for_with_own_context() {
        let a = local("a");
        let b = local("b");
        // outer for whose body is just an inner for with a trailing guard
        let inner = gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("t1"), call_stmt("t2")],
        )]);
        let mut block = Block(vec![gen_for(vec![inner])]);

        recover_guard_continue(&mut block);

        // outer body: just the inner for (outer's last stmt is a loop, not an if -> untouched)
        let outer = loop_body(&block);
        assert_eq!(outer.len(), 1);
        let Statement::GenericFor(gf) = &outer[0] else {
            panic!("expected inner for");
        };
        let inner_body = gf.block.lock();
        assert_eq!(inner_body.0.len(), 4, "inner for guards recovered:\n{}", block);
        assert!(is_continue_guard(&inner_body.0[0]) && is_continue_guard(&inner_body.0[1]));
    }
}
