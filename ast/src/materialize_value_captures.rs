use by_address::ByAddress;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Block, Function, LocalRw, RValue, RcLocal, Statement, Traverse, Upvalue,
    replace_locals::replace_locals, simplify_gotos::dc_block,
};

/// Materialize a by-value (`Upvalue::Copy`) capture of a LOOP-MUTATED local as an
/// explicit per-iteration snapshot `local snap = L`, redirecting the closure to
/// read `snap`.
///
/// Luau emits a value-capture (`LCT_VAL`) only for a local that is never mutated
/// after the closure is created, so a value-captured local that the decompiler
/// shows as mutated by the ENCLOSING LOOP has been coalesced onto the loop
/// variable. The rendered closure then captures that variable by reference (Lua
/// closures are by-ref) and every instance reads its final value:
///
/// ```text
///   for i = 1, 3 do t[i] = function() return i end end   -- prints 3,3,3 not 1,2,3
/// ```
///
/// Snapshotting restores the per-iteration binding faithfully — a value capture IS
/// a snapshot of the value at closure-creation time — WITHOUT an SSA-level "don't
/// coalesce" guard (which strands loop-carried copies the restructurer cannot
/// lower, or a write-back that clobbers the source). It is scoped to the enclosing
/// loop's mutated locals so stable upvalues (a module config captured by value) are
/// left untouched. `local snap = L` is itself captured, so `inline_temps` /
/// `copy_cleanup` (which refuse to touch a captured local) leave it intact.
pub fn materialize_value_captures(block: &mut Block) {
    materialize_in_block(block, &FxHashSet::default());
}

/// `loop_mutated` is the set of locals the CURRENT enclosing loop mutates (empty
/// when not inside a loop). A `Copy` capture of one of those is the C6 bug.
fn materialize_in_block(block: &mut Block, loop_mutated: &FxHashSet<RcLocal>) {
    for statement in &mut block.0 {
        // Closure bodies are a fresh scope: their own loops, not this one.
        let mut functions = Vec::new();
        statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
            if let RValue::Closure(closure) = rvalue {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            materialize_in_block(&mut function.lock().body, &FxHashSet::default());
        }
        // Nested control flow: an `if` inherits the enclosing loop; a nested loop
        // adds its own mutated locals to the enclosing set. A closure in the
        // nested loop may capture an outer loop variable, so replacing the set
        // would lose the snapshot requirement for that variable.
        match statement {
            Statement::If(r#if) => {
                materialize_in_block(&mut r#if.then_block.lock(), loop_mutated);
                materialize_in_block(&mut r#if.else_block.lock(), loop_mutated);
            }
            Statement::While(r#while) => {
                let mut m = loop_mutated_set(&r#while.block.lock(), &[]);
                m.extend(loop_mutated.iter().cloned());
                materialize_in_block(&mut r#while.block.lock(), &m);
            }
            Statement::Repeat(repeat) => {
                let mut m = loop_mutated_set(&repeat.block.lock(), &[]);
                m.extend(loop_mutated.iter().cloned());
                materialize_in_block(&mut repeat.block.lock(), &m);
            }
            Statement::NumericFor(numeric_for) => {
                let mut m = loop_mutated_set(&numeric_for.block.lock(), &[numeric_for.counter.clone()]);
                m.extend(loop_mutated.iter().cloned());
                materialize_in_block(&mut numeric_for.block.lock(), &m);
            }
            Statement::GenericFor(generic_for) => {
                let mut m = loop_mutated_set(&generic_for.block.lock(), &generic_for.res_locals);
                m.extend(loop_mutated.iter().cloned());
                materialize_in_block(&mut generic_for.block.lock(), &m);
            }
            _ => {}
        }
    }

    // Snapshot mutated value-captures at this level, inserting the declarations
    // immediately BEFORE the statement that creates the closure.
    let mut index = 0;
    while index < block.0.len() {
        let snapshots = snapshot_value_captures(&mut block.0[index], loop_mutated);
        let inserted = snapshots.len();
        for (offset, snapshot) in snapshots.into_iter().enumerate() {
            block.0.insert(index + offset, snapshot);
        }
        index += inserted + 1;
    }
}

/// Loop variable(s) plus every local written in the loop body OUTSIDE a nested
/// closure (a closure's writes are to its own private copy of a captured value).
fn loop_mutated_set(block: &Block, loop_vars: &[RcLocal]) -> FxHashSet<RcLocal> {
    let mut set: FxHashSet<RcLocal> = loop_vars.iter().cloned().collect();
    collect_written_outside_closures(block, &mut set);
    set
}

fn collect_written_outside_closures(block: &Block, set: &mut FxHashSet<RcLocal>) {
    for statement in &block.0 {
        for written in statement.values_written() {
            set.insert(written.clone());
        }
        match statement {
            Statement::If(r#if) => {
                collect_written_outside_closures(&r#if.then_block.lock(), set);
                collect_written_outside_closures(&r#if.else_block.lock(), set);
            }
            Statement::While(r#while) => {
                collect_written_outside_closures(&r#while.block.lock(), set)
            }
            Statement::Repeat(repeat) => {
                collect_written_outside_closures(&repeat.block.lock(), set)
            }
            Statement::NumericFor(numeric_for) => {
                collect_written_outside_closures(&numeric_for.block.lock(), set)
            }
            Statement::GenericFor(generic_for) => {
                collect_written_outside_closures(&generic_for.block.lock(), set)
            }
            _ => {}
        }
    }
}

/// For every `Upvalue::Copy(L)` in a closure embedded in `statement`'s expressions
/// where `L` is in `loop_mutated`, replace `L` with a fresh `snap` in the closure
/// body and the upvalue, returning the `local snap = L` declarations to insert.
fn snapshot_value_captures(
    statement: &mut Statement,
    loop_mutated: &FxHashSet<RcLocal>,
) -> Vec<Statement> {
    let mut snapshots = Vec::new();
    if loop_mutated.is_empty() {
        return snapshots;
    }
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            let to_snapshot: Vec<(usize, RcLocal)> = closure
                .upvalues
                .iter()
                .enumerate()
                .filter_map(|(i, upvalue)| match upvalue {
                    Upvalue::Copy(local) if loop_mutated.contains(local) => {
                        Some((i, local.clone()))
                    }
                    _ => None,
                })
                .collect();
            // De-inline duplicates a region by SHALLOW-cloning the `RValue::Closure`,
            // so sibling duplicates can share one Function Arc while keeping separate
            // `upvalues` vectors. Snapshotting mutates the function body recursively;
            // sharing would leak the L->snap rename to siblings (which do not receive
            // this instance's declaration). Clone the entire function/closure tree,
            // including nested closure bodies, before applying the map. A memo preserves
            // recursive closure graphs without minting duplicate nodes indefinitely.
            if !to_snapshot.is_empty() {
                let mut memo = FxHashMap::default();
                closure.function = ByAddress(clone_function_tree(&closure.function.0, &mut memo));
            }
            for (upvalue_index, local) in to_snapshot {
                let snap = RcLocal::default();
                let mut map: FxHashMap<RcLocal, RcLocal> = FxHashMap::default();
                map.insert(local.clone(), snap.clone());
                replace_locals(&mut closure.function.lock().body, &map);
                closure.upvalues[upvalue_index] = Upvalue::Copy(snap.clone());
                let mut declaration = Assign::new(vec![snap.into()], vec![RValue::Local(local)]);
                declaration.prefix = true;
                snapshots.push(declaration.into());
            }
        }
        None
    });
    snapshots
}

/// Deep-clone a Function and every nested closure body while preserving RcLocal
/// identities. Materialization runs after upvalue linking, so fresh Function
/// addresses are safe here (later passes build any identity maps from the cloned
/// tree). The memo also handles a recursive closure graph if one is synthesized.
fn clone_function_tree(
    function: &Arc<Mutex<Function>>,
    memo: &mut FxHashMap<*const Mutex<Function>, Arc<Mutex<Function>>>,
) -> Arc<Mutex<Function>> {
    let key = Arc::as_ptr(function);
    if let Some(existing) = memo.get(&key) {
        return existing.clone();
    }

    // Install a placeholder before descending so a self/mutually recursive
    // closure graph resolves back to the same clone instead of recursing forever.
    let clone = Arc::new(Mutex::new(Function::default()));
    memo.insert(key, clone.clone());

    let (
        bytecode_proto_id,
        bytecode_function_id,
        name,
        parameters,
        parameter_annotations,
        parameter_name_hints,
        is_variadic,
        mut body,
    ) = {
        let source = function.lock();
        (
            source.bytecode_proto_id,
            source.bytecode_function_id.clone(),
            source.name.clone(),
            source.parameters.clone(),
            source.parameter_annotations.clone(),
            source.parameter_name_hints.clone(),
            source.is_variadic,
            dc_block(&source.body),
        )
    };
    remint_nested_closures(&mut body, memo);
    *clone.lock() = Function {
        bytecode_proto_id,
        bytecode_function_id,
        name,
        parameters,
        parameter_annotations,
        parameter_name_hints,
        is_variadic,
        body,
    };
    clone
}

fn remint_nested_closures(
    block: &mut Block,
    memo: &mut FxHashMap<*const Mutex<Function>, Arc<Mutex<Function>>>,
) {
    for statement in &mut block.0 {
        statement.traverse_rvalues(&mut |rvalue| {
            if let RValue::Closure(closure) = rvalue {
                closure.function = ByAddress(clone_function_tree(&closure.function.0, memo));
            }
        });
        match statement {
            Statement::If(node) => {
                remint_nested_closures(&mut node.then_block.lock(), memo);
                remint_nested_closures(&mut node.else_block.lock(), memo);
            }
            Statement::While(node) => remint_nested_closures(&mut node.block.lock(), memo),
            Statement::Repeat(node) => remint_nested_closures(&mut node.block.lock(), memo),
            Statement::NumericFor(node) => remint_nested_closures(&mut node.block.lock(), memo),
            Statement::GenericFor(node) => remint_nested_closures(&mut node.block.lock(), memo),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::materialize_value_captures;
    use crate::{
        Assign, Block, Closure, Function, Literal, LValue, Local, NumericFor, RValue, RcLocal,
        Return, Statement, Upvalue,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_owned())))
    }

    fn number(value: f64) -> RValue {
        Literal::Number(value).into()
    }

    #[test]
    fn nested_loop_preserves_enclosing_mutated_capture_snapshot() {
        let outer_counter = local("i");
        let inner_counter = local("j");
        let destination = local("slot");
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![Return::new(vec![RValue::Local(outer_counter.clone())]).into()]),
            ..Function::default()
        }));
        let closure = RValue::Closure(Closure {
            function: ByAddress(function),
            upvalues: vec![Upvalue::Copy(outer_counter.clone())],
        });
        let inner = NumericFor::new(
            number(1.0),
            number(3.0),
            number(1.0),
            inner_counter,
            Block(vec![
                Assign::new(vec![LValue::Local(destination)], vec![closure]).into(),
            ]),
        );
        let outer = NumericFor::new(
            number(1.0),
            number(3.0),
            number(1.0),
            outer_counter.clone(),
            Block(vec![Statement::NumericFor(inner).into()]),
        );
        let mut block = Block(vec![Statement::NumericFor(outer).into()]);

        materialize_value_captures(&mut block);

        let outer = block[0].as_numeric_for().unwrap();
        let outer_body = outer.block.lock();
        let inner = outer_body[0].as_numeric_for().unwrap();
        let inner_body = inner.block.lock();
        let Statement::Assign(snapshot) = &inner_body[0] else {
            panic!("expected a snapshot declaration before the closure assignment");
        };
        let LValue::Local(snapshot_local) = &snapshot.left[0] else {
            panic!("expected the snapshot to declare a local");
        };
        assert!(snapshot.prefix);
        assert_eq!(snapshot.right, vec![RValue::Local(outer_counter)]);
        let Statement::Assign(assignment) = &inner_body[1] else {
            panic!("expected the closure assignment after the snapshot");
        };
        let RValue::Closure(closure) = &assignment.right[0] else {
            panic!("expected closure assignment");
        };
        assert_eq!(closure.upvalues, vec![Upvalue::Copy(snapshot_local.clone())]);
        assert!(matches!(
            closure.function.lock().body[0],
            Statement::Return(ref ret) if ret.values == vec![RValue::Local(snapshot_local.clone())]
        ));
    }

    #[test]
    fn shared_nested_closure_bodies_are_cloned_before_snapshot_rename() {
        let counter = local("i");
        let first_slot = local("first");
        let second_slot = local("second");
        let nested_function = Arc::new(Mutex::new(Function {
            body: Block(vec![Return::new(vec![RValue::Local(counter.clone())]).into()]),
            ..Function::default()
        }));
        let outer_function = Arc::new(Mutex::new(Function {
            body: Block(vec![
                Return::new(vec![RValue::Closure(Closure {
                    function: ByAddress(nested_function.clone()),
                    upvalues: vec![Upvalue::Copy(counter.clone())],
                })])
                .into(),
            ]),
            ..Function::default()
        }));
        let make_closure = || {
            RValue::Closure(Closure {
                function: ByAddress(outer_function.clone()),
                upvalues: vec![Upvalue::Copy(counter.clone())],
            })
        };
        let loop_body = Block(vec![
            Assign::new(vec![LValue::Local(first_slot)], vec![make_closure()]).into(),
            Assign::new(vec![LValue::Local(second_slot)], vec![make_closure()]).into(),
        ]);
        let loop_statement = NumericFor::new(
            number(1.0),
            number(2.0),
            number(1.0),
            counter.clone(),
            loop_body,
        );
        let mut block = Block(vec![Statement::NumericFor(loop_statement).into()]);

        materialize_value_captures(&mut block);

        let outer = block[0].as_numeric_for().unwrap();
        let body = outer.block.lock();
        let first = body[1].as_assign().unwrap();
        let second = body[3].as_assign().unwrap();
        let RValue::Closure(first_outer) = &first.right[0] else {
            panic!("expected first outer closure");
        };
        let RValue::Closure(second_outer) = &second.right[0] else {
            panic!("expected second outer closure");
        };
        assert!(!Arc::ptr_eq(&first_outer.function.0, &second_outer.function.0));
        let Upvalue::Copy(first_snapshot) = &first_outer.upvalues[0] else {
            panic!("expected first snapshot capture");
        };
        let Upvalue::Copy(second_snapshot) = &second_outer.upvalues[0] else {
            panic!("expected second snapshot capture");
        };

        let nested_function = |outer: &RValue| {
            let RValue::Closure(outer) = outer else {
                unreachable!()
            };
            let function = outer.function.lock();
            let Statement::Return(ret) = &function.body[0] else {
                panic!("expected nested closure return");
            };
            let RValue::Closure(nested) = &ret.values[0] else {
                panic!("expected nested closure");
            };
            nested.function.0.clone()
        };
        let first_nested = nested_function(&first.right[0]);
        let second_nested = nested_function(&second.right[0]);
        assert!(!Arc::ptr_eq(&first_nested, &second_nested));
        assert!(matches!(
            first_nested.lock().body[0],
            Statement::Return(ref ret) if ret.values[0] == RValue::Local(first_snapshot.clone())
        ));
        assert!(matches!(
            second_nested.lock().body[0],
            Statement::Return(ref ret) if ret.values[0] == RValue::Local(second_snapshot.clone())
        ));
    }
}
