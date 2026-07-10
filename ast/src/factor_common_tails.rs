//! Factor duplicated structured continuations out of conditional branches.
//!
//! Luau's inliner turns an early `return` from the callee into a jump to the
//! caller continuation.  When the CFG is converted back to a tree, that shared
//! continuation can be cloned into several branches.  Besides bloating output,
//! the clones prevent the de-inliner from seeing the original helper body.
//!
//! This pass performs the structured equivalent of CFG cross-jumping.  It is
//! deliberately exact: statements must be bit-for-bit structurally equal (local
//! identity included), and a branch/parent overlap is removed only when the
//! shared continuation is guaranteed to transfer control.

use crate::{
    deinline::{
        collect_declared_locals, unify_local, unify_lvalue, unify_rvalue, Bindings, MatchCtx,
    },
    Block, LValue, RValue, Statement, Traverse,
};
use parking_lot::Mutex;
use rustc_hash::FxHashSet;
use triomphe::Arc;

/// Remove exact duplicate continuations throughout `body`, including closures.
/// Returns whether the AST changed.
pub fn factor_common_tails(body: &mut Block) -> bool {
    unshare_blocks(body);
    factor_block(&mut body.0)
}

/// Give every statement owner a private nested-block tree before owner-local
/// rewrites. Luau inlining can leave shallow-cloned statements sharing these
/// Arcs; truncating one arm while it is shared would silently mutate siblings.
pub(crate) fn unshare_blocks(body: &mut Block) {
    for statement in &mut body.0 {
        match statement {
            Statement::If(node) => {
                ensure_unique(&mut node.then_block);
                ensure_unique(&mut node.else_block);
                unshare_blocks(&mut node.then_block.lock());
                unshare_blocks(&mut node.else_block.lock());
            }
            Statement::While(node) => {
                ensure_unique(&mut node.block);
                unshare_blocks(&mut node.block.lock());
            }
            Statement::Repeat(node) => {
                ensure_unique(&mut node.block);
                unshare_blocks(&mut node.block.lock());
            }
            Statement::NumericFor(node) => {
                ensure_unique(&mut node.block);
                unshare_blocks(&mut node.block.lock());
            }
            Statement::GenericFor(node) => {
                ensure_unique(&mut node.block);
                unshare_blocks(&mut node.block.lock());
            }
            _ => {}
        }
        for value in crate::deinline::stmt_rvalues_mut(statement) {
            unshare_rvalue(value);
        }
    }
}

fn ensure_unique(block: &mut Arc<Mutex<Block>>) {
    if Arc::strong_count(block) > 1 {
        let clone = crate::simplify_gotos::dc_block(&block.lock());
        *block = Arc::new(Mutex::new(clone));
    }
}

fn unshare_rvalue(value: &mut RValue) {
    if let RValue::Closure(closure) = value {
        unshare_blocks(&mut closure.function.0.lock().body);
        return;
    }
    for child in value.rvalues_mut() {
        unshare_rvalue(child);
    }
}

fn factor_block(stmts: &mut Vec<Statement>) -> bool {
    let mut changed = false;

    // Work bottom-up.  Shrinking an inner continuation often exposes a larger
    // common tail in its parent on the next local fixed-point iteration.
    for stmt in stmts.iter_mut() {
        changed |= factor_children(stmt);
    }

    loop {
        let Some(action) = find_action(stmts) else {
            break;
        };
        apply_action(stmts, action);
        changed = true;

        // A moved tail can itself contain conditionals.  It was already visited
        // in both source branches, so no recursive work is normally needed, but
        // re-running children here keeps the invariant obvious and handles a
        // newly adjacent parent continuation in one invocation.
        for stmt in stmts.iter_mut() {
            changed |= factor_children(stmt);
        }
    }

    changed
}

fn factor_children(stmt: &mut Statement) -> bool {
    let mut changed = match stmt {
        Statement::If(node) => {
            factor_block(&mut node.then_block.lock().0)
                | factor_block(&mut node.else_block.lock().0)
        }
        Statement::While(node) => factor_block(&mut node.block.lock().0),
        Statement::Repeat(node) => factor_block(&mut node.block.lock().0),
        Statement::NumericFor(node) => factor_block(&mut node.block.lock().0),
        Statement::GenericFor(node) => factor_block(&mut node.block.lock().0),
        _ => false,
    };

    // Closure bodies can occur in any expression position, including conditions,
    // table fields and call arguments.  `Traverse` keeps this exhaustive when a
    // new expression form is added.
    for value in crate::deinline::stmt_rvalues_mut(stmt) {
        changed |= factor_in_rvalue(value);
    }
    changed
}

fn factor_in_rvalue(value: &mut RValue) -> bool {
    if let RValue::Closure(closure) = value {
        return factor_block(&mut closure.function.0.lock().body.0);
    }
    let mut changed = false;
    for child in value.rvalues_mut() {
        changed |= factor_in_rvalue(child);
    }
    changed
}

#[derive(Clone, Copy, Debug)]
enum Action {
    /// Both arms end in the same sequence.  Move one copy after the `if`.
    MergeArms { at: usize, count: usize },
    /// One or both arms end in the terminal prefix immediately following the
    /// `if`.  Let those arms fall through to the parent copy instead.
    ReuseParent {
        at: usize,
        then_count: usize,
        else_count: usize,
    },
}

fn find_action(stmts: &[Statement]) -> Option<Action> {
    for at in (0..stmts.len()).rev() {
        let Statement::If(node) = &stmts[at] else {
            continue;
        };
        let then_block = node.then_block.lock();
        let else_block = node.else_block.lock();

        // Prefer an already-present parent continuation over manufacturing a
        // second adjacent copy by first merging the arm tails.
        let parent = &stmts[at + 1..];
        if !parent.is_empty() {
            let then_count = terminal_parent_overlap(&then_block.0, parent);
            let else_count = terminal_parent_overlap(&else_block.0, parent);
            if then_count > 0 || else_count > 0 {
                return Some(Action::ReuseParent {
                    at,
                    then_count,
                    else_count,
                });
            }
        }

        let common = common_suffix_len(&then_block.0, &else_block.0);
        if common > 0 {
            let then_prefix = &then_block.0[..then_block.0.len() - common];
            let else_prefix = &else_block.0[..else_block.0.len() - common];
            // A local declared before the shared tail is branch-scoped.  Moving
            // uses of it outside the branch would produce invalid Lua.  The
            // common compiler-generated continuations use hoisted locals, so this
            // conservative gate costs little recall while making scope safety
            // structural rather than heuristic.
            if !has_local_declaration(then_prefix) && !has_local_declaration(else_prefix) {
                return Some(Action::MergeArms { at, count: common });
            }
        }
    }
    None
}

fn apply_action(stmts: &mut Vec<Statement>, action: Action) {
    match action {
        Action::MergeArms { at, count } => {
            let moved = {
                let Statement::If(node) = &mut stmts[at] else {
                    unreachable!()
                };
                let mut then_block = node.then_block.lock();
                let mut else_block = node.else_block.lock();
                let split = then_block.0.len() - count;
                let moved = then_block.0.split_off(split);
                let else_len = else_block.0.len() - count;
                else_block.0.truncate(else_len);
                moved
            };
            stmts.splice(at + 1..at + 1, moved);
        }
        Action::ReuseParent {
            at,
            then_count,
            else_count,
        } => {
            let Statement::If(node) = &mut stmts[at] else {
                unreachable!()
            };
            if then_count > 0 {
                let mut block = node.then_block.lock();
                let len = block.0.len() - then_count;
                block.0.truncate(len);
            }
            if else_count > 0 {
                let mut block = node.else_block.lock();
                let len = block.0.len() - else_count;
                block.0.truncate(len);
            }
        }
    }
}

fn common_suffix_len(a: &[Statement], b: &[Statement]) -> usize {
    let limit = a.len().min(b.len());
    (1..=limit)
        .rev()
        .find(|&count| block_alpha_eq(&a[a.len() - count..], &b[b.len() - count..]))
        .unwrap_or(0)
}

/// Length of the branch suffix equal to a prefix of the parent continuation,
/// provided that equal prefix always transfers control.  We select the longest
/// terminal prefix so one rewrite removes as much duplicated code as possible.
fn terminal_parent_overlap(branch: &[Statement], parent: &[Statement]) -> usize {
    let limit = branch.len().min(parent.len());
    let mut best = 0;
    for count in 1..=limit {
        let branch_tail = &branch[branch.len() - count..];
        let parent_head = &parent[..count];
        if block_alpha_eq(branch_tail, parent_head) && sequence_terminates(parent_head) {
            best = count;
        }
    }
    best
}

fn sequence_terminates(stmts: &[Statement]) -> bool {
    let Some(last) = stmts
        .iter()
        .rev()
        .find(|stmt| !matches!(stmt, Statement::Empty(_) | Statement::Comment(_)))
    else {
        return false;
    };
    match last {
        Statement::Return(_) | Statement::Break(_) | Statement::Continue(_) => true,
        Statement::If(node) => {
            sequence_terminates(&node.then_block.lock().0)
                && sequence_terminates(&node.else_block.lock().0)
        }
        _ => false,
    }
}

fn has_local_declaration(stmts: &[Statement]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Statement::Assign(assign) if assign.prefix => assign
            .left
            .iter()
            .any(|left| matches!(left, LValue::Local(_))),
        _ => false,
    })
}

/// Alpha-equivalence for one continuation.  Only locals declared inside the
/// pattern continuation may be renamed; every external/upvalue local must retain
/// pointer identity.  The shared de-inline unifier supplies bit-exact literals,
/// injective local bindings and exhaustive expression matching.
pub(crate) fn block_alpha_eq(pattern: &[Statement], candidate: &[Statement]) -> bool {
    block_alpha_eq_impl(pattern, candidate, &FxHashSet::default())
}

/// Return the injective local-renaming proof so callers that relax identity for
/// selected destinations can validate the mapped candidate cells as well.
pub(crate) fn block_alpha_bindings_with_locals(
    pattern: &[Statement],
    candidate: &[Statement],
    localizable: &FxHashSet<crate::RcLocal>,
) -> Option<Bindings> {
    if pattern.len() != candidate.len() {
        return None;
    }
    let params = FxHashSet::default();
    let mut locals = FxHashSet::default();
    collect_declared_locals(pattern, &mut locals);
    locals.extend(localizable.iter().cloned());
    let ctx = MatchCtx {
        params: &params,
        locals: &locals,
    };
    let mut bindings = Bindings::default();
    alpha_block(pattern, candidate, &ctx, &mut bindings).then_some(bindings)
}

fn block_alpha_eq_impl(
    pattern: &[Statement],
    candidate: &[Statement],
    extra_locals: &FxHashSet<crate::RcLocal>,
) -> bool {
    block_alpha_bindings_with_locals(pattern, candidate, extra_locals).is_some()
}

fn alpha_block(
    pattern: &[Statement],
    candidate: &[Statement],
    ctx: &MatchCtx,
    bindings: &mut Bindings,
) -> bool {
    pattern.len() == candidate.len()
        && pattern
            .iter()
            .zip(candidate)
            .all(|(left, right)| alpha_stmt(left, right, ctx, bindings))
}

fn alpha_values(
    pattern: &[RValue],
    candidate: &[RValue],
    ctx: &MatchCtx,
    bindings: &mut Bindings,
) -> bool {
    pattern.len() == candidate.len()
        && pattern
            .iter()
            .zip(candidate)
            .all(|(left, right)| unify_rvalue(ctx, left, right, bindings).is_ok())
}

fn alpha_arc_eq(
    pattern: &Arc<Mutex<Block>>,
    candidate: &Arc<Mutex<Block>>,
    ctx: &MatchCtx,
    bindings: &mut Bindings,
) -> bool {
    if Arc::ptr_eq(pattern, candidate) {
        return true;
    }
    alpha_block(&pattern.lock().0, &candidate.lock().0, ctx, bindings)
}

fn alpha_stmt(a: &Statement, b: &Statement, ctx: &MatchCtx, bindings: &mut Bindings) -> bool {
    match (a, b) {
        (Statement::Empty(_), Statement::Empty(_)) => true,
        (Statement::Comment(x), Statement::Comment(y)) => x == y,
        (Statement::Call(x), Statement::Call(y)) => {
            unify_rvalue(ctx, &x.value, &y.value, bindings).is_ok()
                && alpha_values(&x.arguments, &y.arguments, ctx, bindings)
        }
        (Statement::MethodCall(x), Statement::MethodCall(y)) => {
            x.method == y.method
                && unify_rvalue(ctx, &x.value, &y.value, bindings).is_ok()
                && alpha_values(&x.arguments, &y.arguments, ctx, bindings)
        }
        (Statement::Assign(x), Statement::Assign(y)) => {
            x.prefix == y.prefix
                && x.parallel == y.parallel
                && x.left.len() == y.left.len()
                && x.left
                    .iter()
                    .zip(&y.left)
                    .all(|(left, right)| unify_lvalue(ctx, left, right, bindings).is_ok())
                && alpha_values(&x.right, &y.right, ctx, bindings)
        }
        (Statement::If(x), Statement::If(y)) => {
            unify_rvalue(ctx, &x.condition, &y.condition, bindings).is_ok()
                && alpha_arc_eq(&x.then_block, &y.then_block, ctx, bindings)
                && alpha_arc_eq(&x.else_block, &y.else_block, ctx, bindings)
        }
        (Statement::While(x), Statement::While(y)) => {
            unify_rvalue(ctx, &x.condition, &y.condition, bindings).is_ok()
                && alpha_arc_eq(&x.block, &y.block, ctx, bindings)
        }
        (Statement::Repeat(x), Statement::Repeat(y)) => {
            unify_rvalue(ctx, &x.condition, &y.condition, bindings).is_ok()
                && alpha_arc_eq(&x.block, &y.block, ctx, bindings)
        }
        (Statement::NumericFor(x), Statement::NumericFor(y)) => {
            unify_rvalue(ctx, &x.initial, &y.initial, bindings).is_ok()
                && unify_rvalue(ctx, &x.limit, &y.limit, bindings).is_ok()
                && unify_rvalue(ctx, &x.step, &y.step, bindings).is_ok()
                && unify_local(ctx, &x.counter, &y.counter, bindings).is_ok()
                && alpha_arc_eq(&x.block, &y.block, ctx, bindings)
        }
        (Statement::GenericFor(x), Statement::GenericFor(y)) => {
            x.res_locals.len() == y.res_locals.len()
                && x.res_locals
                    .iter()
                    .zip(&y.res_locals)
                    .all(|(left, right)| unify_local(ctx, left, right, bindings).is_ok())
                && alpha_values(&x.right, &y.right, ctx, bindings)
                && alpha_arc_eq(&x.block, &y.block, ctx, bindings)
        }
        (Statement::Return(x), Statement::Return(y)) => {
            alpha_values(&x.values, &y.values, ctx, bindings)
        }
        (Statement::Continue(_), Statement::Continue(_))
        | (Statement::Break(_), Statement::Break(_)) => true,
        (Statement::SetList(x), Statement::SetList(y)) => {
            unify_local(ctx, &x.object_local, &y.object_local, bindings).is_ok()
                && x.index == y.index
                && alpha_values(&x.values, &y.values, ctx, bindings)
                && match (&x.tail, &y.tail) {
                    (Some(left), Some(right)) => unify_rvalue(ctx, left, right, bindings).is_ok(),
                    (None, None) => true,
                    _ => false,
                }
        }
        // Low-level/goto/close nodes should not survive to this pass.  Refusing
        // equality is safer than cross-jumping across labels or close boundaries.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Global, If, Literal, RcLocal, Return};

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn call(name: &str) -> Statement {
        Statement::Call(Call::new(global(name), Vec::new()))
    }

    fn ret(name: &str) -> Statement {
        Statement::Return(Return::new(vec![global(name)]))
    }

    fn cond_if(then_stmts: Vec<Statement>, else_stmts: Vec<Statement>) -> Statement {
        Statement::If(If::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(then_stmts),
            Block(else_stmts),
        ))
    }

    #[test]
    fn merges_identical_arm_suffix() {
        let mut body = Block(vec![cond_if(
            vec![call("left"), call("shared")],
            vec![call("right"), call("shared")],
        )]);

        assert!(factor_common_tails(&mut body));
        assert_eq!(body.0.len(), 2);
        assert!(block_alpha_eq(&body.0[1..], &[call("shared")]));
        let Statement::If(node) = &body.0[0] else {
            panic!()
        };
        assert_eq!(node.then_block.lock().0.len(), 1);
        assert_eq!(node.else_block.lock().0.len(), 1);
    }

    #[test]
    fn reuses_terminal_parent_continuation() {
        let mut body = Block(vec![
            cond_if(vec![call("left"), ret("value")], vec![call("right")]),
            ret("value"),
        ]);

        assert!(factor_common_tails(&mut body));
        let Statement::If(node) = &body.0[0] else {
            panic!()
        };
        assert_eq!(node.then_block.lock().0.len(), 1);
        assert_eq!(body.0.len(), 2);
    }

    #[test]
    fn keeps_nonterminal_parent_overlap() {
        let original = cond_if(vec![call("left"), call("shared")], Vec::new());
        let mut body = Block(vec![original.clone(), call("shared")]);

        assert!(!factor_common_tails(&mut body));
        assert!(block_alpha_eq(&body.0[..1], &[original]));
    }

    #[test]
    fn keeps_branch_scoped_dependency_inside_arms() {
        let local = RcLocal::default();
        let declaration = Statement::Assign(Assign {
            left: vec![LValue::Local(local.clone())],
            right: vec![RValue::Literal(Literal::Number(1.0))],
            prefix: true,
            parallel: false,
        });
        let other_declaration = Statement::Assign(Assign {
            left: vec![LValue::Local(local.clone())],
            right: vec![RValue::Literal(Literal::Number(2.0))],
            prefix: true,
            parallel: false,
        });
        let use_local = Statement::Call(Call::new(global("print"), vec![RValue::Local(local)]));
        let mut body = Block(vec![cond_if(
            vec![declaration.clone(), use_local.clone()],
            vec![other_declaration, use_local],
        )]);

        assert!(!factor_common_tails(&mut body));
        assert_eq!(body.0.len(), 1);
    }

    #[test]
    fn reaches_fixed_point_through_nested_tail() {
        let inner = cond_if(vec![call("work"), ret("done")], Vec::new());
        let mut body = Block(vec![
            cond_if(vec![inner, ret("done")], vec![call("other"), ret("done")]),
            ret("done"),
        ]);

        assert!(factor_common_tails(&mut body));
        assert_eq!(body.0.len(), 2);
        let Statement::If(outer) = &body.0[0] else {
            panic!()
        };
        assert_eq!(outer.then_block.lock().0.len(), 1);
        assert_eq!(outer.else_block.lock().0.len(), 1);
    }

    #[test]
    fn alpha_does_not_map_hoisted_external_destinations() {
        let pattern_temp = RcLocal::default();
        let candidate_temp = RcLocal::default();
        let assign = |local: &RcLocal| {
            Statement::Assign(Assign {
                left: vec![LValue::Local(local.clone())],
                right: vec![global("source")],
                prefix: false,
                parallel: false,
            })
        };
        let use_temp = |local: &RcLocal| {
            Statement::Call(Call::new(global("use"), vec![RValue::Local(local.clone())]))
        };

        assert!(!block_alpha_eq(
            &[assign(&pattern_temp), use_temp(&pattern_temp)],
            &[assign(&candidate_temp), use_temp(&candidate_temp)],
        ));
        assert!(!block_alpha_eq(
            &[use_temp(&pattern_temp), assign(&pattern_temp)],
            &[use_temp(&candidate_temp), assign(&candidate_temp)],
        ));
    }

    #[test]
    fn unshares_arc_backed_blocks_before_owner_local_rewrite() {
        let shared = Arc::new(Mutex::new(Block(vec![call("work"), ret("done")])));
        let make_owner = || {
            let mut node = If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block::default(),
                Block::default(),
            );
            node.then_block = shared.clone();
            Statement::If(node)
        };
        let mut body = Block(vec![make_owner(), make_owner()]);

        unshare_blocks(&mut body);

        let (Statement::If(first), Statement::If(second)) = (&body.0[0], &body.0[1]) else {
            panic!()
        };
        assert!(!Arc::ptr_eq(&first.then_block, &second.then_block));
        assert_eq!(first.then_block.lock().0.len(), 2);
        assert_eq!(second.then_block.lock().0.len(), 2);
    }
}
