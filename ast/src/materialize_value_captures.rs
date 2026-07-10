use by_address::ByAddress;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    replace_locals::replace_locals, simplify_gotos::dc_block, Assign, Block, Function, LocalRw,
    RValue, RcLocal, Statement, Traverse, Upvalue,
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
        // Nested control flow: an `if` inherits the enclosing loop; a loop starts a
        // new mutated set (its loop variable(s) + everything written in its body).
        match statement {
            Statement::If(r#if) => {
                materialize_in_block(&mut r#if.then_block.lock(), loop_mutated);
                materialize_in_block(&mut r#if.else_block.lock(), loop_mutated);
            }
            Statement::While(r#while) => {
                let m = loop_mutated_set(&r#while.block.lock(), &[]);
                materialize_in_block(&mut r#while.block.lock(), &m);
            }
            Statement::Repeat(repeat) => {
                let m = loop_mutated_set(&repeat.block.lock(), &[]);
                materialize_in_block(&mut repeat.block.lock(), &m);
            }
            Statement::NumericFor(numeric_for) => {
                let m = loop_mutated_set(&numeric_for.block.lock(), &[numeric_for.counter.clone()]);
                materialize_in_block(&mut numeric_for.block.lock(), &m);
            }
            Statement::GenericFor(generic_for) => {
                let m = loop_mutated_set(&generic_for.block.lock(), &generic_for.res_locals);
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
            // so sibling duplicates SHARE one `Arc<Mutex<Function>>` body
            // (`closure.rs` `function: ByAddress<Arc<Mutex<Function>>>`) while each keeps
            // its own `upvalues` Vec and enclosing block. The snapshot below mutates the
            // body (`replace_locals` L->snap) but inserts the `local snap = L` decl + the
            // upvalue swap into THIS instance only — so a shared body would leak the
            // rename into the sibling, which never receives the decl, leaving it reading
            // an undeclared `snap` (the C6 "Unknown global vN" regression). Give this
            // instance its OWN body first: a deep clone (`dc_block` rebuilds nested
            // `Arc<Mutex<Block>>` sub-blocks). `RcLocal` identities stay shared, so the
            // capture semantics are unchanged. (A capture read strictly inside a nested
            // closure of this body is a known residual `dc_block` does not cover — nested
            // closure Arcs can't be re-minted without panicking a later `Arc::as_ptr`
            // lookup; it has no corpus occurrence and is strictly better than pre-fix.)
            // Gated on a shared Arc so a genuine
            // per-iteration loop closure (`strong_count == 1`, C6's real target) is
            // byte-identical to before. (Pass order: this runs after `deinline` and after
            // the recursive descent into closure bodies above, so the only live holders
            // of this Arc are the sibling duplicate statements — not a same-pass artifact.)
            if !to_snapshot.is_empty() && Arc::strong_count(&closure.function.0) > 1 {
                let cloned = {
                    let f = closure.function.0.lock();
                    Function {
                        name: f.name.clone(),
                        parameters: f.parameters.clone(),
                        is_variadic: f.is_variadic,
                        body: dc_block(&f.body),
                    }
                };
                closure.function = ByAddress(Arc::new(Mutex::new(cloned)));
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
