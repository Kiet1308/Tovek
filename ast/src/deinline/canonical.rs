//! Exact normalization of return/guard control flow before structural proof.
//!
//! These rules build owned canonical statements without mutating shared source
//! blocks. Tail position is explicit, and the allocation-free length check must
//! agree with the corresponding top-level normalization. Candidate selection,
//! window memoization, and search budgets remain outside this module.

use parking_lot::Mutex;
use triomphe::Arc;

use crate::{
    Assign, Binary, BinaryOperation, Block, GenericFor, If, LValue, LocalRw,
    NumericFor, RValue, RcLocal, Repeat, Return, Statement, Unary, UnaryOperation, While,
};

use super::{dprof, is_internal_marker, is_match_trivia, is_scalar_return_value};

pub(super) fn negate_canon(cond: RValue) -> RValue {
    match cond {
        RValue::Unary(u) if u.operation == UnaryOperation::Not => *u.value,
        RValue::Binary(b)
            if matches!(
                b.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            let operation = if b.operation == BinaryOperation::Equal {
                BinaryOperation::NotEqual
            } else {
                BinaryOperation::Equal
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

/// P9: EVERY condition has an exact boolean inverse for the §8 guard-polarity
/// flip. The Lua identity `if C then A else B ≡ if not C then B else A` holds for
/// ANY expression C — C is still evaluated exactly once, in the same place, with
/// the same short-circuit behaviour; only the branch order swaps. `negate_canon`
/// realises this inverse losslessly: it strips a leading `not`, swaps `==`/`~=`,
/// and for everything else (relational `< <= > >=`, `and`/`or`, calls, …) simply
/// WRAPS in `not`. That wrap is purely structural — it does NOT push the `not`
/// inward (no De Morgan) and does NOT turn `not (a < b)` into the NaN-unsafe
/// `a >= b`. The pattern side is already in this wrapped form (`unguard` applies
/// `negate_canon` to every guard condition unconditionally), so the wrapped
/// candidate condition unifies with it EXACTLY. Hence the flip is value-exact and
/// NaN-safe for all conditions, and gating it added nothing but missed matches —
/// so it now admits every condition.
pub(super) fn cond_exact_invertible(_c: &RValue) -> bool {
    true
}

pub(crate) fn canon(stmts: &[Statement]) -> Vec<Statement> {
    canon_tail(stmts, true)
}

/// `tail` = whether this block sits in the function's tail-control position, i.e.
/// a `return` here exits with *nothing* of the function left to run after it.
///
/// Stripping a trailing void `return` (N1) and folding guards (N3) are sound
/// ONLY at tail position. A `return` inside a non-tail nested block is non-local:
/// it skips not just the rest of *this* block but every statement after the
/// enclosing block too. Folding `if c then return end; REST` into
/// `if not c then REST end` there would wrongly let that outer continuation run
/// when the guard fires — conflating two different control flows. So below tail
/// position we leave returns intact; a surviving void return then trips the
/// `block_has_return` refusal gate (for patterns) / `plain_blocked` gate (for
/// candidates), keeping detection consistent and sound.
fn canon_tail(stmts: &[Statement], tail: bool) -> Vec<Statement> {
    let canonical = canon_recurse(canon_top(stmts, tail), tail);
    if tail { canonical } else { nest_value_return_guards(canonical) }
}

/// Below the function's tail a `return` still ends every path it is on, so
/// the statements after an `if` that returns a value on all its paths but
/// one belong on that path: `if a then return x end; if b then return y
/// end; REST` is `if a then return x elseif b then return y else REST end`.
/// A value helper's loop body returns in the first shape, its inlined copy
/// leaves through `break`s structured in the second (`match_value_loop`).
/// Blocks with a void `return` are left as written: the CPS matcher reads
/// their guards as they are.
fn nest_value_return_guards(stmts: Vec<Statement>) -> Vec<Statement> {
    fn returns(stmts: &[Statement], void: &mut bool, value: &mut bool) {
        for statement in stmts {
            match statement {
                Statement::Return(ret) if ret.values.is_empty() => *void = true,
                Statement::Return(_) => *value = true,
                Statement::If(branch) => {
                    returns(&branch.then_block.lock().0, void, value);
                    returns(&branch.else_block.lock().0, void, value);
                }
                _ => {}
            }
        }
    }
    let at = stmts.iter().position(|statement| {
        let Statement::If(branch) = statement else { return false };
        let (mut void, mut value) = (false, false);
        returns(std::slice::from_ref(statement), &mut void, &mut value);
        value && !void && is_open_guard(branch)
    });
    let Some(at) = at else { return stmts };
    if stmts[at + 1..].iter().all(is_match_trivia) {
        return stmts;
    }
    let mut stmts = stmts;
    let rest = nest_value_return_guards(stmts.split_off(at + 1));
    let guard = stmts.pop().expect("the guard");
    stmts.extend(graft(vec![guard], rest));
    stmts
}

/// The *top-level* (non-recursive) half of canon: the only edits that change the
/// statement count — N2 (drop `Empty`), and at tail N1 (drop a trailing void
/// `return`) and N3 (un-guard). It does NOT descend into nested blocks, so
/// `canon_top(w, tail).len() == canon_tail(w, tail).len()` exactly (the recursive
/// half is a 1:1 statement map). Callers use this to reject a candidate window by
/// length *before* paying for the deep nested-block rebuild in `canon_recurse`.
pub(super) fn canon_top(stmts: &[Statement], tail: bool) -> Vec<Statement> {
    // N2: drop Empty placeholders AND our own reconstruction markers (runtime
    // no-ops). An inner de-inline may have spliced a CALL_MARKER into a shared
    // callee body mid-fixed-point; dropping it here keeps a re-collected pattern
    // length-aligned with its candidate windows (both stripped symmetrically), and
    // a stripped marker contributes 0 anchors / does not perturb the length gates.
    let mut s: Vec<Statement> = stmts
        .iter()
        .filter(|st| {
            !matches!(st, Statement::Empty(_))
                && !matches!(st, Statement::Comment(c) if is_internal_marker(c))
        })
        .cloned()
        .collect();
    if tail {
        // N1: drop a trailing void return at THIS (tail) level (implicit-return no-op).
        if matches!(s.last(), Some(Statement::Return(r)) if r.values.is_empty()) {
            s.pop();
        }
        // N3: un-guard at THIS (tail) level — the guards' `[return]` then-blocks
        // are still intact here (we have NOT yet recursed into child blocks).
        s = unguard(s);
    }
    s
}

/// Replace `if c then x = a else x = b end` with `x = <select_value(c, a, b)>`,
/// the form SSA gives a register select when it can. Matching fuses it on both
/// sides, so a helper and its inlined copy agree whichever of them SSA fused.
fn fuse_assign_diamond(statement: &mut Statement) {
    fn single_store(block: &Block) -> Option<(RcLocal, RValue)> {
        let mut real = block.0.iter().filter(|s| !matches!(s, Statement::Empty(_)));
        match (real.next(), real.next()) {
            (Some(Statement::Assign(a)), None)
                if !a.prefix && !a.parallel && !a.compound && a.left.len() == 1 && a.right.len() == 1 =>
            {
                let LValue::Local(local) = &a.left[0] else { return None };
                Some((local.clone(), a.right[0].clone()))
            }
            _ => None,
        }
    }
    let Statement::If(f) = &*statement else {
        return;
    };
    let (Some((then_local, then_value)), Some((else_local, else_value))) =
        (single_store(&f.then_block.lock()), single_store(&f.else_block.lock()))
    else {
        return;
    };
    if then_local != else_local {
        return;
    }
    let mut condition = f.condition.clone();
    if let Some(value) = crate::select_value(&mut condition, then_value, else_value) {
        *statement = Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(then_local)],
            right: vec![value],
            prefix: false,
            parallel: false,
            compound: false,
        });
    }
}

/// `local u = E; return u or b` (or `u and b`) as `return E or b`: the step SSA
/// takes after fusing a select that read `u` twice into one that reads it
/// once, which is how the copy Luau inlined into a caller reads. `u` must be
/// read only there, first, and not by `E`. Nested blocks only: the top-level
/// statement count of a window is what the length gates measure.
fn fold_select_temp(stmts: &mut Vec<Statement>) {
    let real: Vec<usize> = (0..stmts.len()).filter(|&i| !is_match_trivia(&stmts[i])).collect();
    let [.., decl_at, ret_at] = real[..] else { return };
    let (Statement::Assign(decl), Statement::Return(ret)) = (&stmts[decl_at], &stmts[ret_at]) else {
        return;
    };
    let ([LValue::Local(temp)], [init], [RValue::Binary(select)]) =
        (decl.left.as_slice(), decl.right.as_slice(), ret.values.as_slice())
    else {
        return;
    };
    if !decl.prefix
        || decl.parallel
        || !matches!(select.operation, BinaryOperation::Or | BinaryOperation::And)
        || !matches!(select.left.as_ref(), RValue::Local(read) if read == temp)
        || select.right.any_local_read(&mut |read| read == temp)
        || init.any_local_read(&mut |read| read == temp)
        || !is_scalar_return_value(init)
    {
        return;
    }
    let folded = Binary::new(init.clone(), select.right.as_ref().clone(), select.operation).into();
    stmts[ret_at] = Statement::Return(Return { node_origin: Default::default(), values: vec![folded] });
    stmts.remove(decl_at);
}

/// Replace a trailing `if c then return a else return b end` (one scalar value
/// each) with `return <select_value(c, a, b)>`. One statement for one, so the
/// canonical length is unchanged.
fn fuse_return_diamond(stmts: &mut [Statement]) {
    // One value each: a scalar, or a call adjusted to one result
    // (`return (f())`), which the select reads as an operand.
    fn single_value(block: &Block) -> Option<RValue> {
        let mut real = block.0.iter().filter(|s| !matches!(s, Statement::Empty(_)));
        match (real.next(), real.next()) {
            (Some(Statement::Return(r)), None)
                if r.values.len() == 1
                    && (is_scalar_return_value(&r.values[0]) || matches!(r.values[0], RValue::Select(_))) =>
            {
                Some(r.values[0].clone())
            }
            _ => None,
        }
    }
    let Some(Statement::If(f)) = stmts.last() else {
        return;
    };
    let (Some(then_value), Some(else_value)) =
        (single_value(&f.then_block.lock()), single_value(&f.else_block.lock()))
    else {
        return;
    };
    let mut condition = f.condition.clone();
    if let Some(value) = crate::select_value(&mut condition, then_value, else_value) {
        *stmts.last_mut().unwrap() = Statement::Return(Return {
            node_origin: Default::default(),
            values: vec![value],
        });
    }
}

/// The recursive half of canon: canonicalise each statement's child blocks.
/// A 1:1 statement map, so it never changes the count produced by `canon_top`.
/// Tail-position only propagates to the LAST statement's branches (and never
/// into loop bodies, where a `return` exits across iterations + the loop).
pub(super) fn canon_recurse(s: Vec<Statement>, tail: bool) -> Vec<Statement> {
    let n = s.len();
    // Consume the owned Vec by value: a leaf statement (Assign/Call/Return/…) is
    // MOVED through untouched — no second deep clone — since `canon_top` already
    // produced this owned Vec. Only the block-bearing statements are rebuilt (their
    // child blocks are re-canon'd into fresh `Block`s; the shared original block Arc
    // must not be mutated). This halves the per-leaf clone cost in the hot matcher.
    let mut s: Vec<Statement> = s
        .into_iter()
        .enumerate()
        .map(|(j, st)| canon_children_owned(st, tail && j + 1 == n))
        .collect();
    // Select diamonds are matched in their `and`/`or` form, the one SSA gives
    // the copy Luau inlined into a caller where the value lands in a register
    // (`if c then return a end return b` ~ `v = c and a or b`). Post-order, so
    // a chain of them fuses inside out as SSA does. One statement for one:
    // `canon_top_len` still describes the result.
    for statement in &mut s {
        fuse_assign_diamond(statement); // N5, any level
    }
    if tail {
        fuse_return_diamond(&mut s); // N4, a trailing return diamond
    }
    s
}

fn canon_children_owned(s: Statement, tail: bool) -> Statement {
    let arm = |block: &Block| {
        let mut stmts = canon_tail(&block.0, tail);
        if tail {
            fold_select_temp(&mut stmts);
        }
        stmts
    };
    match s {
        Statement::If(f) => Statement::If(If::new(
            f.condition,
            Block(arm(&f.then_block.lock())),
            Block(arm(&f.else_block.lock())),
        )),
        Statement::While(w) => Statement::While(While::new(
            w.condition,
            Block(canon_tail(&w.block.lock().0, false)),
        )),
        Statement::Repeat(r) => Statement::Repeat(Repeat::new(
            r.condition,
            Block(canon_tail(&r.block.lock().0, false)),
        )),
        Statement::NumericFor(nf) => Statement::NumericFor(Box::new(NumericFor {
            block: Arc::new(Mutex::new(Block(canon_tail(&nf.block.lock().0, false)))),
            initial: nf.initial,
            limit: nf.limit,
            step: nf.step,
            counter: nf.counter,
        })),
        Statement::GenericFor(gf) => Statement::GenericFor(GenericFor {
            res_locals: gf.res_locals,
            right: gf.right,
            block: Arc::new(Mutex::new(Block(canon_tail(&gf.block.lock().0, false)))),
            origin: gf.origin,
        }),
        other => other,
    }
}

/// `canon_top` already cloned these statements, including their expression
/// origins. Consume that owned copy: cloning it again only repeats payload and
/// operand allocations (Origin::clone's `cloned` flag is already set). Child
/// block Arcs remain shared, so their early-return prefix still needs a clone.
pub(super) fn unguard(stmts: Vec<Statement>) -> Vec<Statement> {
    unguard_owned(stmts.into_iter())
}

fn unguard_owned(mut stmts: std::vec::IntoIter<Statement>) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::new();
    while let Some(statement) = stmts.next() {
        if let Statement::If(f) = &statement {
            // A guard may do work before returning:
            // `if cond then PREFIX; return [X] end; REST`.  Re-nest the shared
            // continuation into the exact structured form produced at inlined
            // sites: `if not cond then REST else PREFIX; return X end`.  For a
            // void return the terminal return is omitted; tail fall-through is
            // equivalent and `PREFIX` remains in the else arm.
            let guard: Option<(Vec<Statement>, Option<RValue>)> = {
                let then = f.then_block.lock();
                let els = f.else_block.lock();
                if els.0.is_empty() {
                    match then.0.split_last() {
                        Some((Statement::Return(r), prefix)) if r.values.is_empty() => {
                            Some((prefix.to_vec(), None))
                        }
                        Some((Statement::Return(r), prefix)) if r.values.len() == 1 => {
                            Some((prefix.to_vec(), Some(r.values[0].clone())))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            };
            let open = guard.is_none() && is_open_guard(f);
            if let Some((mut early_prefix, ret_val)) = guard {
                if !stmts.as_slice().is_empty() {
                    let Statement::If(f) = statement else { unreachable!() };
                    let cond = f.condition;
                    let folded = unguard_owned(stmts);
                    if let Some(x) = ret_val {
                        early_prefix.push(Statement::Return(Return { node_origin: Default::default(), values: vec![x] }));
                    }
                    out.push(Statement::If(If::new(
                        negate_canon(cond),
                        Block(folded),
                        Block(early_prefix),
                    )));
                    return out;
                }
            } else if open && !stmts.as_slice().is_empty() {
                let folded = unguard_owned(stmts);
                out.extend(graft(vec![statement], folded));
                return out;
            }
        }
        out.push(statement);
    }
    out
}

/// Paths through `stmts` that fall off its end (0 when every one returns), or
/// `None` for a leaf canon does not fold across: a multi-value return, or
/// control that leaves the block another way.
fn open_ends(stmts: &[Statement]) -> Option<usize> {
    match stmts.iter().rev().find(|s| !is_match_trivia(s)) {
        None => Some(1),
        Some(Statement::Return(r)) => (r.values.len() <= 1).then_some(0),
        Some(Statement::If(f)) => {
            let then = open_ends(&f.then_block.lock().0)?;
            let els = open_ends(&f.else_block.lock().0)?;
            Some(then + els)
        }
        Some(Statement::Break(_) | Statement::Continue(_) | Statement::Goto(_) | Statement::Label(_)) => None,
        Some(_) => Some(1),
    }
}

/// An `if` that returns on every path but one, beyond the single guard: what
/// follows it runs only on that path, so it belongs at that path's end
/// (`if a then return x elseif b then return y end REST` is `if a then return
/// x elseif b then return y else REST end`).
pub(super) fn is_open_guard(f: &If) -> bool {
    let then = open_ends(&f.then_block.lock().0);
    let els = open_ends(&f.else_block.lock().0);
    matches!((then, els), (Some(a), Some(b)) if a + b == 1)
}

/// `stmts` with `rest` placed at its one open end (see `open_ends`). Blocks
/// are shared, so a grafted `if` is rebuilt rather than changed in place.
pub(super) fn graft(mut stmts: Vec<Statement>, rest: Vec<Statement>) -> Vec<Statement> {
    if let Some(at) = stmts.iter().rposition(|s| !is_match_trivia(s))
        && let Statement::If(f) = &stmts[at]
    {
        let then = f.then_block.lock().0.clone();
        let els = f.else_block.lock().0.clone();
        let (then, els) = if open_ends(&then) == Some(1) {
            (graft(then, rest), els)
        } else {
            (then, graft(els, rest))
        };
        stmts[at] = If::new(f.condition.clone(), Block(then), Block(els)).into();
        return stmts;
    }
    stmts.extend(rest);
    stmts
}

/// The foldable-guard shape `unguard` collapses: `if cond then return [X] end`
/// (empty else, then-block a single 0-or-1-value return). Factored out so
/// `canon_top_len` computes the post-unguard length using the EXACT same predicate
/// `unguard` folds on (no drift); `canon_top_len`'s debug_assert cross-checks the
/// whole length against the real `canon_top`.
fn is_foldable_guard(s: &Statement) -> bool {
    if let Statement::If(f) = s {
        let guard = {
            let then = f.then_block.lock();
            let els = f.else_block.lock();
            els.0.is_empty()
                && matches!(then.0.last(), Some(Statement::Return(r)) if r.values.len() <= 1)
        };
        guard || is_open_guard(f)
    } else {
        false
    }
}

/// `canon_top(stmts, tail).len()` WITHOUT allocating the canon'd Vec — a hot-path
/// pre-filter so the per-width matcher loops pay the `canon_top` + `canon_recurse`
/// allocations only on a window whose canon'd length actually equals the pattern
/// length (the common case in the width scan is a NON-match). Mirrors `canon_top`
/// exactly: count non-trivia (N2); at tail, drop a trailing void return (N1) then
/// apply `unguard`'s length effect (N3 — the FIRST foldable guard that has a
/// following effective statement folds everything after it into one `If`, so the
/// length becomes that guard's index + 1; otherwise no change). The debug_assert
/// pins it to the real `canon_top` length in debug / test builds.
pub(super) fn canon_top_len(stmts: &[Statement], tail: bool) -> usize {
    let n = canon_top_len_of(stmts.iter(), tail);
    debug_assert_eq!(
        n,
        canon_top(stmts, tail).len(),
        "canon_top_len must mirror canon_top length exactly"
    );
    n
}

/// `canon_top_len` over statements that need not be contiguous (a callee
/// prefix followed by a region, without building the joined window).
pub(super) fn canon_top_len_of<'a, I>(stmts: I, tail: bool) -> usize
where
    I: DoubleEndedIterator<Item = &'a Statement> + Clone,
{
    dprof::inc(&dprof::CANON_TOP_LEN_CALLS, 1);
    crate::telemetry::count("canonical_top_length_calls", 1);
    let _t = dprof::T::new(&dprof::CANON_TOP_LEN_US);
    let total = stmts.clone().filter(|s| !is_match_trivia(s)).count();
    if !tail || total == 0 {
        return total;
    }
    // N1: drop a trailing void return (the LAST non-trivia statement).
    let last_void = stmts
        .clone()
        .rev()
        .find(|s| !is_match_trivia(s))
        .is_some_and(|s| matches!(s, Statement::Return(r) if r.values.is_empty()));
    let effective = total - usize::from(last_void);
    // N3: unguard folds at the first foldable guard that has a following
    // (within-`effective`) statement -> top-level length is its index + 1.
    let mut len = effective;
    for (idx, s) in stmts
        .filter(|s| !is_match_trivia(s))
        .take(effective)
        .enumerate()
    {
        if idx + 1 < effective && is_foldable_guard(s) {
            len = idx + 1;
            break;
        }
    }
    len
}
