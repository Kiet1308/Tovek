use std::collections::HashMap;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    inline_temps::{collect_usage, is_generated_temp, statement_writes_any_local, Usage},
    replace_locals::replace_locals,
    Block, LValue, RValue, RcLocal, Statement, Traverse,
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
    cleanup_nested_blocks(block);
    while cleanup_once(block) {}
}

/// Recurse into nested blocks and closures first (mirrors
/// `inline_temps::inline_single_use_temps`), so the fixpoint at every level only
/// has to consider its own statement list.
fn cleanup_nested_blocks(block: &mut Block) {
    for statement in &mut block.0 {
        cleanup_nested_in_statement(statement);
    }
}

fn cleanup_nested_in_statement(statement: &mut Statement) {
    cleanup_closures_in_statement(statement);
    match statement {
        Statement::If(r#if) => {
            copy_cleanup(&mut r#if.then_block.lock());
            copy_cleanup(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => copy_cleanup(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => copy_cleanup(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => copy_cleanup(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => copy_cleanup(&mut generic_for.block.lock()),
        _ => {}
    }
}

fn cleanup_closures_in_statement(statement: &mut Statement) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        copy_cleanup(&mut function.lock().body);
    }
}

fn cleanup_once(block: &mut Block) -> bool {
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
    //    `replace_locals`-by-value reasoning here; reject it.
    if dst_usage.captured {
        return false;
    }
    // 5. MANDATORY: a captured `src` could be mutated by a closure that runs in
    //    the live window — invisible to `src_written_after` (which does not
    //    recurse into closure bodies). Rejecting captured `src` closes that hole.
    if usage.get(src).is_some_and(|u| u.captured) {
        return false;
    }
    true
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
        let src = local("v9");
        let dst = local("v2");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&handler, closure_capturing(&src)),
            declare(&dst, local_value(&src)),
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 3);
        // dst decl (index 1) must survive.
        assert!(matches!(&block.0[1], crate::Statement::Assign(_)));
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
