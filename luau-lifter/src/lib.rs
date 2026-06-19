mod deserializer;
mod instruction;
mod lifter;
mod op_code;

use ast::{
    flatten_guards::flatten_guards,
    local_declarations::LocalDeclarer,
    name_locals::name_locals_with_script_name,
    replace_locals::replace_locals,
    simplify_gotos::{hoist_locals_for_gotos, simplify_gotos},
    Traverse,
};

use by_address::ByAddress;
use cfg::{
    function::Function,
    ssa::{
        self,
        structuring::{structure_conditionals, structure_jumps},
    },
};
use indexmap::IndexMap;

use lifter::Lifter;

//use cfg_ir::{dot, function::Function, ssa};
use parking_lot::Mutex;
use petgraph::algo::dominators::simple_fast;

use rustc_hash::FxHashMap;
use triomphe::Arc;

use std::sync::Once;

use deserializer::bytecode::Bytecode;

// NOTE: the `#[global_allocator]` (mimalloc by default, dhat under the
// `dhat-heap` feature) lives in the BINARY crate root (`main.rs`), NOT here. A
// `#[global_allocator]` in this library would be inherited by every downstream
// consumer — including `web-server` (which the report wants on the system
// allocator) and, fatally, the `luau-worker` wasm32 cdylib, whose build cannot
// compile mimalloc's C source. Keeping the allocator choice in the binaries
// leaves the library target-agnostic.

/// Install a process-global quiet panic hook exactly once.
///
/// The decompiler intentionally panics on a small fraction of functions and
/// catches them with `catch_unwind`; the default hook would spam stderr with a
/// "thread panicked" line per caught panic. Installing one silent hook up front
/// (before any parallel region) both suppresses that noise and avoids the data
/// race that per-call `set_hook`/`take_hook` would otherwise create across the
/// rayon threads of the `decompile-folder` driver.
pub fn install_quiet_panic_hook() {
    static QUIET_HOOK: Once = Once::new();
    QUIET_HOOK.call_once(|| std::panic::set_hook(Box::new(|_| {})));
}

pub fn decompile_bytecode(bytecode: &[u8], encode_key: u8) -> String {
    decompile_bytecode_with_script_name(bytecode, encode_key, None)
}

pub fn decompile_bytecode_with_script_name(
    bytecode: &[u8],
    encode_key: u8,
    script_name: Option<&str>,
) -> String {
    try_decompile_bytecode_with_script_name(bytecode, encode_key, script_name).unwrap()
}

/// Like [`decompile_bytecode_with_script_name`] but returns the chunk-level
/// deserialize failure as `Err` instead of panicking. Used by the batch
/// (`decompile-folder`) driver so a malformed or empty input is reported as a
/// failure rather than crashing the whole run.
pub fn try_decompile_bytecode_with_script_name(
    bytecode: &[u8],
    encode_key: u8,
    script_name: Option<&str>,
) -> Result<String, String> {
    // Reset the per-thread local-id sequence so this decompilation's `RcLocal`
    // ids (and thus the FxHash-iteration order that depends on them, and the
    // generated local names) are independent of any earlier work this thread
    // did. Without this, parallel `decompile-folder` runs are nondeterministic
    // even though each file is processed on a single thread. See ast::RcLocal.
    ast::reset_local_ids();
    let chunk =
        deserializer::deserialize(bytecode, encode_key).map_err(|e| format!("deserialize: {e}"))?;
    match chunk {
        Bytecode::Error(msg) => Ok(msg),
        Bytecode::Chunk(chunk) => {
            let mut lifted = Vec::new();
            let mut stack = vec![(Arc::<Mutex<ast::Function>>::default(), chunk.main)];
            while let Some((ast_func, func_id)) = stack.pop() {
                let (function, upvalues, child_functions) =
                    Lifter::lift(&chunk.functions, &chunk.string_table, func_id);
                lifted.push((ast_func, function, upvalues));
                // The whole-program decompile order determines the monotonic
                // local-id assignment and thus the generated local names, so it
                // must be deterministic. `child_functions` is already in bytecode
                // (PC) order; sort it by the bytecode `func_index` (a STABLE sort,
                // so PC order breaks any `func_index` ties — the same proto can be
                // instantiated by several closure sites) for a fully reproducible
                // order independent of heap addresses.
                let mut children = child_functions
                    .into_iter()
                    .map(|(a, f)| (a.0, f))
                    .collect::<Vec<_>>();
                children.sort_by_key(|&(_, func_index)| func_index);
                stack.extend(children);
            }

            let (main, ..) = lifted.first().unwrap().clone();
            // Lifting (above) minted ids in `[0, id_base)`. Give each function a
            // disjoint, stride-spaced id range keyed by its position in the
            // deterministic lift order, so the ids it mints are independent of
            // scheduling once the loop is parallelized. STRIDE ≫ any plausible
            // per-function local count, and the first base equals the lifting
            // high-water mark, so the ranges never overlap each other or lifting.
            // The output does NOT depend on the absolute id values (only on the
            // per-function creation ORDER, which is thread-independent) — verified
            // byte-identical to the serial path — so no post-merge renumber is
            // needed; the strided bases alone make the whole pipeline deterministic.
            let id_base = ast::current_local_id();
            let func_count = lifted.len() as u64;
            const ID_STRIDE: u64 = 1 << 40;
            // Decompile every function in parallel. Each function is independent
            // (its only cross-function coupling was the shared monotonic id
            // counter, now made per-function and scheduling-independent via the
            // stride-spaced base above). `catch_unwind` + the process-global quiet
            // panic hook isolate a panicking function as a comment without racing.
            // Collect into an index-ordered Vec first so the result is
            // deterministic regardless of completion order, then build the map.
            use rayon::prelude::*;
            let decompiled = lifted
                .into_par_iter()
                .enumerate()
                .map(|(func_idx, (ast_function, function, upvalues_in))| {
                    use std::{fmt::Write, panic};

                    ast::set_local_id_base(id_base + func_idx as u64 * ID_STRIDE);
                    let function_id = function.id;
                    let mut args = std::panic::AssertUnwindSafe(Some((
                        ast_function.clone(),
                        function,
                        upvalues_in,
                    )));

                    // Panic suppression is handled process-globally by
                    // install_quiet_panic_hook(). We must NOT swap the global
                    // panic hook here: under the parallel `decompile-folder`
                    // driver many threads run this concurrently, and racing
                    // set_hook/take_hook corrupts the hook. catch_unwind alone
                    // isolates the per-function panic.
                    let result = panic::catch_unwind(move || {
                        let (ast_function, function, upvalues_in) = args.take().unwrap();
                        decompile_function(ast_function, function, upvalues_in)
                    });

                    match result {
                        Ok(r) => r,
                        Err(e) => {
                            let panic_information = match e.downcast::<String>() {
                                Ok(v) => *v,
                                Err(e) => match e.downcast::<&str>() {
                                    Ok(v) => v.to_string(),
                                    _ => "Unknown Source of Error".to_owned(),
                                },
                            };

                            let mut message = String::new();
                            writeln!(message, "failed to decompile").unwrap();
                            // writeln!(message, "function {} panicked at '{}'", function_id, panic_information).unwrap();
                            // if let Some(backtrace) = BACKTRACE.with(|b| b.borrow_mut().take()) {
                            //     write!(message, "stack backtrace:\n{}", backtrace).unwrap();
                            // }

                            ast_function.lock().body.extend(
                                message
                                    .trim_end()
                                    .split('\n')
                                    .map(|s| ast::Comment::new(s.to_string()).into()),
                            );
                            (ByAddress(ast_function), Vec::new())
                        }
                    }
                })
                .collect::<Vec<_>>();
            let mut upvalues = decompiled.into_iter().collect::<FxHashMap<_, _>>();

            // The rayon driver thread participated in the pool, so its thread-local
            // id counter is now left at some function's (scheduling-dependent)
            // strided range. The single-threaded serial tail below runs on this
            // thread; pin the counter to a fixed value above every function range
            // so any local it mints (e.g. `split_reused_loop_local` in name_locals)
            // gets a deterministic id. Today those are all NAMED locals whose
            // rendering is id-independent, but this keeps determinism structural
            // rather than incidental.
            ast::set_local_id_base(id_base + func_count * ID_STRIDE);

            let main = ByAddress(main);
            upvalues.remove(&main);
            let mut body = Arc::try_unwrap(main.0).unwrap().into_inner().body;
            link_upvalues(&mut body, &mut upvalues);
            ast::deinline::deinline(&mut body);
            ast::cleanup_returns::cleanup_redundant_returns(&mut body);
            name_locals_with_script_name(&mut body, true, script_name);
            // §2.8: recover OOP colon-method definitions. Runs after name_locals
            // (so first params are named `p`/`pN`) and before inline_temps (whose
            // receiver-deref shapes — `p:sibling()`, `p._field`, `p.field = ..` —
            // this pass keys on must still be present). Renames a genuine
            // receiver param[0] to `self`; the formatter then emits colon-form.
            ast::recover_methods::recover_methods(&mut body);
            ast::inline_temps::inline_single_use_temps(&mut body);
            ast::conditional_expressions::reconstruct_conditional_expressions(&mut body);
            ast::rebuild_table_literals::rebuild_table_literals(&mut body);
            ast::inline_temps::inline_single_use_temps(&mut body);
            // Redundant local-copy cleanup (proposal §2.9 A): delete junk
            // `local dst = src` aliases and substitute `src` for `dst`. Runs
            // AFTER the second inline_temps (the copies are only stabilized once
            // all single-use temps + table rebuild are done) and BEFORE
            // expr_deinline (which neither creates nor consumes this idiom). With
            // pass (B) below it reproduces the source `lastStats.floors += 1`.
            ast::copy_cleanup::copy_cleanup(&mut body);
            // Expression-level de-inline (proposal §7): recover small pure scalar
            // helpers that `-O2` inlined as a sub-expression of a caller's
            // condition/RValue. MUST run after reconstruct_conditional_expressions
            // (IfExpression/and/or now exist) and BEFORE normalize_conditions: the
            // latter De-Morgans a `not (helper-body)` call-site copy into a
            // disjunction that no longer matches the conjunctive helper body. Run
            // here and both sides are the same freshly-reconstructed tree; the
            // emitted `not helperName(args)` is then preserved by normalize.
            ast::expr_deinline::expr_deinline(&mut body);
            // Normalize boolean/condition shapes (proposal §10): collapse
            // reconstructed `if c then a else b` ternaries into and/or/not and
            // De-Morgan `not (...)` conditions. NaN-safe (never flips relational)
            // and never calls reduce, so it is safe before recover_guard_continue.
            ast::normalize_conditions::normalize_conditions(&mut body);
            // MUST remain the last AST transform (only the formatter follows). Do
            // not insert any reduce/reduce_condition/normalize pass after it: the
            // manufactured `not (a < b)` would be turned into the NaN-unsafe
            // `a >= b` if any later pass reduced it.
            ast::recover_guard_continue::recover_guard_continue(&mut body);
            Ok(body.to_string())
        }
    }
}

fn decompile_function(
    ast_function: Arc<Mutex<ast::Function>>,
    mut function: Function,
    upvalues_in: Vec<ast::RcLocal>,
) -> (ByAddress<Arc<Mutex<ast::Function>>>, Vec<ast::RcLocal>) {
    let (local_count, local_groups, upvalue_in_groups, upvalue_passed_groups) =
        cfg::ssa::construct(&mut function, &upvalues_in);
    let upvalue_to_group = upvalue_in_groups
        .into_iter()
        .chain(
            upvalue_passed_groups
                .into_iter()
                .map(|m| (ast::RcLocal::default(), m)),
        )
        .flat_map(|(i, g)| g.into_iter().map(move |u| (u, i.clone())))
        .collect::<IndexMap<_, _>>();
    // TODO: do we even need this?
    let local_to_group = local_groups
        .into_iter()
        .enumerate()
        .flat_map(|(i, g)| g.into_iter().map(move |l| (l, i)))
        .collect::<FxHashMap<_, _>>();
    // TODO: REFACTOR: some way to write a macro that states
    // if cfg::ssa::inline results in change then structure_jumps, structure_compound_conditionals,
    // structure_for_loops and remove_unnecessary_params must run again.
    // if structure_compound_conditionals results in change then dominators and post dominators
    // must be recalculated.
    // etc.
    // the macro could also maybe generate an optimal ordering?
    let mut changed = true;
    while changed {
        changed = false;

        let dominators = simple_fast(function.graph(), function.entry().unwrap());
        changed |= structure_jumps(&mut function, &dominators);

        ssa::inline::inline(&mut function, &local_to_group, &upvalue_to_group);

        if structure_conditionals(&mut function)
        // || {
        //     let post_dominators = post_dominators(function.graph_mut());
        //     structure_for_loops(&mut function, &dominators, &post_dominators)
        // }
        // we can't structure method calls like this because of __namecall
        // || structure_method_calls(&mut function)
        {
            changed = true;
        }
        let mut local_map = FxHashMap::default();
        // TODO: loop until returns false?
        if ssa::construct::remove_unnecessary_params(&mut function, &mut local_map) {
            changed = true;
        }
        ssa::construct::apply_local_map(&mut function, local_map);
    }
    // cfg::dot::render_to(&function, &mut std::io::stdout()).unwrap();
    ssa::Destructor::new(
        &mut function,
        upvalue_to_group,
        upvalues_in.iter().cloned().collect(),
        local_count,
    )
    .destruct();

    let params = std::mem::take(&mut function.parameters);
    let is_variadic = function.is_variadic;
    let mut lifted = restructure::lift(function);
    simplify_gotos(&mut lifted);
    flatten_guards(&mut lifted);
    let block = Arc::new(lifted.into());
    LocalDeclarer::default().declare_locals(
        // TODO: why does block.clone() not work?
        Arc::clone(&block),
        &upvalues_in.iter().chain(params.iter()).cloned().collect(),
    );
    hoist_locals_for_gotos(&mut block.lock());

    {
        let mut ast_function = ast_function.lock();
        ast_function.body = Arc::try_unwrap(block).unwrap().into_inner();
        ast_function.parameters = params;
        ast_function.is_variadic = is_variadic;
    }
    (ByAddress(ast_function), upvalues_in)
}

fn link_upvalues(
    body: &mut ast::Block,
    upvalues: &mut FxHashMap<ByAddress<Arc<Mutex<ast::Function>>>, Vec<ast::RcLocal>>,
) {
    for stat in &mut body.0 {
        stat.traverse_rvalues(&mut |rvalue| {
            if let ast::RValue::Closure(closure) = rvalue {
                let old_upvalues = &upvalues[&closure.function];
                let mut function = closure.function.lock();
                // TODO: inefficient, try constructing a map of all up -> new up first
                // and then call replace_locals on main body
                let mut local_map =
                    FxHashMap::with_capacity_and_hasher(old_upvalues.len(), Default::default());
                for (old, new) in
                    old_upvalues
                        .iter()
                        .zip(closure.upvalues.iter().map(|u| match u {
                            ast::Upvalue::Copy(l) | ast::Upvalue::Ref(l) => l,
                        }))
                {
                    // println!("{} -> {}", old, new);
                    local_map.insert(old.clone(), new.clone());
                }
                link_upvalues(&mut function.body, upvalues);
                replace_locals(&mut function.body, &local_map);
            }
        });
        match stat {
            ast::Statement::If(r#if) => {
                link_upvalues(&mut r#if.then_block.lock(), upvalues);
                link_upvalues(&mut r#if.else_block.lock(), upvalues);
            }
            ast::Statement::While(r#while) => {
                link_upvalues(&mut r#while.block.lock(), upvalues);
            }
            ast::Statement::Repeat(repeat) => {
                link_upvalues(&mut repeat.block.lock(), upvalues);
            }
            ast::Statement::NumericFor(numeric_for) => {
                link_upvalues(&mut numeric_for.block.lock(), upvalues);
            }
            ast::Statement::GenericFor(generic_for) => {
                link_upvalues(&mut generic_for.block.lock(), upvalues);
            }
            _ => {}
        }
    }
}
