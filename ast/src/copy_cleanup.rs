use std::collections::{BTreeSet, HashMap};

use rustc_hash::{FxHashMap, FxHashSet};
#[cfg(test)]
use crate::inline_temps::collect_usage;

use crate::{
    analysis_session::{any_statement_bounded, AnalysisSession, ChangeSet},
    inline_temps::{is_generated_temp, Usage},
    replace_locals::replace_locals_in_statement,
    Block, LValue, LocalRw, RValue, RcLocal, Statement, Traverse,
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
    copy_cleanup_with_analysis(block, &mut AnalysisSession::new());
}

pub fn copy_cleanup_with_analysis(block: &mut Block, session: &mut AnalysisSession) {
    // Every rewrite starts at a `local dst = src` alias.
    match any_statement_bounded(block, &mut |statement| candidate_copy(statement).is_some()) {
        Some(true) => {}
        Some(false) => return,
        None => { session.refuse(); return; }
    }
    // Whole-program capture set, computed ONCE (the per-block usage recomputed
    // during recursion is blind to a closure that captures a local but lives in a
    // sibling/enclosing scope — the C10 family). Mirrors `inline_temps`.
    let Some((revision, mut captured)) = session.take_captures(block) else {
        session.refuse();
        return;
    };
    let mut changes = ChangeSet::capture_preserving();
    cleanup_in_block(block, &mut captured, true, &FxHashSet::default(), &mut changes);
    session.publish_captures(block, revision, captured, changes);
}

/// `upvalues`: the stable ids of the upvalues of the function `block` belongs
/// to; any other local it reads is one of its registers.
fn cleanup_in_block(block: &mut Block, captured: &mut FxHashSet<u64>, function_root: bool, upvalues: &FxHashSet<u64>, changes: &mut ChangeSet) {
    cleanup_nested_blocks(block, captured, upvalues, changes);
    cleanup_current_block(block, captured, function_root, upvalues, changes);
}

/// Alias substitution changes counts only for the two bindings. Preserve
/// source-order decisions with an ordered queue, and replace only statements
/// indexed under the removed binding, keeping positions stable until the end.
fn cleanup_current_block(block: &mut Block, captured: &mut FxHashSet<u64>, function_root: bool,
    upvalues: &FxHashSet<u64>, changes: &mut ChangeSet) {
    let mut pending: BTreeSet<_> = block.iter().enumerate()
        .filter_map(|(index, statement)| candidate_copy(statement).map(|_| index)).collect();
    if pending.is_empty() { return; }
    let mut usage = FxHashMap::<RcLocal, Usage>::default();
    let mut sites = FxHashMap::<RcLocal, BTreeSet<usize>>::default();
    let mut runtime_reads = FxHashMap::<RcLocal, BTreeSet<usize>>::default();
    let mut last_writes = FxHashMap::default();
    let mut declarations = FxHashMap::default();
    for (index, statement) in block.iter().enumerate() {
        let mut local_usage = FxHashMap::default();
        crate::inline_temps::collect_usage_in_statement(statement, &mut local_usage);
        for (local, counts) in local_usage {
            let total = usage.entry(local.clone()).or_default();
            total.reads += counts.reads;
            total.writes += counts.writes;
            total.captured |= counts.captured;
            sites.entry(local).or_default().insert(index);
        }
        index_runtime_sites(statement, index, &mut runtime_reads, &mut last_writes);
        if let Statement::Assign(assign) = statement && assign.prefix {
            for local in assign.left.iter().filter_map(LValue::as_local) {
                declarations.entry(local.clone()).or_insert(index);
            }
        }
    }
    let mut removed = vec![false; block.len()];
    while let Some(index) = pending.pop_first() {
        let Some((dst, src)) = candidate_copy(&block[index]) else { continue; };
        if !copy_is_removable(&dst, &src, &usage)
            || last_writes.get(&src).is_some_and(|&written| written > index) { continue; }
        let dst_was_captured = usage[&dst].captured;
        if dst_was_captured && (captured.contains(&src.stable_id())
            || (!function_root && !declarations.get(&src).is_some_and(|&at| at < index))) { continue; }
        if captured.contains(&src.stable_id())
            && captured_src_mutated_before_indexed_use(block, index, &dst, &runtime_reads,
                !upvalues.contains(&src.stable_id())) { continue; }

        changes.touch(&dst);
        changes.touch(&src);
        removed[index] = true;
        block[index] = crate::Empty {}.into();
        let affected = sites.remove(&dst).unwrap_or_default();
        let mut map = FxHashMap::default();
        map.insert(dst.clone(), src.clone());
        for &site in &affected {
            if site == index { continue; }
            replace_locals_in_statement(&mut block[site], &map);
            // Only aliases whose initializer changed can acquire a different
            // source-write/capture proof. All unrelated refusals remain valid.
            if candidate_copy(&block[site]).is_some() { pending.insert(site); }
        }
        let source_sites = sites.entry(src.clone()).or_default();
        source_sites.extend(affected);
        source_sites.remove(&index);
        let read_sites = runtime_reads.remove(&dst).unwrap_or_default();
        let source_reads = runtime_reads.entry(src.clone()).or_default();
        source_reads.extend(read_sites);
        source_reads.remove(&index);
        let destination = usage.remove(&dst).unwrap();
        let source = usage.entry(src.clone()).or_default();
        // Remove `local dst = src`, then redirect every remaining read of dst.
        source.reads = source.reads - 1 + destination.reads;
        source.captured |= destination.captured;
        last_writes.remove(&dst);
        declarations.remove(&dst);
        if let Some(&source_declaration) = declarations.get(&src) {
            pending.insert(source_declaration);
        }
        if dst_was_captured { captured.insert(src.stable_id()); }
    }
    let mut index = 0;
    block.0.retain(|_| { let keep = !removed[index]; index += 1; keep });
}

/// Position facts follow executed control-flow blocks, not closure bodies:
/// closure cell captures are direct reads, while execution of the body is
/// handled by the separate whole-function capture proofs above.
fn index_runtime_sites(
    statement: &Statement,
    index: usize,
    reads: &mut FxHashMap<RcLocal, BTreeSet<usize>>,
    writes: &mut FxHashMap<RcLocal, usize>,
) {
    statement.visit_local_reads(&mut |local| { reads.entry(local.clone()).or_default().insert(index); true });
    for local in statement.values_written() { writes.insert(local.clone(), index); }
    let mut child = |block: &Block| {
        for statement in &block.0 { index_runtime_sites(statement, index, reads, writes); }
    };
    match statement {
        Statement::If(node) => { child(&node.then_block.lock()); child(&node.else_block.lock()); }
        Statement::While(node) => child(&node.block.lock()),
        Statement::Repeat(node) => child(&node.block.lock()),
        Statement::NumericFor(node) => child(&node.block.lock()),
        Statement::GenericFor(node) => child(&node.block.lock()),
        _ => {}
    }
}

fn captured_src_mutated_before_indexed_use(
    block: &Block,
    decl_index: usize,
    dst: &RcLocal,
    reads: &FxHashMap<RcLocal, BTreeSet<usize>>,
    register: bool,
) -> bool {
    let Some(&bound) = reads.get(dst).and_then(BTreeSet::last).filter(|&&at| at > decl_index) else {
        return false;
    };
    block.0[decl_index + 1..bound].iter().any(crate::statement_is_observable)
        || reads_local_nested(&block[bound], dst)
        || !crate::evaluation_order::can_reuse_capture(&block[bound], dst, register)
}

/// Recurse into nested blocks and closures first (mirrors
/// `inline_temps::inline_single_use_temps`), so the fixpoint at every level only
/// has to consider its own statement list.
fn cleanup_nested_blocks(block: &mut Block, captured: &mut FxHashSet<u64>, upvalues: &FxHashSet<u64>, changes: &mut ChangeSet) {
    for statement in &mut block.0 {
        cleanup_nested_in_statement(statement, captured, upvalues, changes);
    }
}

fn cleanup_nested_in_statement(statement: &mut Statement, captured: &mut FxHashSet<u64>, upvalues: &FxHashSet<u64>, changes: &mut ChangeSet) {
    cleanup_closures_in_statement(statement, captured, changes);
    match statement {
        Statement::If(r#if) => {
            cleanup_in_block(&mut r#if.then_block.lock(), captured, false, upvalues, changes);
            cleanup_in_block(&mut r#if.else_block.lock(), captured, false, upvalues, changes);
        }
        Statement::While(r#while) => cleanup_in_block(&mut r#while.block.lock(), captured, false, upvalues, changes),
        Statement::Repeat(repeat) => cleanup_in_block(&mut repeat.block.lock(), captured, false, upvalues, changes),
        Statement::NumericFor(numeric_for) => {
            cleanup_in_block(&mut numeric_for.block.lock(), captured, false, upvalues, changes)
        }
        Statement::GenericFor(generic_for) => {
            cleanup_in_block(&mut generic_for.block.lock(), captured, false, upvalues, changes)
        }
        _ => {}
    }
}

fn cleanup_closures_in_statement(statement: &mut Statement, captured: &mut FxHashSet<u64>, changes: &mut ChangeSet) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            let upvalues = closure.upvalues.iter().map(|upvalue| {
                let (crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local)) = upvalue;
                local.stable_id()
            }).collect::<FxHashSet<_>>();
            functions.push((closure.function.clone(), upvalues));
        }
        None
    });
    for (function, upvalues) in functions {
        cleanup_in_block(&mut function.lock().body, captured, true, &upvalues, changes);
    }
}

#[cfg(test)]
fn cleanup_once(block: &mut Block, captured: &mut FxHashSet<u64>, function_root: bool) -> bool {
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
        let dst_was_captured = usage.get(&dst).is_some_and(|usage| usage.captured);
        // A captured alias is removable when its source was not already an
        // externally mutable cell and is never written after the snapshot.
        // Rewriting the closure's upvalue from `dst` to `src` then preserves the
        // exact stable binding (Thunderstorm's `local v3 = sound` pattern).
        if dst_was_captured && captured.contains(&src.stable_id()) {
            continue;
        }
        // A nested block cannot see writes in its enclosing continuation or on
        // an enclosing-loop back-edge. Only extend a captured destination's
        // lifetime onto `src` when either this is the whole function body (whose
        // suffix `src_written_after` scans), or `src` is a binding declared in
        // this exact lexical block before the snapshot. The latter is recreated
        // on every block activation and covers Thunderstorm's body-local
        // `sound`; it deliberately excludes outer loop cells and the snapshots
        // minted by `materialize_value_captures`.
        if dst_was_captured && !function_root && !source_declared_before(block, index, &src) {
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
        if captured.contains(&src.stable_id()) && captured_src_mutated_before_use(block, index, &dst) {
            continue;
        }

        // Remove the declaration FIRST, then rewrite `dst -> src` across the
        // whole block (recurses into nested blocks/closures). The decl index is
        // not reused after the remove.
        block.0.remove(index);
        let mut map: FxHashMap<RcLocal, RcLocal> = FxHashMap::default();
        map.insert(dst, src);
        crate::replace_locals::replace_locals(block, &map);
        if dst_was_captured {
            // The replacement made `src` captured. Thread this monotone fact
            // through the remaining fixed point so a later alias cannot treat
            // the newly captured cell as an ordinary local.
            captured.extend(map.into_values().map(|local| local.stable_id()));
        }
        return true;
    }
    false
}

#[cfg(test)]
fn source_declared_before(block: &Block, index: usize, source: &RcLocal) -> bool {
    block.0[..index].iter().any(|statement| {
        matches!(
            statement,
            Statement::Assign(assign)
                if assign.prefix
                    && assign
                        .left
                        .iter()
                        .any(|left| matches!(left, LValue::Local(local) if local == source))
        )
    })
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
fn copy_is_removable(
    dst: &RcLocal,
    src: &RcLocal,
    usage: &HashMap<RcLocal, Usage, impl std::hash::BuildHasher>,
) -> bool {
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
    // Captured destinations are decided in `cleanup_once`, which has both the
    // whole-function capture set and the positional source-write proof.
    let _ = src;
    true
}

/// True when reading `local` is observable in `statement` directly OR inside a
/// nested control-flow block. `LocalRw::values_read` does NOT recurse into nested
/// blocks (`If::values_read` returns only the condition), so a plain
/// `values_read` scan would miss a read inside an `if`/`while`/`for` body. (A read
/// inside a CLOSURE is represented by the separate capture gates, so closures
/// need no recursion here.)
fn reads_local_deep(statement: &Statement, local: &RcLocal) -> bool {
    if statement.any_local_read(&mut |read| read == local) {
        return true;
    }
    reads_local_nested(statement, local)
}

fn reads_local_nested(statement: &Statement, local: &RcLocal) -> bool {
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
#[cfg(test)]
fn captured_src_mutated_before_use(block: &Block, decl_index: usize, dst: &RcLocal) -> bool {
    // The last top-level statement that reads `dst`, directly OR in a nested block.
    let Some(bound) = (decl_index + 1..block.0.len())
        .rev()
        .find(|&i| reads_local_deep(&block.0[i], dst))
    else {
        return false;
    };
    // A side effect strictly before that statement is unsafe.
    if (decl_index + 1..bound).any(|i| crate::statement_is_observable(&block.0[i])) {
        return true;
    }
    // Check every read in the terminal statement, including reads after calls,
    // indexing and operator metamethods. Direct and nested reads can coexist;
    // the direct order model does not prove safety across a nested region.
    reads_local_nested(&block.0[bound], dst)
        || !crate::evaluation_order::can_reuse_capture(&block.0[bound], dst, true)
}

/// Gate 6 — anti-swap / anti-stale-copy: `src` must NOT be reassigned anywhere
/// after the decl. Recurses into If/While/Repeat/For blocks via
/// `statement_writes_any_local`. Given gate 5 (`!src.captured`) this
/// whole-remainder check is sound (a closure can no longer hide a write to
/// `src`).
#[cfg(test)]
fn src_written_after(block: &Block, decl_index: usize, src: &RcLocal) -> bool {
    let mut set = FxHashSet::default();
    set.insert(src.clone());
    block.0[decl_index + 1..]
        .iter()
        .any(|statement| crate::inline_temps::statement_writes_any_local(statement, &set))
}

#[cfg(test)]
mod tests {
    use super::copy_cleanup;
    use crate::{
        Assign, Block, Call, Closure, Function, Global, If, Index, LValue, Literal, Local, RValue,
        RcLocal, Upvalue, While,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    #[test]
    fn indexed_alias_cleanup_matches_full_rescans() {
        // Independently construct the two trees: substitution mutates shared
        // closure bodies/local metadata, so a shallow clone is not a reference.
        fn input(seed: u64) -> Block {
            let mut random = seed + 1;
            let mut next = || { random ^= random << 13; random ^= random >> 7; random ^= random << 17; random as usize };
            let sources = [local("source"), local("other")];
            let mut locals = sources.to_vec();
            let mut block = Block::default();
            for index in 0..30 {
                let dst = local(&format!("v{index}"));
                let src = locals[next() % locals.len()].clone();
                block.push(declare(&dst, src.clone().into()));
                match next() % 6 {
                    0 => block.push(print(dst.clone().into())),
                    1 => block.push(assign(src.clone().into(), global("replacement"))),
                    2 => block.push(print(closure_capturing(&dst))),
                    3 => block.push(If::new(global("flag"), Block(vec![print(dst.clone().into())]), Block::default()).into()),
                    4 => block.push(print(src.clone().into())),
                    _ => {}
                }
                locals.push(dst);
            }
            block.push(crate::Return::new(locals.into_iter().skip(2).map(RValue::from).collect()).into());
            block
        }
        for seed in 0..200 {
            for function_root in [false, true] {
                let mut expected = input(seed);
                let mut actual = input(seed);
                let captures = |block: &Block| super::collect_usage(block).into_iter()
                    .filter_map(|(local, usage)| usage.captured.then(|| local.stable_id())).collect();
                let mut expected_captures = captures(&expected);
                let mut actual_captures = captures(&actual);
                while super::cleanup_once(&mut expected, &mut expected_captures, function_root) {}
                super::cleanup_current_block(&mut actual, &mut actual_captures, function_root, &Default::default(), &mut super::ChangeSet::capture_preserving());
                assert_eq!(actual.to_string(), expected.to_string(), "seed {seed}, function root {function_root}");
            }
        }
    }

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
            node_origin: Default::default(),
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
            print(RValue::Index(Index::new(
                local_value(&dst),
                string("floors"),
            ))),
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

        assert_eq!(block.to_string(), "local v3 = v1\nv1 = v2\nv2 = v3");
    }

    #[test]
    fn does_not_collapse_captured_destination() {
        let src = local("v9");
        let dst = local("v2");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&dst, local_value(&src)),
            declare(&handler, closure_capturing(&dst)),
            assign(LValue::Local(src.clone()), string("changed")),
            print(local_value(&dst)),
        ]);

        copy_cleanup(&mut block);

        // The decl must survive (dst captured).
        assert!(matches!(&block.0[0], crate::Statement::Assign(_)));
        assert_eq!(block.0.len(), 4);
    }

    #[test]
    fn collapses_captured_destination_onto_stable_source() {
        let sound = local("sound");
        let snapshot = local("v3");
        let handler = local("handler");
        let mut block = Block(vec![
            declare(&snapshot, local_value(&sound)),
            declare(&handler, closure_capturing(&snapshot)),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 1);
        let crate::Statement::Assign(assign) = &block.0[0] else {
            panic!()
        };
        let RValue::Closure(closure) = &assign.right[0] else {
            panic!()
        };
        assert!(matches!(closure.upvalues.as_slice(), [Upvalue::Ref(local)] if local == &sound));
    }

    #[test]
    fn keeps_nested_captured_snapshot_before_enclosing_source_write() {
        let source = local("source");
        let snapshot = local("v3");
        let handler = local("handler");
        let branch = If::new(
            global("condition"),
            Block(vec![
                declare(&snapshot, local_value(&source)),
                declare(&handler, closure_capturing(&snapshot)),
            ]),
            Block::default(),
        );
        let branch_ref = branch.then_block.clone();
        let mut block = Block(vec![
            branch.into(),
            assign(LValue::Local(source), string("changed")),
        ]);

        copy_cleanup(&mut block);

        let branch = branch_ref.lock();
        assert_eq!(branch.0.len(), 2);
        assert!(matches!(&branch.0[0], crate::Statement::Assign(_)));
    }

    #[test]
    fn keeps_captured_snapshot_of_loop_mutated_outer_cell() {
        let source = local("source");
        let snapshot = local("v3");
        let handler = local("handler");
        let loop_ = While::new(
            global("condition"),
            Block(vec![
                assign(LValue::Local(source.clone()), global("nextValue")),
                declare(&snapshot, local_value(&source)),
                declare(&handler, closure_capturing(&snapshot)),
            ]),
        );
        let body_ref = loop_.block.clone();
        let mut block = Block(vec![loop_.into()]);

        copy_cleanup(&mut block);

        let body = body_ref.lock();
        assert_eq!(body.0.len(), 3);
        assert!(matches!(&body.0[1], crate::Statement::Assign(_)));
    }

    #[test]
    fn collapses_nested_captured_snapshot_of_same_block_binding() {
        let sound = local("sound");
        let snapshot = local("v3");
        let handler = local("handler");
        let branch = If::new(
            global("condition"),
            Block(vec![
                declare(&sound, global("newSound")),
                declare(&snapshot, local_value(&sound)),
                declare(&handler, closure_capturing(&snapshot)),
            ]),
            Block::default(),
        );
        let branch_ref = branch.then_block.clone();
        let mut block = Block(vec![branch.into()]);

        copy_cleanup(&mut block);

        let branch = branch_ref.lock();
        assert_eq!(branch.0.len(), 2);
        let crate::Statement::Assign(assign) = &branch.0[1] else {
            panic!()
        };
        let RValue::Closure(closure) = &assign.right[0] else {
            panic!()
        };
        assert!(matches!(closure.upvalues.as_slice(), [Upvalue::Ref(local)] if local == &sound));
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
            crate::Return::new(vec![local_value(&dst)]).into(),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.0.len(), 2);
        assert_eq!(block.0[1].to_string(), "return v9");
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
                print(RValue::Index(Index::new(
                    local_value(&dst),
                    string("floors"),
                ))),
            ]),
            Block(vec![]),
        )
        .into()]);

        copy_cleanup(&mut block);

        assert_eq!(block.to_string(), "if true then\n\tprint(v9.floors)\nend");
    }

    #[test]
    fn collapses_copy_inside_closure_body() {
        // The pass recurses into closures.
        let src = local("v9");
        let dst = local("v2");
        let function = Arc::new(Mutex::new(Function::default()));
        function.lock().body = Block(vec![
            declare(&dst, local_value(&src)),
            crate::Return::new(vec![RValue::Index(Index::new(
                local_value(&dst),
                string("floors"),
            ))]).into(),
        ]);
        let closure = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(function.clone()),
            upvalues: vec![Upvalue::Ref(src.clone())],
        });
        let holder = local("fn");
        let mut block = Block(vec![declare(&holder, closure)]);

        copy_cleanup(&mut block);

        assert_eq!(function.lock().body.to_string(), "return v9.floors");
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
            print(RValue::Index(Index::new(
                local_value(&dst),
                string("floors"),
            ))),
        ]);

        copy_cleanup(&mut block);

        assert_eq!(block.to_string(), "print(v9.floors)");
    }
}
