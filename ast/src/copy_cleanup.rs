use std::collections::HashMap;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    inline_temps::{collect_usage, is_generated_temp, statement_writes_any_local, Usage},
    replace_locals::replace_locals,
    Block, LValue, LocalRw, RValue, RcLocal, SideEffects, Statement, Traverse,
};

/// Remove redundant local copies: `local dst = src` where `dst` is a generated
/// temporary that only aliases another local `src`. The declaration is deleted
/// and every read of `dst` is rewritten to `src` (§2.9 A).
///
/// `-O2` introduces these copies all over the corpus (e.g. FloorVfxLod's
/// `addRecordStats` has 19 `local vN = v9`). They are pure aliases: removing the
/// copy and substituting the source is value-identical because `RcLocal`
/// equality is id-based, so only the exact `dst` handle is rewritten.
///
/// The pass is deliberately conservative; a copy is collapsed only when every
/// gate in [`cleanup_once`] holds. The hard cases it must reject are the SWAP
/// idiom (`local v3 = v1; v1 = v2; v2 = v3`) and the STALE-COPY idiom
/// (`local dst = src; ...; src = nil; ... use dst`), both of which reassign
/// `src` while `dst` is still live and so are caught by the src-not-rewritten
/// gate.
pub fn copy_cleanup(block: &mut Block) {
    // Whole-program capture set, computed ONCE (the per-block usage recomputed
    // during recursion is blind to a closure that captures a local but lives in a
    // sibling/enclosing scope — the C10 family). Mirrors `inline_temps`.
    let captured: FxHashSet<RcLocal> = collect_usage(block)
        .into_iter()
        .filter(|(_, u)| u.captured)
        .map(|(l, _)| l)
        .collect();
    cleanup_in_block(block, &captured);
}

fn cleanup_in_block(block: &mut Block, captured: &FxHashSet<RcLocal>) {
    cleanup_nested_blocks(block, captured);
    while cleanup_once(block, captured) {}
}

/// Recurse into nested blocks and closures first (mirrors
/// `inline_temps::inline_single_use_temps`), so the fixpoint at every level only
/// has to consider its own statement list.
fn cleanup_nested_blocks(block: &mut Block, captured: &FxHashSet<RcLocal>) {
    for statement in &mut block.0 {
        cleanup_nested_in_statement(statement, captured);
    }
}

fn cleanup_nested_in_statement(statement: &mut Statement, captured: &FxHashSet<RcLocal>) {
    cleanup_closures_in_statement(statement, captured);
    match statement {
        Statement::If(r#if) => {
            cleanup_in_block(&mut r#if.then_block.lock(), captured);
            cleanup_in_block(&mut r#if.else_block.lock(), captured);
        }
        Statement::While(r#while) => cleanup_in_block(&mut r#while.block.lock(), captured),
        Statement::Repeat(repeat) => cleanup_in_block(&mut repeat.block.lock(), captured),
        Statement::NumericFor(numeric_for) => {
            cleanup_in_block(&mut numeric_for.block.lock(), captured)
        }
        Statement::GenericFor(generic_for) => {
            cleanup_in_block(&mut generic_for.block.lock(), captured)
        }
        _ => {}
    }
}

fn cleanup_closures_in_statement(statement: &mut Statement, captured: &FxHashSet<RcLocal>) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        cleanup_in_block(&mut function.lock().body, captured);
    }
}

fn cleanup_once(block: &mut Block, captured: &FxHashSet<RcLocal>) -> bool {
    let usage = collect_usage(block);
    for index in 0..block.0.len() {
        let Some((dst, src)) = candidate_copy(&block.0[index]) else {
            continue;
        };
        if !copy_is_removable(&dst, &src, &usage) {
            continue;
        }
        if src_written_after(block, index, &src) {
            continue;
        }
        // A captured `src` may be mutated by a closure invoked by a side-effecting
        // statement that sits BETWEEN the decl and a later read of `dst`; collapsing
        // `dst -> src` would then read the post-mutation value instead of the
        // snapshot (C10). The per-block `usage` is blind to a mutating closure in a
        // sibling/enclosing scope, so use the whole-program `captured` set. Made
        // window-aware so a captured `src` with NO intervening side effect (e.g. a
        // closure's own `local v = upvalue; print(v.x)`) still collapses.
        // `src_written_after` already covers DIRECT writes of `src`.
        if captured.contains(&src) && captured_src_mutated_before_use(block, index, &dst) {
            continue;
        }

        // Remove the declaration FIRST, then rewrite `dst -> src` across the
        // whole block (recurses into nested blocks/closures). The decl index is
        // not reused after the remove.
        block.0.remove(index);
        let mut map: FxHashMap<RcLocal, RcLocal> = FxHashMap::default();
        map.insert(dst, src);
        replace_locals(block, &map);
        return true;
    }
    false
}

/// Detect `local dst = src` where the RHS is a bare local. Returns `(dst, src)`.
fn candidate_copy(statement: &Statement) -> Option<(RcLocal, RcLocal)> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(dst) = &assign.left[0] else {
        return None;
    };
    let RValue::Local(src) = &assign.right[0] else {
        return None;
    };
    Some((dst.clone(), src.clone()))
}

/// Gates that depend only on usage counts and capture flags (everything except
/// the positional src-write check, which needs the statement window).
fn copy_is_removable(dst: &RcLocal, src: &RcLocal, usage: &HashMap<RcLocal, Usage, impl std::hash::BuildHasher>) -> bool {
    // 1. A self-copy `local v = v` carries no information; nothing to do (and
    //    rewriting `v -> v` would loop). Also it would never be `prefix`-real.
    if dst == src {
        return false;
    }
    // 2. Never collapse a meaningfully-named local — substituting it away would
    //    lose a real name the user wrote.
    if !is_generated_temp(dst) {
        return false;
    }
    let Some(dst_usage) = usage.get(dst) else {
        return false;
    };
    // 3. The decl must be the ONLY write to `dst` and there must be at least one
    //    read (otherwise it is dead, handled elsewhere, and `reads >= 1` keeps
    //    this pass focused on real aliases).
    if dst_usage.writes != 1 || dst_usage.reads < 1 {
        return false;
    }
    // 4. A captured `dst` is referenced by a closure we cannot see through with
    //    `replace_locals`-by-value reasoning here; reject it. (Per-block `usage`
    //    suffices: `dst` is a generated temp declared in THIS block, so any closure
    //    that captures it is reached by the same per-block walk.)
    if dst_usage.captured {
        return false;
    }
    // The captured-`src` hole (a closure mutating `src` in the live window) is now
    // handled window-aware in `cleanup_once` with the whole-program capture set.
    let _ = src;
    true
}

/// True when reading `local` is observable in `statement` directly OR inside a
/// nested control-flow block. `LocalRw::values_read` does NOT recurse into nested
/// blocks (`If::values_read` returns only the condition), so a plain
/// `values_read` scan would miss a read inside an `if`/`while`/`for` body. (A read
/// inside a CLOSURE means `local` is captured, which gate 4 of `copy_is_removable`
/// already rejects, so closures need no recursion here.)
fn reads_local_deep(statement: &Statement, local: &RcLocal) -> bool {
    if statement.values_read().iter().any(|r| *r == local) {
        return true;
    }
    let any = |block: &Block| block.0.iter().any(|s| reads_local_deep(s, local));
    match statement {
        Statement::If(r#if) => any(&r#if.then_block.lock()) || any(&r#if.else_block.lock()),
        Statement::While(r#while) => any(&r#while.block.lock()),
        Statement::Repeat(repeat) => any(&repeat.block.lock()),
        Statement::NumericFor(numeric_for) => any(&numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => any(&generic_for.block.lock()),
        _ => false,
    }
}

/// True when, for `local dst = src` at `decl_index`, a side-effecting statement
/// sits between the decl and the LAST read of `dst`. That is the window in which a
/// call could invoke a closure that mutates the (captured) `src` cell, so
/// collapsing `dst -> src` would read the mutated value instead of the snapshot.
fn captured_src_mutated_before_use(block: &Block, decl_index: usize, dst: &RcLocal) -> bool {
    // The last top-level statement that reads `dst`, directly OR in a nested block.
    let Some(bound) = (decl_index + 1..block.0.len())
        .rev()
        .find(|&i| reads_local_deep(&block.0[i], dst))
    else {
        return false;
    };
    // A side effect strictly before that statement is unsafe.
    if (decl_index + 1..bound).any(|i| block.0[i].has_side_effects()) {
        return true;
    }
    // If the read is NESTED inside a compound statement (not a direct top-level
    // read), a side effect earlier in that same statement could precede the read,
    // which the window above cannot see. Conservatively block when that statement
    // itself has a side effect.
    !block.0[bound].values_read().iter().any(|r| *r == dst)
        && block.0[bound].has_side_effects()
}

/// Gate 6 — anti-swap / anti-stale-copy: `src` must NOT be reassigned anywhere
/// after the decl. Recurses into If/While/Repeat/For blocks via
/// `statement_writes_any_local`. Given gate 5 (`!src.captured`) this
/// whole-remainder check is sound (a closure can no longer hide a write to
/// `src`).
fn src_written_after(block: &Block, decl_index: usize, src: &RcLocal) -> bool {
    let mut set = FxHashSet::default();
    set.insert(src.clone());
    block.0[decl_index + 1..]
        .iter()
        .any(|statement| statement_writes_any_local(statement, &set))
}

#[cfg(test)]
mod tests {
    use super::copy_cleanup;
    use crate::{
        Assign, Block, Call, Closure, Function, Global, If, Index, LValue, Literal, Local, RValue,
        RcLocal, Upvalue,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn declare(local: &RcLocal, value: RValue) -> crate::Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    fn assign(left: LValue, value: RValue) -> crate::Statement {
        Assign::new(vec![left], vec![value]).into()
    }

    fn print(value: RValue) -> crate::Statement {
        Call::new(global("print"), vec![value]).into()
    }

    fn closure_capturing(local: &RcLocal) -> RValue {
        RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
    }

    #[test]
    fn removes_copy_and_substitutes_source() {
        let src = local("v9");
        let dst = local("v2");
        let mut block = Block(vec![
            declare(&dst, local_value(&src)),
            print(RValue::Index(Index::new(local_value(&dst), string("floors")))),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 1);
        assert_eq!(block.to_string(), "print(v9.floors)");
    }

    #[test]
    fn does_not_collapse_self_copy() {
        // `local v9 = v9` is degenerate (dst == src); gate 1 must leave it alone
        // (rewriting `v9 -> v9` would also spin the fixpoint).
        let v = local("v9");
        let mut block = Block(vec![declare(&v, local_value(&v)), print(local_value(&v))]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 2);
        assert_eq!(block.to_string(), "local v9 = v9\nprint(v9)");
    }

    #[test]
    fn does_not_collapse_swap_triple() {
        // local v3 = v1; v1 = v2; v2 = v3 — `v1`/`v2`/`v3` form a swap; `v1` and
        // the copy source are reassigned in the live window, so gate 6 rejects.
        let v1 = local("v1");
        let v2 = local("v2");
        let v3 = local("v3");
        let mut block = Block(vec![
            declare(&v3, local_value(&v1)),
            assign(LValue::Local(v1.clone()), local_value(&v2)),
            assign(LValue::Local(v2.clone()), local_value(&v3)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(
            block.to_string(),
            "local v3 = v1\nv1 = v2\nv2 = v3"
        );
    }

    #[test]
    fn does_not_collapse_captured_destination() {
        let src = local("v9");
        let dst = local("v2");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&dst, local_value(&src)),
            declare(&handler, closure_capturing(&dst)),
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        // The decl must survive (dst captured).
        assert!(matches!(&block.0[0], crate::Statement::Assign(_)));
        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn does_not_collapse_captured_source() {
        // A captured `src` with a side-effecting statement BETWEEN the snapshot
        // and the read of `dst`: the call could invoke `handler` and mutate `v9`,
        // so collapsing `v2 -> v9` would read the post-mutation value (C10). The
        // decl must survive.
        let src = local("v9");
        let dst = local("v2");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&src)),
            declare(&dst, local_value(&src)),
            print(global("tick")), // intervening side effect (could call handler)
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 4);
        // dst decl (index 1) must survive.
        assert!(matches!(&block.0[1], crate::Statement::Assign(_)));
    }

    #[test]
    fn collapses_captured_source_without_intervening_call() {
        // A captured `src` but NO side effect between the snapshot and the read:
        // nothing can call `handler`, so `v9` cannot change and collapsing
        // `v2 -> v9` is value-identical. The decl is removed (window-aware C10).
        let src = local("v9");
        let dst = local("v2");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&src)),
            declare(&dst, local_value(&src)),
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 2);
        assert_eq!(block.0[1].to_string(), "print(v9)");
    }

    #[test]
    fn does_not_collapse_meaningfully_named_destination() {
        let src = local("v9");
        let result = local("result");
        let mut block = Block(vec![
            declare(&result, local_value(&src)),
            print(local_value(&result)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 2);
        assert_eq!(block.to_string(), "local result = v9\nprint(result)");
    }

    #[test]
    fn does_not_collapse_when_source_reassigned_after_decl() {
        // local v2 = src; src = "changed"; print(v2) — stale copy: `src` is
        // reassigned while `v2` is still live, so substituting would read the new
        // value. Gate 6 rejects.
        let src = local("src");
        let dst = local("v2");
        let mut block = Block(vec![
            declare(&dst, local_value(&src)),
            assign(LValue::Local(src.clone()), string("changed")),
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn rejects_source_reassigned_inside_nested_if() {
        // The src-write gate must recurse into control-flow blocks.
        let src = local("src");
        let dst = local("v2");
        let mut block = Block(vec![
            declare(&dst, local_value(&src)),
            If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![assign(LValue::Local(src.clone()), string("changed"))]),
                Block(vec![]),
            )
            .into(),
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 3);
    }

    #[test]
    fn does_not_collapse_captured_source_with_nested_read_after_side_effect() {
        // `dst` is read ONLY inside a nested `if`, with a side-effecting call
        // between the snapshot and the `if`. That call could invoke `handler` and
        // mutate the captured `v9`, so the copy must NOT collapse. Regression for
        // F2: `LocalRw::values_read` does not recurse into nested blocks, so the
        // window scan must use `reads_local_deep`.
        let src = local("v9");
        let dst = local("v2");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&src)),
            declare(&dst, local_value(&src)),
            print(global("tick")), // intervening side effect (could call handler)
            If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![print(local_value(&dst))]), // only read of dst, NESTED
                Block(vec![]),
            )
            .into(),
        ]);

        copy_cleanup(&mut block);

        // dst decl (index 1) must survive.
        assert!(matches!(&block.0[1], crate::Statement::Assign(_)));
    }

    #[test]
    fn collapses_copy_inside_nested_if() {
        // The pass recurses into nested blocks and collapses copies there.
        let src = local("v9");
        let dst = local("v2");
        let mut block = Block(vec![If::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![
                declare(&dst, local_value(&src)),
                print(RValue::Index(Index::new(local_value(&dst), string("floors")))),
            ]),
            Block(vec![]),
        )
        .into()]);

        copy_cleanup(&mut block);

        assert_eq!(
            block.to_string(),
            "if true then\n\tprint(v9.floors)\nend"
        );
    }

    #[test]
    fn collapses_copy_inside_closure_body() {
        // The pass recurses into closures.
        let src = local("v9");
        let dst = local("v2");
        let function = Arc::new(Mutex::new(Function::default()));
        function.lock().body = Block(vec![
            declare(&dst, local_value(&src)),
            print(RValue::Index(Index::new(local_value(&dst), string("floors")))),
        ]);
        let closure = RValue::Closure(Closure {
            function: ByAddress(function.clone()),
            upvalues: vec![Upvalue::Ref(src.clone())],
        });
        let holder = local("fn");
        let mut block = Block(vec![declare(&holder, closure)]);

        copy_cleanup(&mut block);

        assert_eq!(function.lock().body.to_string(), "print(v9.floors)");
    }

    #[test]
    fn collapses_chain_of_copies() {
        // local v2 = v9; local v3 = v2; print(v3.floors) -> print(v9.floors)
        let src = local("v9");
        let mid = local("v2");
        let dst = local("v3");
        let mut block = Block(vec![
            declare(&mid, local_value(&src)),
            declare(&dst, local_value(&mid)),
            print(RValue::Index(Index::new(local_value(&dst), string("floors")))),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.to_string(), "print(v9.floors)");
    }
}
