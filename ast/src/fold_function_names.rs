//! The late half of the SSA inliner's function-name fold. A function whose
//! binder only feeds the store of its own name (`local function F ... end;
//! ...; M.F = F`) prints as `function M.F`. The SSA inliner proves that move
//! and marks the closure ([`crate::Function::named_store_fold`]) but leaves
//! both statements, since Luau `-O2` may have inlined every call of `F`: the
//! statement de-inliner rebuilds those calls from the binder first. This pass
//! makes the move for each marked binder that gained no other read, so a
//! helper without a rebuilt call prints exactly as the SSA fold printed it.
//!
//! The move is the SSA inliner's own, proven there. It is checked once more
//! on the final block: the store must still be the binder's only read, in the
//! same block after it, and no statement in between may write a local the
//! closure captures by value, whose value it would otherwise take at the
//! store (`local x = 1; local function F() return x end; x = 2; M.F = F`).
//! A capture by reference reads the cell when the function runs, which it
//! cannot before the store.

use rustc_hash::FxHashMap;

use crate::{
    deinline::collect_written,
    Block, LValue, LocalRw, RValue, RcLocal, Statement, Traverse, Upvalue,
};

pub fn fold_function_names(body: &mut Block) {
    // Each marked binder, with the reads of it the module has.
    let mut reads: FxHashMap<RcLocal, usize> = FxHashMap::default();
    collect_binders(&body.0, &mut reads);
    if reads.is_empty() {
        return;
    }
    count_reads(&body.0, &mut reads);
    fold_block(&mut body.0, &reads);
}

/// `local F = function ... end` of a closure the SSA inliner marked.
fn marked_binder(statement: &Statement) -> Option<&RcLocal> {
    let Statement::Assign(assign) = statement else { return None };
    match (assign.left.as_slice(), assign.right.as_slice()) {
        ([LValue::Local(binder)], [RValue::Closure(closure)])
            if assign.prefix && !assign.parallel && closure.function.lock().named_store_fold =>
        {
            Some(binder)
        }
        _ => None,
    }
}

/// Calls `visit` on every block nested in `statement`, closure bodies aside.
fn each_nested_block(statement: &Statement, visit: &mut impl FnMut(&[Statement])) {
    match statement {
        Statement::If(branch) => {
            visit(&branch.then_block.lock().0);
            visit(&branch.else_block.lock().0);
        }
        Statement::While(node) => visit(&node.block.lock().0),
        Statement::Repeat(node) => visit(&node.block.lock().0),
        Statement::NumericFor(node) => visit(&node.block.lock().0),
        Statement::GenericFor(node) => visit(&node.block.lock().0),
        _ => {}
    }
}

/// Calls `visit` on the body of every closure `statement` creates, at any
/// depth of its values.
fn each_closure_body(statement: &Statement, visit: &mut impl FnMut(&[Statement])) {
    statement.traverse_rvalues_ref(&mut |value| {
        if let RValue::Closure(closure) = value {
            visit(&closure.function.0.lock().body.0);
        }
    });
}

fn collect_binders(stmts: &[Statement], reads: &mut FxHashMap<RcLocal, usize>) {
    for statement in stmts {
        if let Some(binder) = marked_binder(statement) {
            reads.insert(binder.clone(), 0);
        }
        each_nested_block(statement, &mut |block| collect_binders(block, reads));
        each_closure_body(statement, &mut |block| collect_binders(block, reads));
    }
}

/// Every read of a binder in `stmts`, nested blocks and closure bodies
/// included. A capture counts too, and again for each read in the body
/// capturing it: any capture is a read besides the store.
fn count_reads(stmts: &[Statement], reads: &mut FxHashMap<RcLocal, usize>) {
    for statement in stmts {
        statement.visit_local_reads(&mut |local| {
            if let Some(count) = reads.get_mut(local) {
                *count += 1;
            }
            true
        });
        each_nested_block(statement, &mut |block| count_reads(block, reads));
        each_closure_body(statement, &mut |block| count_reads(block, reads));
    }
}

fn fold_block(stmts: &mut Vec<Statement>, reads: &FxHashMap<RcLocal, usize>) {
    for statement in stmts.iter_mut() {
        match statement {
            Statement::If(branch) => {
                fold_block(&mut branch.then_block.lock().0, reads);
                fold_block(&mut branch.else_block.lock().0, reads);
            }
            Statement::While(node) => fold_block(&mut node.block.lock().0, reads),
            Statement::Repeat(node) => fold_block(&mut node.block.lock().0, reads),
            Statement::NumericFor(node) => fold_block(&mut node.block.lock().0, reads),
            Statement::GenericFor(node) => fold_block(&mut node.block.lock().0, reads),
            _ => {}
        }
        statement.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value {
                fold_block(&mut closure.function.0.lock().body.0, reads);
            }
        });
    }
    // The moves of this block, last first so earlier indices stay valid.
    let mut moves: Vec<(usize, usize)> = Vec::new();
    for (at, statement) in stmts.iter().enumerate() {
        let Some(binder) = marked_binder(statement) else { continue };
        if reads.get(binder) != Some(&1) {
            continue;
        }
        let Some(store) = (at + 1..stmts.len()).find(|&k| {
            let mut found = false;
            stmts[k].visit_local_reads(&mut |local| {
                found |= local == binder;
                !found
            });
            found
        }) else {
            continue;
        };
        if !crate::assignment_preserves_function_name(&stmts[store], binder) && !named_field_of(&stmts[store], binder) {
            continue;
        }
        let Statement::Assign(declaration) = statement else { unreachable!() };
        let RValue::Closure(closure) = &declaration.right[0] else { unreachable!() };
        let mut written = rustc_hash::FxHashSet::default();
        collect_written(&stmts[at + 1..=store], &mut written);
        let copied_written = closure.upvalues.iter().any(|upvalue| matches!(upvalue, Upvalue::Copy(local) if written.contains(local)));
        if !copied_written {
            moves.push((at, store));
        }
    }
    // Each closure goes into its store first; the declarations go after,
    // last first, so no index moves before it is used.
    for &(at, store) in &moves {
        let Statement::Assign(declaration) = std::mem::replace(&mut stmts[at], crate::Empty {}.into()) else { unreachable!() };
        let LValue::Local(binder) = &declaration.left[0] else { unreachable!() };
        let mut closure = declaration.right.into_iter().next();
        match &mut stmts[store] {
            Statement::Assign(assign) if assign.right.len() == 1 && assign.right[0].as_local() == Some(binder) => {
                assign.right[0] = closure.take().unwrap();
            }
            target => target.traverse_rvalues(&mut |value| {
                if let RValue::Table(table) = value {
                    for (_, field) in &mut table.0 {
                        if field.as_local() == Some(binder) && let Some(closure) = closure.take() {
                            *field = closure;
                        }
                    }
                }
            }),
        }
        debug_assert!(closure.is_none());
    }
    for &(at, _) in moves.iter().rev() {
        stmts.remove(at);
    }
}

/// Whether `statement` builds a table holding `binder` in the field of the
/// function's own name (`{ F = F }`): the SSA inliner's store `t.F = F`,
/// which its table pass then wrote into the constructor of `t`.
fn named_field_of(statement: &Statement, binder: &RcLocal) -> bool {
    let name = {
        let local = binder.0.lock();
        if local.2.is_empty() || local.2.iter().any(|b| !matches!(b.origin, crate::BindingOrigin::Function { .. })) {
            return false;
        }
        let Some(name) = local.source_name() else { return false };
        name.to_owned()
    };
    let mut found = false;
    statement.traverse_rvalues_ref(&mut |value| {
        if let RValue::Table(table) = value {
            found |= table.0.iter().any(|(key, field)| {
                field.as_local() == Some(binder)
                    && matches!(key, Some(RValue::Literal(crate::Literal::String(key))) if key == name.as_bytes())
            });
        }
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Closure, Function, Global, Index, Literal, Local, Return, Table};
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn helper(name: &str) -> RcLocal {
        let local = RcLocal::new(Local::new(None));
        local.0.lock().add_source_binding(crate::SourceBinding {
            origin: crate::BindingOrigin::Function { prototype: 1 },
            name: name.into(),
        });
        local
    }

    fn declaration(binder: &RcLocal, marked: bool, upvalues: Vec<Upvalue>) -> Statement {
        let function = Function { named_store_fold: marked, ..Function::default() };
        let closure = Closure { node_origin: Default::default(), function: by_address::ByAddress(Arc::new(Mutex::new(function))), upvalues };
        let mut assign = Assign::new(vec![LValue::Local(binder.clone())], vec![closure.into()]);
        assign.prefix = true;
        assign.into()
    }

    fn store(base: &RcLocal, field: &str, value: &RcLocal) -> Statement {
        let index = Index::new(RValue::Local(base.clone()), RValue::Literal(Literal::String(field.as_bytes().to_vec())));
        Assign::new(vec![LValue::Index(index)], vec![RValue::Local(value.clone())]).into()
    }

    #[test]
    fn a_marked_binder_read_only_by_its_named_store_folds_into_it() {
        let module = RcLocal::new(Local::new(Some("M".into())));
        let f = helper("f");
        let mut body = Block(vec![
            declaration(&f, true, Vec::new()),
            crate::Call::new(RValue::Global(Global::new(b"print".to_vec())), Vec::new()).into(),
            store(&module, "f", &f),
        ]);
        fold_function_names(&mut body);
        assert_eq!(body.0.len(), 2);
        assert!(matches!(&body.0[1], Statement::Assign(a) if matches!(a.right[0], RValue::Closure(_))));
    }

    #[test]
    fn a_binder_with_a_rebuilt_call_or_no_mark_stays() {
        let module = RcLocal::new(Local::new(Some("M".into())));
        for marked in [false, true] {
            let f = helper("f");
            let mut body = Block(vec![declaration(&f, marked, Vec::new()), store(&module, "f", &f)]);
            if marked {
                body.0.push(crate::Call::new(RValue::Local(f.clone()), Vec::new()).into());
            }
            let before = body.to_string();
            fold_function_names(&mut body);
            assert_eq!(body.to_string(), before);
        }
    }

    #[test]
    fn a_value_capture_written_before_the_store_keeps_the_binder() {
        let module = RcLocal::new(Local::new(Some("M".into())));
        let x = RcLocal::new(Local::new(Some("x".into())));
        for (capture, folds) in [(Upvalue::Copy(x.clone()), false), (Upvalue::Ref(x.clone()), true)] {
            let f = helper("f");
            let mut body = Block(vec![
                declaration(&f, true, vec![capture]),
                Assign::new(vec![LValue::Local(x.clone())], vec![RValue::Literal(Literal::Number(2.0))]).into(),
                store(&module, "f", &f),
            ]);
            fold_function_names(&mut body);
            assert_eq!(body.0.len(), if folds { 2 } else { 3 });
        }
    }

    #[test]
    fn a_named_table_field_takes_the_function() {
        let (f, g) = (helper("f"), helper("g"));
        let table = Table::new(vec![
            (Some(RValue::Literal(Literal::String(b"f".to_vec()))), RValue::Local(f.clone())),
            (Some(RValue::Literal(Literal::String(b"other".to_vec()))), RValue::Local(g.clone())),
        ]);
        let mut body = Block(vec![
            declaration(&f, true, Vec::new()),
            declaration(&g, true, Vec::new()),
            Return::new(vec![table.into()]).into(),
        ]);
        fold_function_names(&mut body);
        // `g` sits in a field of another name: it stays a local.
        assert_eq!(body.0.len(), 2);
        let Statement::Return(ret) = &body.0[1] else { panic!() };
        let RValue::Table(table) = &ret.values[0] else { panic!() };
        assert!(matches!(table.0[0].1, RValue::Closure(_)));
        assert!(matches!(&table.0[1].1, RValue::Local(local) if *local == g));
    }
}
