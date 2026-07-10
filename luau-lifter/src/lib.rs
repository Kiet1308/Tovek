mod deserializer;
mod instruction;
mod lifter;
mod op_code;

use ast::{
    flatten_guards::flatten_guards,
    local_declarations::LocalDeclarer,
    name_locals::{name_locals_with_options, NameLocalOptions},
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

pub const DONT_REUSE_VAR: u32 = 1 << 0;
pub const NO_SYNTH_HELPERS: u32 = 1 << 1;
pub const ASSUME_NO_NAN: u32 = 1 << 2;

// ---- TEMPORARY PROFILING (env-gated, remove before ship) ----
#[doc(hidden)]
pub mod prof {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    pub static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    pub fn on() -> bool {
        *ENABLED.get_or_init(|| std::env::var("MEDAL_PROF").is_ok())
    }

    macro_rules! counters {
        ($($name:ident),* $(,)?) => {
            $(pub static $name: AtomicU64 = AtomicU64::new(0);)*
            pub fn dump() {
                eprintln!("---- MEDAL_PROF (us) ----");
                $(eprintln!("{:<28} {:>10}", stringify!($name), $name.load(Ordering::Relaxed));)*
            }
        };
    }
    counters!(
        DESER_LIFT,
        PAR_LOOP_WALL,
        F_SSA_CONSTRUCT,
        F_SIMPLE_FAST,
        F_STRUCTURE_JUMPS,
        F_SSA_INLINE,
        F_STRUCTURE_CONDS,
        F_REMOVE_PARAMS,
        F_APPLY_MAP,
        F_DESTRUCT,
        F_RESTRUCTURE,
        F_SIMPLIFY_GOTOS,
        F_FLATTEN_GUARDS,
        F_DECLARE_LOCALS,
        F_HOIST,
        S_LINK_UPVALUES,
        S_DEINLINE,
        S_CLEANUP_RETURNS,
        S_MATERIALIZE,
        S_REHOIST_CONSTANTS,
        S_NAME_LOCALS,
        S_RECOVER_METHODS,
        S_INLINE_TEMPS_1,
        S_COND_EXPRS,
        S_REBUILD_TABLES,
        S_MATERIALIZE_CALL_RECEIVERS,
        S_COPY_CLEANUP,
        S_REBALANCE_EXPRS,
        S_CLEANUP_FINAL,
        S_ELIMINATE_NIL,
        S_RECOVER_CONN,
        S_EXPR_DEINLINE,
        S_NORMALIZE_CONDS,
        S_GUARD_CONTINUE,
        S_FORMAT,
    );

    pub struct Timer(Option<(Instant, &'static AtomicU64)>);
    impl Timer {
        pub fn new(c: &'static AtomicU64) -> Self {
            Timer(on().then(|| (Instant::now(), c)))
        }
    }
    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some((s, c)) = self.0.take() {
                c.fetch_add(s.elapsed().as_micros() as u64, Ordering::Relaxed);
            }
        }
    }
}
macro_rules! ptime {
    ($c:ident) => {
        let _t = crate::prof::Timer::new(&crate::prof::$c);
    };
}
// ---- END TEMPORARY PROFILING ----

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecompileOptions {
    pub dont_reuse_var: bool,
    pub no_synth_helpers: bool,
    pub assume_no_nan: bool,
}

impl DecompileOptions {
    pub fn from_flag_bits(bits: u32) -> Option<Self> {
        if bits & !(DONT_REUSE_VAR | NO_SYNTH_HELPERS | ASSUME_NO_NAN) != 0 {
            return None;
        }
        Some(Self {
            dont_reuse_var: bits & DONT_REUSE_VAR != 0,
            no_synth_helpers: bits & NO_SYNTH_HELPERS != 0,
            assume_no_nan: bits & ASSUME_NO_NAN != 0,
        })
    }

    pub fn bits(self) -> u32 {
        u32::from(self.dont_reuse_var) * DONT_REUSE_VAR
            | u32::from(self.no_synth_helpers) * NO_SYNTH_HELPERS
            | u32::from(self.assume_no_nan) * ASSUME_NO_NAN
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            dont_reuse_var: self.dont_reuse_var || other.dont_reuse_var,
            no_synth_helpers: self.no_synth_helpers || other.no_synth_helpers,
            assume_no_nan: self.assume_no_nan || other.assume_no_nan,
        }
    }
}

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
    decompile_bytecode_with_options(
        bytecode,
        encode_key,
        script_name,
        DecompileOptions::default(),
    )
}

pub fn decompile_bytecode_with_options(
    bytecode: &[u8],
    encode_key: u8,
    script_name: Option<&str>,
    options: DecompileOptions,
) -> String {
    try_decompile_bytecode_with_options(bytecode, encode_key, script_name, options).unwrap()
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
    try_decompile_bytecode_with_options(
        bytecode,
        encode_key,
        script_name,
        DecompileOptions::default(),
    )
}

pub fn try_decompile_bytecode_with_options(
    bytecode: &[u8],
    encode_key: u8,
    script_name: Option<&str>,
    options: DecompileOptions,
) -> Result<String, String> {
    // Reset the per-thread local-id sequence so this decompilation's `RcLocal`
    // ids (and thus the FxHash-iteration order that depends on them, and the
    // generated local names) are independent of any earlier work this thread
    // did. Without this, parallel `decompile-folder` runs are nondeterministic
    // even though each file is processed on a single thread. See ast::RcLocal.
    ast::reset_local_ids();
    let deser_timer = prof::Timer::new(&prof::DESER_LIFT);
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
            drop(deser_timer);
            let id_base = ast::current_local_id();
            let func_count = lifted.len() as u64;
            const ID_STRIDE: u64 = 1 << 40;
            let par_timer = prof::Timer::new(&prof::PAR_LOOP_WALL);
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

                    // LOAD-BEARING for both single and batch determinism: every
                    // closure that mints an `RcLocal` MUST re-base the thread-local
                    // id counter here, as its first act, before any `RcLocal::new`.
                    // The base depends only on `func_idx` (deterministic lift order),
                    // so a function's ids are independent of the rayon worker that
                    // runs it and of any sibling work stolen onto that worker —
                    // including, under `decompile_batch`, functions from a *different*
                    // script. Do not introduce id minting above this line or move the
                    // serial tail into a rayon region without an equivalent re-base.
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
            drop(par_timer);
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
            {
                ptime!(S_LINK_UPVALUES);
                link_upvalues(&mut body, &mut upvalues);
            }
            // Reverse continuation cloning introduced while structuring inlined
            // early returns.  This is the structured cross-jumping half of P1:
            // exact common tails are shared before the statement de-inliner tries
            // to recover helper calls from the now-compact regions.
            ast::factor_common_tails::factor_common_tails(&mut body);
            loop {
                ptime!(S_DEINLINE);
                ast::deinline::deinline(&mut body);
                // Replacing an inlined region by a call can make formerly
                // different branch tails identical. Cross-jump those fresh
                // tails, then let de-inline consume any newly exposed site.
                if !ast::factor_common_tails::factor_common_tails(&mut body) {
                    break;
                }
            }
            // Tier-B fallback for terminal continuations that cannot be hoisted
            // through every structured branch. It has its own bounded fixed point;
            // running de-inline again was measured byte-identical on the corpus.
            if !options.no_synth_helpers {
                ast::synthesize_terminal_helpers::synthesize_terminal_helpers(&mut body);
            }
            {
                ptime!(S_CLEANUP_RETURNS);
                ast::cleanup_returns::cleanup_redundant_returns(&mut body);
            }
            // Restore the per-iteration snapshot of a by-value (`Upvalue::Copy`)
            // capture that out-of-SSA coalescing merged onto a mutated (loop)
            // variable (C6). Runs before `name_locals` so the `local snap = L` it
            // mints gets named, and before `inline_temps`/`copy_cleanup` (which then
            // protect it as a captured local).
            {
                ptime!(S_MATERIALIZE);
                ast::materialize_value_captures::materialize_value_captures(&mut body);
            }
            {
                ptime!(S_REHOIST_CONSTANTS);
                ast::rehoist_constants::rehoist_constants(&mut body);
            }
            {
                ptime!(S_NAME_LOCALS);
                name_locals_with_options(
                    &mut body,
                    true,
                    script_name,
                    NameLocalOptions {
                        dont_reuse_var: options.dont_reuse_var,
                    },
                );
            }
            // §2.8: recover OOP colon-method definitions. Runs after name_locals
            // (so first params are named `p`/`pN`) and before inline_temps (whose
            // receiver-deref shapes — `p:sibling()`, `p._field`, `p.field = ..` —
            // this pass keys on must still be present). Renames a genuine
            // receiver param[0] to `self`; the formatter then emits colon-form.
            {
                ptime!(S_RECOVER_METHODS);
                ast::recover_methods::recover_methods(&mut body);
            }
            {
                ptime!(S_INLINE_TEMPS_1);
                ast::inline_temps::inline_single_use_temps(&mut body);
            }
            {
                ptime!(S_COND_EXPRS);
                ast::conditional_expressions::reconstruct_conditional_expressions(&mut body);
            }
            // Rebuild declarative table trees from the leaves upward. Inlining a
            // child table can make a parent's formerly-separated field writes
            // contiguous, so the two monotone passes share capture facts and a
            // fixed point.
            {
                ptime!(S_REBUILD_TABLES);
                ast::inline_temps::rebuild_ui_expression_trees(&mut body);
            }
            // Calls make property-assignment receivers hard to scan and can
            // already exist before the UI inliner. Restore the single-value
            // receiver temp after all UI-tree collapsing is complete.
            {
                ptime!(S_MATERIALIZE_CALL_RECEIVERS);
                ast::materialize_call_receivers::materialize_call_assignment_receivers(&mut body);
            }
            // Redundant local-copy cleanup (proposal §2.9 A): delete junk
            // `local dst = src` aliases and substitute `src` for `dst`. Runs
            // AFTER the second inline_temps (the copies are only stabilized once
            // all single-use temps + table rebuild are done) and BEFORE
            // expr_deinline (which neither creates nor consumes this idiom). With
            // pass (B) below it reproduces the source `lastStats.floors += 1`.
            {
                ptime!(S_COPY_CLEANUP);
                ast::copy_cleanup::copy_cleanup(&mut body);
            }
            // Eliminate redundant `x = nil` stores left by SSA phi-node
            // materialization (a predeclared `local x` then explicit `x = nil` on
            // every path it stays nil). A forward "definitely-nil" dataflow deletes
            // a `x = nil` only when x is provably already nil there. Runs AFTER
            // `reconstruct_conditional_expressions` (214) — which needs the
            // predecl+phi diamond to recover `if c then A else nil` ternaries — and
            // after the write-count-gated `inline_single_use_temps`/`copy_cleanup`
            // (whose decisions a write-count change here must not perturb). BEFORE
            // `recover_guard_continue` (which must stay last).
            {
                ptime!(S_ELIMINATE_NIL);
                ast::eliminate_nil::eliminate_redundant_nil(&mut body);
            }
            // C13: re-target a dropped connection write `local _ = sig:Connect(
            // function() ... cell:Disconnect() ... end)` back to the captured `cell`
            // the SSA orphaned (the parent never models the closure's by-ref write).
            {
                ptime!(S_RECOVER_CONN);
                ast::recover_dropped_connection::recover_dropped_connection(&mut body);
            }
            // Expression-level de-inline (proposal §7): recover small pure scalar
            // helpers that `-O2` inlined as a sub-expression of a caller's
            // condition/RValue. MUST run after reconstruct_conditional_expressions
            // (IfExpression/and/or now exist) and BEFORE normalize_conditions: the
            // latter De-Morgans a `not (helper-body)` call-site copy into a
            // disjunction that no longer matches the conjunctive helper body. Run
            // here and both sides are the same freshly-reconstructed tree; the
            // emitted `not helperName(args)` is then preserved by normalize.
            {
                ptime!(S_EXPR_DEINLINE);
                ast::expr_deinline::expr_deinline(&mut body);
            }
            // Balance only after expression de-inline has had the original
            // scalar tree available for helper matching. Expanding a long
            // conditional before this point would hide recoverable helpers such
            // as `getEffectKind` behind fresh statement-level control flow.
            {
                ptime!(S_REBALANCE_EXPRS);
                ast::rebalance_expressions::rebalance_expressions(&mut body);
            }
            // P5: low-risk final cleanup. This may fold literal boolean branches,
            // so it runs before the final condition normalization/guard passes.
            {
                ptime!(S_CLEANUP_FINAL);
                ast::cleanup_final::cleanup_final(&mut body, script_name);
            }
            // Normalize boolean/condition shapes (proposal §10): collapse
            // reconstructed `if c then a else b` ternaries into and/or/not and
            // De-Morgan `not (...)` conditions. NaN-safe by default (relational
            // complements require proof for both operands) and never calls the
            // generic reducer, so it is safe before recover_guard_continue.
            {
                ptime!(S_NORMALIZE_CONDS);
                ast::canonicalize_branches::canonicalize_branches(&mut body);
                ast::normalize_conditions::normalize_conditions_with_options(
                    &mut body,
                    options.assume_no_nan,
                );
            }
            // MUST remain the last condition-changing AST transform. Do not
            // insert any reduce/reduce_condition/normalize pass after it: the
            // manufactured `not (a < b)` would be turned into the NaN-unsafe
            // `a >= b` if any later pass reduced it.
            {
                ptime!(S_GUARD_CONTINUE);
                ast::recover_guard_continue::recover_guard_continue(&mut body);
            }
            // Late conditional/guard reconstruction can expose an already-dead
            // suffix after `return` (and Luau requires return to be last in its
            // block). This cleanup only truncates unreachable statements and
            // removes redundant function-tail void returns; it never rewrites a
            // condition, so the NaN-safety invariant above remains intact.
            {
                ptime!(S_CLEANUP_RETURNS);
                ast::cleanup_returns::cleanup_redundant_returns(&mut body);
            }
            let out = {
                ptime!(S_FORMAT);
                body.to_string()
            };
            if prof::on() {
                prof::dump();
            }
            Ok(out)
        }
    }
}

/// One script to decompile as part of a [`decompile_batch`] call.
pub struct BatchInput<'a> {
    /// Raw, already-base64-decoded Luau bytecode for this script.
    pub bytecode: &'a [u8],
    /// Per-script decode key (`op = op * key % 256`). 203 for Roblox client
    /// bytecode; 1 for unencoded Luau bytecode.
    pub encode_key: u8,
    /// Optional chunk name (used for naming + `require()`-path resolution).
    pub script_name: Option<&'a str>,
}

/// Decompile many scripts in one call, in parallel, preserving input order.
///
/// Each item is decompiled by the very same
/// [`try_decompile_bytecode_with_script_name`] the single-script path uses, so
/// every item's output is **byte-identical to decompiling that script on its
/// own**: that function resets the per-thread local-id counter at entry and gives
/// each of its functions a strided, lift-order-keyed id base, which makes its
/// output independent of the absolute ids and therefore of scheduling and of what
/// other items run concurrently. This is the same outer-parallel-over-items ×
/// inner-parallel-over-functions nesting the `decompile-folder` driver (`batch.rs`
/// → `try_decompile_bytecode_with_script_name`) already relies on for its
/// corpus-byte-identical guarantee.
///
/// Returns one `Result` per input, in input order: `Ok(source)` on success, or
/// `Err(reason)` if that one script failed to deserialize/decompile or panicked.
/// A failure (or panic) in one item never affects the others. Callers should
/// install the process-global quiet panic hook once up front via
/// [`install_quiet_panic_hook`].
pub fn decompile_batch(items: &[BatchInput<'_>]) -> Vec<Result<String, String>> {
    decompile_batch_with_options(items, DecompileOptions::default())
}

pub fn decompile_batch_with_options(
    items: &[BatchInput<'_>],
    options: DecompileOptions,
) -> Vec<Result<String, String>> {
    use rayon::prelude::*;
    items
        .par_iter()
        .map(|item| {
            // `try_decompile_bytecode_with_script_name` already catches per-function
            // panics internally; the outer guard here recovers the rarer panics in
            // lifting or the serial tail so one bad script can't poison the batch.
            // AssertUnwindSafe is sound because the only state the call mutates is
            // the per-thread id counter, and the next item on this worker calls
            // `reset_local_ids()` before minting any id (see `try_decompile_*`).
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                try_decompile_bytecode_with_options(
                    item.bytecode,
                    item.encode_key,
                    item.script_name,
                    options,
                )
            }));
            match caught {
                Ok(result) => result,
                Err(payload) => Err(format!("panicked: {}", panic_payload_message(&payload))),
            }
        })
        .collect()
}

/// Extract a human-readable message from a caught-panic payload (mirrors the
/// downcast ladder used inside the per-function decompile loop). Lives here in the
/// library (not the bin-only `decompile_core`) so [`decompile_batch`] can use it.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn decompile_function(
    ast_function: Arc<Mutex<ast::Function>>,
    mut function: Function,
    upvalues_in: Vec<ast::RcLocal>,
) -> (ByAddress<Arc<Mutex<ast::Function>>>, Vec<ast::RcLocal>) {
    let (local_count, local_groups, upvalue_in_groups, upvalue_passed_groups) = {
        ptime!(F_SSA_CONSTRUCT);
        cfg::ssa::construct(&mut function, &upvalues_in)
    };
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

        let dominators = {
            ptime!(F_SIMPLE_FAST);
            simple_fast(function.graph(), function.entry().unwrap())
        };
        {
            ptime!(F_STRUCTURE_JUMPS);
            changed |= structure_jumps(&mut function, &dominators);
        }

        {
            ptime!(F_SSA_INLINE);
            ssa::inline::inline(&mut function, &local_to_group, &upvalue_to_group);
        }

        let sc = {
            ptime!(F_STRUCTURE_CONDS);
            structure_conditionals(&mut function)
        };
        if sc
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
        let rp = {
            ptime!(F_REMOVE_PARAMS);
            ssa::construct::remove_unnecessary_params(
                &mut function,
                &mut local_map,
                Some(&upvalue_to_group),
            )
        };
        if rp {
            changed = true;
        }
        {
            ptime!(F_APPLY_MAP);
            ssa::construct::apply_local_map(&mut function, local_map);
        }
    }
    // cfg::dot::render_to(&function, &mut std::io::stdout()).unwrap();
    {
        ptime!(F_DESTRUCT);
        ssa::Destructor::new(
            &mut function,
            upvalue_to_group,
            upvalues_in.iter().cloned().collect(),
            local_count,
        )
        .destruct();
    }
    let params = std::mem::take(&mut function.parameters);
    let is_variadic = function.is_variadic;
    let mut lifted = {
        ptime!(F_RESTRUCTURE);
        restructure::lift(function)
    };
    {
        ptime!(F_SIMPLIFY_GOTOS);
        simplify_gotos(&mut lifted);
    }
    {
        ptime!(F_FLATTEN_GUARDS);
        flatten_guards(&mut lifted);
    }
    let block = Arc::new(lifted.into());
    {
        ptime!(F_DECLARE_LOCALS);
        LocalDeclarer::default().declare_locals(
            // TODO: why does block.clone() not work?
            Arc::clone(&block),
            &upvalues_in.iter().chain(params.iter()).cloned().collect(),
        );
    }
    {
        ptime!(F_HOIST);
        hoist_locals_for_gotos(&mut block.lock());
    }

    {
        let mut ast_function = ast_function.lock();
        ast_function.body = Arc::try_unwrap(block).unwrap().into_inner();
        ast_function.parameters = params;
        ast_function.is_variadic = is_variadic;
    }
    (ByAddress(ast_function), upvalues_in)
}

#[cfg(test)]
mod option_tests {
    use super::{DecompileOptions, ASSUME_NO_NAN, DONT_REUSE_VAR, NO_SYNTH_HELPERS};

    #[test]
    fn decompile_option_bits_round_trip() {
        let options = DecompileOptions {
            dont_reuse_var: true,
            no_synth_helpers: true,
            assume_no_nan: true,
        };
        assert_eq!(
            DecompileOptions::from_flag_bits(options.bits()),
            Some(options)
        );
        assert_eq!(
            options.bits(),
            DONT_REUSE_VAR | NO_SYNTH_HELPERS | ASSUME_NO_NAN
        );
        assert!(DecompileOptions::from_flag_bits(1 << 31).is_none());
    }
}

#[cfg(test)]
mod v11_fixtures {
    //! Hand-crafted Luau v11 bytecode fixtures.
    //!
    //! Roblox ships v9 and the open-source compiler targets v7, so no real v10/v11
    //! blob exists to test against. These build minimal-but-valid v11 chunks by hand
    //! to exercise: the per-proto feedback-vector read, the new aux-bearing opcodes
    //! (GETUDATAKS/SETUDATAKS/NAMECALLUDATA/NEWCLASSMEMBER/CALLFB) and the AD-form
    //! CMPPROTO. `encode_key = 1` makes the per-opcode `wrapping_mul` descramble an
    //! identity, so opcode bytes are literal ordinals.

    use super::try_decompile_bytecode_with_script_name as decompile;

    // --- opcode ordinals used below ---
    const LOADN: u8 = 4;
    const GETGLOBAL: u8 = 7; // aux
    const CALL: u8 = 21;
    const RETURN: u8 = 22;
    const NEWTABLE: u8 = 53; // aux
    const GETUDATAKS: u8 = 83; // aux
    const SETUDATAKS: u8 = 84; // aux
    const NAMECALLUDATA: u8 = 85; // aux
    const NEWCLASSMEMBER: u8 = 86; // aux
    const CALLFB: u8 = 87; // aux
    const CMPPROTO: u8 = 88; // aux, AD-form

    fn leb128(mut n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if n == 0 {
                break;
            }
        }
        out
    }

    fn abc(op: u8, a: u8, b: u8, c: u8) -> u32 {
        (op as u32) | ((a as u32) << 8) | ((b as u32) << 16) | ((c as u32) << 24)
    }
    fn ad(op: u8, a: u8, d: i16) -> u32 {
        (op as u32) | ((a as u32) << 8) | ((d as u16 as u32) << 16)
    }
    /// A `CONSTANT_STRING` (tag 3) pointing at a 1-based string-table index.
    fn const_string(string_index_1based: u64) -> Vec<u8> {
        let mut v = vec![3u8];
        v.extend(leb128(string_index_1based));
        v
    }

    #[derive(Default)]
    struct Proto {
        max_stack: u8,
        num_params: u8,
        num_upvalues: u8,
        is_vararg: u8,
        /// Raw 32-bit instruction words, INCLUDING aux words (as the on-wire stream).
        words: Vec<u32>,
        constants: Vec<Vec<u8>>,
        child_protos: Vec<usize>,
        function_name: usize,
        /// v11 feedback slots: (slot_type, pc). slot_type 0 == LFT_CALLTARGET.
        feedback: Vec<(u8, u64)>,
    }

    fn build_proto(p: &Proto, version: u8) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(p.max_stack);
        out.push(p.num_params);
        out.push(p.num_upvalues);
        out.push(p.is_vararg);
        out.push(0); // flags
        out.extend(leb128(0)); // typeinfo blob length = 0
        out.extend(leb128(p.words.len() as u64));
        for w in &p.words {
            out.extend(w.to_le_bytes());
        }
        out.extend(leb128(p.constants.len() as u64));
        for c in &p.constants {
            out.extend(c);
        }
        out.extend(leb128(p.child_protos.len() as u64));
        for &cp in &p.child_protos {
            out.extend(leb128(cp as u64));
        }
        out.extend(leb128(0)); // line_defined
        out.extend(leb128(p.function_name as u64)); // debugname (0 = none)
        out.push(0); // has line info
        out.push(0); // has debug info
        if version >= 11 {
            out.extend(leb128(p.feedback.len() as u64));
            for &(slot_type, pc) in &p.feedback {
                out.push(slot_type);
                out.extend(leb128(pc));
            }
        }
        out
    }

    fn build_chunk(
        version: u8,
        types_version: u8,
        strings: &[&str],
        protos: &[Proto],
        main: usize,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(version);
        if version >= 4 {
            out.push(types_version);
        }
        out.extend(leb128(strings.len() as u64));
        for s in strings {
            out.extend(leb128(s.len() as u64));
            out.extend(s.as_bytes());
        }
        out.extend(leb128(protos.len() as u64));
        for p in protos {
            out.extend(build_proto(p, version));
        }
        out.extend(leb128(main as u64));
        out
    }

    /// A one-proto chunk that does `LOADN r0, 1; return r0`.
    fn simple_return_proto(feedback: Vec<(u8, u64)>) -> Proto {
        Proto {
            max_stack: 1,
            words: vec![ad(LOADN, 0, 1), abc(RETURN, 0, 2, 0)],
            feedback,
            ..Default::default()
        }
    }

    #[test]
    fn v11_empty_feedback() {
        let blob = build_chunk(11, 1, &[], &[simple_return_proto(vec![])], 0);
        let out = decompile(&blob, 1, None).expect("v11 empty-feedback chunk must deserialize");
        assert!(out.contains("return"), "got: {out:?}");
    }

    #[test]
    fn v11_nonempty_feedback_consumes_exact_bytes() {
        // Single proto, main=0, but a NON-empty feedback vector. If the feedback read
        // miscounts bytes, the trailing `main` varint desyncs (reads main=1, which is
        // out of bounds for a 1-proto chunk) and this fails — so success proves the
        // per-slot read (1 byte type + 1 varint pc) is exact.
        let empty = decompile(
            &build_chunk(11, 1, &[], &[simple_return_proto(vec![])], 0),
            1,
            None,
        )
        .unwrap();
        let with_fb = decompile(
            &build_chunk(11, 1, &[], &[simple_return_proto(vec![(0, 7)])], 0),
            1,
            None,
        )
        .expect("v11 non-empty feedback must deserialize");
        assert_eq!(
            empty, with_fb,
            "feedback vector must not affect source output"
        );
    }

    #[test]
    fn v11_multislot_feedback_no_desync_across_protos() {
        // proto0 carries a 2-slot feedback vector and is followed by proto1 (the main).
        // If proto0's feedback read desyncs, proto1's header parses as garbage and the
        // chunk fails — success proves alignment is preserved across protos.
        let proto0 = simple_return_proto(vec![(0, 3), (0, 9)]);
        let proto1 = simple_return_proto(vec![]);
        let blob = build_chunk(11, 1, &[], &[proto0, proto1], 1);
        let out = decompile(&blob, 1, None).expect("multi-slot feedback must not desync");
        assert!(out.contains("return"), "got: {out:?}");
    }

    #[test]
    fn v11_unknown_feedback_slot_type_is_error_not_panic() {
        // slot_type 1 is not LFT_CALLTARGET — must surface as a clean Err, never a
        // silent skip (which would desync) or a panic.
        let blob = build_chunk(11, 1, &[], &[simple_return_proto(vec![(1, 0)])], 0);
        let err = decompile(&blob, 1, None);
        assert!(
            err.is_err(),
            "unknown feedback slot type must be a deserialize error"
        );
    }

    #[test]
    fn v11_getudataks_lifts_like_field_access() {
        // r0 = obj (global); r1 = r0.field (GETUDATAKS); return r1.
        // The aux carries the constant index in its LOW 16 bits (1 -> "field") and a
        // userdata atom-cache value (5) in its HIGH 16 bits. If the lifter failed to
        // mask with & 0xFFFF it would index constant 0x50001 (out of bounds -> panic),
        // so this fixture genuinely exercises the mask rather than passing trivially.
        let proto = Proto {
            max_stack: 2,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0, // aux: constant index 0 ("obj")
                abc(GETUDATAKS, 1, 0, 0),
                (5 << 16) | 1, // aux: high16 = atom cache, low16 = const index 1 ("field")
                abc(RETURN, 1, 2, 0),
            ],
            constants: vec![const_string(1), const_string(2)],
            ..Default::default()
        };
        let blob = build_chunk(11, 1, &["obj", "field"], &[proto], 0);
        let out = decompile(&blob, 1, None).expect("GETUDATAKS chunk must deserialize+lift");
        assert!(out.contains("field"), "GETUDATAKS key must appear: {out:?}");
    }

    #[test]
    fn v11_setudataks_lifts_like_field_write() {
        // r0 = obj (global); r1 = 5; obj.field = r1 (SETUDATAKS); return r0.
        // aux high16 (7) is the atom cache; low16 (1) is the constant index for "field".
        let proto = Proto {
            max_stack: 2,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0, // aux: "obj"
                ad(LOADN, 1, 5),
                abc(SETUDATAKS, 1, 0, 0),
                (7 << 16) | 1, // aux: atom cache | const index 1 ("field")
                abc(RETURN, 0, 2, 0),
            ],
            constants: vec![const_string(1), const_string(2)],
            ..Default::default()
        };
        let blob = build_chunk(11, 1, &["obj", "field"], &[proto], 0);
        let out = decompile(&blob, 1, None).expect("SETUDATAKS chunk must deserialize+lift");
        assert!(out.contains("field"), "SETUDATAKS key must appear: {out:?}");
    }

    #[test]
    fn v11_namecalludata_and_callfb_followup_match_namecall() {
        // The most delicate change: NAMECALLUDATA lifts like NAMECALL (with an aux & 0xFFFF
        // key mask), and a NAMECALL/NAMECALLUDATA may be followed by CALLFB instead of CALL
        // (whose injected aux NOP must be consumed by the next loop iteration, not here).
        // Build `obj:method()` three ways and assert all produce identical source.
        const NAMECALL: u8 = 20;
        let strings = ["obj", "method"];
        // aux for the method name: high16 atom cache (only honored by the UDATA variant) | low16 const idx 1.
        let masked_method_aux: u32 = (9 << 16) | 1;

        // (1) plain NAMECALL + CALL
        let nc_call = Proto {
            max_stack: 3,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0, // aux: "obj"
                abc(NAMECALL, 0, 0, 0),
                1, // aux: full aux = const idx 1 ("method")
                abc(CALL, 0, 2, 1),
                abc(RETURN, 0, 1, 0),
            ],
            constants: vec![const_string(1), const_string(2)],
            ..Default::default()
        };
        // (2) NAMECALLUDATA + CALL — exercises the aux & 0xFFFF method-key mask
        let ncu_call = Proto {
            max_stack: 3,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0,
                abc(NAMECALLUDATA, 0, 0, 0),
                masked_method_aux, // high bits must be masked off -> const idx 1
                abc(CALL, 0, 2, 1),
                abc(RETURN, 0, 1, 0),
            ],
            constants: vec![const_string(1), const_string(2)],
            ..Default::default()
        };
        // (3) NAMECALL + CALLFB — exercises the CALLFB followup + its injected NOP
        let nc_callfb = Proto {
            max_stack: 3,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0,
                abc(NAMECALL, 0, 0, 0),
                1,
                abc(CALLFB, 0, 2, 1),
                0xFFFF_FFFF, // aux: feedback slot id (sealed) — discarded
                abc(RETURN, 0, 1, 0),
            ],
            constants: vec![const_string(1), const_string(2)],
            ..Default::default()
        };

        let out_nc_call = decompile(&build_chunk(11, 1, &strings, &[nc_call], 0), 1, None).unwrap();
        let out_ncu_call =
            decompile(&build_chunk(11, 1, &strings, &[ncu_call], 0), 1, None).unwrap();
        let out_nc_callfb =
            decompile(&build_chunk(11, 1, &strings, &[nc_callfb], 0), 1, None).unwrap();

        assert!(
            out_nc_call.contains("method"),
            "method name must appear: {out_nc_call:?}"
        );
        assert!(
            out_nc_call.contains(':'),
            "should be a colon method call: {out_nc_call:?}"
        );
        assert_eq!(
            out_nc_call, out_ncu_call,
            "NAMECALLUDATA must lift identically to NAMECALL (masked key)"
        );
        assert_eq!(
            out_nc_call, out_nc_callfb,
            "a CALLFB followup must lift identically to a CALL followup"
        );
    }

    #[test]
    fn v11_callfb_lifts_identically_to_call() {
        // print(1): GETGLOBAL r0,"print"; LOADN r1,1; <CALL|CALLFB> r0; return
        let strings = ["print"];
        let call_proto = Proto {
            max_stack: 2,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0, // aux: "print"
                ad(LOADN, 1, 1),
                abc(CALL, 0, 2, 1),
                abc(RETURN, 0, 1, 0),
            ],
            constants: vec![const_string(1)],
            ..Default::default()
        };
        let callfb_proto = Proto {
            max_stack: 2,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0,
                ad(LOADN, 1, 1),
                abc(CALLFB, 0, 2, 1),
                0xFFFF_FFFF, // aux: feedback slot id (sealed) — discarded
                abc(RETURN, 0, 1, 0),
            ],
            constants: vec![const_string(1)],
            ..Default::default()
        };
        let call_out = decompile(&build_chunk(11, 1, &strings, &[call_proto], 0), 1, None).unwrap();
        let callfb_out =
            decompile(&build_chunk(11, 1, &strings, &[callfb_proto], 0), 1, None).unwrap();
        assert!(call_out.contains("print"), "got: {call_out:?}");
        assert_eq!(call_out, callfb_out, "CALLFB must lift identically to CALL");
    }

    #[test]
    fn v11_newclassmember_lifts_to_field_assign() {
        // local t = {}; t.method = 5; return t
        let proto = Proto {
            max_stack: 2,
            words: vec![
                abc(NEWTABLE, 0, 0, 0),
                0, // aux: array size
                ad(LOADN, 1, 5),
                abc(NEWCLASSMEMBER, 0, 0, 1),
                0, // aux: member-name constant index 0 ("method")
                abc(RETURN, 0, 2, 0),
            ],
            constants: vec![const_string(1)],
            ..Default::default()
        };
        let blob = build_chunk(11, 1, &["method"], &[proto], 0);
        let out = decompile(&blob, 1, None).expect("NEWCLASSMEMBER chunk must deserialize+lift");
        assert!(out.contains("method"), "member name must appear: {out:?}");
    }

    #[test]
    fn v11_cmpproto_lowers_to_fallthrough_without_panic() {
        // LOADN r0,1; CMPPROTO r0 (guard, ignored); return — must not panic.
        let proto = Proto {
            max_stack: 1,
            words: vec![
                ad(LOADN, 0, 1),
                ad(CMPPROTO, 0, 0),
                0, // aux: proto id
                abc(RETURN, 0, 1, 0),
            ],
            ..Default::default()
        };
        let blob = build_chunk(11, 1, &[], &[proto], 0);
        let out = decompile(&blob, 1, None).expect("CMPPROTO chunk must deserialize+lift");
        // No assertion on content — CMPPROTO has no source form; it must simply
        // lower to a fall-through and not panic / not desync.
        let _ = out;
    }

    #[test]
    fn v10_newclassmember_without_feedback_vector() {
        // Same NEWCLASSMEMBER program but as a v10 chunk: no feedback vector is read,
        // proving the version gate is correct (v10 must NOT try to read v11's section).
        let proto = Proto {
            max_stack: 2,
            words: vec![
                abc(NEWTABLE, 0, 0, 0),
                0,
                ad(LOADN, 1, 5),
                abc(NEWCLASSMEMBER, 0, 0, 1),
                0,
                abc(RETURN, 0, 2, 0),
            ],
            constants: vec![const_string(1)],
            ..Default::default()
        };
        let blob = build_chunk(10, 1, &["method"], &[proto], 0);
        let out = decompile(&blob, 1, None).expect("v10 NEWCLASSMEMBER chunk must deserialize");
        assert!(out.contains("method"), "got: {out:?}");
    }

    #[test]
    fn batch_matches_individual_and_preserves_order() {
        // Three distinguishable chunks so order-preservation is observable.
        let ret = build_chunk(11, 1, &[], &[simple_return_proto(vec![])], 0);

        let print_proto = Proto {
            max_stack: 2,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0, // aux: "print"
                ad(LOADN, 1, 1),
                abc(CALL, 0, 2, 1),
                abc(RETURN, 0, 1, 0),
            ],
            constants: vec![const_string(1)],
            ..Default::default()
        };
        let print = build_chunk(11, 1, &["print"], &[print_proto], 0);

        let field_proto = Proto {
            max_stack: 2,
            words: vec![
                abc(GETGLOBAL, 0, 0, 0),
                0, // aux: "obj"
                abc(GETUDATAKS, 1, 0, 0),
                (5 << 16) | 1, // aux: atom cache | const idx 1 ("field")
                abc(RETURN, 1, 2, 0),
            ],
            constants: vec![const_string(1), const_string(2)],
            ..Default::default()
        };
        let field = build_chunk(11, 1, &["obj", "field"], &[field_proto], 0);

        // Individual (serial) decompilation — the gold standard.
        let i_ret = decompile(&ret, 1, None).unwrap();
        let i_print = decompile(&print, 1, None).unwrap();
        let i_field = decompile(&field, 1, None).unwrap();
        assert_ne!(i_ret, i_print);
        assert_ne!(i_print, i_field);

        // Batch (outer-parallel) decompilation must match item-for-item, in order.
        let inputs = vec![
            super::BatchInput {
                bytecode: &ret,
                encode_key: 1,
                script_name: None,
            },
            super::BatchInput {
                bytecode: &print,
                encode_key: 1,
                script_name: None,
            },
            super::BatchInput {
                bytecode: &field,
                encode_key: 1,
                script_name: None,
            },
        ];
        let out = super::decompile_batch(&inputs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].as_ref().unwrap(), &i_ret);
        assert_eq!(out[1].as_ref().unwrap(), &i_print);
        assert_eq!(out[2].as_ref().unwrap(), &i_field);
    }

    #[test]
    fn batch_isolates_per_item_failure() {
        // Quiet the panic the bad item triggers (kept idempotent/global by Once).
        super::install_quiet_panic_hook();

        // First byte 99 is an unsupported bytecode version → the deserializer
        // `panic!`s. decompile_batch's outer catch_unwind must turn that into this
        // item's own Err, leaving the good item byte-identical and in order.
        let good = build_chunk(11, 1, &[], &[simple_return_proto(vec![])], 0);
        let good_src = decompile(&good, 1, None).unwrap();
        let garbage: &[u8] = &[99u8, 0, 0];

        let inputs = vec![
            super::BatchInput {
                bytecode: garbage,
                encode_key: 1,
                script_name: None,
            },
            super::BatchInput {
                bytecode: &good,
                encode_key: 1,
                script_name: None,
            },
        ];
        let out = super::decompile_batch(&inputs);
        assert_eq!(out.len(), 2);
        assert!(
            out[0].is_err(),
            "a panicking item must fail only its own slot, got: {:?}",
            out[0]
        );
        assert_eq!(
            out[1].as_ref().unwrap(),
            &good_src,
            "the good item must be byte-identical and stay at index 1"
        );
    }

    #[test]
    fn batch_empty_is_empty() {
        assert!(super::decompile_batch(&[]).is_empty());
    }
}

#[cfg(test)]
mod correctness_regressions {
    use base64::prelude::{Engine as _, BASE64_STANDARD};

    /// Compiled with `luau-compile --binary -O2 -g0` from a closure that rebuilds
    /// a captured table, followed by a numeric loop that reads it inside an outer
    /// while loop. The nested loop phis form a mutually-recursive SCC. Keep the
    /// auditable source beside the test even though the test embeds bytecode to
    /// avoid a runtime dependency on a particular Luau compiler binary.
    const NESTED_LOOP_UPVALUE_REBIND_SOURCE: &str =
        include_str!("../tests/fixtures/nested_loop_upvalue_rebind.luau");
    const NESTED_LOOP_UPVALUE_REBIND: &str = "CwMMBWl0ZW1zBmFjdGl2ZQRkYXRhCmZsb29ySW5kZXgDa2V5BXRhYmxlBmluc2VydAR0YXNrBHdhaXQJZ2V0Rmxvb3JzBXByaW50BXNwYXduAAMOAQEAAAApNQEAAAAAAAAKAQAAGQABABYAAQAGAQAAAgIAAAIDAABMAR0AGgUcAA8GBRgAAAAAGgYZAA8GBRgAAAAAAgcAAAIIAABMBhIAGgoRAA8LCuMBAAAAGgsOAA8LCiYCAAAAGgsLAAkMAAA2DQUAEAQNPgMAAAAQCQ1KBAAAAEo0DAMNAAAADAsIAAAcYIAVCwMBOgbt/wIAAAA6AeL/AgAAABYAAQAJAwEDAgMDAwQDBQUCAwQDBgMHBAAcYIAABAAAAAAHAAAACAAcNQAAAAAAAAATAQAARgEAAAwCAgAABACABAMBAFcCAgIAAAAAGgIQAAYCAQAMAwQAAAAwQBUDAQAVAgABBAQBADQCAAAEAwEAOAIGAAwFBgAAAFBADQYABFcFAgEBAAAAOQL6/xgA6v8LAAAAFgABAAcDCAMJBAAEAIADCgQAADBAAwsEAABQQAEAAQAAAAIABwAWAwAAAQIAB0EAAABAAAAADAEDAAAIEIAGAgAAFQECARYAAQAEBgEDCAMMBAAIEIABAQEAAAAAAg==";

    #[test]
    fn nested_loop_reads_live_rebound_upvalue_cell() {
        let bytecode = BASE64_STANDARD.decode(NESTED_LOOP_UPVALUE_REBIND).unwrap();
        let output = super::try_decompile_bytecode_with_script_name(&bytecode, 1, None).unwrap();
        assert!(NESTED_LOOP_UPVALUE_REBIND_SOURCE.contains("#cache"));

        let cell = output
            .lines()
            .find_map(|line| {
                line.strip_prefix("\tlocal ")
                    .and_then(|decl| decl.strip_suffix(" = {}"))
                    .filter(|name| !name.contains(char::is_whitespace))
            })
            .expect("fixture must declare the captured table cell");
        let stale_alias_suffix = format!(" = {cell}");

        assert!(
            !output.lines().any(|line| {
                let line = line.trim();
                line.starts_with("local ") && line.ends_with(&stale_alias_suffix)
            }),
            "must not snapshot the captured table before the loop:\n{output}"
        );
        assert!(
            output.lines().any(|line| {
                let line = line.trim();
                line.starts_with("for ") && line.ends_with(&format!("#{cell} do"))
            }),
            "the numeric loop must read the cell rebound by the child closure:\n{output}"
        );
    }
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
