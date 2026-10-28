//! Name single-use builtin imports directly at adjacent call sites when the
//! evaluation-order proof preserves their lookup. Enabling FASTCALL can move
//! that lookup after arguments, so observable arguments keep the snapshot.
use crate::{Block, LValue, RValue, RcLocal, Select, Statement, Traverse};

pub fn fold_import_callees(body: &mut Block) {
    let census = crate::deinline_safety::CaptureSafety::new(body);
    if census.dynamic_environment() {
        return;
    }
    block(&mut body.0, &census, true);
}

/// A specialized matching pattern owns its statement blocks, but closure
/// literals still share their Function bodies with the real tree. Keep those
/// untouched, and use the full module census for its remaining captured reads.
pub(crate) fn fold_specialized_import_callees(stmts: &mut Vec<Statement>, census: &crate::deinline_safety::CaptureSafety) -> bool {
    if census.dynamic_environment() { return false; }
    block(stmts, census, false)
}

fn block(stmts: &mut Vec<Statement>, census: &crate::deinline_safety::CaptureSafety, descend_functions: bool) -> bool {
    let mut changed = false;
    for statement in stmts.iter_mut() {
        nested_blocks(statement, &mut |nested| { changed |= block(nested, census, descend_functions); });
        if descend_functions {
            statement.traverse_rvalues(&mut |value| {
                if let RValue::Closure(closure) = value {
                    changed |= block(&mut closure.function.0.lock().body.0, census, true);
                }
            });
        }
    }
    let mut i = 0;
    while i < stmts.len() {
        if let Some((alias, path)) = import_alias(&stmts[i])
            // shortcut: only cross comments/empty statements; extend with an explicit intervening-motion proof.
            && let Some(next) = (i + 1..stmts.len()).find(|&at| !matches!(stmts[at], Statement::Comment(_) | Statement::Empty(_)))
            && crate::deinline::count_local_reads(&stmts[i + 1..], &alias) == 1
            && !written(&stmts[i + 1..], &alias)
            && crate::evaluation_order::can_sink(&stmts[next], &alias, &path, &|local| !census.uncaptured(local))
            && name_callee(&mut stmts[next], &alias, &path)
        {
            stmts.remove(i);
            changed = true;
            continue;
        }
        i += 1;
    }
    changed
}

/// Restrict this presentation pass to builtin names; this is no purity proof.
fn import_alias(statement: &Statement) -> Option<(RcLocal, RValue)> {
    let Statement::Assign(assign) = statement else { return None };
    let ([LValue::Local(alias)], [path]) = (assign.left.as_slice(), assign.right.as_slice()) else {
        return None;
    };
    (assign.prefix && crate::deinline_safety::library_import(path)).then(|| (alias.clone(), path.clone()))
}

fn written(stmts: &[Statement], local: &RcLocal) -> bool {
    let mut written = rustc_hash::FxHashSet::default();
    crate::deinline::collect_written(stmts, &mut written);
    written.contains(local)
}

/// Replaces the call of `alias` outside closures with a call of `path`;
/// false if `alias` is not called there.
fn name_callee(statement: &mut Statement, alias: &RcLocal, path: &RValue) -> bool {
    let rename = |call: &mut crate::Call| {
        let calls_alias = matches!(&*call.value, RValue::Local(local) if local == alias);
        if calls_alias {
            *call.value = path.clone();
        }
        calls_alias
    };
    let mut done = match statement {
        Statement::Call(call) => rename(call),
        _ => false,
    };
    if !done {
        statement.traverse_rvalues(&mut |value| {
            if !done && let RValue::Call(call) | RValue::Select(Select::Call(call)) = value {
                done = rename(call);
            }
        });
    }
    done
}

fn nested_blocks(statement: &mut Statement, visit: &mut impl FnMut(&mut Vec<Statement>)) {
    match statement {
        Statement::If(branch) => {
            visit(&mut branch.then_block.lock().0);
            visit(&mut branch.else_block.lock().0);
        }
        Statement::While(node) => visit(&mut node.block.lock().0),
        Statement::Repeat(node) => visit(&mut node.block.lock().0),
        Statement::NumericFor(node) => visit(&mut node.block.lock().0),
        Statement::GenericFor(node) => visit(&mut node.block.lock().0),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Closure, Function, Global, Index, Literal, Local, Return};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn alias(name: &str) -> (RcLocal, Statement) {
        let local = RcLocal::new(Local::new(Some(name.to_string())));
        let path = Index::new(Global::from("Vector2").into(), Literal::String(b"new".to_vec()).into());
        let mut declaration = Assign::new(vec![LValue::Local(local.clone())], vec![path.into()]);
        declaration.prefix = true;
        (local, declaration.into())
    }

    fn call(local: &RcLocal) -> RValue {
        RValue::Call(Call::new(RValue::Local(local.clone()), vec![RValue::Call(Call::new(Global::from("f").into(), Vec::new()))]))
    }

    fn run(statements: Vec<Statement>) -> String {
        let mut block = Block(statements);
        fold_import_callees(&mut block);
        block.to_string()
    }

    #[test]
    fn an_import_read_once_as_a_callee_is_named_in_place() {
        let (new, declaration) = alias("new");
        let value = Call::new(new.into(), vec![Literal::Number(1.0).into()]).into();
        let output = run(vec![declaration, Return::new(vec![value]).into()]);
        assert_eq!(output, "return Vector2.new(1)");
    }

    #[test]
    fn callee_snapshot_stays_before_effectful_or_captured_arguments() {
        let (new, declaration) = alias("new");
        let output = run(vec![declaration, Return::new(vec![call(&new)]).into()]);
        assert!(output.starts_with("local new = Vector2.new"), "{output}");

        let (new, declaration) = alias("new");
        let cell = RcLocal::new(Local::new(Some("cell".into())));
        let writer = Function { body: Block(vec![
            Assign::new(vec![cell.clone().into()], vec![Literal::Number(20.0).into()]).into(),
        ]), ..Default::default() };
        let closure = Closure { node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(writer))), upvalues: vec![crate::Upvalue::Ref(cell.clone())] };
        let value = Call::new(new.into(), vec![cell.into()]).into();
        let output = run(vec![declaration, Return::new(vec![value, closure.into()]).into()]);
        assert!(output.starts_with("local new = Vector2.new"), "{output}");
    }

    #[test]
    fn specialized_import_folding_uses_the_full_capture_census() {
        let cell = RcLocal::new(Local::new(Some("cell".into())));
        let writer = Closure { node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                body: Block(vec![Assign::new(vec![cell.clone().into()], vec![Literal::Number(20.0).into()]).into()]),
                ..Default::default()
            }))), upvalues: vec![crate::Upvalue::Ref(cell.clone())] };
        let module = Block(vec![Return::new(vec![writer.into()]).into()]);
        let census = crate::deinline_safety::CaptureSafety::new(&module);
        for (argument, allowed) in [
            (Literal::Number(1.0).into(), true),
            (cell.into(), false),
            (Global::from("external").into(), false),
            (Call::new(Global::from("change").into(), vec![]).into(), false),
        ] {
            let (new, declaration) = alias("new");
            let mut pattern = vec![declaration, Return::new(vec![Call::new(new.into(), vec![argument]).into()]).into()];
            let before = format!("{pattern:?}");
            assert_eq!(fold_specialized_import_callees(&mut pattern, &census), allowed);
            if !allowed { assert_eq!(format!("{pattern:?}"), before); }
        }
        let (new, declaration) = alias("new");
        let mut pattern = vec![declaration, Return::new(vec![Call::new(new.into(), vec![Literal::Number(1.0).into()]).into()]).into()];
        assert!(!fold_specialized_import_callees(&mut pattern,
            &crate::deinline_safety::CaptureSafety::exhausted_for_tests()));
        assert_eq!(pattern.len(), 2);
    }

    #[test]
    fn specialized_import_folding_owns_blocks_but_never_changes_shared_functions() {
        let make_body = || {
            let (new, declaration) = alias("new");
            Block(vec![declaration, Return::new(vec![Call::new(new.into(), vec![Literal::Number(1.0).into()]).into()]).into()])
        };
        let function = ByAddress(Arc::new(Mutex::new(Function { body: make_body(), ..Default::default() })));
        let closure = Closure { node_origin: Default::default(), function: function.clone(), upvalues: vec![] };
        let original = Block(vec![crate::If::new(Global::from("condition").into(),
            make_body(), Block(vec![Return::new(vec![closure.into()]).into()])).into()]);
        let census = crate::deinline_safety::CaptureSafety::new(&original);
        let before = original.to_string();
        let function_before = function.lock().body.to_string();
        let mut pattern = crate::deinline::canon(&original.0);
        assert!(fold_specialized_import_callees(&mut pattern, &census));
        assert_eq!(original.to_string(), before);
        assert_eq!(function.lock().body.to_string(), function_before);
        assert!(Block(pattern).to_string().contains("return Vector2.new(1)"));
    }

    #[test]
    fn callee_lookup_does_not_cross_statements_or_become_conditional() {
        for mode in ["lookup", "mutation", "branch"] {
            let (new, declaration) = alias("new");
            let result = Return::new(vec![Call::new(new.into(), vec![Literal::Number(1.0).into()]).into()]).into();
            let statements = match mode {
                "branch" => vec![declaration, crate::If::new(Global::from("flag").into(), Block(vec![result]), Block::default()).into()],
                "mutation" => {
                    let path = Index::new(Global::from("Vector2").into(), Literal::String(b"new".to_vec()).into());
                    vec![declaration, Assign::new(vec![path.into()], vec![Literal::Nil.into()]).into(), result]
                }
                _ => vec![declaration, Call::new(Global::from("observe").into(), vec![]).into(), result],
            };
            let output = run(statements);
            assert!(output.starts_with("local new = Vector2.new"), "{output}");
        }
    }

    #[test]
    fn an_alias_stays_where_its_value_could_differ_or_is_reused() {
        // Read twice.
        let (new, declaration) = alias("new");
        let output = run(vec![declaration, Return::new(vec![call(&new), call(&new)]).into()]);
        assert!(output.starts_with("local new = Vector2.new"), "{output}");

        // The script assigns `Vector2`.
        let (new, declaration) = alias("new");
        let assignment = Assign::new(vec![LValue::Global(Global::from("Vector2"))], vec![Literal::Nil.into()]);
        let output = run(vec![declaration, assignment.into(), Return::new(vec![call(&new)]).into()]);
        assert!(output.starts_with("local new = Vector2.new"), "{output}");

        // Called only inside a closure, which reads the alias as an upvalue.
        let (new, declaration) = alias("new");
        let function = Function { body: Block(vec![Return::new(vec![call(&new)]).into()]), ..Function::default() };
        let closure = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: vec![crate::Upvalue::Copy(new.clone())],
        });
        let output = run(vec![declaration, Return::new(vec![closure]).into()]);
        assert!(output.starts_with("local new = Vector2.new"), "{output}");
    }
}
