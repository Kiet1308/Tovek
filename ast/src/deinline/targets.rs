//! Helper discovery, return-shape classification, and eligibility gates.
//!
//! Discovery computes the facts the site matcher will require before it tries
//! any candidate window: stable binders, return lowering, binding-hole sets,
//! capture safety, and bounded pattern metadata. The proof and replacement
//! engines consume these targets without widening their eligibility rules.

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Block, Function, If, LValue, Literal, LocalRw, RValue, RcLocal,
    Return, Select, Statement, Traverse, Upvalue,
};

use super::{
    Target, TKind, ValueAnchor, anchor_score, block_has_return, body_unsafe,
    branch_conditions_read_any, canon, collect_declared_locals, collect_reads,
    collect_written, count_local_reads, dbg_stmt_node_count, each_closure_decl,
    has_loop_void_return, is_match_trivia, is_scalar_return_value, stmt_anchor_key,
    tail_spine_len, truth_tested_params, visit_stmt_rvalues,
};

/// De-inline rejection reason (P11-C telemetry). Recorded by `deinline_reject!`
/// at each `collect_targets` gate so a corpus run can report WHICH gate refuses
/// each candidate (directing where remaining recall actually is) instead of
/// guessing. Defined unconditionally (it is tiny + `dead_code` without the
/// feature) so call sites compile in both modes.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum RejectReason {
    /// Binder written more than once (reassigned) — not a stable call target.
    TargetStillReferenced,
    /// Variadic helper — `...`→multi-arg arity unprovable (P5-B).
    Variadic,
    /// Body contains a closure / goto / label / close / comment / lifter for-node.
    UnsafeBody,
    /// Return shape refused: multi-value, mixed void/value, bare-vararg leaf, or a
    /// non-terminal value return (`classify_returns` / `value_leaf_shape`).
    UnsupportedReturnShape,
    /// Canon collapsed the body to nothing.
    EmptyPattern,
    /// Below the `anchors >= 2` readability floor (P3, kept refused).
    LowAnchorScore,
    /// Capture shape cannot be proven within the deterministic target budget.
    ShapeBudget,
    /// A value helper writes a parameter: its sites start with `local L = ARG`
    /// copies, which only the void matcher consumes.
    WrittenValueParameter,
}

impl RejectReason {
    fn counter(self) -> &'static str {
        match self {
            Self::TargetStillReferenced => "reject_reassigned_binder",
            Self::Variadic => "reject_variadic",
            Self::UnsafeBody => "reject_unsafe_body",
            Self::UnsupportedReturnShape => "reject_return_shape",
            Self::EmptyPattern => "reject_empty_pattern",
            Self::LowAnchorScore => "reject_low_anchors",
            Self::ShapeBudget => "reject_shape_budget",
            Self::WrittenValueParameter => "reject_written_value_parameter",
        }
    }
}

/// Trace a de-inline target rejection. With the `deinline_trace` feature it prints
/// the gate + function name to stderr. Optional JSON profiling records only
/// a static reason counter; it never evaluates the function-name argument.
macro_rules! deinline_reject {
    ($reason:expr, $name:expr) => {{
        if crate::telemetry::enabled() { crate::telemetry::count($reason.counter(), 1); }
        #[cfg(feature = "deinline_trace")]
        {
            let _r: RejectReason = $reason;
            eprintln!("DEINLINE_REJECT\treason={:?}\tfn={}", _r, $name);
        }
    }};
}

/// Whether some `local f = function ... end` passes the gates of
/// [`collect_targets`] that depend only on the helper itself (a necessary
/// condition for any target).
pub(super) fn any_structural_target(body: &Block) -> bool {
    let mut found = false;
    each_closure_decl(&body.0, &mut |_, function| {
        if found {
            return;
        }
        let g = function.lock();
        if g.is_variadic || body_unsafe(&g.body.0) {
            return;
        }
        let (body, _) = pattern_body(&g.body.0, &g.parameters);
        let Some((kind, falls_off)) = classify_returns(&body) else { return; };
        let pattern = if falls_off { canon(&returning_nil(&body)) } else { canon(&body) };
        if pattern.is_empty() {
            return;
        }
        let loop_exit = kind == TKind::Value && !value_leaf_shape(&pattern) && loop_return_split(&pattern).is_some();
        if anchor_score(&pattern, &g.parameters) + LOOP_EXIT_ANCHORS * usize::from(loop_exit) < 2 {
            return;
        }
        found = match kind {
            TKind::Void => !block_has_return(&pattern)
                || has_loop_void_return(&pattern, false),
            TKind::Value => value_leaf_shape(&pattern) || loop_exit,
        };
    });
    found
}

/// The part of a helper's body its sites repeat, and the locals it returns
/// in place of a call's results ([`local_tuple_return`],
/// [`branch_tuple_return`]; usually none).
fn pattern_body<'a>(body: &'a [Statement], parameters: &[RcLocal]) -> (std::borrow::Cow<'a, [Statement]>, Vec<RcLocal>) {
    if let Some((rest, returned)) = local_tuple_return(body, parameters) {
        return (std::borrow::Cow::Borrowed(rest), returned);
    }
    if let Some((lowered, returned)) = branch_tuple_return(body, parameters) {
        return (std::borrow::Cow::Owned(lowered), returned);
    }
    (std::borrow::Cow::Borrowed(body), Vec::new())
}

/// A helper returning the same number (two or more) of values on every path,
/// through branches: `local i = find(s, " "); if i then return sub(s, 1, i -
/// 1), i end; return s, 1`. Luau inlines `local name, at = split(s)` as the
/// body with each `return` storing into the caller's locals, and a local the
/// body returns sharing the register of its result: `local i = find(s, " ");
/// local name; if i then name = sub(s, 1, i - 1) else name = s; i = 1 end`.
/// Returns that body, the stores in place of the returns, and the locals
/// standing for the results in order. A result is the body's own top-level
/// local when it is the only local returned in that position, no later
/// result reads it and no closure captures it; otherwise a fresh local
/// declared before its first store.
pub(super) fn branch_tuple_return(body: &[Statement], parameters: &[RcLocal]) -> Option<(Vec<Statement>, Vec<RcLocal>)> {
    if !block_has_return(body) {
        return None;
    }
    let tree = unguard_returns(body)?;
    let mut leaves: Vec<Vec<RValue>> = Vec::new();
    collect_return_leaves(&tree, &mut leaves);
    let arity = leaves.first()?.len();
    // A call before the last value is cut to one result, as its store is; the
    // last value must be one value already.
    let fixed = |leaf: &Vec<RValue>| {
        let (last, rest) = leaf.split_last().unwrap();
        rest.iter().all(is_truncatable_return_value)
            && (is_scalar_return_value(last) || matches!(last, RValue::Select(Select::Call(_) | Select::MethodCall(_))))
    };
    if leaves.len() < 2 || arity < 2 || leaves.iter().any(|leaf| leaf.len() != arity || !fixed(leaf)) {
        return None;
    }
    let declared: Vec<RcLocal> = body
        .iter()
        .filter_map(|statement| match statement {
            Statement::Assign(assign) if assign.prefix => Some(assign.left.iter().filter_map(LValue::as_local).cloned()),
            _ => None,
        })
        .flatten()
        .filter(|local| !parameters.contains(local))
        .collect();
    let mut results: Vec<RcLocal> = Vec::with_capacity(arity);
    let mut fresh: Vec<RcLocal> = Vec::new();
    for slot in 0..arity {
        let mut own: Option<&RcLocal> = None;
        let mut unique = true;
        for leaf in &leaves {
            if let RValue::Local(local) = &leaf[slot]
                && declared.contains(local)
            {
                match own {
                    None => own = Some(local),
                    Some(other) if other != local => unique = false,
                    _ => {}
                }
            }
        }
        let shared = own.filter(|local| {
            unique
                && !results.contains(local)
                && !closures_capture_any(body, std::slice::from_ref(local))
                // The stores run in result order: a later result may not
                // read the local the store for this one overwrites.
                && leaves.iter().all(|leaf| {
                    leaf[slot + 1..].iter().all(|value| !value.values_read().iter().any(|read| *read == *local))
                })
        });
        match shared {
            Some(local) => results.push(local.clone()),
            None => {
                let result = RcLocal::default();
                fresh.push(result.clone());
                results.push(result);
            }
        }
    }
    let mut lowered = store_returns(&tree, &results);
    for result in fresh {
        let first_store = lowered.iter().position(|statement| {
            let mut written = FxHashSet::default();
            collect_written(std::slice::from_ref(statement), &mut written);
            written.contains(&result)
        })?;
        let mut declaration = Assign::new(vec![LValue::Local(result)], Vec::new());
        declaration.prefix = true;
        lowered.insert(first_store, declaration.into());
    }
    Some((lowered, results))
}

/// `stmts` with every `return` last in its block: a guard `if c then ...
/// return x end; rest` takes `rest` as its `else`. Refuses a `return` inside
/// a loop, a void `return` and a path that runs off the end.
fn unguard_returns(stmts: &[Statement]) -> Option<Vec<Statement>> {
    let mut out = Vec::with_capacity(stmts.len());
    for (index, statement) in stmts.iter().enumerate() {
        let rest = &stmts[index + 1..];
        let rest_is_empty = rest.iter().all(is_match_trivia);
        match statement {
            Statement::Return(ret) if ret.values.is_empty() => return None,
            Statement::Return(_) => {
                out.push(statement.clone());
                return rest_is_empty.then_some(out);
            }
            Statement::If(f) if statement_has_return(statement) => {
                let then_block = unguard_returns(&f.then_block.lock().0)?;
                let else_stmts = f.else_block.lock().0.clone();
                let else_block = if else_stmts.iter().all(is_match_trivia) && !rest_is_empty {
                    // `if c then ... return x end; rest`
                    unguard_returns(rest)?
                } else if rest_is_empty {
                    unguard_returns(&else_stmts)?
                } else {
                    return None;
                };
                out.push(If::new(f.condition.clone(), Block(then_block), Block(else_block)).into());
                return Some(out);
            }
            _ if statement_has_return(statement) => return None,
            _ => out.push(statement.clone()),
        }
    }
    None
}

fn statement_has_return(statement: &Statement) -> bool {
    block_has_return(std::slice::from_ref(statement))
}

/// The values of every `return` of a tree from [`unguard_returns`].
fn collect_return_leaves(stmts: &[Statement], out: &mut Vec<Vec<RValue>>) {
    for statement in stmts {
        match statement {
            Statement::Return(ret) => out.push(ret.values.clone()),
            Statement::If(f) => {
                collect_return_leaves(&f.then_block.lock().0, out);
                collect_return_leaves(&f.else_block.lock().0, out);
            }
            _ => {}
        }
    }
}

/// A tree from [`unguard_returns`] with each `return v1, v2, ...` storing
/// `results[k] = vk` instead; a result returning itself needs no store.
fn store_returns(stmts: &[Statement], results: &[RcLocal]) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len() + results.len());
    for statement in stmts {
        match statement {
            Statement::Return(ret) => {
                for (result, value) in results.iter().zip(&ret.values) {
                    if !matches!(value, RValue::Local(local) if local == result) {
                        out.push(Assign::new(vec![LValue::Local(result.clone())], vec![value.clone()]).into());
                    }
                }
            }
            Statement::If(f) => out.push(
                If::new(
                    f.condition.clone(),
                    Block(store_returns(&f.then_block.lock().0, results)),
                    Block(store_returns(&f.else_block.lock().0, results)),
                )
                .into(),
            ),
            _ => out.push(statement.clone()),
        }
    }
    out
}

pub(super) fn collect_targets(
    body: &Block,
    write_counts: &FxHashMap<RcLocal, usize>,
    captures: std::rc::Rc<crate::deinline_safety::CaptureSafety>,
) -> Vec<Target> {
    // P4: a write-once census (`write_counts`, computed once by the caller — see the
    // invariance note in `deinline`) replaces the old `Arc::count(&l) == 1` gate.
    // The refcount gate was both too STRICT (it dropped any helper that still has a
    // surviving direct call `f(...)`, since each call-site `RValue::Local(f)`
    // raises the count) and UNSTABLE across the fixed-point loop (the first
    // emitted `f(args)` clones the binder, so a multi-site target's count rises
    // above 1 and it is dropped on the next iteration — defeating the chained /
    // nested reconstruction P1/P6 rely on). The census instead counts WRITES to
    // the binder: a binder assigned only by its own `local f = function…end`
    // declaration (write_count == 1) is never reassigned, so an emitted `f(args)`
    // always resolves to this function — sound regardless of how many times `f`
    // is read/called elsewhere. Mirrors the §7 expression de-inliner's proven gate
    // (`expr_deinline::collect_expr_targets`); the census is shared (not copied) so
    // the two cannot drift.
    let mut decls: Vec<(RcLocal, Arc<Mutex<Function>>)> = Vec::new();
    each_closure_decl(&body.0, &mut |l, fa| {
        decls.push((l.clone(), fa.clone()));
    });

    let mut targets = Vec::new();
    for (f_local, func) in decls {
        crate::telemetry::count("candidate_binders", 1);
        // gate: the binder is written exactly once (its declaration) — never
        // reassigned, so `f(args)` is unambiguous (see the census note above).
        if write_counts.get(&f_local).copied().unwrap_or(0) != 1 {
            deinline_reject!(RejectReason::TargetStillReferenced, "<binder>");
            continue;
        }
        let g = func.lock();
        // P5-A: drop the `g.name.is_none()` gate. `g.name` is only the bytecode
        // debugname — never consumed by emission (the call/marker use `f_local`,
        // line ~1377) nor the formatter; only this gate and a debug-trace string
        // read it. Refusing a name-less closure therefore dropped the `name == 0`
        // subset of the IDENTICAL `local f = function…end` shape for no soundness
        // reason. (Variadic stays refused — see P5-B: `...`→multi-arg arity is
        // unprovable from the inlined body, so it is left for `body_unsafe`-style
        // refusal here.) Every soundness gate downstream is unchanged.
        if g.is_variadic {
            deinline_reject!(
                RejectReason::Variadic,
                g.name.as_deref().unwrap_or("<anon>")
            );
            continue;
        }
        // Its code runs a call frame deeper inside the helper, where reading
        // frames gives another answer.
        if body_unsafe(&g.body.0) || captures.reads_frames(&g.body.0) {
            deinline_reject!(
                RejectReason::UnsafeBody,
                g.name.as_deref().unwrap_or("<anon>")
            );
            continue;
        }
        let (body, returns) = pattern_body(&g.body.0, &g.parameters);
        let (kind, falls_off) = match classify_returns(&body) {
            Some(classified) => classified,
            None => {
                // multi-return / mixed / bare-vararg leaf / non-terminal value return
                deinline_reject!(
                    RejectReason::UnsupportedReturnShape,
                    g.name.as_deref().unwrap_or("<anon>")
                );
                continue;
            }
        };
        let shape = crate::deinline_safety::CaptureSafety::new(&g.body);
        if !shape.complete() || shape.nodes() > 2048 {
            deinline_reject!(RejectReason::ShapeBudget, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        let pat = if falls_off { canon(&returning_nil(&body)) } else { canon(&body) };
        if pat.is_empty() {
            deinline_reject!(
                RejectReason::EmptyPattern,
                g.name.as_deref().unwrap_or("<anon>")
            );
            continue;
        }
        let cps_loop_return = kind == TKind::Void && has_loop_void_return(&pat, false);
        let loop_exit_at = (kind == TKind::Value && !value_leaf_shape(&pat))
            .then(|| loop_return_split(&pat))
            .flatten();
        match kind {
            // Ordinary void targets must canon away every return.  A narrowly
            // recognized loop-return target is retained for the continuation-
            // proving CPS matcher below.
            TKind::Void => {
                if block_has_return(&pat) && !cps_loop_return {
                    deinline_reject!(
                        RejectReason::UnsupportedReturnShape,
                        g.name.as_deref().unwrap_or("<anon>")
                    );
                    continue;
                }
            }
            // value: every leaf must be a single value-return (the result),
            // or a return from inside a loop (`loop_return_split`).
            TKind::Value => {
                if !value_leaf_shape(&pat) && loop_exit_at.is_none() {
                    deinline_reject!(
                        RejectReason::UnsupportedReturnShape,
                        g.name.as_deref().unwrap_or("<anon>")
                    );
                    continue;
                }
            }
        }
        if crate::env_flag!("DEINLINE_ANCHOR_TRACE") {
            let a = anchor_score(&pat, &g.parameters);
            let nc: usize = pat.iter().map(crate::deinline::dbg_stmt_node_count).sum();
            let nm = g.name.as_deref().unwrap_or("<none>");
            eprintln!(
                "ANCHORTRACE\tanchors={}\tstmts={}\tnodes={}\tkind={:?}\tname={}\tlocal={}\tcps={}",
                a,
                pat.len(),
                nc,
                match kind {
                    TKind::Void => "Void",
                    TKind::Value => "Value",
                },
                nm,
                f_local,
                cps_loop_return,
            );
        }
        if anchor_score(&pat, &g.parameters) + LOOP_EXIT_ANCHORS * usize::from(loop_exit_at.is_some()) < 2 {
            deinline_reject!(
                RejectReason::LowAnchorScore,
                g.name.as_deref().unwrap_or("<anon>")
            );
            continue;
        }
        // Written params (see `Target::written_params`) are matched as callee
        // locals: the site materialises them as `local L = ARG` copies.
        let mut body_written: FxHashSet<RcLocal> = FxHashSet::default();
        collect_written(&g.body.0, &mut body_written);
        let written_params: Vec<RcLocal> = g
            .parameters
            .iter()
            .filter(|p| body_written.contains(*p))
            .cloned()
            .collect();
        if kind == TKind::Value && !written_params.is_empty() {
            deinline_reject!(RejectReason::WrittenValueParameter, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        let params: FxHashSet<RcLocal> = g
            .parameters
            .iter()
            .filter(|p| !body_written.contains(*p))
            .cloned()
            .collect();
        let specializable = branch_conditions_read_any(&pat, &params);
        let (truth_params, optional_params) = truth_tested_params(&pat, &params, &g.parameters);
        let mut locals: FxHashSet<RcLocal> = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        for p in &params {
            locals.remove(p);
        }
        locals.extend(written_params.iter().cloned());
        // A parameter whose argument may run code before the body: read once,
        // first, with the argument evaluated where it stands (`first_reads`)
        // or, a register local, read when its operation runs
        // (`first_register_reads`). The body's own locals are its registers;
        // an outer local is its upvalue, fetched where it stands.
        let facts = crate::evaluation_order::Body {
            registers: &|local| params.contains(local) || locals.contains(local),
            unchanged: &|value| captures.unchanged_by_calls(value),
        };
        let first_read = |p: &RcLocal, register: bool| {
            params.contains(p)
                && crate::evaluation_order::block_reads_first(&pat, p, register, &facts)
                && count_local_reads(&pat, p) == 1
        };
        let first_reads = g.parameters.iter().filter(|p| first_read(p, false)).cloned().collect();
        let first_register_reads = g.parameters.iter().filter(|p| first_read(p, true)).cloned().collect();
        let mut pat_reads: FxHashSet<RcLocal> = FxHashSet::default();
        collect_reads(&pat, &mut pat_reads);
        let mut free_cells: Vec<RcLocal> = pat_reads
            .into_iter()
            .filter(|l| !params.contains(l) && !locals.contains(l) && captures.closure_written(l))
            .collect();
        free_cells.sort();
        // F6a: parameters the body never READS. Build the read-set in ONE pass over
        // the RAW body (`g.body.0`) — O(body + params), not a per-param re-traversal,
        // and over the body the helper ACTUALLY runs, which decouples F6a soundness
        // from canon's drop-semantics (canon only ever removes read-free statements,
        // so this yields the identical set today, but is self-evidently correct even
        // if canon ever changes). `collect_reads` also enters the bodies of the
        // closures `body_unsafe` admits, so a read there counts too.
        //
        // A WRITTEN param needs no special case: it appears in the body as an
        // `LValue::Local(p)`, which `unify_local`'s param-identity branch forces the
        // candidate to write through the *same* callee local — impossible at the call
        // site (the caller writes a different register), so the whole match fails in
        // `unify_block` BEFORE `try_unify_site`'s args loop. That holds whether or not
        // the param is also read: a pure write-only `p = X` IS classified unread here
        // (it has zero reads), but it can still never receive a wrong `nil`, because
        // the site is refused upstream.
        let mut body_reads: FxHashSet<RcLocal> = FxHashSet::default();
        collect_reads(&g.body.0, &mut body_reads);
        let unread: FxHashSet<RcLocal> = g
            .parameters
            .iter()
            .filter(|p| !body_reads.contains(*p))
            .cloned()
            .collect();
        let func_ptr = Arc::as_ptr(&func);
        let pat_raw_len = body.len();
        let pat_spine_len = tail_spine_len(&body);
        let param_order = g.parameters.clone();
        let pat0_kind = std::mem::discriminant(&pat[0]);
        // §8 + P6: a Value target whose canon'd body is `<K leading non-branch
        // callee statements> ; <value branch>` is matched at the call site with the
        // RESULT-register decl INTERPOSED after those K leading statements (those
        // are the callee's own locals/effects, computed before the value is
        // produced). `value_leaf_shape` guarantees the value branch is `pat`'s
        // unique LAST statement and the prefix is return-free, so the prefix is
        // exactly `pat[..k]` with `k == pat.len() - 1`.
        //
        // §8 scoped this to K==1; P6 generalises to 1..=MAX_PREFIX. The prefix
        // statements must be NON-BRANCH (`Assign`/`Call`/`MethodCall`) for two
        // reasons: (1) it keeps the `pat0_kind` O(1) prefilter sound (canon
        // preserves the first surviving statement's variant, and these variants
        // survive canon unchanged — a leading `If` prefix would be folded/unguarded
        // and is left on the `AtResultDecl` path); (2) a branch in the prefix would
        // be a different inlining shape. Soundness is otherwise unchanged: every
        // whole-window analysis in `match_value_prefixed` (exact unify over the
        // union, region-write arg-safety, RESULT identity + never-read, callee-temp
        // liveness) runs over `prefix ++ region`, so K>1 cannot smuggle anything
        // past the gates the K==1 path already enforces. MAX_PREFIX bounds the
        // per-position work (the matcher's single per-width loop is unchanged).
        const MAX_PREFIX: usize = 4;
        let (value_anchor, prefix_len) = if kind == TKind::Value && pat.len() >= 2 && loop_exit_at.is_none() {
            let k = pat.len() - 1;
            if (1..=MAX_PREFIX).contains(&k)
                && pat[..k].iter().all(|s| {
                    matches!(
                        s,
                        Statement::Assign(_) | Statement::Call(_) | Statement::MethodCall(_)
                    )
                })
            {
                (ValueAnchor::AtPrefix, k)
            } else {
                (ValueAnchor::AtResultDecl, 0)
            }
        } else {
            (ValueAnchor::AtResultDecl, 0)
        };
        // A written-param target's site always STARTS with the `local L = ARG`
        // copies, so its head anchor is a (prefix) `Assign`, not `pat[0]`.
        let (pat0_kind, pat0_anchor_key) = if written_params.is_empty() {
            (pat0_kind, stmt_anchor_key(&pat[0]))
        } else {
            (
                std::mem::discriminant(&Statement::Assign(Assign::new(Vec::new(), Vec::new()))),
                None,
            )
        };
        crate::call_origins::register_callee(f_local.stable_id(), g.bytecode_proto_id);
        drop(g);
        let pat_nodes = pat.iter().map(dbg_stmt_node_count).sum();
        targets.push(Target {
            f_local,
            func_ptr,
            kind,
            pat,
            pat_raw_len,
            pat_spine_len,
            pat_nodes,
            focused: true,
            value_anchor,
            prefix_len,
            pat0_kind,
            pat0_anchor_key,
            params,
            locals,
            param_order,
            written_params,
            unread,
            first_reads,
            first_register_reads,
            free_cells,
            specializable,
            truth_params,
            optional_params,
            specializations: Default::default(),
            falls_off,
            cps_loop_return,
            loop_exit_at,
            returns,
            captures: captures.clone(),
            search: Default::default(),
        });
    }
    targets
}

/// Decide the return shape: `Void` (no value returns), `Value` (single scalar
/// value on every path), or refuse (`None`) for multi-return, mixed void/value,
/// a non-scalar value (call/method/vararg/select — arity unprovable), or a body
/// whose value returns are not all terminal leaves after canonicalization.
/// With the flag `Target::falls_off`: a value function that may also return
/// nothing is matched as [`returning_nil`] of its body.
pub(super) fn classify_returns(body: &[Statement]) -> Option<(TKind, bool)> {
    let mut has_void = false;
    let mut has_value = false;
    if returns_bad(body, &mut has_void, &mut has_value) {
        return None;
    }
    if !has_value {
        return Some((TKind::Void, false));
    }
    if !has_void {
        let pattern = canon(body);
        if value_leaf_shape(&pattern) || loop_return_split(&pattern).is_some() {
            return Some((TKind::Value, false));
        }
    }
    let filled = canon(&returning_nil(body));
    (value_leaf_shape(&filled) || loop_return_split(&filled).is_some()).then_some((TKind::Value, true))
}

/// What a value return from inside a loop counts toward the anchor floor
/// (`anchor_score`): its copies leave the loop through a stored result, a
/// flag and a `break` (`match_value_loop`), which plain caller code does not
/// write, so it passes the floor alone (`for i = 1, #t do if t[i] == v then
/// return i end end`).
const LOOP_EXIT_ANCHORS: usize = 2;

/// A value helper returning from inside a loop: `PRE; for … do … return x …
/// end; TAIL`, where `PRE` returns nothing, every `return` of the loop gives
/// one value outside any loop nested in it, and `TAIL` is a value leaf shape
/// (`value_leaf_shape`). Returns the loop's index in `pattern`. Its copies
/// store the value and leave the loop through a flag ([`super::match_value_loop`]).
pub(super) fn loop_return_split(pattern: &[Statement]) -> Option<usize> {
    fn exits_once(stmts: &[Statement], found: &mut bool) -> bool {
        stmts.iter().all(|statement| match statement {
            Statement::Return(ret) => {
                *found = true;
                ret.values.len() == 1 && is_truncatable_return_value(&ret.values[0])
            }
            Statement::If(branch) => {
                exits_once(&branch.then_block.lock().0, found) && exits_once(&branch.else_block.lock().0, found)
            }
            // A `return` two loops deep leaves through two flags.
            other => !statement_has_return(other),
        })
    }
    let at = pattern.iter().position(|statement| {
        matches!(statement, Statement::GenericFor(_) | Statement::NumericFor(_) | Statement::While(_))
            && statement_has_return(statement)
    })?;
    let mut found = false;
    let exits = match &pattern[at] {
        Statement::GenericFor(node) => exits_once(&node.block.lock().0, &mut found),
        Statement::NumericFor(node) => exits_once(&node.block.lock().0, &mut found),
        Statement::While(node) => exits_once(&node.block.lock().0, &mut found),
        _ => false,
    };
    (exits && found && !block_has_return(&pattern[..at]) && value_leaf_shape(&pattern[at + 1..])).then_some(at)
}

/// A helper that builds several values in its own locals and returns them:
/// `local total = 0; local function set(...) ... end; local function
/// key(...) ... end; return set, key`. Luau inlines `local set, key =
/// makeTrack(x)` as the body alone, the caller's locals taking the place of
/// the returned ones. Returns the body before the `return` and the returned
/// locals, when that is exactly what a site can show:
/// * two or more values: distinct locals, each declared by a top-level
///   statement (a single value is a [`TKind::Value`] target), then any
///   parameters the body never writes, which hand back the arguments
///   (`return names, list` -> `local names = toNames(children)`);
/// * no other `return`, so every call reaches this one;
/// * no closure of the body captures the locals. A captured one is shared
///   with the caller's code at the site, but a snapshot after a call, so a
///   later write to it would be seen differently.
pub(super) fn local_tuple_return<'a>(body: &'a [Statement], parameters: &[RcLocal]) -> Option<(&'a [Statement], Vec<RcLocal>)> {
    let (Statement::Return(ret), rest) = body.split_last()? else {
        return None;
    };
    if ret.values.len() < 2 || block_has_return(rest) {
        return None;
    }
    let mut returned: Vec<RcLocal> = Vec::with_capacity(ret.values.len());
    let mut written: Option<FxHashSet<RcLocal>> = None;
    for value in &ret.values {
        let RValue::Local(local) = value else { return None };
        if parameters.contains(local) {
            // The caller passed it, so its value is already the caller's.
            let written = written.get_or_insert_with(|| {
                let mut written = FxHashSet::default();
                collect_written(rest, &mut written);
                written
            });
            if written.contains(local) {
                return None;
            }
            continue;
        }
        // Locals first: one after a parameter would need a placeholder.
        if written.is_some() || returned.contains(local) {
            return None;
        }
        returned.push(local.clone());
    }
    if returned.is_empty() {
        return None;
    }
    let declared = |local: &RcLocal| {
        rest.iter().any(|statement| {
            matches!(statement, Statement::Assign(a)
                if a.prefix && a.left.iter().any(|l| matches!(l, LValue::Local(x) if x == local)))
        })
    };
    if !returned.iter().all(declared) || closures_capture_any(rest, &returned) {
        return None;
    }
    Some((rest, returned))
}

/// Whether a closure created by `stmts` captures one of `locals`. A closure
/// nested in another reaches an outer local only through its parent's
/// captures, so closure bodies are not entered.
fn closures_capture_any(stmts: &[Statement], locals: &[RcLocal]) -> bool {
    fn in_value(value: &RValue, locals: &[RcLocal]) -> bool {
        if let RValue::Closure(closure) = value {
            return closure.upvalues.iter().any(|upvalue| {
                let (Upvalue::Copy(captured) | Upvalue::Ref(captured)) = upvalue;
                locals.contains(captured)
            });
        }
        let mut found = false;
        value.visit_rvalues(&mut |child| {
            found = in_value(child, locals);
            !found
        });
        found
    }
    stmts.iter().any(|statement| {
        let mut found = false;
        visit_stmt_rvalues(statement, &mut |value| {
            found = in_value(value, locals);
            !found
        });
        found
            || match statement {
                Statement::If(f) => {
                    closures_capture_any(&f.then_block.lock().0, locals)
                        || closures_capture_any(&f.else_block.lock().0, locals)
                }
                Statement::While(w) => closures_capture_any(&w.block.lock().0, locals),
                Statement::Repeat(r) => closures_capture_any(&r.block.lock().0, locals),
                Statement::NumericFor(nf) => closures_capture_any(&nf.block.lock().0, locals),
                Statement::GenericFor(gf) => closures_capture_any(&gf.block.lock().0, locals),
                _ => false,
            }
    })
}

/// `body` with every exit that returns nothing returning `nil`: a void
/// `return`, and the end of the body where it can be reached. This is the
/// copy Luau inlines for a single result, which stores `nil` on those paths
/// (`if c then r = x else r = nil end` for `if c then return x end`).
pub(super) fn returning_nil(body: &[Statement]) -> Vec<Statement> {
    fn fill(stmts: &[Statement]) -> Vec<Statement> {
        let mut out: Vec<Statement> = stmts.iter().map(fill_statement).collect();
        if !matches!(out.last(), Some(Statement::Return(_))) && !ends_in_if_returning(&out) {
            out.push(Return::new(vec![RValue::Literal(Literal::Nil)]).into());
        }
        out
    }
    fn ends_in_if_returning(stmts: &[Statement]) -> bool {
        // An `if` at the end was filled arm by arm; both arms now return.
        matches!(stmts.last(), Some(Statement::If(f))
            if matches!(f.then_block.lock().0.last(), Some(Statement::Return(_)))
                && matches!(f.else_block.lock().0.last(), Some(Statement::Return(_))))
    }
    fn fill_statement(statement: &Statement) -> Statement {
        match statement {
            Statement::Return(r) if r.values.is_empty() => Return::new(vec![RValue::Literal(Literal::Nil)]).into(),
            Statement::If(f) => If::new(
                f.condition.clone(),
                Block(fill_nested(&f.then_block.lock().0)),
                Block(fill_nested(&f.else_block.lock().0)),
            )
            .into(),
            other => other.clone(),
        }
    }
    // Inside an `if` arm, the end falls through to what follows the `if`:
    // only void returns change there. The body's last `if` is filled whole.
    fn fill_nested(stmts: &[Statement]) -> Vec<Statement> {
        stmts.iter().map(fill_statement).collect()
    }
    let mut out = fill_nested(body);
    match out.last_mut() {
        Some(Statement::If(f)) => {
            let filled = If::new(f.condition.clone(), Block(fill(&f.then_block.lock().0)), Block(fill(&f.else_block.lock().0)));
            *out.last_mut().unwrap() = filled.into();
        }
        Some(Statement::Return(_)) => {}
        _ => out.push(Return::new(vec![RValue::Literal(Literal::Nil)]).into()),
    }
    out
}

fn returns_bad(stmts: &[Statement], has_void: &mut bool, has_value: &mut bool) -> bool {
    for s in stmts {
        let bad = match s {
            Statement::Return(r) => {
                if r.values.is_empty() {
                    *has_void = true;
                    false
                } else if r.values.len() == 1 {
                    *has_value = true;
                    // P7-A: a single-value return is admissible if it is TRUNCATABLE
                    // — a scalar, or a call/method-call leaf (`return g(x)`) which a
                    // single-LHS `RESULT = g(x)` inlined site truncates to exactly
                    // one value (the candidate shape itself proves the arity). A
                    // bare `...`/`Select::VarArg` is still refused (no provable
                    // single-value truncation point).
                    !is_truncatable_return_value(&r.values[0])
                } else {
                    true // multi-value return
                }
            }
            Statement::If(f) => {
                returns_bad(&f.then_block.lock().0, has_void, has_value)
                    || returns_bad(&f.else_block.lock().0, has_void, has_value)
            }
            Statement::While(w) => returns_bad(&w.block.lock().0, has_void, has_value),
            Statement::Repeat(r) => returns_bad(&r.block.lock().0, has_void, has_value),
            Statement::NumericFor(nf) => returns_bad(&nf.block.lock().0, has_void, has_value),
            Statement::GenericFor(gf) => returns_bad(&gf.block.lock().0, has_void, has_value),
            _ => false,
        };
        if bad {
            return true;
        }
    }
    false
}

/// P7-A: a return value admissible for a Value target on the RESULT-decl path.
/// Superset of `is_scalar_return_value` that ALSO admits a call/method-call leaf
/// (`return g(x)`): at the inlined site such a leaf was lowered to a single-LHS
/// `RESULT = g(x)`, which Lua truncates to exactly one value — so the candidate
/// shape itself proves the arity, and the reconstruction `local RESULT = f(args)`
/// (also single-LHS) truncates identically. A bare `...` / `Select::VarArg` is
/// still refused: its multi-value spread has no provable single-value truncation
/// point. SOUND ONLY on the RESULT-decl path: the (Return, Assign) unify arm
/// requires `ca.left.len() == 1`, so a multi-value site (`local a, b = g(x)`)
/// never matches; the void-tail path (`value_tail_ret`) keeps its own call-leaf
/// refusal; and the §7 expression de-inliner keeps the stricter
/// `is_scalar_return_value` (its expression slot may be a multi-value position).
fn is_truncatable_return_value(rv: &RValue) -> bool {
    !matches!(rv, RValue::VarArg(_) | RValue::Select(Select::VarArg(_)))
}

/// Value targets are only sound when, after `canon`, every `return X` is a
/// terminal leaf. A `return` in a prefix statement or loop body would skip a
/// suffix that the lowered `RESULT = X` candidate still runs.
pub(super) fn value_leaf_shape(stmts: &[Statement]) -> bool {
    let Some((last, prefix)) = stmts.split_last() else {
        return false;
    };
    if block_has_return(prefix) {
        return false;
    }
    match last {
        // P7-A: a call/method leaf is admissible here (truncated by the single-LHS
        // RESULT lowering); bare `...`/`Select::VarArg` stays refused.
        Statement::Return(r) => r.values.len() == 1 && is_truncatable_return_value(&r.values[0]),
        Statement::If(f) => {
            let then_ok = value_leaf_shape(&f.then_block.lock().0);
            let else_ok = {
                let else_block = f.else_block.lock();
                !else_block.0.is_empty() && value_leaf_shape(&else_block.0)
            };
            then_ok && else_ok
        }
        _ => false,
    }
}
