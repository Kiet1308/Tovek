//! Present forward-declared functions the way Lua source writes them.
//!
//! A local that is declared `nil` and afterwards only ever receives closures is
//! the forward-declaration idiom:
//!
//! ```lua
//! local mountNode
//! ...
//! mountNode = function(element) ... end
//! ```
//!
//! The lifter sees the declaration's `LOADNIL` as an explicit `= nil`. Both
//! spellings compile to the same instruction, so the bare declaration, which
//! real code uses for 51 of 54 forward declarations in the pinned public and
//! game corpora, is chosen whenever every other write of the local, anywhere in
//! the function tree, is a closure.

use rustc_hash::FxHashMap;

use crate::{Block, LValue, Literal, LocalRw, RValue, RcLocal, Statement};

#[derive(Default)]
struct Writes {
    nil_declarations: usize,
    closures: usize,
    other: usize,
}

/// Rewrite `local f = nil` to `local f` for every forward-declared function.
pub fn bare_forward_declarations(block: &mut Block) {
    // Candidates are the `local f = nil` declarations, usually few; only their
    // writes are counted.
    let mut writes = FxHashMap::default();
    visit_statements(block, &mut |statement| {
        if let Some(local) = nil_declaration(statement) {
            writes.insert(local.clone(), Writes::default());
        }
    });
    if writes.is_empty() {
        return;
    }
    visit_statements(block, &mut |statement| census_statement(statement, &mut writes));
    if writes.values().any(is_forward_function) {
        rewrite_block(block, &writes);
    }
}

fn is_forward_function(writes: &Writes) -> bool {
    writes.nil_declarations == 1 && writes.closures != 0 && writes.other == 0
}

fn nil_declaration(statement: &Statement) -> Option<&RcLocal> {
    let Statement::Assign(assign) = statement else { return None };
    match (assign.left.as_slice(), assign.right.as_slice()) {
        ([LValue::Local(local)], [RValue::Literal(Literal::Nil)]) if assign.prefix => Some(local),
        _ => None,
    }
}

/// Every statement of the function tree, including nested closures.
fn visit_statements(block: &Block, visit: &mut impl FnMut(&Statement)) {
    for statement in &block.0 {
        visit(statement);
        let mut functions = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            functions.push(closure.function.clone());
        });
        for function in functions {
            visit_statements(&function.lock().body, visit);
        }
        for_each_child_block(statement, &mut |block| visit_statements(block, visit));
    }
}

fn census_statement(statement: &Statement, writes: &mut FxHashMap<RcLocal, Writes>) {
    if let Statement::Assign(assign) = statement {
        let aligned = assign.left.len() == assign.right.len();
        for (index, left) in assign.left.iter().enumerate() {
            let LValue::Local(local) = left else { continue };
            let Some(entry) = writes.get_mut(local) else { continue };
            match aligned.then(|| &assign.right[index]) {
                Some(RValue::Closure(_)) => entry.closures += 1,
                Some(RValue::Literal(Literal::Nil)) if assign.prefix && assign.left.len() == 1 => {
                    entry.nil_declarations += 1
                }
                _ => entry.other += 1,
            }
        }
    } else {
        statement.visit_local_writes(&mut |local| {
            if let Some(entry) = writes.get_mut(local) {
                entry.other += 1;
            }
            true
        });
    }
}

fn rewrite_block(block: &mut Block, writes: &FxHashMap<RcLocal, Writes>) {
    for statement in &mut block.0 {
        if let Statement::Assign(assign) = statement
            && assign.prefix
            && assign.left.len() == 1
            && matches!(assign.right.as_slice(), [RValue::Literal(Literal::Nil)])
            && let LValue::Local(local) = &assign.left[0]
            && writes.get(local).is_some_and(is_forward_function)
        {
            assign.right.clear();
        }
        let mut functions = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            functions.push(closure.function.clone());
        });
        for function in functions {
            rewrite_block(&mut function.lock().body, writes);
        }
        for_each_child_block(statement, &mut |block| rewrite_block(block, writes));
    }
}

fn for_each_child_block(statement: &Statement, f: &mut impl FnMut(&mut Block)) {
    match statement {
        Statement::If(r#if) => {
            f(&mut r#if.then_block.lock());
            f(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => f(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => f(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => f(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => f(&mut generic_for.block.lock()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::bare_forward_declarations;
    use crate::{Assign, Block, Closure, Function, LValue, Literal, Local, RValue, RcLocal};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn closure() -> RValue {
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![],
        })
    }

    fn declare_nil(local: &RcLocal) -> crate::Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![Literal::Nil.into()]);
        assign.prefix = true;
        assign.into()
    }

    #[test]
    fn forward_declared_function_uses_the_lua_idiom() {
        let mount = local("mount");
        let mut block = Block(vec![
            declare_nil(&mount),
            Assign::new(vec![LValue::Local(mount.clone())], vec![closure()]).into(),
        ]);
        bare_forward_declarations(&mut block);
        let text = block.to_string();
        assert!(text.starts_with("local mount\n"), "{text}");
        assert!(!text.contains("= nil"), "{text}");
    }

    #[test]
    fn a_nil_local_with_other_writes_keeps_its_initializer() {
        let state = local("state");
        let mut block = Block(vec![
            declare_nil(&state),
            Assign::new(vec![LValue::Local(state.clone())], vec![closure()]).into(),
            Assign::new(vec![LValue::Local(state.clone())], vec![Literal::Boolean(true).into()]).into(),
        ]);
        bare_forward_declarations(&mut block);
        assert!(block.to_string().starts_with("local state = nil\n"));
    }
}
