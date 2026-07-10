//! Eliminate redundant `x = nil` stores: SSA phi-node materialization
//! predeclares a local (`local x`, implicitly nil) and then explicitly stores
//! `x = nil` on every control-flow path where the value stays nil. Those stores
//! are dead — `x` is already nil there. This pass deletes a `x = nil` assignment
//! exactly when `x` is *provably* nil at that point, via a forward
//! "definitely-nil" dataflow over the structured AST.
//!
//! ```text
//! local main                  -- main is nil
//! if localPlayer then
//!     ...
//!     if playerGui then
//!         main = playerGui:FindFirstChild("Main")  -- main may be non-nil now
//!         if not (main and main:IsA("ScreenGui")) then
//!             main = nil       -- KEPT: main was just assigned non-nil
//!         end
//!     else
//!         main = nil           -- DELETED: main is provably still nil here
//!     end
//! else
//!     main = nil               -- DELETED
//! end
//! ```
//!
//! ## Soundness
//! The tracked set `nil` is a MUST-analysis: a local is in it only when it is nil
//! on *every* path reaching the current point (intersection at `if` merges). We
//! only ever under-approximate "definitely nil"; deleting `x = nil` when
//! `x ∈ nil` removes a provable no-op store. The hazards and their conservative
//! rules:
//! * A closure capturing `x` by `Upvalue::Ref` can mutate the parent cell via a
//!   call we cannot see through, so any *captured* local (Copy or Ref) is
//!   excluded entirely and never tracked or deleted. The exclusion set is
//!   computed over the WHOLE tree up front (`collect_usage` recurses into every
//!   nested block and closure), so a capture textually later than a store is
//!   still seen.
//! * A loop body runs 0+ times with loop-carried state, so its body is analysed
//!   from an EMPTY incoming set (assume nothing at the loop head) and, after the
//!   loop, every local the body could write — *including* the for-loop control
//!   locals, which the construct writes — is dropped from `nil`.
//! * Unstructured `goto`/`::label::` breaks straight-line reasoning, so a block
//!   containing either is "bailed": no deletion happens at that level and the
//!   block's effect on the caller's `nil` is the conservative `nil − written`
//!   (so an `if`-arm that bails cannot leave a stale "definitely nil" claim for
//!   the merge to trust). The corpus currently emits no goto/label, but the
//!   guard keeps the pass sound if one ever survives.
//! * Only a bare-`Local` LHS with a literal-`nil` RHS, non-`prefix`,
//!   non-`parallel`, single target, is ever deleted. `prefix` declarations,
//!   `t.x = nil`/`_G.x = nil`, and parallel/multi-target assigns are never
//!   deleted (only treated as kills of their written locals).
//!
//! A single forward pass suffices: deleting a redundant `x = nil` does not change
//! `x`'s nil-status (it was already nil), so it can never make a *later* store
//! newly redundant — no fixpoint iteration is required.

use parking_lot::Mutex;
use rustc_hash::FxHashSet;
use triomphe::Arc;

use crate::{
    inline_temps::collect_usage, Assign, Block, LValue, Literal, LocalRw, RValue, RcLocal,
    Statement, Traverse,
};

/// Delete redundant `x = nil` stores throughout `block`, its nested blocks, and
/// its closures. See the module docs for the soundness argument.
pub fn eliminate_redundant_nil(block: &mut Block) {
    let usage = collect_usage(block);
    let excluded: FxHashSet<RcLocal> = usage
        .into_iter()
        .filter(|(_, u)| u.captured)
        .map(|(local, _)| local)
        .collect();
    let mut nil = FxHashSet::default();
    walk(&mut block.0, &mut nil, &excluded);
}

/// Whether a statement list contains an unstructured jump at its own level.
fn contains_goto_or_label(statements: &[Statement]) -> bool {
    statements
        .iter()
        .any(|s| matches!(s, Statement::Goto(_) | Statement::Label(_)))
}

/// Recurse into every closure embedded directly in `statement`'s rvalues. Each
/// closure body is an independent scope, analysed from an empty `nil` set.
/// (`Closure`'s `Traverse` impl does not descend into its body, so a closure
/// nested inside another closure is reached by that body's own walk — no
/// double-processing.)
fn clean_closures(statement: &mut Statement, excluded: &FxHashSet<RcLocal>) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        let mut nil = FxHashSet::default();
        walk(&mut function.lock().body.0, &mut nil, excluded);
    }
}

/// Collect every bare local written anywhere in `statements` (recursively,
/// including nested loop control locals, which `values_written` surfaces for the
/// loop construct). Used to conservatively kill `nil` entries after a loop or a
/// bailed block.
fn collect_written(statements: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for statement in statements {
        for local in statement.values_written() {
            out.insert(local.clone());
        }
        match statement {
            Statement::If(r#if) => {
                collect_written(&r#if.then_block.lock().0, out);
                collect_written(&r#if.else_block.lock().0, out);
            }
            Statement::While(r#while) => collect_written(&r#while.block.lock().0, out),
            Statement::Repeat(repeat) => collect_written(&repeat.block.lock().0, out),
            Statement::NumericFor(nf) => collect_written(&nf.block.lock().0, out),
            Statement::GenericFor(gf) => collect_written(&gf.block.lock().0, out),
            _ => {}
        }
    }
}

/// Walk a loop body from an EMPTY incoming set (loop-carried state is unknown at
/// the head — deleting based on the outer set would be unsound across a
/// back-edge), then drop every local the body could write, plus the loop's own
/// `control` locals, from the caller's `nil`.
fn walk_loop_body(
    block: &Arc<Mutex<Block>>,
    control: &[RcLocal],
    nil: &mut FxHashSet<RcLocal>,
    excluded: &FxHashSet<RcLocal>,
) {
    let mut body = block.lock();
    let mut body_nil = FxHashSet::default();
    walk(&mut body.0, &mut body_nil, excluded);
    let mut written = FxHashSet::default();
    collect_written(&body.0, &mut written);
    for local in control {
        written.insert(local.clone());
    }
    nil.retain(|local| !written.contains(local));
}

/// Independently clean a statement's nested blocks (each from an empty set),
/// without threading any `nil` state. Used only on the bail path.
fn clean_child_blocks(statement: &mut Statement, excluded: &FxHashSet<RcLocal>) {
    let mut empty = FxHashSet::default();
    match statement {
        Statement::If(r#if) => {
            walk(&mut r#if.then_block.lock().0, &mut empty, excluded);
            empty.clear();
            walk(&mut r#if.else_block.lock().0, &mut empty, excluded);
        }
        Statement::While(r#while) => walk(&mut r#while.block.lock().0, &mut empty, excluded),
        Statement::Repeat(repeat) => walk(&mut repeat.block.lock().0, &mut empty, excluded),
        Statement::NumericFor(nf) => walk(&mut nf.block.lock().0, &mut empty, excluded),
        Statement::GenericFor(gf) => walk(&mut gf.block.lock().0, &mut empty, excluded),
        _ => {}
    }
}

/// The redundant-store predicate: a non-`prefix`, non-`parallel`, single-target
/// `x = nil` whose target is a tracked (non-excluded) definitely-nil local.
fn is_redundant_nil_store(
    assign: &Assign,
    nil: &FxHashSet<RcLocal>,
    excluded: &FxHashSet<RcLocal>,
) -> bool {
    if assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return false;
    }
    let LValue::Local(target) = &assign.left[0] else {
        return false;
    };
    matches!(&assign.right[0], RValue::Literal(Literal::Nil))
        && !excluded.contains(target)
        && nil.contains(target)
}

/// Apply a (non-deleted) assignment's effect on the definitely-nil set.
fn apply_assign(assign: &Assign, nil: &mut FxHashSet<RcLocal>, excluded: &FxHashSet<RcLocal>) {
    if assign.prefix {
        // `local a, b` (no initializers) — every declared local starts nil.
        if assign.right.is_empty() {
            for lvalue in &assign.left {
                if let LValue::Local(local) = lvalue {
                    if !excluded.contains(local) {
                        nil.insert(local.clone());
                    }
                }
            }
            return;
        }
        // `local a, b = <rhs...>` — a local is known-nil only when its OWN slot is
        // a literal nil. A multi-value tail (`local a, b = f()`, right.len() == 1)
        // leaves the extra slots UNKNOWN (they take f()'s returns, not nil), so
        // only an explicit `Some(Literal::Nil)` adds.
        for (i, lvalue) in assign.left.iter().enumerate() {
            if let LValue::Local(local) = lvalue {
                if matches!(assign.right.get(i), Some(RValue::Literal(Literal::Nil)))
                    && !excluded.contains(local)
                {
                    nil.insert(local.clone());
                } else {
                    nil.remove(local);
                }
            }
        }
        return;
    }

    // Non-prefix single store. A `x = nil` that reaches here was NOT redundant
    // (x ∉ nil), so x becomes nil now; a non-nil RHS clears it.
    if !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1 {
        if let LValue::Local(local) = &assign.left[0] {
            if matches!(&assign.right[0], RValue::Literal(Literal::Nil)) {
                if !excluded.contains(local) {
                    nil.insert(local.clone());
                }
            } else {
                nil.remove(local);
            }
        }
        // An Index/Global LHS writes no bare local — nil is unchanged.
        return;
    }

    // Parallel / multi-target: conservatively clear every bare local written.
    for lvalue in &assign.left {
        if let LValue::Local(local) = lvalue {
            nil.remove(local);
        }
    }
}

/// Forward "definitely-nil" walk over `statements`, threading `nil` and deleting
/// redundant `x = nil` stores in place. Recurses into nested blocks and closures.
fn walk(
    statements: &mut Vec<Statement>,
    nil: &mut FxHashSet<RcLocal>,
    excluded: &FxHashSet<RcLocal>,
) {
    // Unstructured control flow at this level: clean nested blocks/closures
    // independently but do not delete or track here. Report the conservative
    // `nil − written` to the caller so an `if`-merge above cannot trust a stale
    // claim for a local this block may have reassigned non-nil.
    if contains_goto_or_label(statements) {
        let mut written = FxHashSet::default();
        collect_written(statements, &mut written);
        for statement in statements.iter_mut() {
            clean_closures(statement, excluded);
            clean_child_blocks(statement, excluded);
        }
        nil.retain(|local| !written.contains(local));
        return;
    }

    let mut index = 0;
    while index < statements.len() {
        clean_closures(&mut statements[index], excluded);

        // Decide deletion with a shared borrow that ends before the `remove`.
        let delete = matches!(
            &statements[index],
            Statement::Assign(assign) if is_redundant_nil_store(assign, nil, excluded)
        );
        if delete {
            statements.remove(index);
            // `x` is still nil; `nil` is unchanged. Do not advance — the next
            // statement now occupies `index`.
            continue;
        }

        match &mut statements[index] {
            Statement::Assign(assign) => apply_assign(assign, nil, excluded),
            Statement::If(r#if) => {
                let mut then_nil = nil.clone();
                walk(&mut r#if.then_block.lock().0, &mut then_nil, excluded);
                let mut else_nil = nil.clone();
                walk(&mut r#if.else_block.lock().0, &mut else_nil, excluded);
                // Merge = intersection: definitely-nil after the `if` only if
                // definitely-nil on BOTH arms. This only shrinks `nil`, so it can
                // never wrongly mark a local nil.
                nil.retain(|local| then_nil.contains(local) && else_nil.contains(local));
            }
            Statement::While(r#while) => walk_loop_body(&r#while.block, &[], nil, excluded),
            Statement::Repeat(repeat) => walk_loop_body(&repeat.block, &[], nil, excluded),
            Statement::NumericFor(nf) => {
                let control = [nf.counter.clone()];
                walk_loop_body(&nf.block, &control, nil, excluded);
            }
            Statement::GenericFor(gf) => {
                let control = gf.res_locals.clone();
                walk_loop_body(&gf.block, &control, nil, excluded);
            }
            // Only Assign and loops write bare locals; this is belt-and-braces.
            other => {
                for local in other.values_written() {
                    nil.remove(local);
                }
            }
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::eliminate_redundant_nil;
    use crate::{
        Assign, Block, Call, Closure, Function, Global, If, Index, LValue, Label, Literal, Local,
        MethodCall, NumericFor, RValue, RcLocal, Statement, Upvalue, While,
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

    fn nil() -> RValue {
        RValue::Literal(Literal::Nil)
    }

    /// `local x` — bare predeclaration (prefix, empty RHS).
    fn declare_bare(local: &RcLocal) -> Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![]);
        assign.prefix = true;
        assign.into()
    }

    /// `x = value` — a non-prefix store.
    fn store(local: &RcLocal, value: RValue) -> Statement {
        Assign::new(vec![LValue::Local(local.clone())], vec![value]).into()
    }

    fn print(value: RValue) -> Statement {
        Call::new(global("print"), vec![value]).into()
    }

    fn if_stmt(condition: RValue, then_block: Block, else_block: Block) -> Statement {
        If::new(condition, then_block, else_block).into()
    }

    fn closure_capturing(local: &RcLocal) -> RValue {
        RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(local.clone())],
        })
    }

    /// The motivating shape (InitNpcQuest::ensureAdhesiveCraftingInitialize): the
    /// two `else: main = nil` arms are redundant and removed; the inner
    /// `main = nil` after `main = ...:FindFirstChild("Main")` is KEPT.
    #[test]
    fn removes_redundant_else_nil_keeps_post_assign_nil() {
        let main = local("main");
        let inner_if = if_stmt(
            global("notValid"),
            Block(vec![store(&main, nil())]), // KEPT: main was just assigned
            Block(vec![]),
        );
        let then_inner = if_stmt(
            global("playerGui"),
            Block(vec![
                store(
                    &main,
                    RValue::MethodCall(MethodCall::new(
                        global("playerGui"),
                        "FindFirstChild".to_string(),
                        vec![string("Main")],
                    )),
                ),
                inner_if,
            ]),
            Block(vec![store(&main, nil())]), // DELETED
        );
        let mut block = Block(vec![
            declare_bare(&main),
            if_stmt(
                global("localPlayer"),
                Block(vec![then_inner]),
                Block(vec![store(&main, nil())]), // DELETED
            ),
            print(RValue::Local(main.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        let text = block.to_string();
        // Exactly one `main = nil` survives (the inner, post-assignment one).
        assert_eq!(text.matches("main = nil").count(), 1, "\n{text}");
        // Both `else` arms are now empty.
        assert!(
            !text.contains("else\n\tmain = nil") && !text.contains("else\n\t\tmain = nil"),
            "\n{text}"
        );
    }

    /// A plain `local x; x = nil` is redundant and removed.
    #[test]
    fn removes_simple_redundant_nil() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            store(&x, nil()),
            print(RValue::Local(x.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string(), "local x\nprint(x)");
    }

    /// Two consecutive redundant stores are both removed (delete must not skip the
    /// statement that slides into the freed index).
    #[test]
    fn removes_consecutive_redundant_nils() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            store(&x, nil()),
            store(&x, nil()),
            print(RValue::Local(x.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string(), "local x\nprint(x)");
    }

    /// `x = nil` is KEPT when an `if` arm assigned `x` non-nil before it (the merge
    /// must not consider x definitely-nil).
    #[test]
    fn keeps_nil_after_branch_assigns_non_nil() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            if_stmt(
                global("c"),
                Block(vec![store(&x, global("value"))]),
                Block(vec![]),
            ),
            store(&x, nil()),
            print(RValue::Local(x.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("x = nil").count(), 1);
    }

    /// Loop back-edge: `x = nil` at the top of a body that later reassigns `x`
    /// non-nil must be KEPT (on iterations 2+ x is the prior tail value, not nil).
    #[test]
    fn keeps_nil_at_loop_head_with_back_edge() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            While::new(
                global("c"),
                Block(vec![
                    store(&x, nil()),
                    print(RValue::Local(x.clone())),
                    store(&x, global("value")),
                ]),
            )
            .into(),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("x = nil").count(), 1);
    }

    /// `x = nil` after a loop that writes `x` non-nil must be KEPT (post-loop kill
    /// drops x from the nil set).
    #[test]
    fn keeps_nil_after_loop_writes_local() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            While::new(global("c"), Block(vec![store(&x, global("value"))])).into(),
            store(&x, nil()),
            print(RValue::Local(x.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("x = nil").count(), 1);
    }

    /// A for-loop control local re-niled after the loop must be KEPT (the kill set
    /// includes the construct's counter).
    #[test]
    fn keeps_nil_after_for_loop_for_control_local() {
        let i = local("i");
        let mut block = Block(vec![
            declare_bare(&i),
            NumericFor::new(
                RValue::Literal(Literal::Number(1.0)),
                RValue::Literal(Literal::Number(3.0)),
                RValue::Literal(Literal::Number(1.0)),
                i.clone(),
                Block(vec![print(RValue::Local(i.clone()))]),
            )
            .into(),
            store(&i, nil()),
            print(RValue::Local(i.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("i = nil").count(), 1);
    }

    /// A captured local (mutated by a closure) is excluded entirely: its
    /// `x = nil` cleanup store is never deleted, even if it looks redundant.
    #[test]
    fn keeps_nil_for_captured_local() {
        let connection = local("connection");
        let handler = local("handler");
        let mut block = Block(vec![
            declare_bare(&connection),
            // A closure captures `connection` by Ref (could write it via a call).
            store(&handler, closure_capturing(&connection)),
            store(&connection, nil()),
            print(RValue::Local(connection.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("connection = nil").count(), 1);
    }

    /// Both `else: x = nil` arms removed but the genuinely-needed store kept, with
    /// an empty else producing a clean `if c then ... end`.
    #[test]
    fn removes_nil_in_empty_else_only_branch() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            if_stmt(global("c"), Block(vec![]), Block(vec![store(&x, nil())])),
            print(RValue::Local(x.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("x = nil").count(), 0);
    }

    /// `local x = nil` (prefix declaration) is NEVER deleted — removing it would
    /// de-scope the local.
    #[test]
    fn keeps_prefix_nil_declaration() {
        let x = local("x");
        let mut assign = Assign::new(vec![LValue::Local(x.clone())], vec![nil()]);
        assign.prefix = true;
        let mut block = Block(vec![assign.into(), print(RValue::Local(x.clone()))]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string(), "local x = nil\nprint(x)");
    }

    /// Regression for the `if`-arm-bail hole: a `then`-arm that reassigns `x`
    /// non-nil AND contains a label (so the arm bails on unstructured control
    /// flow) must NOT let the post-`if` `x = nil` be deleted. The bail path reports
    /// `nil − written` to the merge, so x is not claimed definitely-nil.
    #[test]
    fn keeps_nil_after_branch_that_bails_on_label() {
        let x = local("x");
        let mut block = Block(vec![
            declare_bare(&x),
            if_stmt(
                global("c"),
                Block(vec![
                    store(&x, global("value")),
                    Statement::Label(Label("L".to_string())),
                    print(global("f")),
                ]),
                Block(vec![]),
            ),
            store(&x, nil()),
            print(RValue::Local(x.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("x = nil").count(), 1);
    }

    /// An index-target `t.x = nil` is never deleted (it has `__newindex` side
    /// effects and is not a bare-local store).
    #[test]
    fn keeps_index_target_nil() {
        let t = local("t");
        let mut block = Block(vec![
            declare_bare(&t),
            Assign::new(
                vec![LValue::Index(Index::new(
                    RValue::Local(t.clone()),
                    string("field"),
                ))],
                vec![nil()],
            )
            .into(),
            print(RValue::Local(t.clone())),
        ]);

        eliminate_redundant_nil(&mut block);

        assert_eq!(block.to_string().matches("nil").count(), 1);
    }
}
