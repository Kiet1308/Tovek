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
//! * Adjacent guards with an exactly equal terminal action are folded into one
//!   `or` guard. `if a then return end; if b then return end` and
//!   `if a or b then return end` evaluate `a`, then evaluate `b` only when `a`
//!   is falsy, and perform the same terminal action. Runs are capped at four
//!   disjuncts so the readability rewrite cannot manufacture a huge condition.
//!
//! ## Placement (HARD invariant)
//!
//! This MUST run as the last **condition-changing** AST transform. A later
//! control-neutral cleanup may truncate unreachable statements/void tail
//! returns, but no later `reduce`/`reduce_condition` pass may touch the
//! manufactured `not (a < b)` and turn it into the NaN-unsafe `a >= b`.

use crate::{
    deinline::rvalue_exact_eq,
    expression_budget::guard_expression_cost as expression_cost,
    flatten_guards::{block_size, contains_goto_or_label, negate},
    Binary, BinaryOperation, Block, Continue, If, RValue, Statement, Traverse, Unary,
    UnaryOperation,
};

/// A single heavy predicate (e.g. a `string.find(...)` call) over an otherwise
/// trivial body is still worth turning into a guard. Calibrated against
/// `expression_cost` (a `Call`/`MethodCall` costs 8): ~24 ≈ "more than one call,
/// or a call plus a comparison", so a lone cheap `if a.b then ...` stays untouched
/// while a genuinely heavy single condition fires.
const COND_COST_THRESHOLD: usize = 24;

/// Keep folded guard conditions compact enough to scan at a glance. This is a
/// condition-leaf cap rather than a statement cap, so re-running the pass cannot
/// grow an already-folded four-way guard.
const MAX_MERGED_GUARD_DISJUNCTS: usize = 4;

/// Four syntactically tiny checks are readable on one line; two call-heavy
/// predicates often are not. The AST cost gate prevents guard folding from
/// trading vertical noise for an opaque, extremely long condition.
const MAX_MERGED_GUARD_COST: usize = 48;

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
    merge_adjacent_terminal_guards(&mut block.0);
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

/// Fold a maximal readable prefix of adjacent, one-sided guards that perform
/// exactly the same terminal action. The conditions remain in their original
/// left-to-right order, and `or` preserves the original conditional evaluation
/// of every later predicate (including side effects).
fn merge_adjacent_terminal_guards(body: &mut Vec<Statement>) {
    let input = std::mem::take(body);
    let mut output = Vec::with_capacity(input.len());
    let mut input = input.into_iter().peekable();

    while let Some(first) = input.next() {
        let Some(mut disjuncts) = guard_disjunct_count(&first) else {
            output.push(first);
            continue;
        };
        let mut merged_cost = guard_condition_cost(&first).unwrap();
        let mut run = Vec::with_capacity(MAX_MERGED_GUARD_DISJUNCTS);
        run.push(first);

        while let Some(next) = input.peek() {
            if !guards_have_same_terminal(&run[0], next) {
                break;
            }
            let Some(next_disjuncts) = guard_disjunct_count(next) else {
                break;
            };
            let next_cost = guard_condition_cost(next).unwrap();
            if disjuncts + next_disjuncts > MAX_MERGED_GUARD_DISJUNCTS
                || merged_cost.saturating_add(1).saturating_add(next_cost) > MAX_MERGED_GUARD_COST
            {
                break;
            }
            disjuncts += next_disjuncts;
            merged_cost += 1 + next_cost;
            run.push(input.next().unwrap());
        }

        if run.len() == 1 {
            output.push(run.pop().unwrap());
        } else {
            output.push(merge_guard_run(run));
        }
    }
    *body = output;
}

fn merge_guard_run(guards: Vec<Statement>) -> Statement {
    let mut guards = guards.into_iter();
    let Statement::If(first) = guards.next().unwrap() else {
        unreachable!()
    };
    let mut conditions = Vec::with_capacity(guards.size_hint().0 + 1);
    conditions.push(first.condition);
    let terminal = std::mem::take(&mut first.then_block.lock().0)
        .pop()
        .expect("mergeable guard must have one terminal statement");

    for guard in guards {
        let Statement::If(guard) = guard else {
            unreachable!()
        };
        conditions.push(guard.condition);
    }

    If::new(
        merge_guard_conditions(conditions),
        Block(vec![terminal]),
        Block::default(),
    )
    .into()
}

/// Prefer the positive-source shape `not (a and b)` when every guard predicate
/// can be inverted without introducing another wrapper. Otherwise retain the
/// direct `guard1 or guard2` form, which is always exact and usually clearer for
/// already-positive predicates such as `failedCheck()`.
fn merge_guard_conditions(conditions: Vec<RValue>) -> RValue {
    if conditions.iter().all(can_invert_guard_cleanly) {
        let mut iter = conditions.into_iter().map(negate);
        let first = iter.next().expect("a merged run has at least two guards");
        let conjunction = iter.fold(first, |left, right| {
            Binary::new(left, right, BinaryOperation::And).into()
        });
        Unary::new(conjunction, UnaryOperation::Not).into()
    } else {
        let mut iter = conditions.into_iter();
        let first = iter.next().expect("a merged run has at least two guards");
        iter.fold(first, |left, right| {
            Binary::new(left, right, BinaryOperation::Or).into()
        })
    }
}

fn can_invert_guard_cleanly(condition: &RValue) -> bool {
    matches!(
        condition,
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not
    ) || matches!(
        condition,
        RValue::Binary(binary)
            if matches!(binary.operation, BinaryOperation::Equal | BinaryOperation::NotEqual)
    )
}

fn guard_disjunct_count(statement: &Statement) -> Option<usize> {
    let Statement::If(r#if) = statement else {
        return None;
    };
    if !r#if.else_block.lock().0.is_empty() || !is_bare_terminator(&r#if.then_block.lock().0) {
        return None;
    }
    Some(or_disjunct_count(&r#if.condition))
}

fn guard_condition_cost(statement: &Statement) -> Option<usize> {
    let Statement::If(r#if) = statement else {
        return None;
    };
    Some(expression_cost(&r#if.condition))
}

fn or_disjunct_count(value: &RValue) -> usize {
    match value {
        RValue::Binary(binary) if binary.operation == BinaryOperation::Or => {
            or_disjunct_count(&binary.left)
                .saturating_add(or_disjunct_count(&binary.right))
                .min(MAX_MERGED_GUARD_DISJUNCTS + 1)
        }
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => {
            and_conjunct_count(&unary.value)
        }
        _ => 1,
    }
}

fn and_conjunct_count(value: &RValue) -> usize {
    match value {
        RValue::Binary(binary) if binary.operation == BinaryOperation::And => {
            and_conjunct_count(&binary.left)
                .saturating_add(and_conjunct_count(&binary.right))
                .min(MAX_MERGED_GUARD_DISJUNCTS + 1)
        }
        _ => 1,
    }
}

fn guards_have_same_terminal(left: &Statement, right: &Statement) -> bool {
    let (Statement::If(left), Statement::If(right)) = (left, right) else {
        return false;
    };
    if !right.else_block.lock().0.is_empty() {
        return false;
    }

    let left_then = left.then_block.lock();
    let right_then = right.then_block.lock();
    match (left_then.0.as_slice(), right_then.0.as_slice()) {
        ([Statement::Continue(_)], [Statement::Continue(_)])
        | ([Statement::Break(_)], [Statement::Break(_)]) => true,
        ([Statement::Return(left)], [Statement::Return(right)]) => {
            left.values.len() == right.values.len()
                && left
                    .values
                    .iter()
                    .zip(&right.values)
                    .all(|(left, right)| rvalue_exact_eq(left, right))
        }
        _ => false,
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
    use super::{or_disjunct_count, recover_guard_continue};
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
            && matches!(
                r#if.then_block.lock().0.as_slice(),
                [Statement::Continue(_)]
            )
    }

    fn guard_condition(stmt: &Statement) -> RValue {
        let Statement::If(r#if) = stmt else {
            panic!("not an if");
        };
        r#if.condition.clone()
    }

    fn contains_binary_operation(value: &RValue, operation: BinaryOperation) -> bool {
        match value {
            RValue::Binary(binary) => {
                binary.operation == operation
                    || contains_binary_operation(&binary.left, operation)
                    || contains_binary_operation(&binary.right, operation)
            }
            RValue::Unary(unary) => contains_binary_operation(&unary.value, operation),
            _ => false,
        }
    }

    #[test]
    fn folds_split_top_level_and_guards() {
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![gen_for(vec![if_then(
            and(lv(&a), lv(&b)),
            vec![call_stmt("f"), call_stmt("g")],
        )])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(
            body.len(),
            3,
            "one folded guard + two body statements:\n{}",
            block
        );
        assert!(is_continue_guard(&body[0]));
        assert!(matches!(guard_condition(&body[0]),
            RValue::Unary(u)
                if u.operation == UnaryOperation::Not
                    && matches!(&*u.value,
                        RValue::Binary(b) if b.operation == BinaryOperation::And)));
        assert!(matches!(&body[1], Statement::Call(_)));
        assert!(matches!(&body[2], Statement::Call(_)));
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
        assert_eq!(
            body.len(),
            3,
            "one or-guard + two body statements:\n{}",
            block
        );
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
        // Four disjuncts fit the readability cap and become one guard + body.
        assert_eq!(body.len(), 2, "one four-way guard + body:\n{}", block);
        assert!(is_continue_guard(&body[0]));
        let condition = guard_condition(&body[0]);
        assert!(matches!(condition,
            RValue::Binary(ref b) if b.operation == BinaryOperation::Or));
        assert!(contains_binary_operation(
            &condition,
            BinaryOperation::NotEqual
        ));
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
        assert_eq!(
            body.len(),
            3,
            "single guard (no or-split) + body:\n{}",
            block
        );
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
        assert_eq!(
            body.len(),
            3,
            "one folded guard + two body statements:\n{}",
            block
        );
        assert!(is_continue_guard(&body[0]));
        assert!(matches!(&body[1], Statement::Call(_)) && matches!(&body[2], Statement::Call(_)));
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
        let condition = guard_condition(&body[0]);
        assert!(matches!(condition,
            RValue::Unary(ref unary) if unary.operation == UnaryOperation::Not));
        assert!(contains_binary_operation(
            &condition,
            BinaryOperation::Equal
        ));
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
        assert_eq!(
            body.len(),
            1,
            "compact positive conjunction must stay untouched:\n{}",
            block
        );
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
        assert_eq!(
            body.len(),
            2,
            "negated conjunction collapses to one or-guard + body:\n{}",
            block
        );
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
        assert_eq!(body.0.len(), 3);
        assert!(is_continue_guard(&body.0[0]));
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
        assert_eq!(
            body.len(),
            3,
            "one folded guard + two body statements:\n{}",
            block
        );
        assert!(is_continue_guard(&body[0]));
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
        assert_eq!(
            body.len(),
            3,
            "one folded guard + prep + return:\n{}",
            block
        );
        assert!(is_continue_guard(&body[0]));
        assert!(matches!(&body[2], Statement::Return(_)));
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
        for guard in body.iter().take(1) {
            let Statement::If(r#if) = guard else {
                panic!("expected guard if");
            };
            assert!(
                matches!(
                    r#if.then_block.lock().0.as_slice(),
                    [Statement::Continue(_)]
                ),
                "continue must be the sole/last statement of its block (Luau rule):\n{}",
                block
            );
        }
    }

    #[test]
    fn merges_adjacent_exact_return_guards() {
        let a = local("a");
        let b = local("b");
        let result = local("result");
        let mut block = Block(vec![
            if_then(
                call_val("first"),
                vec![Return::new(vec![lv(&result)]).into()],
            ),
            if_then(
                call_val("second"),
                vec![Return::new(vec![lv(&result)]).into()],
            ),
        ]);

        recover_guard_continue(&mut block);

        assert_eq!(
            block.0.len(),
            1,
            "equal return guards should fold:\n{block}"
        );
        let Statement::If(guard) = &block.0[0] else {
            panic!("expected folded guard")
        };
        assert!(matches!(guard.condition,
            RValue::Binary(ref binary) if binary.operation == BinaryOperation::Or));
        assert!(matches!(guard.then_block.lock().0.as_slice(),
            [Statement::Return(ret)] if ret.values == vec![lv(&result)]));

        // Keep these locals alive in the constructed function and ensure local
        // identity does not accidentally affect condition folding.
        let _ = (a, b);
    }

    #[test]
    fn refuses_different_return_values() {
        let a = local("a");
        let b = local("b");
        let left = local("left");
        let right = local("right");
        let mut block = Block(vec![
            if_then(lv(&a), vec![Return::new(vec![lv(&left)]).into()]),
            if_then(lv(&b), vec![Return::new(vec![lv(&right)]).into()]),
        ]);

        recover_guard_continue(&mut block);

        assert_eq!(block.0.len(), 2, "different return values must not fold");
    }

    #[test]
    fn caps_merged_guard_at_four_disjuncts() {
        let conditions: Vec<_> = (0..5).map(|i| local(&format!("c{i}"))).collect();
        let guards = conditions
            .iter()
            .map(|condition| if_then(lv(condition), vec![Continue {}.into()]))
            .collect();
        let mut block = Block(vec![gen_for(guards)]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(
            body.len(),
            2,
            "four-way guard plus the fifth guard:\n{block}"
        );
        assert_eq!(or_disjunct_count(&guard_condition(&body[0])), 4);
        assert_eq!(or_disjunct_count(&guard_condition(&body[1])), 1);
    }

    #[test]
    fn canonical_not_and_guard_keeps_cap_on_second_pass() {
        let conditions: Vec<_> = (0..5).map(|i| local(&format!("c{i}"))).collect();
        let guards = conditions
            .iter()
            .map(|condition| if_then(not(lv(condition)), vec![Continue {}.into()]))
            .collect();
        let mut block = Block(vec![gen_for(guards)]);

        recover_guard_continue(&mut block);
        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(
            body.len(),
            2,
            "four-way guard plus the fifth guard:\n{block}"
        );
        assert_eq!(or_disjunct_count(&guard_condition(&body[0])), 4);
        assert_eq!(or_disjunct_count(&guard_condition(&body[1])), 1);
    }

    #[test]
    fn cost_cap_keeps_call_heavy_guard_runs_scannable() {
        let argument = local("argument");
        let guards = (0..4)
            .map(|index| {
                let condition = Call::new(
                    global(&format!("predicate{index}")),
                    vec![lv(&argument), lv(&argument), lv(&argument)],
                )
                .into();
                if_then(condition, vec![Continue {}.into()])
            })
            .collect();
        let mut block = Block(vec![gen_for(guards)]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(body.len(), 2, "cost cap should split the run:\n{block}");
        assert_eq!(or_disjunct_count(&guard_condition(&body[0])), 3);
        assert_eq!(or_disjunct_count(&guard_condition(&body[1])), 1);
    }

    // --- refusal cases ---

    #[test]
    fn refuses_trivial_one_liner() {
        let c = local("c");
        let mut block = Block(vec![gen_for(vec![if_then(lv(&c), vec![call_stmt("one")])])]);

        recover_guard_continue(&mut block);

        let body = loop_body(&block);
        assert_eq!(
            body.len(),
            1,
            "trivial guard must be left untouched:\n{}",
            block
        );
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
        assert!(
            !r#if.else_block.lock().0.is_empty(),
            "else branch must be preserved"
        );
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
        assert!(
            !is_continue_guard(&body[0]),
            "non-trailing if must be left intact:\n{}",
            block
        );
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
        assert!(
            !is_continue_guard(&block.0[0]),
            "must not synthesize continue outside a loop:\n{}",
            block
        );
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
        assert!(
            !is_continue_guard(&body[0]),
            "must not synthesize continue targeting a repeat:\n{}",
            block
        );
        // ...but the nested for must have its guards recovered
        let Statement::GenericFor(gf) = &body[1] else {
            panic!("expected nested for");
        };
        let inner = gf.block.lock();
        assert_eq!(
            inner.0.len(),
            3,
            "nested for inside repeat should still fire"
        );
        assert!(is_continue_guard(&inner.0[0]));
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
        assert_eq!(
            body.len(),
            1,
            "existing `if bad then continue end` must be kept as-is:\n{}",
            block
        );
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
        assert_eq!(
            body.len(),
            1,
            "trailing `if bad then break end` must be kept:\n{}",
            block
        );
        assert!(matches!(
            body[0].as_if().unwrap().then_block.lock().0.as_slice(),
            [Statement::Break(_)]
        ));
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
        assert_eq!(
            body.len(),
            1,
            "goto in body must block the transform:\n{}",
            block
        );
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
        assert_eq!(inner_body.0.len(), 3, "inner for guards recovered");
        assert!(is_continue_guard(&inner_body.0[0]));
    }
}
