//! Expression-level de-inliner (proposal §7): reverses Luau `-O2` inlining of a
//! small pure scalar helper whose body was copied into a caller as a
//! *sub-expression* of a larger condition / RValue — the case the
//! statement-region [`crate::deinline`] pass cannot see.
//!
//! Flagship (`DecompiledTest/Client/ChatTipsClient.luau`):
//!
//! ```luau
//! local function isFiniteNumber(p)
//!     return if typeof(p) == "number" and (p == p and p > -math.huge) then p < math.huge else false
//! end
//!
//! -- caller, with isFiniteNumber inlined as an expression under `not (...)`:
//! local v4 = not (if typeof(v2) == "number" and (v2 == v2 and v2 > -math.huge) then v2 < math.huge else false)
//!     and 240 or math.clamp(math.floor(v2 + 0.5), 5, 86400)
//! ```
//!
//! becomes
//!
//! ```luau
//! local v4 = not isFiniteNumber(v2) and 240 or math.clamp(math.floor(v2 + 0.5), 5, 86400)
//! ```
//!
//! # Where this runs, and why it matters
//!
//! This pass MUST run AFTER `reconstruct_conditional_expressions` (so
//! `IfExpression`/`and`/`or` exist) but BEFORE `normalize_conditions`. The latter
//! pushes `not` inward by De Morgan: the call-site copy above, sitting under
//! `not (...)`, would be rewritten into a disjunction
//! (`typeof(v2) ~= "number" or ... or not (v2 < math.huge)`) while the standalone
//! helper body collapses to a conjunction — two structurally unrelated trees an
//! EXACT unifier can never bridge. Run *before* normalization and both the helper
//! body `E` and the embedded copy are the same freshly-reconstructed tree; the
//! `not (...)` wrapper is preserved verbatim by the in-place rewrite, and the
//! later `normalize_conditions` keeps `not isFiniteNumber(v2)` (it cannot De Morgan
//! a call). See `luau-lifter/src/lib.rs`.
//!
//! # Why it is correct (refuse-by-default, like [`crate::deinline`])
//!
//! Replacing an in-place sub-expression `S = E[params := args]` with
//! `helper(args)` is observationally equivalent in Luau when:
//!
//!   * **In place** — the call occupies `S`'s exact evaluation slot, so the
//!     enclosing short-circuit / conditionality (`and`/`or`/`if-expr`) and the
//!     body's own internal short-circuits + side effects are reproduced
//!     identically (guaranteed by the exact structural match). Body purity is NOT
//!     required: `typeof(p)` is a side-effecting call yet is fine, because the
//!     body's effects happen identically either way.
//!   * **Single scalar result** — the helper returns exactly one scalar on every
//!     path (`is_scalar_return_value` on `E`'s root), so `helper(args)` yields one
//!     value in ANY expression slot, including a multi-value tail position.
//!   * **Arg hoist-safety** — `helper(args)` evaluates each argument once, eagerly,
//!     left-to-right, whereas `S` evaluates each parameter occurrence lazily at its
//!     position. The only semantic delta is therefore arg-evaluation timing/count;
//!     it vanishes iff every bound argument is **side-effect-free** (so an arg that
//!     `E` never evaluates on some path, or evaluates several times, is neutral)
//!     AND **value-stable** (reads no local written inside `S` — provably vacuous
//!     here, since an eligible `E` is a closure-free RValue with no statement
//!     writes, but kept as belt-and-suspenders).
//!
//! Everything is matched by the same exact unifier the statement pass uses
//! ([`crate::deinline::unify_rvalue`]): parameters are bind-once holes, callee
//! locals an injective renaming, and globals/literals(NaN-bit-exact)/operators/
//! upvalues must match exactly — NO commutativity, associativity, or De-Morgan.

use std::mem::Discriminant;

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::deinline::{
    anchors_in_rvalue, body_unsafe, canon, each_closure_decl, insert_def_markers,
    is_scalar_return_value, stmt_rvalues, stmt_rvalues_mut, unify_rvalue, Bindings, MatchCtx,
};
use crate::{Block, Call, Function, LValue, LocalRw, RValue, RcLocal, Statement, Traverse};

type FnPtr = *const Mutex<Function>;

/// Cost gate — readability only, all soundness-neutral. `E` must carry at least
/// this many "anchors" (globals + string literals + method calls) so a trivial
/// helper like `double(x) = x * 2` (0 anchors) is never replaced. The flagship
/// `isFiniteNumber` body sits at exactly 2 (the `typeof` global + the `"number"`
/// string; `math.huge` folds to a `Number` literal contributing 0), so this floor
/// must NOT be raised.
const ANCHOR_FLOOR: usize = 2;
/// `E` must have at least this many RValue nodes — a second readability floor that
/// rejects single-operator helpers the anchor gate might admit.
const NODE_COUNT_FLOOR: usize = 5;
/// A site is only rewritten when the inlined subtree `S` is at least this many
/// nodes larger than its replacement `helper(args)` — so `f(bigExpr)` non-shrinks
/// are refused.
const NET_SAVING_FLOOR: usize = 4;

/// A pure-scalar helper eligible for expression-level de-inlining.
struct ExprTarget {
    /// The `local f` the helper is bound to (the call we emit).
    f_local: RcLocal,
    /// Identity of the helper's `Function`, for the self-match / scope guards.
    func_ptr: FnPtr,
    /// The canonical body expression `E` (one pure scalar RValue).
    expr: RValue,
    /// `E`'s parameter binding-holes (bind-once during unification).
    params: FxHashSet<RcLocal>,
    /// Callee-declared locals (always empty for a single-expression body, kept for
    /// the [`MatchCtx`] the shared unifier expects).
    locals: FxHashSet<RcLocal>,
    /// Parameters in declaration order, to reconstruct the argument list.
    param_order: Vec<RcLocal>,
}

impl ExprTarget {
    fn ctx(&self) -> MatchCtx<'_> {
        MatchCtx {
            params: &self.params,
            locals: &self.locals,
        }
    }
}

pub fn expr_deinline(body: &mut Block) {
    let targets = collect_expr_targets(body);
    if targets.is_empty() {
        return;
    }
    // f_local -> target index, so we recognise each helper's declaration during
    // the scan and only activate it for sites in its lexical scope.
    let decl_map: FxHashMap<RcLocal, usize> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| (t.f_local.clone(), i))
        .collect();
    // E-root discriminant -> candidate target indices. An exact unify requires the
    // candidate node to share `E`'s root variant (the root is always a compound
    // expression — a bare param/local/literal root is refused by the cost gate),
    // so this is a sound, false-negative-free prefilter.
    let mut by_root: FxHashMap<Discriminant<RValue>, Vec<usize>> = FxHashMap::default();
    for (i, t) in targets.iter().enumerate() {
        by_root
            .entry(std::mem::discriminant(&t.expr))
            .or_default()
            .push(i);
    }
    let mut converted: FxHashSet<RcLocal> = FxHashSet::default();
    walk_block(&mut body.0, &targets, &by_root, &decl_map, &[], None, &mut converted);
    if !converted.is_empty() {
        insert_def_markers(&mut body.0, &converted);
    }
}

// ===================================================================
// Target collection
// ===================================================================

fn collect_expr_targets(body: &Block) -> Vec<ExprTarget> {
    // Writes to every local across the whole module. The statement de-inliner gates
    // on `Arc::count(f) == 1` — proving the helper binder is referenced ONLY by its
    // declaration, hence never reassigned, so an emitted `f(args)` always targets
    // the recovered function. A §7 helper legitimately fails that gate (it may be
    // genuinely called or statement-deinlined elsewhere, raising the count), so we
    // drop it — but must otherwise refuse a helper whose binder is REASSIGNED: if
    // `local f = function...end` is later rebound (`f = otherFn`), emitting `f(args)`
    // at a site past the rebind would call the wrong function. A binder that is
    // never rebound is written exactly once (its declaration); any extra write
    // refuses it.
    let mut write_counts: FxHashMap<RcLocal, usize> = FxHashMap::default();
    collect_write_counts(&body.0, &mut write_counts);

    let mut targets = Vec::new();
    each_closure_decl(&body.0, &mut |l, fa| {
        // Refuse a reassigned helper binder (written anywhere beyond its decl).
        if write_counts.get(l).copied().unwrap_or(0) != 1 {
            return;
        }
        let g = fa.lock();
        // Fixed-arity, no goto/label/comment/close/for-init, NO nested closure
        // (identity-matching closures is unsound). P5-A: the `g.name.is_none()`
        // gate is dropped here too — `g.name` is only the bytecode debugname,
        // never consumed by emission (the call uses `f_local`) or the formatter;
        // the ANCHOR_FLOOR/NODE_COUNT_FLOOR cost gates below filter trivial
        // helpers regardless of debugname. Variadic stays refused (unprovable
        // arity in a multi-value slot).
        if g.is_variadic || body_unsafe(&g.body.0) {
            return;
        }
        // The body must canonicalise to EXACTLY `return <one value>`. `canon` folds
        // the guard/early-return duality and drops a trailing void return; a body
        // that still has statement-level branching after that (a multi-leaf
        // if/else diamond — which `value_leaf_shape` would accept) cannot be a
        // single embeddable expression, so refuse it. We do NOT fold such a body
        // ourselves (that would re-derive a slice of the reconstructor and risk
        // building an `E` that diverges from real call sites).
        let pat = canon(&g.body.0);
        let expr = match pat.as_slice() {
            [Statement::Return(r)] if r.values.len() == 1 => r.values[0].clone(),
            _ => return,
        };
        // Root must yield a single value (not Call/MethodCall/VarArg/Select — those
        // have unprovable arity in a multi-value slot). Nested calls like `typeof`
        // inside `E` are fine; only the ROOT is constrained.
        if !is_scalar_return_value(&expr) {
            return;
        }
        // Recursion guard: a body that reads its own binder would emit a call to
        // itself for one unrolled level. Refuse (sound; rare).
        if expr.values_read().iter().any(|&rl| rl == l) {
            return;
        }
        // Cost gate (specificity + size). Both reject trivial helpers.
        let mut anchors = 0usize;
        anchors_in_rvalue(&expr, &mut anchors);
        if anchors < ANCHOR_FLOOR {
            return;
        }
        if node_count(&expr) < NODE_COUNT_FLOOR {
            return;
        }
        let params: FxHashSet<RcLocal> = g.parameters.iter().cloned().collect();
        let param_order = g.parameters.clone();
        targets.push(ExprTarget {
            f_local: l.clone(),
            func_ptr: Arc::as_ptr(fa),
            expr,
            params,
            locals: FxHashSet::default(),
            param_order,
        });
    });
    targets
}

/// Count writes to each local across `stmts`, recursing into nested statement
/// blocks AND closure bodies (a rebind could hide in either). Mirrors the write
/// sites `deinline::collect_written` recognises (assignment LHS, numeric/generic
/// `for` induction locals, `SetList` object) but accumulates counts. Runs once at
/// collection time (cold), not in the matching hot path.
///
/// `pub(crate)` so the statement de-inliner (`crate::deinline::collect_targets`,
/// proposal P4) shares this single source of truth instead of copying it — the
/// `stmt_rvalues` note below is load-bearing and two copies would drift.
pub(crate) fn collect_write_counts(stmts: &[Statement], out: &mut FxHashMap<RcLocal, usize>) {
    for s in stmts {
        match s {
            Statement::Assign(a) => {
                for lhs in &a.left {
                    if let LValue::Local(x) = lhs {
                        *out.entry(x.clone()).or_default() += 1;
                    }
                }
            }
            Statement::NumericFor(nf) => {
                *out.entry(nf.counter.clone()).or_default() += 1;
                collect_write_counts(&nf.block.lock().0, out);
            }
            Statement::GenericFor(gf) => {
                for x in &gf.res_locals {
                    *out.entry(x.clone()).or_default() += 1;
                }
                collect_write_counts(&gf.block.lock().0, out);
            }
            Statement::SetList(sl) => {
                *out.entry(sl.object_local.clone()).or_default() += 1;
            }
            Statement::If(f) => {
                collect_write_counts(&f.then_block.lock().0, out);
                collect_write_counts(&f.else_block.lock().0, out);
            }
            Statement::While(w) => collect_write_counts(&w.block.lock().0, out),
            Statement::Repeat(r) => collect_write_counts(&r.block.lock().0, out),
            _ => {}
        }
        // Use `stmt_rvalues` (not the `Traverse::rvalues` accessor): for an
        // `Assign` the latter exposes only `right`, omitting LHS `Index` operands,
        // whereas `collect_written` — which this mirrors — also visits them. A
        // closure rebinding the helper hidden in an LHS index operand
        // (`t[(function() f = g end)()] = x`) must still be counted, so the
        // reassignment-refusal gate cannot be silently bypassed.
        for rv in stmt_rvalues(s) {
            write_counts_in_closures(rv, out);
        }
    }
}

pub(crate) fn write_counts_in_closures(rv: &RValue, out: &mut FxHashMap<RcLocal, usize>) {
    if let RValue::Closure(c) = rv {
        collect_write_counts(&c.function.0.lock().body.0, out);
        return;
    }
    for child in rv.rvalues() {
        write_counts_in_closures(child, out);
    }
}

// ===================================================================
// Traversal: two-phase scope walk mirroring `deinline::deinline_block`
// ===================================================================

fn walk_block(
    stmts: &mut Vec<Statement>,
    targets: &[ExprTarget],
    by_root: &FxHashMap<Discriminant<RValue>, Vec<usize>>,
    decl_map: &FxHashMap<RcLocal, usize>,
    outer_active: &[usize],
    current_func: Option<FnPtr>,
    converted: &mut FxHashSet<RcLocal>,
) {
    // Phase 1: recurse into nested statement blocks and closure bodies. A child
    // only sees targets whose declaration lexically precedes it, so `active` grows
    // as each declaration in THIS block is passed.
    {
        let mut active: Vec<usize> = outer_active.to_vec();
        for s in stmts.iter_mut() {
            match s {
                Statement::If(f) => {
                    walk_block(&mut f.then_block.lock().0, targets, by_root, decl_map, &active, current_func, converted);
                    walk_block(&mut f.else_block.lock().0, targets, by_root, decl_map, &active, current_func, converted);
                }
                Statement::While(w) => {
                    walk_block(&mut w.block.lock().0, targets, by_root, decl_map, &active, current_func, converted)
                }
                Statement::Repeat(r) => {
                    walk_block(&mut r.block.lock().0, targets, by_root, decl_map, &active, current_func, converted)
                }
                Statement::NumericFor(nf) => {
                    walk_block(&mut nf.block.lock().0, targets, by_root, decl_map, &active, current_func, converted)
                }
                Statement::GenericFor(gf) => {
                    walk_block(&mut gf.block.lock().0, targets, by_root, decl_map, &active, current_func, converted)
                }
                _ => {}
            }
            for rv in stmt_rvalues_mut(s) {
                recurse_into_closures(rv, targets, by_root, decl_map, &active, converted);
            }
            if let Some(idx) = target_decl_index(s, decl_map, targets) {
                active.push(idx);
            }
        }
    }

    // Phase 2: scan this block left to right, matching each statement's own
    // expressions, activating each target after its declaration.
    let mut active: Vec<usize> = outer_active.to_vec();
    for s in stmts.iter_mut() {
        // Skip the per-statement rvalue scan (and its allocation) entirely until a
        // helper is in scope.
        if !active.is_empty() {
            for rv in stmt_rvalues_mut(s) {
                try_rewrite(rv, targets, by_root, &active, current_func, converted);
            }
        }
        if let Some(idx) = target_decl_index(s, decl_map, targets) {
            active.push(idx);
        }
    }
}

/// Descend `rv` to find closures, running the full walk on each closure body with
/// `current_func` set to that closure (so a helper never matches inside its own
/// body) and the current `active` set (targets in scope at the closure's site are
/// visible inside it as upvalues).
fn recurse_into_closures(
    rv: &mut RValue,
    targets: &[ExprTarget],
    by_root: &FxHashMap<Discriminant<RValue>, Vec<usize>>,
    decl_map: &FxHashMap<RcLocal, usize>,
    active: &[usize],
    converted: &mut FxHashSet<RcLocal>,
) {
    if let RValue::Closure(c) = rv {
        let fp = Arc::as_ptr(&c.function.0);
        walk_block(
            &mut c.function.0.lock().body.0,
            targets,
            by_root,
            decl_map,
            active,
            Some(fp),
            converted,
        );
        return;
    }
    for child in rv.rvalues_mut() {
        recurse_into_closures(child, targets, by_root, decl_map, active, converted);
    }
}

/// If `s` is the declaration `local f = function ... end` of one of our targets,
/// returns that target's index (scope activation, mirroring
/// `deinline::target_decl_index`).
fn target_decl_index(
    s: &Statement,
    decl_map: &FxHashMap<RcLocal, usize>,
    targets: &[ExprTarget],
) -> Option<usize> {
    if let Statement::Assign(a) = s
        && a.prefix
        && a.left.len() == 1
        && a.right.len() == 1
        && let LValue::Local(l) = &a.left[0]
        && let RValue::Closure(c) = &a.right[0]
        && let Some(&idx) = decl_map.get(l)
        && Arc::as_ptr(&c.function.0) == targets[idx].func_ptr
    {
        return Some(idx);
    }
    None
}

// ===================================================================
// Matching + rewrite (outermost-first)
// ===================================================================

fn try_rewrite(
    rv: &mut RValue,
    targets: &[ExprTarget],
    by_root: &FxHashMap<Discriminant<RValue>, Vec<usize>>,
    active: &[usize],
    current_func: Option<FnPtr>,
    converted: &mut FxHashSet<RcLocal>,
) {
    // No helper is in lexical scope here, so no node in this subtree can match
    // (`active` is monotone-nondecreasing down the descent, and `try_match` only
    // considers `active` targets). Prune the whole subtree — this skips the entire
    // pre-declaration region of every block. Mirrors the statement pass's
    // `if active.is_empty()` guard in `deinline::try_match_at`.
    if active.is_empty() {
        return;
    }
    // Outermost-first: try to match the WHOLE node before descending, so the
    // largest equivalent subtree is collapsed into one call.
    if let Some(cands) = by_root.get(&std::mem::discriminant(&*rv)) {
        let mut hit: Option<(usize, Vec<RValue>)> = None;
        let mut ambiguous = false;
        for &idx in cands {
            if !active.contains(&idx) {
                continue; // helper not yet in lexical scope here
            }
            let t = &targets[idx];
            if current_func == Some(t.func_ptr) {
                continue; // never match a helper against its own body
            }
            if let Some(args) = try_match(t, rv) {
                if hit.is_some() {
                    ambiguous = true; // two distinct helpers match this node: refuse
                    break;
                }
                hit = Some((idx, args));
            }
        }
        if !ambiguous {
            if let Some((idx, args)) = hit {
                let t = &targets[idx];
                let call = Call::new(RValue::Local(t.f_local.clone()), args);
                *rv = RValue::Call(call);
                converted.insert(t.f_local.clone());
                // Do NOT descend into the freshly-emitted args (idempotence +
                // largest-match): the call root is never a pattern (scalar-root gate
                // excludes Call), and the args are already-final caller expressions.
                return;
            }
        }
    }
    // No unambiguous match here — descend into children (a smaller subtree, or a
    // sibling, may still match). Closures yield no children here (handled in the
    // phase-1 closure recursion), so we never re-enter a closure body.
    for child in rv.rvalues_mut() {
        try_rewrite(child, targets, by_root, active, current_func, converted);
    }
}

/// Attempt to unify target `t`'s body `E` against the candidate subtree `rv` and,
/// if it matches under all gates, return the reconstructed argument list.
fn try_match(t: &ExprTarget, rv: &RValue) -> Option<Vec<RValue>> {
    let mut b = Bindings::default();
    if unify_rvalue(&t.ctx(), &t.expr, rv, &mut b).is_err() {
        return None;
    }
    // Every parameter must have bound to an argument (an unread parameter cannot be
    // reconstructed) — refuse otherwise.
    let mut args = Vec::with_capacity(t.param_order.len());
    for p in &t.param_order {
        match b.params.get(p) {
            Some(e) => args.push(e.clone()),
            None => return None,
        }
    }
    // Arg hoist-safety. Turning `S = E[params := args]` back into `helper(args)`
    // evaluates each argument ONCE, EAGERLY, before the body, whereas `S` evaluates
    // each parameter occurrence lazily at its position. For the rewrite to be
    // observationally equivalent each bound argument must be hoist-safe, which means
    // more than merely side-effect-free:
    //   * TOTAL — it must not be able to RAISE. A pure-looking `a + b`, `a .. b`,
    //     `-x` or `#t` is `has_side_effects() == false` (the crate only propagates
    //     effects from operands) yet can throw a type/metamethod error. If such an
    //     arg binds to a parameter `E` evaluates only on SOME path (an `IfExpression`
    //     branch, or the right operand of `and`/`or` — the very shapes this pass
    //     targets), eager evaluation would raise an error the inlined original never
    //     raised.
    //   * IDENTITY-STABLE — a `{}` / `{...}` constructor is effect-free but yields a
    //     FRESH reference per evaluation, so a parameter used twice (`p == p`) would
    //     flip from two distinct tables to one shared reference.
    // A bare `Local` or `Literal` satisfies both unconditionally (and is also
    // value-stable: re-reading is identical). It covers every argument observed in
    // the corpus — Luau materialises any compound operand into a register before the
    // inlined body, so a real inlined arg is already a local/constant. Anything else
    // (Binary/Unary/Index/Call/Table/IfExpression/...) is REFUSED, not trusted.
    if !args
        .iter()
        .all(|a| matches!(a, RValue::Local(_) | RValue::Literal(_)))
    {
        return None;
    }
    // Cost: the replacement must be a net node saving against the specialised
    // subtree `S` (rejects `f(bigExpr)` non-shrinks). Computed only on a real match.
    let s_nodes = node_count(rv);
    let args_nodes: usize = args.iter().map(node_count).sum();
    if s_nodes < 1 + args_nodes + NET_SAVING_FLOOR {
        return None;
    }
    Some(args)
}

/// Number of RValue nodes in `rv`. Single post-order recursion via the `Traverse`
/// child accessor (each node visited once → O(n)); a `Closure` exposes no child
/// rvalues, so its body is not counted.
fn node_count(rv: &RValue) -> usize {
    1 + rv.rvalues().iter().map(|c| node_count(c)).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Assign, Binary, BinaryOperation, Closure, Function, Global, Index, Literal, Local, Return,
        Unary, UnaryOperation,
    };
    use by_address::ByAddress;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }
    fn lv(l: &RcLocal) -> RValue {
        RValue::Local(l.clone())
    }
    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }
    fn string(s: &str) -> RValue {
        RValue::Literal(Literal::String(s.as_bytes().to_vec()))
    }
    fn number(n: f64) -> RValue {
        RValue::Literal(Literal::Number(n))
    }
    fn bin(l: RValue, op: BinaryOperation, r: RValue) -> RValue {
        RValue::Binary(Binary::new(l, r, op))
    }
    fn not_rv(v: RValue) -> RValue {
        RValue::Unary(Unary {
            value: Box::new(v),
            operation: UnaryOperation::Not,
        })
    }
    fn call(callee: RValue, args: Vec<RValue>) -> RValue {
        RValue::Call(Call::new(callee, args))
    }

    /// `typeof(v) == "number" and v > 0` — 2 anchors (typeof Global, "number"
    /// String), 9 nodes, scalar (Binary) root: a valid expression-deinline target.
    fn num_positive(v: &RValue) -> RValue {
        bin(
            bin(call(global("typeof"), vec![v.clone()]), BinaryOperation::Equal, string("number")),
            BinaryOperation::And,
            bin(v.clone(), BinaryOperation::GreaterThan, number(0.0)),
        )
    }

    fn helper_decl(f: &RcLocal, name: &str, params: Vec<RcLocal>, body: Vec<Statement>) -> Statement {
        let func = Arc::new(Mutex::new(Function {
            name: Some(name.to_string()),
            parameters: params,
            is_variadic: false,
            body: Block(body),
        }));
        Statement::Assign(Assign {
            left: vec![LValue::Local(f.clone())],
            right: vec![RValue::Closure(Closure {
                function: ByAddress(func),
                upvalues: vec![],
            })],
            prefix: true,
            parallel: false,
        })
    }

    /// `local r = <rhs>` (a declaration).
    fn local_decl(r: &RcLocal, rhs: RValue) -> Statement {
        Statement::Assign(Assign {
            left: vec![LValue::Local(r.clone())],
            right: vec![rhs],
            prefix: true,
            parallel: false,
        })
    }

    fn rhs_of(s: &Statement) -> &RValue {
        match s {
            Statement::Assign(a) => &a.right[0],
            _ => panic!("expected assign"),
        }
    }

    fn is_call_to(rv: &RValue, f: &RcLocal) -> bool {
        matches!(rv, RValue::Call(c) if matches!(c.value.as_ref(), RValue::Local(l) if l == f))
    }

    #[test]
    fn recovers_inlined_expression() {
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        // caller decl is now at index 2 (a DEF_MARKER comment was inserted before
        // the helper decl), and its RHS is `isNumPositive(x)`.
        let caller = block.0.last().unwrap();
        let rv = rhs_of(caller);
        assert!(is_call_to(rv, &f), "expected isNumPositive(x), got {rv:?}");
        if let RValue::Call(c) = rv {
            assert_eq!(c.arguments.len(), 1);
            assert!(matches!(&c.arguments[0], RValue::Local(l) if *l == x));
        }
    }

    #[test]
    fn recovers_under_not() {
        // The flagship shape: `not (E[p:=x])` -> `not f(x)` (the `not` wrapper is
        // preserved by the in-place rewrite; matching the operand, not the `not`).
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, not_rv(num_positive(&lv(&x)))),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(block.0.last().unwrap());
        match rv {
            RValue::Unary(u) if u.operation == UnaryOperation::Not => {
                assert!(is_call_to(&u.value, &f), "expected not isNumPositive(x)");
            }
            _ => panic!("expected `not isNumPositive(x)`, got {rv:?}"),
        }
    }

    #[test]
    fn refuses_side_effecting_arg() {
        // The argument binds to `getX()` (a Call → side-effecting): eager-once
        // evaluation could reorder/duplicate/drop it, so REFUSE — leave inlined.
        let f = local("isNumPositive");
        let p = local("p");
        let r = local("r");
        let get_x = || call(global("getX"), vec![]);
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, num_positive(&get_x())),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(block.0.last().unwrap());
        assert!(!is_call_to(rv, &f), "side-effecting arg must be refused");
        assert!(matches!(rv, RValue::Binary(_)), "expression must stay inlined");
    }

    #[test]
    fn refuses_out_of_scope_use() {
        // The inlined copy appears BEFORE the helper declaration — emitting a call
        // would reference an out-of-scope local. Refuse.
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            local_decl(&r, num_positive(&lv(&x))),
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(&block.0[0]);
        assert!(!is_call_to(rv, &f), "out-of-scope use must not be rewritten");
    }

    #[test]
    fn refuses_trivial_helper() {
        // `p > 0` — 0 anchors, 3 nodes: below the cost floor, never a target.
        let f = local("isPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let triv = |v: &RValue| bin(v.clone(), BinaryOperation::GreaterThan, number(0.0));
        let mut block = Block(vec![
            helper_decl(&f, "isPositive", vec![p.clone()], vec![Return::new(vec![triv(&lv(&p))]).into()]),
            local_decl(&r, triv(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "trivial helper must be refused");
    }

    #[test]
    fn refuses_recursive_helper() {
        // Body reads its own binder `f` — refuse as a target (would emit a call to
        // itself for one unrolled level).
        let f = local("rec");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        // E = `typeof(p) == "number" and rec` (reads f) — contrived but reads f_local.
        let body_e = |v: &RValue, fl: &RcLocal| {
            bin(
                bin(call(global("typeof"), vec![v.clone()]), BinaryOperation::Equal, string("number")),
                BinaryOperation::And,
                lv(fl),
            )
        };
        let mut block = Block(vec![
            helper_decl(&f, "rec", vec![p.clone()], vec![Return::new(vec![body_e(&lv(&p), &f)]).into()]),
            local_decl(&r, body_e(&lv(&x), &f)),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "recursive helper must be refused");
    }

    #[test]
    fn param_consistency_blocks_divergent_args() {
        // `p` appears twice in E; the caller has TWO DIFFERENT subexprs at those
        // positions, so `p` cannot bind consistently — no match.
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let y = local("y");
        let r = local("r");
        // diverged copy: typeof(x) == "number" and y > 0  (x vs y)
        let diverged = bin(
            bin(call(global("typeof"), vec![lv(&x)]), BinaryOperation::Equal, string("number")),
            BinaryOperation::And,
            bin(lv(&y), BinaryOperation::GreaterThan, number(0.0)),
        );
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, diverged),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "inconsistent param binding must refuse");
    }

    #[test]
    fn ambiguity_refused() {
        // Two distinct helpers with structurally-identical bodies both match the
        // node — refuse it.
        let f1 = local("checkA");
        let f2 = local("checkB");
        let p1 = local("p");
        let p2 = local("q");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(&f1, "checkA", vec![p1.clone()], vec![Return::new(vec![num_positive(&lv(&p1))]).into()]),
            helper_decl(&f2, "checkB", vec![p2.clone()], vec![Return::new(vec![num_positive(&lv(&p2))]).into()]),
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(block.0.last().unwrap());
        assert!(!is_call_to(rv, &f1) && !is_call_to(rv, &f2), "ambiguous match must be refused");
    }

    #[test]
    fn idempotent_second_run_is_noop() {
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        let after_first = format!("{block}");
        expr_deinline(&mut block);
        let after_second = format!("{block}");
        assert_eq!(after_first, after_second, "second run must be a no-op");
    }

    #[test]
    fn refuses_call_rooted_helper() {
        // Helper body root is a Call (`getThing(p)`): unprovable arity in a
        // multi-value slot → not a target (is_scalar_return_value root gate).
        let f = local("getThing");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        // E root = Call, but include nested anchors so only the root gate decides.
        let e = |v: &RValue| call(global("transform"), vec![call(global("typeof"), vec![v.clone()]), string("number")]);
        let mut block = Block(vec![
            helper_decl(&f, "getThing", vec![p.clone()], vec![Return::new(vec![e(&lv(&p))]).into()]),
            local_decl(&r, e(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "Call-rooted helper must not be a target");
    }

    #[test]
    fn refuses_throwing_compound_arg() {
        // Arg binds to `a + b` (a pure Binary → side-effect-free, but can THROW and
        // is not a bare Local/Literal). `p` sits in a conditionally-evaluated spot
        // (RHS of `and`), so eager hoisting could raise where the inline didn't.
        // Must REFUSE.
        let f = local("isNumPositive");
        let p = local("p");
        let a = local("a");
        let b = local("b");
        let r = local("r");
        let sum = || bin(lv(&a), BinaryOperation::Add, lv(&b));
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, num_positive(&sum())),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "throwing compound arg must be refused");
    }

    #[test]
    fn accepts_literal_arg() {
        // A bare Literal arg is total + value-stable → accepted.
        let f = local("isNumPositive");
        let p = local("p");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, num_positive(&number(5.0))),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(block.0.last().unwrap());
        assert!(is_call_to(rv, &f), "literal arg should be accepted");
        if let RValue::Call(c) = rv {
            assert!(matches!(&c.arguments[0], RValue::Literal(Literal::Number(n)) if *n == 5.0));
        }
    }

    #[test]
    fn refuses_reassigned_helper() {
        // `local f = function...end` is later rebound (`f = otherFn`); emitting a
        // call to `f` at a later site could hit the wrong function. Refuse the target.
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let reassign = Statement::Assign(Assign {
            left: vec![LValue::Local(f.clone())],
            right: vec![global("otherFn")],
            prefix: false,
            parallel: false,
        });
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            reassign,
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "reassigned helper must be refused");
    }

    #[test]
    fn refuses_field_access_arg() {
        // Arg binds to `obj.field` (an Index → side-effecting per the crate): refuse.
        let f = local("isNumPositive");
        let p = local("p");
        let obj = local("obj");
        let r = local("r");
        let field = RValue::Index(Index::new(lv(&obj), string("field")));
        let mut block = Block(vec![
            helper_decl(&f, "isNumPositive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
            local_decl(&r, num_positive(&field)),
        ]);
        expr_deinline(&mut block);
        assert!(!is_call_to(rhs_of(block.0.last().unwrap()), &f), "field-access arg must be refused");
    }
}
