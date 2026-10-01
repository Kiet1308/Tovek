//! A callee read into a local before the arguments run, because they may
//! run code: `local new = Vector2.new; ...; return new(f(x), g(y))`. Luau
//! resolves a global path the script never assigns once, when it loads
//! (`deinline_safety::CaptureSafety::constant_import`), so no code can
//! change what it reads, and the call may name it in place:
//! `Vector2.new(f(x), g(y))`, which compiles to the same read before the
//! arguments.
use crate::{Block, LValue, RValue, RcLocal, Select, Statement, Traverse};

pub fn fold_import_callees(body: &mut Block) {
    let census = crate::deinline_safety::CaptureSafety::new(body);
    if census.dynamic_environment() {
        return;
    }
    block(&mut body.0, &census);
}

fn block(stmts: &mut Vec<Statement>, census: &crate::deinline_safety::CaptureSafety) {
    for statement in stmts.iter_mut() {
        nested_blocks(statement, &mut |nested| block(nested, census));
        statement.traverse_rvalues(&mut |value| {
            if let RValue::Closure(closure) = value {
                block(&mut closure.function.0.lock().body.0, census);
            }
        });
    }
    let mut i = 0;
    while i < stmts.len() {
        if let Some((alias, path)) = import_alias(&stmts[i], census)
            && crate::deinline::count_local_reads(&stmts[i + 1..], &alias) == 1
            && !written(&stmts[i + 1..], &alias)
            && name_callee(&mut stmts[i + 1..], &alias, &path)
        {
            stmts.remove(i);
            continue;
        }
        i += 1;
    }
}

/// `local alias = Global.path`, a constant import.
fn import_alias(statement: &Statement, census: &crate::deinline_safety::CaptureSafety) -> Option<(RcLocal, RValue)> {
    let Statement::Assign(assign) = statement else { return None };
    let ([LValue::Local(alias)], [path]) = (assign.left.as_slice(), assign.right.as_slice()) else {
        return None;
    };
    (assign.prefix && census.constant_import(path)).then(|| (alias.clone(), path.clone()))
}

fn written(stmts: &[Statement], local: &RcLocal) -> bool {
    let mut written = rustc_hash::FxHashSet::default();
    crate::deinline::collect_written(stmts, &mut written);
    written.contains(local)
}

/// Replaces the call of `alias` outside closures with a call of `path`;
/// false if `alias` is not called there.
fn name_callee(stmts: &mut [Statement], alias: &RcLocal, path: &RValue) -> bool {
    let rename = |call: &mut crate::Call| {
        let calls_alias = matches!(&*call.value, RValue::Local(local) if local == alias);
        if calls_alias {
            *call.value = path.clone();
        }
        calls_alias
    };
    for statement in stmts {
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
        if !done {
            nested_blocks(statement, &mut |nested| done = done || name_callee(nested, alias, path));
        }
        if done {
            return true;
        }
    }
    false
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
        let output = run(vec![declaration, Return::new(vec![call(&new)]).into()]);
        assert_eq!(output, "return Vector2.new(f())");
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
