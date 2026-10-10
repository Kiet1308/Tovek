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
//!
//! The additional [`arithmetic`] family accepts named bytecode helpers without
//! global/string anchors. It constructs an internal return/selection pattern,
//! admits only parameter/literal arithmetic, checks reference-capture stability,
//! and uses deterministic search budgets. Its separate output marker identifies
//! equivalent-call inference; it does not assert unique original call sites.

use std::mem::Discriminant;

pub(crate) mod arithmetic;

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::deinline::{
    Bindings, MatchCtx, anchors_in_rvalue, body_unsafe, canon, each_closure_decl,
    is_scalar_return_value, visit_stmt_rvalues, visit_stmt_rvalues_mut, unify_rvalue,
};
use crate::{Block, Call, Function, LValue, LocalRw, RValue, RcLocal, Statement, Traverse};

type FnPtr = *const Mutex<Function>;

/// Legacy expression-family cost gate — readability only. `E` must carry at least
/// this many "anchors" (globals + string literals + method calls) so a trivial
/// helper like `double(x) = x * 2` (0 anchors) is never replaced. The flagship
/// `isFiniteNumber` body sits at exactly 2 (the `typeof` global + the `"number"`
/// string; `math.huge` folds to a `Number` literal contributing 0), so this floor
/// must NOT be raised.
const ANCHOR_FLOOR: usize = 2;
/// `E` must have at least this many RValue nodes — a second readability floor that
/// rejects single-operator helpers the anchor gate might admit.
pub(crate) const NODE_COUNT_FLOOR: usize = 5;
/// A site is only rewritten when the inlined subtree `S` is at least this many
/// nodes larger than its replacement `helper(args)` — so `f(bigExpr)` non-shrinks
/// are refused.
pub(crate) const NET_SAVING_FLOOR: usize = 4;

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
    /// Additional, bounded proof path for named bytecode arithmetic helpers.
    arithmetic: Option<std::rc::Rc<arithmetic::AttemptBudget>>,
    /// Parameters the body reads once, before anything a call could change,
    /// in the order it reads them, in the body's own statement order (`expr`
    /// may fold `local q = ...`): `evaluation_order::LeadingReads`.
    leading: crate::evaluation_order::LeadingReads,
    /// Outer locals `expr` reads that a closure assigns: the helper fetches
    /// one as an upvalue where it stands, a site holding it in a register
    /// reads it when its operation runs (`x + change()` reads `x` after the
    /// call), so such a site is refused.
    free_cells: Vec<RcLocal>,
    /// The locals each function reads as upvalues, which Luau fetches where
    /// they are read; any other local of a site is a register.
    upvalues: std::rc::Rc<FunctionUpvalues>,
    captures: std::rc::Rc<crate::deinline_safety::CaptureSafety>,
    search: std::rc::Rc<crate::deinline_safety::SearchBudget>,
    protect_definition: bool,
    /// Below the floors (anchors, size, saving): matched only where line
    /// info shows the helper's copies, all of them in a function or none
    /// (plan E1, [`crate::deinline::evidence`]), in the evidence rounds.
    evidence: bool,
    /// The helper's bytecode prototype, which line evidence names.
    proto: Option<usize>,
}

impl ExprTarget {
    fn ctx(&self) -> MatchCtx<'_> {
        MatchCtx {
            params: &self.params,
            locals: &self.locals,
            captures: None,
        }
    }
}

pub fn expr_deinline(body: &mut Block) { run(body, false); }

/// Match named scalar helpers before cleanup can consume their only binding
/// into an export property. A committed call keeps that lexical binder alive.
pub fn arithmetic_deinline_early(body: &mut Block) { run(body, true); }

fn run(body: &mut Block, arithmetic_only: bool) {
    let copies = crate::deinline::evidence::current();
    let mut targets = collect_expr_targets(body, arithmetic_only, copies.as_deref());
    if arithmetic_only {
        targets.retain(|t| t.arithmetic.is_some());
        for target in &mut targets { target.protect_definition = true; }
    }
    // Luau neither inlines nor imports where a function may get its own
    // globals, and `debug.info` under another name may see a helper's own
    // call frame. Every target shares the module's census.
    if targets.first().is_none_or(|target| {
        target.captures.dynamic_environment() || target.captures.call_frames_untracked()
    }) {
        return;
    }
    // f_local -> its targets' indices (a helper and its evidence twin sit
    // side by side), so we recognise each helper's declaration during the
    // scan and only activate it for sites in its lexical scope.
    let mut decl_map: FxHashMap<RcLocal, std::ops::Range<usize>> = FxHashMap::default();
    for (i, t) in targets.iter().enumerate() {
        decl_map.entry(t.f_local.clone()).and_modify(|range| range.end = i + 1).or_insert(i..i + 1);
    }
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
        if t.arithmetic.is_some() && matches!(t.expr, RValue::IfExpression(_)) {
            by_root
                .entry(std::mem::discriminant(&RValue::Binary(crate::Binary::new(
                    crate::Literal::Nil.into(),
                    crate::Literal::Nil.into(),
                    crate::BinaryOperation::Or,
                ))))
                .or_default()
                .push(i);
        }
    }
    let mut walk = Walk {
        targets: &targets,
        by_root: &by_root,
        decl_map: &decl_map,
        round: Round::Normal,
        copies: copies.clone(),
        protos: FxHashMap::default(),
        probed: FxHashMap::default(),
        rebuilt: FxHashMap::default(),
        twinned: targets.iter().filter(|t| t.evidence).map(|t| t.f_local.clone()).collect(),
    };
    walk_block(&mut body.0, &mut walk, &[], None);
    // The helpers below the floors, where line info shows their copies
    // (E1): a probe counts each one's matches in each function, then the
    // pairs whose counts are its copies there are rebuilt; again while
    // that rebuilds anything, as a rebuilt copy may hold others.
    if copies.is_none() || !targets.iter().any(|t| t.evidence) {
        return;
    }
    let mut rebuilt: FxHashSet<RcLocal> = FxHashSet::default();
    for _ in 0..4 {
        walk.round = Round::Probe;
        walk.probed.clear();
        walk.rebuilt.clear();
        walk_block(&mut body.0, &mut walk, &[], None);
        let admitted = walk.admitted();
        if admitted.is_empty() {
            break;
        }
        walk.round = Round::Admit(admitted);
        walk.probed.clear();
        walk_block(&mut body.0, &mut walk, &[], None);
        if walk.probed.is_empty() {
            break;
        }
        rebuilt.extend(walk.probed.keys().map(|(_, binder)| binder.clone()));
    }
    if !rebuilt.is_empty() {
        fold_literal_arguments(&mut body.0, &rebuilt);
    }
}

/// A literal Luau evaluated into a register for an argument of a copy, left
/// as `local x = "TypeOrder"` right before the statement holding the call
/// now rebuilt from that copy (`sorter(x, true)`), goes into the call where
/// `x` is read nowhere else and never written: evaluating a constant there
/// or before the statement is the same (`sorter("TypeOrder", true)`). Only
/// for the calls of `binders` the evidence rounds rebuilt; a local with a
/// source name keeps its declaration.
fn fold_literal_arguments(stmts: &mut Vec<Statement>, binders: &FxHashSet<RcLocal>) {
    fn fill(value: &mut RValue, local: &RcLocal, literal: &RValue, binders: &FxHashSet<RcLocal>) -> bool {
        if let RValue::Call(call) | RValue::Select(crate::Select::Call(call)) = value
            && call.rebuilt.is_some()
            && matches!(call.value.as_ref(), RValue::Local(callee) if binders.contains(callee))
            && let Some(argument) = call.arguments.iter_mut().find(|argument| matches!(argument, RValue::Local(read) if read == local))
        {
            *argument = literal.clone();
            return true;
        }
        if matches!(value, RValue::Closure(_)) {
            return false;
        }
        let mut done = false;
        value.visit_rvalues_mut(&mut |child| {
            done = fill(child, local, literal, binders);
            !done
        });
        done
    }
    for statement in stmts.iter_mut() {
        match statement {
            Statement::If(branch) => {
                fold_literal_arguments(&mut branch.then_block.lock().0, binders);
                fold_literal_arguments(&mut branch.else_block.lock().0, binders);
            }
            Statement::While(node) => fold_literal_arguments(&mut node.block.lock().0, binders),
            Statement::Repeat(node) => fold_literal_arguments(&mut node.block.lock().0, binders),
            Statement::NumericFor(node) => fold_literal_arguments(&mut node.block.lock().0, binders),
            Statement::GenericFor(node) => fold_literal_arguments(&mut node.block.lock().0, binders),
            _ => {}
        }
        visit_stmt_rvalues_mut(statement, &mut |value| {
            fold_in_closures(value, binders);
            true
        });
    }
    fn fold_in_closures(value: &mut RValue, binders: &FxHashSet<RcLocal>) {
        if let RValue::Closure(closure) = value {
            fold_literal_arguments(&mut closure.function.0.lock().body.0, binders);
            return;
        }
        value.visit_rvalues_mut(&mut |child| {
            fold_in_closures(child, binders);
            true
        });
    }
    let mut index = 1;
    while index < stmts.len() {
        let declared = match &stmts[index - 1] {
            Statement::Assign(assign)
                if assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1 =>
            {
                match (&assign.left[0], &assign.right[0]) {
                    (LValue::Local(local), RValue::Literal(_)) if !local.preserve_binding() => {
                        Some((local.clone(), assign.right[0].clone()))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let folds = declared.as_ref().is_some_and(|(local, _)| {
            let mut written = FxHashSet::default();
            crate::deinline::collect_written(&stmts[index..], &mut written);
            !written.contains(local) && crate::deinline::count_local_reads(&stmts[index..], local) == 1
        });
        if folds
            && let Some((local, literal)) = declared
            && {
                let mut filled = false;
                visit_stmt_rvalues_mut(&mut stmts[index], &mut |value| {
                    filled = fill(value, &local, &literal, binders);
                    !filled
                });
                filled
            }
        {
            stmts.remove(index - 1);
            // The statement before may hold another argument's literal.
            index = index.saturating_sub(1).max(1);
            continue;
        }
        index += 1;
    }
}

/// Which targets a walk tries ([`ExprTarget::evidence`]).
enum Round {
    /// Every target above the floors, as without line evidence.
    Normal,
    /// The evidence targets in each function holding copies of their
    /// helper, their matches counted, nothing rewritten; the others are
    /// rivals, never rewritten.
    Probe,
    /// The evidence targets of the pairs of a function and a helper the
    /// count oracle admitted are rebuilt there; the others are rivals. The
    /// rebuilt ones are counted (`probed`).
    Admit(FxHashSet<(Option<FnPtr>, RcLocal)>),
}

/// One walk over the module: its targets and round, and what line evidence
/// needs counted.
struct Walk<'a> {
    targets: &'a [ExprTarget],
    by_root: &'a FxHashMap<Discriminant<RValue>, Vec<usize>>,
    decl_map: &'a FxHashMap<RcLocal, std::ops::Range<usize>>,
    round: Round,
    copies: Option<std::rc::Rc<crate::deinline::evidence::Copies>>,
    /// Each function body's prototype, read where the walk enters it.
    protos: FxHashMap<FnPtr, Option<usize>>,
    /// Each evidence helper's matches (or rebuilt calls, in an admitting
    /// round) per function body, with its prototype.
    probed: FxHashMap<(Option<FnPtr>, RcLocal), (usize, Option<usize>)>,
    /// The calls of each evidence helper already rebuilt per function body.
    rebuilt: FxHashMap<(Option<FnPtr>, RcLocal), usize>,
    /// The helpers with an evidence twin: in an evidence round the twin
    /// stands for them.
    twinned: FxHashSet<RcLocal>,
}

impl Walk<'_> {
    /// The prototype of `function` (`None`: the chunk's main one).
    fn caller(&self, function: Option<FnPtr>) -> Option<usize> {
        match function {
            None => self.copies.as_ref().map(|copies| copies.main),
            Some(function) => self.protos.get(&function).copied().flatten(),
        }
    }

    /// The copies of target `t`'s helper in `function`'s code.
    fn copies_in(&self, function: Option<FnPtr>, t: &ExprTarget) -> u32 {
        match (&self.copies, self.caller(function), t.proto) {
            (Some(copies), Some(caller), Some(helper)) => copies.copies(Some(caller), helper),
            _ => 0,
        }
    }

    /// How target `idx` takes part at a site in `function`: whether it may
    /// be rebuilt there, and whether it is tried at all (as a rival).
    fn role(&self, idx: usize, function: Option<FnPtr>) -> (bool, bool) {
        let t = &self.targets[idx];
        match &self.round {
            Round::Normal => (!t.evidence, !t.evidence),
            Round::Probe => {
                let tried = if t.evidence { self.copies_in(function, t) > 0 } else { !self.twinned.contains(&t.f_local) };
                (t.evidence && tried, tried)
            }
            Round::Admit(admitted) => {
                let admit = t.evidence && admitted.contains(&(function, t.f_local.clone()));
                let tried = admit
                    || if t.evidence { self.copies_in(function, t) > 0 } else { !self.twinned.contains(&t.f_local) };
                (admit, tried)
            }
        }
    }

    /// The count oracle ([`crate::deinline::evidence::admitted`]) over the
    /// probe's counts.
    fn admitted(&self) -> FxHashSet<(Option<FnPtr>, RcLocal)> {
        let Some(copies) = &self.copies else { return FxHashSet::default() };
        let pairs: Vec<_> = self.probed.iter().collect();
        let counts: Vec<_> = pairs
            .iter()
            .map(|&(key, &(hits, proto))| crate::deinline::evidence::Probed {
                caller: self.caller(key.0),
                helper: proto,
                hits,
                rebuilt: self.rebuilt.get(key).copied().unwrap_or(0),
            })
            .collect();
        let admit = crate::deinline::evidence::admitted(copies, &counts);
        if crate::env_flag!("MEDAL_TRACE_EVIDENCE") {
            for ((key, _), (count, admit)) in pairs.iter().zip(counts.iter().zip(&admit)) {
                eprintln!(
                    "EVIDENCE expression caller=p{:?} helper={} proto=p{:?} hits={} rebuilt={} admit={admit}",
                    count.caller, key.1, count.helper, count.hits, count.rebuilt
                );
            }
        }
        pairs.into_iter().zip(admit).filter(|(_, admit)| *admit).map(|((key, _), _)| key.clone()).collect()
    }
}

// ===================================================================
// Target collection
// ===================================================================

/// Structural facts of one helper declaration. Every per-declaration gate is
/// pure, so all of them run before the module-wide capture and write censuses:
/// a module without an eligible helper never pays for either census.
struct HelperCandidate {
    f_local: RcLocal,
    func_ptr: FnPtr,
    expr: RValue,
    parameters: Vec<RcLocal>,
    prototype: Option<usize>,
    arithmetic: bool,
    function: Arc<Mutex<Function>>,
    /// [`ExprTarget::evidence`].
    evidence: bool,
}

/// The helper candidates of `body`. With line info (`copies`), a helper
/// with copies somewhere that the floors refuse is an evidence candidate,
/// and one they admit gets an evidence twin besides, for the sites whose
/// call saves less than the floor asks (E1).
fn helper_candidates(body: &Block, arithmetic_only: bool, copies: Option<&crate::deinline::evidence::Copies>) -> Vec<HelperCandidate> {
    let mut candidates = Vec::new();
    each_closure_decl(&body.0, &mut |l, fa| {
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
        let candidate = |expr: RValue, arithmetic, evidence| HelperCandidate {
            f_local: l.clone(),
            func_ptr: Arc::as_ptr(fa),
            expr,
            parameters: g.parameters.clone(),
            prototype: g.bytecode_proto_id,
            arithmetic,
            function: fa.clone(),
            evidence,
        };
        let inlined = g.bytecode_proto_id.is_some_and(|proto| copies.is_some_and(|copies| copies.inlined_anywhere(proto)));
        if let Some(expr) = arithmetic::pattern(&g) {
            if inlined {
                candidates.push(candidate(expr.clone(), true, false));
                candidates.push(candidate(expr, true, true));
            } else {
                candidates.push(candidate(expr, true, false));
            }
            return;
        }
        if arithmetic_only {
            return;
        }
        // The body must canonicalise to EXACTLY `return <one value>`. `canon` folds
        // the guard/early-return duality and drops a trailing void return; a body
        // that still has statement-level branching after that (a multi-leaf
        // if/else diamond — which `value_leaf_shape` would accept) cannot be a
        // single embeddable expression, so refuse it. We do NOT fold such a body
        // ourselves (that would re-derive a slice of the reconstructor and risk
        // building an `E` that diverges from real call sites).
        let shape = crate::deinline_safety::CaptureSafety::new(&g.body);
        if !shape.complete() || shape.nodes() > 2048 { return; }
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
        if expr.any_local_read(&mut |rl| rl == l) {
            return;
        }
        // Cost gate (specificity + size). Both reject trivial helpers, but
        // where line info shows copies: then a compound value is enough.
        let mut anchors = 0usize;
        anchors_in_rvalue(&expr, &mut anchors);
        if anchors < ANCHOR_FLOOR || node_count(&expr) < NODE_COUNT_FLOOR {
            if inlined && node_count(&expr) >= 2 {
                candidates.push(candidate(expr, false, true));
            }
            return;
        }
        if inlined {
            candidates.push(candidate(expr.clone(), false, false));
            candidates.push(candidate(expr, false, true));
        } else {
            candidates.push(candidate(expr, false, false));
        }
    });
    candidates
}

fn collect_expr_targets(body: &Block, arithmetic_only: bool, copies: Option<&crate::deinline::evidence::Copies>) -> Vec<ExprTarget> {
    // The early phase keeps only arithmetic helpers, so without one its result
    // is empty. Other helpers still register callees for call provenance, so
    // that report keeps the complete collection.
    let recording = crate::call_origins::active();
    if arithmetic_only && !recording && helper_candidates(body, true, None).is_empty() {
        return Vec::new();
    }
    let candidates = helper_candidates(body, false, copies);
    if candidates.is_empty() {
        return Vec::new();
    }
    // Writes to every local across the whole module, shared with the statement
    // de-inliner. Refcounts cannot establish whether a helper is reassigned: a
    // genuine call or an earlier de-inline also raises them. Refuse a binder
    // with a second write: if
    // `local f = function...end` is later rebound (`f = otherFn`), emitting `f(args)`
    // at a site past the rebind would call the wrong function. A binder that is
    // never rebound is written exactly once (its declaration); any extra write
    // refuses it.
    let captures = std::rc::Rc::new(crate::deinline_safety::CaptureSafety::new(body));
    if !captures.complete() { return Vec::new(); }
    let upvalues = std::rc::Rc::new(function_upvalues(body));
    let mut write_counts: FxHashMap<RcLocal, usize> = FxHashMap::default();
    collect_write_counts(&body.0, &mut write_counts);
    let search = std::rc::Rc::new(crate::deinline_safety::SearchBudget::default());
    let arithmetic_budget = std::rc::Rc::new(arithmetic::AttemptBudget::default());
    let mut arithmetic_targets = 0usize;
    let mut targets = Vec::new();
    for candidate in candidates {
        // Refuse a reassigned helper binder (written anywhere beyond its
        // decl), and one whose expression reads call frames, which runs a
        // frame deeper inside the helper.
        if write_counts.get(&candidate.f_local).copied().unwrap_or(0) != 1
            || captures.value_reads_frames(&candidate.expr)
        {
            continue;
        }
        if candidate.arithmetic && !candidate.evidence {
            arithmetic_targets += 1;
            if arithmetic_targets > arithmetic::MAX_TARGETS {
                continue;
            }
        }
        crate::call_origins::register_callee(candidate.f_local.stable_id(), candidate.prototype);
        let leading = leading_reads(&candidate.function.lock().body.0, &candidate.parameters, &captures);
        let mut free_cells = Vec::new();
        candidate.expr.visit_local_reads(&mut |local| {
            if !candidate.parameters.contains(local) && captures.closure_written(local) && !free_cells.contains(local) {
                free_cells.push(local.clone());
            }
            true
        });
        targets.push(ExprTarget {
            params: candidate.parameters.iter().cloned().collect(),
            f_local: candidate.f_local,
            func_ptr: candidate.func_ptr,
            expr: candidate.expr,
            locals: FxHashSet::default(),
            param_order: candidate.parameters,
            arithmetic: candidate.arithmetic.then(|| arithmetic_budget.clone()),
            leading,
            free_cells,
            upvalues: upvalues.clone(),
            captures: captures.clone(),
            search: search.clone(),
            // Keep competing helper definitions intact in every phase.
            // Folding one helper into another would erase the ambiguity
            // that must also block reconstruction in their callers.
            protect_definition: candidate.arithmetic,
            evidence: candidate.evidence,
            proto: candidate.prototype,
        });
    }
    // The budgets count the targets above the floors; the evidence ones
    // have their own, and go as a whole.
    if targets.iter().filter(|t| !t.evidence).count() > 256 { return Vec::new(); }
    if targets.iter().filter(|t| t.evidence).count() > 256
        || targets.iter().filter(|t| t.evidence && t.arithmetic.is_some()).count() > arithmetic::MAX_TARGETS
    {
        targets.retain(|t| !t.evidence);
    }
    // Never silently truncate the ambiguity set: an omitted helper might also
    // match. Budget exhaustion disables the entire new family for this module.
    if arithmetic_targets > arithmetic::MAX_TARGETS {
        targets.retain(|t| t.arithmetic.is_none());
    }
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
/// statement-root selector note below is load-bearing and two copies would drift.
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
        // Use the de-inliner's statement selector (not `Traverse::rvalues`): for an
        // `Assign` the latter exposes only `right`, omitting LHS `Index` operands,
        // whereas `collect_written` — which this mirrors — also visits them. A
        // closure rebinding the helper hidden in an LHS index operand
        // (`t[(function() f = g end)()] = x`) must still be counted, so the
        // reassignment-refusal gate cannot be silently bypassed.
        visit_stmt_rvalues(s, &mut |rv| {
            write_counts_in_closures(rv, out);
            true
        });
    }
}

pub(crate) fn write_counts_in_closures(rv: &RValue, out: &mut FxHashMap<RcLocal, usize>) {
    if let RValue::Closure(c) = rv {
        collect_write_counts(&c.function.0.lock().body.0, out);
        return;
    }
    rv.visit_rvalues(&mut |child| {
        write_counts_in_closures(child, out);
        true
    });
}

// ===================================================================
// Traversal: two-phase scope walk mirroring `deinline::deinline_block`
// ===================================================================

fn walk_block(
    stmts: &mut Vec<Statement>,
    w: &mut Walk,
    outer_active: &[usize],
    current_func: Option<FnPtr>,
) {
    // Phase 1: recurse into nested statement blocks and closure bodies. A child
    // only sees targets whose declaration lexically precedes it, so `active` grows
    // as each declaration in THIS block is passed.
    {
        let mut active: Vec<usize> = outer_active.to_vec();
        for s in stmts.iter_mut() {
            match s {
                Statement::If(f) => {
                    walk_block(
                        &mut f.then_block.lock().0,
                        w,
                        &active,
                        current_func,
                    );
                    walk_block(
                        &mut f.else_block.lock().0,
                        w,
                        &active,
                        current_func,
                    );
                }
                Statement::While(node) => walk_block(
                    &mut node.block.lock().0,
                    w,
                    &active,
                    current_func,
                ),
                Statement::Repeat(r) => walk_block(
                    &mut r.block.lock().0,
                    w,
                    &active,
                    current_func,
                ),
                Statement::NumericFor(nf) => walk_block(
                    &mut nf.block.lock().0,
                    w,
                    &active,
                    current_func,
                ),
                Statement::GenericFor(gf) => walk_block(
                    &mut gf.block.lock().0,
                    w,
                    &active,
                    current_func,
                ),
                _ => {}
            }
            visit_stmt_rvalues_mut(s, &mut |rv| {
                recurse_into_closures(rv, w, &active);
                true
            });
            if let Some(range) = target_decl_index(s, w) {
                active.extend(range);
            }
        }
    }

    // Phase 2: scan this block left to right, matching each statement's own
    // expressions, activating each target after its declaration.
    let mut active: Vec<usize> = outer_active.to_vec();
    if matches!(w.round, Round::Probe) {
        // The calls of the evidence helpers tried here already rebuilt in
        // this block: the count oracle counts them with the matches.
        let binders: FxHashSet<RcLocal> = w
            .targets
            .iter()
            .filter(|t| t.evidence && w.copies_in(current_func, t) > 0)
            .map(|t| t.f_local.clone())
            .collect();
        if !binders.is_empty() {
            crate::deinline::count_rebuilt_calls(stmts, &binders, current_func, &mut w.rebuilt);
        }
    }
    let mut index = 0;
    while index < stmts.len() {
        // A bounded terminal scalar region has no live continuation. Normalize
        // its lets/guard returns before comparing with named helper patterns.
        // (Not in evidence rounds.)
        if matches!(w.round, Round::Normal) {
            if try_rewrite_region(&mut stmts[index..], w.targets, &active, current_func) {
                stmts.truncate(index + 1);
                break;
            }
            try_rewrite_select(stmts, index, w.targets, &active, current_func);
        }
        let s = &mut stmts[index];
        index += 1;
        // Skip the per-statement rvalue scan (and its allocation) entirely until a
        // helper is in scope.
        if !active.is_empty() {
            visit_stmt_rvalues_mut(s, &mut |rv| {
                try_rewrite(rv, w, &active, current_func);
                true
            });
        }
        if let Some(range) = target_decl_index(s, w) {
            active.extend(range);
        }
    }
}

/// Descend `rv` to find closures, running the full walk on each closure body with
/// `current_func` set to that closure (so a helper never matches inside its own
/// body) and the current `active` set (targets in scope at the closure's site are
/// visible inside it as upvalues).
fn recurse_into_closures(
    rv: &mut RValue,
    w: &mut Walk,
    active: &[usize],
) {
    if let RValue::Closure(c) = rv {
        let fp = Arc::as_ptr(&c.function.0);
        let mut function = c.function.0.lock();
        if w.copies.is_some() {
            w.protos.insert(fp, function.bytecode_proto_id);
        }
        walk_block(&mut function.body.0, w, active, Some(fp));
        return;
    }
    rv.visit_rvalues_mut(&mut |child| {
        recurse_into_closures(child, w, active);
        true
    });
}

/// If `s` is the declaration `local f = function ... end` of one of our targets,
/// returns that target's index (scope activation, mirroring
/// `deinline::target_decl_index`).
fn target_decl_index(s: &Statement, w: &Walk) -> Option<std::ops::Range<usize>> {
    if let Statement::Assign(a) = s
        && a.prefix
        && a.left.len() == 1
        && a.right.len() == 1
        && let LValue::Local(l) = &a.left[0]
        && let RValue::Closure(c) = &a.right[0]
        && let Some(range) = w.decl_map.get(l)
        && Arc::as_ptr(&c.function.0) == w.targets[range.start].func_ptr
    {
        return Some(range.clone());
    }
    None
}

// ===================================================================
// Matching + rewrite (outermost-first)
// ===================================================================

fn try_rewrite_select(
    stmts: &mut Vec<Statement>, index: usize, targets: &[ExprTarget], active: &[usize],
    current_func: Option<FnPtr>,
) {
    if active.is_empty() || current_func.is_some_and(|p| targets.iter().any(|t| t.func_ptr == p)) { return; }
    let [Statement::Assign(decl), Statement::If(_)] = &stmts[index..stmts.len().min(index + 2)] else { return; };
    if !decl.prefix || decl.parallel || decl.left.len() != 1 { return; }
    let LValue::Local(result) = &decl.left[0] else { return; };
    let result = result.clone();
    // The result binding is retained, including debug identity. Captured cells
    // refuse because moving the nil initialization inside a call is observable.
    if !targets[active[0]].captures.uncaptured(&result) { return; }
    let candidate = vec![stmts[index].clone(), stmts[index + 1].clone(), crate::Return::new(vec![result.clone().into()]).into()];
    let Some(value) = arithmetic::region(&candidate, &targets[active[0]].captures) else { return; };
    let mut pick = Pick::default();
    let ordered = crate::reconstruction_search::prioritize(active, current_func.map(|p| p as usize), |i| targets[i].func_ptr as usize);
    for idx in ordered {
        let target = &targets[idx];
        let Some(safety) = &target.arithmetic else { continue; };
        if !safety.spend_attempt() { return; }
        if let Some(found) = try_match(target, &value, current_func) {
            pick.offer(idx, found);
            if pick.settled() { return; }
        }
    }
    let Some((idx, args)) = pick.take() else { return; };
    let target = &targets[idx];
    let call = Call::new(target.f_local.clone().into(), args).reconstructed(crate::call_origins::Kind::ArithmeticDeinline);
    // One result, declared the way the lifter declares any `local r = f()`, so
    // later temp inlining can place it where the value is used.
    let value = RValue::Select(crate::Select::Call(call));
    stmts.splice(index..index + 2, [crate::Assign { node_origin: Default::default(), left: vec![result.into()], right: vec![value], prefix: true, parallel: false, compound: false}.into()]);
}

fn try_rewrite_region(
    stmts: &mut [Statement],
    targets: &[ExprTarget],
    active: &[usize],
    current_func: Option<FnPtr>,
) -> bool {
    if active.is_empty() || stmts.len() > 8 || matches!(stmts, [Statement::Return(_)]) { return false; }
    if current_func.is_some_and(|ptr| targets.iter().any(|t| t.func_ptr == ptr)) { return false; }
    let Some(value) = arithmetic::region(stmts, &targets[active[0]].captures) else { return false; };
    let mut declared = FxHashSet::default();
    crate::deinline::collect_declared_locals(stmts, &mut declared);
    let mut pick = Pick::default();
    for &idx in active {
        let target = &targets[idx];
        let Some(safety) = &target.arithmetic else { continue; };
        if current_func == Some(target.func_ptr) { continue; }
        if !safety.spend_attempt() { return false; }
        if declared.iter().any(|l| l.has_source_binding() || !target.captures.uncaptured(l)) { continue; }
        if let Some(found) = try_match(target, &value, current_func) {
            pick.offer(idx, found);
            if pick.settled() { return false; }
        }
    }
    let Some((idx, args)) = pick.take() else { return false; };
    let target = &targets[idx];
    let call = Call::new(target.f_local.clone().into(), args)
        .reconstructed(crate::call_origins::Kind::ArithmeticDeinline);
    stmts[0] = crate::Return::new(vec![call.into()]).into();
    true
}

fn try_rewrite(
    rv: &mut RValue,
    w: &mut Walk,
    active: &[usize],
    current_func: Option<FnPtr>,
) {
    let targets = w.targets;
    // No helper is in lexical scope here, so no node in this subtree can match
    // (`active` is monotone-nondecreasing down the descent, and `try_match` only
    // considers `active` targets). Prune the whole subtree — this skips the entire
    // pre-declaration region of every block. Mirrors the statement pass's
    // `if active.is_empty()` guard in `deinline::try_match_at`.
    if active.is_empty() || current_func.is_some_and(|ptr| targets.iter().any(|t| t.protect_definition && t.func_ptr == ptr)) {
        return;
    }
    // Outermost-first: try to match the WHOLE node before descending, so the
    // largest equivalent subtree is collapsed into one call.
    if let Some(cands) = w.by_root.get(&std::mem::discriminant(&*rv)) {
        let mut pick = Pick::default();
        let mut ambiguous = false;
        let mut ordered = crate::reconstruction_search::prioritize(cands, current_func.map(|p| p as usize), |i| targets[i].func_ptr as usize);
        // In an evidence round the targets that may be rebuilt here come
        // first, and the rivals are tried only where one of them matches.
        let rivals_from = if matches!(w.round, Round::Normal) {
            ordered.len()
        } else {
            ordered.retain(|&idx| active.contains(&idx) && w.role(idx, current_func).1);
            ordered.sort_by_key(|&idx| !w.role(idx, current_func).0);
            ordered.iter().position(|&idx| !w.role(idx, current_func).0).unwrap_or(ordered.len())
        };
        for (at, &idx) in ordered.iter().enumerate() {
            if at == rivals_from && pick.is_empty() {
                break; // no target that may be rebuilt here matched
            }
            if !active.contains(&idx) {
                continue; // helper not yet in lexical scope here
            }
            if !w.role(idx, current_func).1 {
                continue; // not tried in this round
            }
            let t = &targets[idx];
            if current_func == Some(t.func_ptr) {
                continue; // never match a helper against its own body
            }
            if !t.search.spend(2048) { ambiguous = true; break; }
            if let Some(safety) = &t.arithmetic {
                if !safety.spend_attempt() {
                    ambiguous = true;
                    break;
                }
            }
            if let Some(found) = try_match(t, rv, current_func) {
                pick.offer(idx, found);
                if pick.settled() {
                    break;
                }
            }
        }
        if !ambiguous {
            if let Some((idx, args)) = pick.take() {
                let t = &targets[idx];
                // In an evidence round only an admitted helper is rebuilt;
                // a match counts (probe), and any other one keeps its node
                // as the walk without line evidence left it.
                let (rebuilt, _) = w.role(idx, current_func);
                if !matches!(w.round, Round::Normal) {
                    if rebuilt {
                        w.probed.entry((current_func, t.f_local.clone())).or_insert((0, t.proto)).0 += 1;
                    }
                    if !rebuilt || matches!(w.round, Round::Probe) {
                        return;
                    }
                }
                let call = Call::new(RValue::Local(t.f_local.clone()), args).reconstructed(
                    if t.arithmetic.is_some() { crate::call_origins::Kind::ArithmeticDeinline }
                    else { crate::call_origins::Kind::ExpressionDeinline });
                *rv = RValue::Call(call);
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
    rv.visit_rvalues_mut(&mut |child| {
        try_rewrite(child, w, active, current_func);
        true
    });
}

/// Attempt to unify target `t`'s body `E` against the candidate subtree `rv` and,
/// if it matches under all gates, return the reconstructed argument list.
/// Upvalue local ids of every function, by its identity.
pub(super) type FunctionUpvalues = FxHashMap<usize, FxHashSet<u64>>;

pub(super) fn function_upvalues(body: &Block) -> FunctionUpvalues {
    fn visit(value: &RValue, out: &mut FunctionUpvalues) {
        if let RValue::Closure(closure) = value {
            let function = Arc::as_ptr(&closure.function.0) as usize;
            if !out.contains_key(&function) {
                let ids = closure.upvalues.iter().map(|upvalue| {
                    let (crate::Upvalue::Copy(local) | crate::Upvalue::Ref(local)) = upvalue;
                    local.stable_id()
                });
                out.insert(function, ids.collect());
                block(&closure.function.0.lock().body.0, out);
            }
            return;
        }
        value.visit_rvalues(&mut |child| { visit(child, out); true });
    }
    fn block(stmts: &[Statement], out: &mut FunctionUpvalues) {
        for statement in stmts {
            crate::deinline::visit_stmt_rvalues(statement, &mut |value| { visit(value, out); true });
            match statement {
                Statement::If(f) => {
                    block(&f.then_block.lock().0, out);
                    block(&f.else_block.lock().0, out);
                }
                Statement::While(w) => block(&w.block.lock().0, out),
                Statement::Repeat(r) => block(&r.block.lock().0, out),
                Statement::NumericFor(nf) => block(&nf.block.lock().0, out),
                Statement::GenericFor(gf) => block(&gf.block.lock().0, out),
                _ => {}
            }
        }
    }
    let mut out = FunctionUpvalues::default();
    block(&body.0, &mut out);
    out
}

/// Whether `arg`, at a site in `current_func`, is a register local, which an
/// operation reads only when it runs.
fn is_register(arg: &RValue, upvalues: &FunctionUpvalues, current_func: Option<FnPtr>) -> bool {
    let RValue::Local(local) = arg else { return false };
    current_func
        .and_then(|function| upvalues.get(&(function as usize)))
        .is_none_or(|ids| !ids.contains(&local.stable_id()))
}

fn try_match(t: &ExprTarget, rv: &RValue, current_func: Option<FnPtr>) -> Option<(Vec<RValue>, Hoist)> {
    let mut b = Bindings::default();
    let matched = if t.arithmetic.is_some() {
        arithmetic::unify(&t.ctx(), &t.expr, rv, &mut b)
    } else {
        unify_rvalue(&t.ctx(), &t.expr, rv, &mut b).is_ok()
    };
    if !matched {
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
    // Literals and non-reference-captured locals are total, identity-stable
    // snapshots. A call/metamethod in this expression may mutate a referenced
    // cell despite there being no syntactic assignment in the expression.
    if t.free_cells.iter().any(|local| {
        t.captures.register_of(local, current_func.map(|function| function as usize))
            && crate::evaluation_order::value_late_operand_conflict(rv, local, &t.captures.may_change(local))
    }) {
        return None;
    }
    let hoist = hoist(&t.expr, &t.param_order, &args, |a| t.captures.stable(a), |arg| {
        is_register(arg, &t.upvalues, current_func)
    }, &t.leading)?;
    // The complete CaptureSafety census already excludes every reference-
    // captured local. The arithmetic family's former second census visited
    // the same statement roots (including indexed LHS) and closure bodies;
    // revisiting shared bodies only repeated set insertions. Its other allowed
    // arguments were all literals, a superset of stable() above. Therefore that
    // additional gate and full-module census cannot reject an argument here.
    // Cost: the replacement must be a net node saving against the specialised
    // subtree `S` (rejects `f(bigExpr)` non-shrinks). Computed only on a real match.
    let s_nodes = node_count(rv);
    let args_nodes: usize = args.iter().map(node_count).sum();
    let call_nodes = if t.arithmetic.is_some() { 2 } else { 1 };
    // Where line evidence admits the helper, the call need only be no
    // larger than the copy.
    let floor = if t.evidence { 0 } else { NET_SAVING_FLOOR };
    if s_nodes < call_nodes + args_nodes + floor {
        return None;
    }
    Some((args.into_iter().map(crate::untruncated).collect(), hoist))
}

/// Why a rebuilt call may evaluate its arguments eagerly, before the helper
/// body. Ordered: a `Stable` match outranks a `FirstRead` one (see [`Pick`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Hoist {
    /// Every argument is `stable`: evaluating it early is unobservable.
    Stable,
    /// Some are not, but their parameters are read exactly once each, in
    /// parameter order, before anything observable in the body: Luau
    /// evaluated those arguments into the parameters' registers right before
    /// the inlined body, so the call keeps the original order
    /// (`toSCurveSpace(math.abs(x))`, whose body starts with `math.abs(t)`).
    FirstRead,
}

/// How `args` may be hoisted before the helper body `expr`, or `None` when
/// hoisting them could change what the inlined copy did.
pub(super) fn hoist(
    expr: &RValue,
    params: &[RcLocal],
    args: &[RValue],
    stable: impl Fn(&RValue) -> bool,
    // Whether an argument is a register local of the site's function, read
    // where its operation runs.
    register: impl Fn(&RValue) -> bool,
    leading: &crate::evaluation_order::LeadingReads,
) -> Option<Hoist> {
    let unstable: Vec<usize> = (0..args.len()).filter(|&i| !stable(&args[i])).collect();
    if unstable.is_empty() {
        return Some(Hoist::Stable);
    }
    let in_order = unstable.iter().all(|&i| reads_of(expr, &params[i]) == 1)
        && leading.admits(unstable.iter().map(|&i| (&params[i], register(&args[i]))));
    in_order.then_some(Hoist::FirstRead)
}

/// The parameters a helper body reads first ([`ExprTarget::leading`]).
pub(super) fn leading_reads(
    body: &[Statement],
    parameters: &[RcLocal],
    captures: &crate::deinline_safety::CaptureSafety,
) -> crate::evaluation_order::LeadingReads {
    // The helper's parameters and locals are its registers; an outer local
    // is its upvalue, fetched where it stands. The expression checks the
    // count of reads itself (`hoist`).
    let mut declared = FxHashSet::default();
    crate::deinline::collect_declared_locals(body, &mut declared);
    let facts = crate::evaluation_order::Body {
        registers: &|local| parameters.contains(local) || declared.contains(local),
        unchanged: &|value| captures.unchanged_by_calls(value),
    };
    crate::evaluation_order::LeadingReads::new(body, parameters, |_| true, &facts)
}

/// The one helper call to rebuild at a site. A match with stable arguments
/// outranks a [`Hoist::FirstRead`] one: `multiplyHue(h, s)` over `warp(h + (s -
/// 0.5) * 1)` when `multiplyHue`'s body is `warp`'s, inlined. Two matches of
/// the best rank are ambiguous, and nothing is rebuilt.
#[derive(Default)]
pub(super) struct Pick {
    best: Option<(usize, Vec<RValue>, Hoist)>,
    tied: bool,
}

impl Pick {
    pub(super) fn offer(&mut self, target: usize, (args, hoist): (Vec<RValue>, Hoist)) {
        match &self.best {
            Some((.., best)) if *best < hoist => {}
            Some((.., best)) if *best == hoist => self.tied = true,
            _ => {
                self.best = Some((target, args, hoist));
                self.tied = false;
            }
        }
    }

    /// Whether no later offer can change the outcome: a tie of stable matches.
    pub(super) fn settled(&self) -> bool {
        self.tied && matches!(self.best, Some((.., Hoist::Stable)))
    }

    /// Whether nothing was offered yet.
    pub(super) fn is_empty(&self) -> bool {
        self.best.is_none()
    }

    pub(super) fn take(self) -> Option<(usize, Vec<RValue>)> {
        if self.tied {
            crate::reconstruction_stats::refuse_site("ambiguous_expression");
            return None;
        }
        let (target, args, _) = self.best?;
        Some((target, args))
    }
}

fn reads_of(value: &RValue, local: &RcLocal) -> usize {
    let mut reads = usize::from(matches!(value, RValue::Local(read) if read == local));
    value.visit_rvalues(&mut |child| {
        reads += reads_of(child, local);
        true
    });
    reads
}

/// Number of RValue nodes in `rv`. Single post-order recursion via the `Traverse`
/// child accessor (each node visited once → O(n)); a `Closure` exposes no child
/// rvalues, so its body is not counted.
fn node_count(rv: &RValue) -> usize {
    let mut children = 0;
    rv.visit_rvalues(&mut |child| { children += node_count(child); true });
    1 + children
}

#[cfg(test)]
mod reference;
#[cfg(test)]
mod differential;

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
            node_origin: Default::default(),
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
            bin(
                call(global("typeof"), vec![v.clone()]),
                BinaryOperation::Equal,
                string("number"),
            ),
            BinaryOperation::And,
            bin(v.clone(), BinaryOperation::GreaterThan, number(0.0)),
        )
    }

    fn helper_decl(
        f: &RcLocal,
        name: &str,
        params: Vec<RcLocal>,
        body: Vec<Statement>,
    ) -> Statement {
        let func = Arc::new(Mutex::new(Function {
            name: Some(name.to_string()),
            parameters: params,
            body: Block(body),
            ..Default::default()
        }));
        Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(f.clone())],
            right: vec![RValue::Closure(Closure {
                node_origin: Default::default(),
                function: ByAddress(func),
                upvalues: vec![],
            })],
            prefix: true,
            parallel: false, compound: false,
        })
    }

    /// `local r = <rhs>` (a declaration).
    fn local_decl(r: &RcLocal, rhs: RValue) -> Statement {
        Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(r.clone())],
            right: vec![rhs],
            prefix: true,
            parallel: false, compound: false,
        })
    }

    fn rhs_of(s: &Statement) -> &RValue {
        match s {
            Statement::Assign(a) => &a.right[0],
            _ => panic!("expected assign"),
        }
    }

    fn is_call_to(rv: &RValue, f: &RcLocal) -> bool {
        matches!(rv, RValue::Call(c) | RValue::Select(crate::Select::Call(c))
            if matches!(c.value.as_ref(), RValue::Local(l) if l == f))
    }

    #[test]
    fn a_literal_evaluated_for_an_argument_of_a_rebuilt_call_goes_into_it() {
        let sorter = local("sorter");
        let rebuilt = |argument: RValue| {
            RValue::Call(
                Call::new(lv(&sorter), vec![argument, RValue::Literal(Literal::Boolean(true))])
                    .reconstructed(crate::call_origins::Kind::ExpressionDeinline),
            )
        };
        let binders = FxHashSet::from_iter([sorter.clone()]);
        // `local x = "Order"; t.a = sorter(x, true)` -> `t.a = sorter("Order", true)`.
        let (x, t) = (RcLocal::default(), local("t"));
        let store = |value: RValue| {
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(lv(&t), string("a")))],
                vec![value],
            ))
        };
        let mut block = vec![local_decl(&x, string("Order")), store(rebuilt(lv(&x)))];
        fold_literal_arguments(&mut block, &binders);
        assert_eq!(block.len(), 1);
        assert_eq!(rhs_of(&block[0]), &rebuilt(string("Order")));
        // Read again later, or written: the declaration stays.
        let y = RcLocal::default();
        let mut block = vec![local_decl(&y, string("Order")), store(rebuilt(lv(&y))), store(lv(&y))];
        fold_literal_arguments(&mut block, &binders);
        assert_eq!(block.len(), 3);
        // A call no evidence round rebuilt keeps its argument.
        let z = RcLocal::default();
        let mut block = vec![local_decl(&z, string("Order")), store(rebuilt(lv(&z)))];
        fold_literal_arguments(&mut block, &FxHashSet::default());
        assert_eq!(block.len(), 2);
    }

    #[test]
    fn arguments_that_run_code_hoist_only_into_the_leading_reads_of_their_parameters() {
        let (t, u, x) = (local("t"), local("u"), local("x"));
        let plain = |arg: &RValue| !matches!(arg, RValue::Call(_));
        let register = |arg: &RValue| matches!(arg, RValue::Local(_));
        let runs_code = || call(global("f"), vec![]);
        let pi = RValue::Index(Index::new(global("math"), string("pi")));
        let field = RValue::Index(Index::new(lv(&x), string("y")));
        let unchanged = |_: &RValue| true;
        let leading = |body: &RValue, params: &[RcLocal], unchanged: &dyn Fn(&RValue) -> bool| {
            let facts = crate::evaluation_order::Body { registers: &|_| true, unchanged };
            let body: Statement = crate::Return::new(vec![body.clone()]).into();
            crate::evaluation_order::LeadingReads::new(std::slice::from_ref(&body), params, |_| true, &facts)
        };
        let hoist_into = |body: RValue| {
            let leading = leading(&body, std::slice::from_ref(&t), &unchanged);
            hoist(&body, std::slice::from_ref(&t), &[runs_code()], plain, register, &leading)
        };

        let sum = bin(lv(&t), BinaryOperation::Add, lv(&u));
        let params = [t.clone(), u.clone()];
        assert!(hoist(&sum, &params, &[lv(&x), number(1.0)], plain, register, &leading(&sum, &params, &unchanged)) == Some(Hoist::Stable));
        // `math.pi * t - x`: import paths and local reads are not observable.
        let first = bin(bin(pi, BinaryOperation::Mul, lv(&t)), BinaryOperation::Sub, lv(&x));
        assert!(hoist_into(first) == Some(Hoist::FirstRead));
        // A call or an index runs before `t` is read.
        assert!(hoist_into(bin(call(global("g"), vec![]), BinaryOperation::Add, lv(&t))).is_none());
        assert!(hoist_into(bin(field, BinaryOperation::Add, lv(&t))).is_none());
        // Read twice, or only on one path.
        assert!(hoist_into(bin(lv(&t), BinaryOperation::Add, lv(&t))).is_none());
        assert!(hoist_into(bin(lv(&x), BinaryOperation::And, lv(&t))).is_none());
        // Several arguments move when their parameters lead in parameter
        // order, never in another order.
        let both = bin(lv(&t), BinaryOperation::Add, lv(&u));
        let calls = [runs_code(), runs_code()];
        assert!(hoist(&both, &params, &calls, plain, register, &leading(&both, &params, &unchanged)) == Some(Hoist::FirstRead));
        let reversed = [u.clone(), t.clone()];
        assert!(hoist(&both, &reversed, &calls, plain, register, &leading(&both, &reversed, &unchanged)).is_none());
        // ... nor with something observable between them.
        let between = bin(bin(lv(&t), BinaryOperation::Add, call(global("g"), vec![])), BinaryOperation::Add, lv(&u));
        assert!(hoist(&between, &params, &calls, plain, register, &leading(&between, &params, &unchanged)).is_none());
        // `x * t` where code may change `x`: Luau reads the register `x` when
        // `*` runs, after a call standing for `t`, but before a register
        // local standing for it.
        let changed = |value: &RValue| !matches!(value, RValue::Local(local) if *local == x);
        let product = bin(lv(&x), BinaryOperation::Mul, lv(&t));
        let first = leading(&product, std::slice::from_ref(&t), &changed);
        assert!(hoist(&product, std::slice::from_ref(&t), &[runs_code()], plain, register, &first) == Some(Hoist::FirstRead));
        let held = local("register");
        let unstable = |arg: &RValue| !matches!(arg, RValue::Local(local) if *local == held);
        assert!(hoist(&product, std::slice::from_ref(&t), &[lv(&held)], unstable, register, &first).is_none());
    }

    #[test]
    fn a_match_with_stable_arguments_outranks_a_reordered_one() {
        let args = |n: f64| vec![number(n)];
        let mut pick = Pick::default();
        pick.offer(0, (args(0.0), Hoist::FirstRead));
        pick.offer(1, (args(1.0), Hoist::Stable));
        pick.offer(2, (args(2.0), Hoist::FirstRead));
        assert!(!pick.settled());
        assert!(matches!(pick.take(), Some((1, _))));

        let mut tied = Pick::default();
        tied.offer(0, (args(0.0), Hoist::FirstRead));
        tied.offer(1, (args(1.0), Hoist::FirstRead));
        assert!(!tied.settled(), "a stable match could still win");
        tied.offer(2, (args(2.0), Hoist::Stable));
        tied.offer(3, (args(3.0), Hoist::Stable));
        assert!(tied.settled());
        assert!(tied.take().is_none());
    }

    #[test]
    fn recovers_inlined_expression() {
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        // the caller decl's RHS is now `isNumPositive(x)`.
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
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
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
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
            local_decl(&r, num_positive(&get_x())),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(block.0.last().unwrap());
        assert!(!is_call_to(rv, &f), "side-effecting arg must be refused");
        assert!(
            matches!(rv, RValue::Binary(_)),
            "expression must stay inlined"
        );
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
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(&block.0[0]);
        assert!(
            !is_call_to(rv, &f),
            "out-of-scope use must not be rewritten"
        );
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
            helper_decl(
                &f,
                "isPositive",
                vec![p.clone()],
                vec![Return::new(vec![triv(&lv(&p))]).into()],
            ),
            local_decl(&r, triv(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "trivial helper must be refused"
        );
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
                bin(
                    call(global("typeof"), vec![v.clone()]),
                    BinaryOperation::Equal,
                    string("number"),
                ),
                BinaryOperation::And,
                lv(fl),
            )
        };
        let mut block = Block(vec![
            helper_decl(
                &f,
                "rec",
                vec![p.clone()],
                vec![Return::new(vec![body_e(&lv(&p), &f)]).into()],
            ),
            local_decl(&r, body_e(&lv(&x), &f)),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "recursive helper must be refused"
        );
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
            bin(
                call(global("typeof"), vec![lv(&x)]),
                BinaryOperation::Equal,
                string("number"),
            ),
            BinaryOperation::And,
            bin(lv(&y), BinaryOperation::GreaterThan, number(0.0)),
        );
        let mut block = Block(vec![
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
            local_decl(&r, diverged),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "inconsistent param binding must refuse"
        );
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
            helper_decl(
                &f1,
                "checkA",
                vec![p1.clone()],
                vec![Return::new(vec![num_positive(&lv(&p1))]).into()],
            ),
            helper_decl(
                &f2,
                "checkB",
                vec![p2.clone()],
                vec![Return::new(vec![num_positive(&lv(&p2))]).into()],
            ),
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        let rv = rhs_of(block.0.last().unwrap());
        assert!(
            !is_call_to(rv, &f1) && !is_call_to(rv, &f2),
            "ambiguous match must be refused"
        );
    }

    #[test]
    fn idempotent_second_run_is_noop() {
        let f = local("isNumPositive");
        let p = local("p");
        let x = local("x");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
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
        let e = |v: &RValue| {
            call(
                global("transform"),
                vec![call(global("typeof"), vec![v.clone()]), string("number")],
            )
        };
        let mut block = Block(vec![
            helper_decl(
                &f,
                "getThing",
                vec![p.clone()],
                vec![Return::new(vec![e(&lv(&p))]).into()],
            ),
            local_decl(&r, e(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "Call-rooted helper must not be a target"
        );
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
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
            local_decl(&r, num_positive(&sum())),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "throwing compound arg must be refused"
        );
    }

    #[test]
    fn accepts_literal_arg() {
        // A bare Literal arg is total + value-stable → accepted.
        let f = local("isNumPositive");
        let p = local("p");
        let r = local("r");
        let mut block = Block(vec![
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
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
            node_origin: Default::default(),
            left: vec![LValue::Local(f.clone())],
            right: vec![global("otherFn")],
            prefix: false,
            parallel: false, compound: false,
        });
        let mut block = Block(vec![
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
            reassign,
            local_decl(&r, num_positive(&lv(&x))),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "reassigned helper must be refused"
        );
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
            helper_decl(
                &f,
                "isNumPositive",
                vec![p.clone()],
                vec![Return::new(vec![num_positive(&lv(&p))]).into()],
            ),
            local_decl(&r, num_positive(&field)),
        ]);
        expr_deinline(&mut block);
        assert!(
            !is_call_to(rhs_of(block.0.last().unwrap()), &f),
            "field-access arg must be refused"
        );
    }

    fn adjust_decl(f: &RcLocal) -> Statement {
        let value = local("value");
        let bias = local("bias");
        let mut decl = helper_decl(f, "adjust", vec![value.clone(), bias.clone()], vec![
            crate::If::new(
                bin(lv(&value), BinaryOperation::LessThan, number(0.0)),
                Block(vec![Return::new(vec![lv(&bias)]).into()]),
                Block(vec![
                    Return::new(vec![bin(
                        bin(lv(&value), BinaryOperation::Mul, number(2.0)),
                        BinaryOperation::Add,
                        lv(&bias),
                    )])
                    .into(),
                ]),
            )
            .into(),
        ]);
        if let Statement::Assign(a) = &mut decl {
            if let RValue::Closure(c) = &mut a.right[0] {
                c.function.0.lock().bytecode_proto_id = Some(1);
            }
        }
        decl
    }

    fn adjust_copy(value: RValue, bias: RValue) -> RValue {
        bin(
            bin(
                bin(value.clone(), BinaryOperation::LessThan, number(0.0)),
                BinaryOperation::And,
                bias.clone(),
            ),
            BinaryOperation::Or,
            bin(
                bin(value, BinaryOperation::Mul, number(2.0)),
                BinaryOperation::Add,
                bias,
            ),
        )
    }

    #[test]
    fn named_arithmetic_recovers_two_scalar_calls_and_marks_inference() {
        let f = local("adjust");
        let x = local("x");
        let next = local("next");
        let a = local("a");
        let b = local("b");
        let mut block = Block(vec![
            adjust_decl(&f),
            local_decl(&a, adjust_copy(lv(&x), number(3.0))),
            local_decl(&next, bin(lv(&x), BinaryOperation::Add, number(1.0))),
            local_decl(&b, adjust_copy(lv(&next), number(3.0))),
        ]);
        expr_deinline(&mut block);
        // No marker statement: the rebuilt calls carry their own attribute.
        assert!(block.0.iter().all(|statement| !matches!(statement, Statement::Comment(_))));
        for (index, argument) in [(1, &x), (3, &next)] {
            let RValue::Call(call) = rhs_of(&block.0[index]) else {
                panic!("missing call");
            };
            assert!(is_call_to(rhs_of(&block.0[index]), &f));
            assert!(call.is_inferred());
            assert_eq!(call.arguments, vec![lv(argument), number(3.0)]);
        }
        let before = block.to_string();
        expr_deinline(&mut block);
        assert_eq!(before, block.to_string());
    }

    #[test]
    fn named_arithmetic_refuses_false_nil_and_compound_arguments() {
        let f = local("adjust");
        let x = local("x");
        let r = local("r");
        for (value, bias) in [
            (lv(&x), Literal::Boolean(false).into()),
            (lv(&x), Literal::Nil.into()),
            (lv(&x), lv(&local("unknownTruth"))),
            (bin(lv(&x), BinaryOperation::Add, number(1.0)), number(3.0)),
            (call(global("nextValue"), vec![]), number(3.0)),
        ] {
            let mut block = Block(vec![
                adjust_decl(&f),
                local_decl(&r, adjust_copy(value, bias)),
            ]);
            let before = block.to_string();
            expr_deinline(&mut block);
            assert_eq!(before, block.to_string());
        }
    }

    #[test]
    fn named_arithmetic_requires_prototype_and_unambiguous_helper() {
        let f = local("adjust");
        let x = local("x");
        let r = local("r");
        for ambiguous in [false, true] {
            let decl = adjust_decl(&f);
            if !ambiguous {
                let RValue::Closure(c) = rhs_of(&decl) else {
                    unreachable!();
                };
                c.function.0.lock().bytecode_proto_id = None;
            }
            let mut block = Block(vec![decl]);
            if ambiguous {
                block.0.push(adjust_decl(&local("alsoAdjust")));
            }
            block
                .0
                .push(local_decl(&r, adjust_copy(lv(&x), number(3.0))));
            let before = block.to_string();
            expr_deinline(&mut block);
            assert_eq!(before, block.to_string());
        }
    }

    #[test]
    fn named_arithmetic_refuses_reference_captured_argument() {
        let f = local("adjust");
        let x = local("x");
        let callback = local("mutate");
        let r = local("r");
        let decl = helper_decl(&callback, "mutate", vec![], vec![
            Assign {
                node_origin: Default::default(),
                left: vec![LValue::Local(x.clone())],
                right: vec![number(9.0)],
                prefix: false,
                parallel: false, compound: false,
            }
            .into(),
        ]);
        let mut decl = decl;
        if let Statement::Assign(a) = &mut decl {
            if let RValue::Closure(c) = &mut a.right[0] {
                c.upvalues.push(crate::Upvalue::Ref(x.clone()));
            }
        }
        let mut block = Block(vec![
            adjust_decl(&f),
            decl,
            local_decl(&r, adjust_copy(lv(&x), number(3.0))),
        ]);
        let before = block.to_string();
        expr_deinline(&mut block);
        assert_eq!(before, block.to_string());
    }

    #[test]
    fn legacy_expression_refuses_cell_changed_by_a_call_or_metamethod() {
        for copy in [false, true] {
            let f = local("positive"); let p = local("p"); let x = local("x");
            let mut writer = helper_decl(&local("writer"), "writer", vec![], vec![
                Assign::new(vec![x.clone().into()], vec![number(-1.0)]).into()
            ]);
            if let Statement::Assign(a) = &mut writer {
                if let RValue::Closure(c) = &mut a.right[0] {
                    c.upvalues.push(if copy { crate::Upvalue::Copy(x.clone()) } else { crate::Upvalue::Ref(x.clone()) });
                }
            }
            let mut block = Block(vec![
                helper_decl(&f, "positive", vec![p.clone()], vec![Return::new(vec![num_positive(&lv(&p))]).into()]),
                writer,
                local_decl(&local("result"), num_positive(&lv(&x))),
            ]);
            expr_deinline(&mut block);
            assert_eq!(is_call_to(rhs_of(block.0.last().unwrap()), &f), copy);
        }
    }

    #[test]
    fn scalar_region_normalizes_lets_on_either_side_without_duplicating_effects() {
        fn sum(value: RValue) -> RValue {
            bin(bin(value, BinaryOperation::Add, number(3.0)), BinaryOperation::Add, number(4.0))
        }
        for helper_let in [false, true] {
            let f = local("scale"); let p = local("p"); let temp = local("product");
            let x = local("x"); let site = local("v");
            let product = |v| bin(v, BinaryOperation::Mul, number(2.0));
            let body = if helper_let {
                vec![local_decl(&temp, product(lv(&p))), Return::new(vec![sum(lv(&temp))]).into()]
            } else { vec![Return::new(vec![sum(product(lv(&p)))]).into()] };
            let mut declaration = helper_decl(&f, "scale", vec![p], body);
            if let Statement::Assign(a) = &mut declaration {
                if let RValue::Closure(c) = &mut a.right[0] { c.function.0.lock().bytecode_proto_id = Some(7); }
            }
            let mut block = Block(vec![declaration]);
            if helper_let { block.0.push(Return::new(vec![sum(product(lv(&x)))]).into()); }
            else { block.0.extend([local_decl(&site, product(lv(&x))), Return::new(vec![sum(lv(&site))]).into()]); }
            expr_deinline(&mut block);
            let Statement::Return(ret) = block.0.last().unwrap() else { panic!(); };
            assert!(is_call_to(&ret.values[0], &f), "{}", block);
        }
        let p = local("p"); let t = local("t");
        let product = bin(lv(&p), BinaryOperation::Mul, number(2.0));
        for tail in [
            bin(lv(&t), BinaryOperation::Add, lv(&t)),
            bin(bin(lv(&p), BinaryOperation::Add, number(1.0)), BinaryOperation::Add, lv(&t)),
            crate::IfExpression::new(lv(&p), lv(&t), number(0.0)).into(),
        ] {
            assert!(arithmetic::region(&[local_decl(&t, product.clone()), Return::new(vec![tail]).into()], &crate::deinline_safety::CaptureSafety::default()).is_none());
        }
    }

    #[test]
    fn scalar_phi_normalization_preserves_false_nil_and_branch_arity() {
        let f = local("adjust"); let x = local("x"); let result = local("v");
        let branch = crate::If::new(
            bin(lv(&x), BinaryOperation::LessThan, number(0.0)),
            Block(vec![Assign::new(vec![result.clone().into()], vec![Literal::Boolean(false).into()]).into()]),
            Block(vec![Assign::new(vec![result.clone().into()], vec![bin(bin(lv(&x), BinaryOperation::Mul, number(2.0)), BinaryOperation::Add, Literal::Boolean(false).into())]).into()]),
        );
        let mut block = Block(vec![adjust_decl(&f),
            Assign { node_origin: Default::default(), left: vec![result.clone().into()], right: vec![], prefix: true, parallel: false, compound: false}.into(),
            branch.into(), Return::new(vec![lv(&result)]).into()]);
        expr_deinline(&mut block);
        let Statement::Return(ret) = block.0.last().unwrap() else { panic!(); };
        let RValue::Call(call) = &ret.values[0] else { panic!("{}", block); };
        assert_eq!(call.arguments, vec![lv(&x), Literal::Boolean(false).into()]);

        let nil_region = vec![
            Assign { node_origin: Default::default(), left: vec![result.clone().into()], right: vec![], prefix: true, parallel: false, compound: false}.into(),
            crate::If::new(lv(&x), Block(vec![Assign::new(vec![result.clone().into()], vec![number(7.0)]).into()]), Block::default()).into(),
            Return::new(vec![lv(&result)]).into(),
        ];
        let RValue::IfExpression(select) = arithmetic::region(&nil_region, &crate::deinline_safety::CaptureSafety::default()).unwrap() else { panic!(); };
        assert!(matches!(&*select.else_value, RValue::Literal(Literal::Nil)));
        let mut tuple = nil_region.clone();
        tuple[2] = Return::new(vec![lv(&result), number(2.0)]).into();
        assert!(arithmetic::region(&tuple, &crate::deinline_safety::CaptureSafety::default()).is_none());
        let mut late = nil_region;
        late.insert(2, Assign::new(vec![result.into()], vec![number(8.0)]).into());
        assert!(arithmetic::region(&late, &crate::deinline_safety::CaptureSafety::default()).is_none());
    }

    #[test]
    fn early_scalar_phi_keeps_multiuse_result_and_ambiguous_helpers() {
        for ambiguous in [false, true] {
            let f = local("adjust"); let other = local("alsoAdjust");
            let x = local("x"); let result = local("selected");
            let mut statements = vec![adjust_decl(&f)];
            if ambiguous { statements.push(adjust_decl(&other)); }
            statements.extend([
                Assign { node_origin: Default::default(), left: vec![result.clone().into()], right: vec![], prefix: true, parallel: false, compound: false}.into(),
                crate::If::new(
                    bin(lv(&x), BinaryOperation::LessThan, number(0.0)),
                    Block(vec![Assign::new(vec![result.clone().into()], vec![number(3.0)]).into()]),
                    Block(vec![Assign::new(vec![result.clone().into()], vec![bin(bin(lv(&x), BinaryOperation::Mul, number(2.0)), BinaryOperation::Add, number(3.0))]).into()]),
                ).into(),
                Return::new(vec![lv(&result), lv(&result)]).into(),
            ]);
            let mut block = Block(statements);
            let before = block.to_string();
            arithmetic_deinline_early(&mut block);
            if ambiguous {
                assert_eq!(before, block.to_string());
                expr_deinline(&mut block);
                assert_eq!(before, block.to_string());
            }
            else {
                let assign = block.0.iter().find_map(|s| match s {
                    Statement::Assign(a) if a.left == vec![LValue::Local(result.clone())] => Some(a),
                    _ => None,
                }).unwrap();
                assert!(is_call_to(&assign.right[0], &f), "{block}");
                let ret = block.0.last().unwrap().as_return().unwrap();
                assert_eq!(ret.values, vec![lv(&result), lv(&result)]);
            }
        }
    }

    #[test]
    fn named_arithmetic_keeps_operator_order_and_literal_bits() {
        let f = local("adjust");
        let x = local("x");
        let r = local("r");
        for swap in [false, true] {
            let mut expression = adjust_copy(lv(&x), number(3.0));
            let RValue::Binary(or) = &mut expression else {
                unreachable!();
            };
            if swap {
                let RValue::Binary(add) = &mut *or.right else {
                    unreachable!();
                };
                std::mem::swap(&mut add.left, &mut add.right);
            } else {
                let RValue::Binary(and) = &mut *or.left else {
                    unreachable!();
                };
                let RValue::Binary(lt) = &mut *and.left else {
                    unreachable!();
                };
                lt.right = Box::new(number(-0.0));
            }
            let mut block = Block(vec![adjust_decl(&f), local_decl(&r, expression)]);
            let before = block.to_string();
            expr_deinline(&mut block);
            assert_eq!(before, block.to_string());
        }
    }

    #[test]
    fn named_arithmetic_target_budget_disables_family_without_guessing() {
        let mut block = Block(
            (0..=arithmetic::MAX_TARGETS)
                .map(|i| adjust_decl(&local(&format!("adjust{i}"))))
                .collect(),
        );
        block.0.push(local_decl(
            &local("r"),
            adjust_copy(lv(&local("x")), number(3.0)),
        ));
        let before = block.to_string();
        expr_deinline(&mut block);
        assert_eq!(before, block.to_string());
    }

    #[test]
    fn named_arithmetic_handles_early_return_and_false_if_expression() {
        let f = local("adjust");
        let x = local("x");
        let r = local("r");
        let decl = adjust_decl(&f);
        let RValue::Closure(c) = rhs_of(&decl) else {
            unreachable!();
        };
        {
            let mut function = c.function.0.lock();
            let Statement::If(branch) = &function.body.0[0] else {
                unreachable!();
            };
            let tail = std::mem::take(&mut branch.else_block.lock().0);
            function.body.0.extend(tail);
        }
        let expression = crate::IfExpression::new(
            bin(lv(&x), BinaryOperation::LessThan, number(0.0)),
            Literal::Boolean(false).into(),
            bin(
                bin(lv(&x), BinaryOperation::Mul, number(2.0)),
                BinaryOperation::Add,
                Literal::Boolean(false).into(),
            ),
        )
        .into();
        let mut block = Block(vec![decl, local_decl(&r, expression)]);
        expr_deinline(&mut block);
        assert!(is_call_to(rhs_of(block.0.last().unwrap()), &f));
    }
}
