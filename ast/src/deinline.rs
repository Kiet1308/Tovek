//! De-inliner: reverses Luau `-O2` function inlining.
//!
//! The Roblox Luau compiler at `-O2` inlines small local functions: it copies
//! the callee's body into each caller's bytecode at the call site. medal
//! faithfully reproduces that, so the output shows the function inlined
//! everywhere instead of called. This pass detects those inlined regions and
//! rewrites them back into real calls `funcName(args)`, each marked
//! equivalent-call inferences, and marks the candidate definition too.
//!
//! Correctness is paramount: the pass is verification-gated. It only converts a
//! region when it can structurally *prove* the region is a context-specialised
//! copy of a recovered function body (under a substitution that yields the
//! arguments). Anything unproven is left exactly as-is. See the per-function /
//! per-site refusal gates below — when in doubt, REFUSE.
//!
//! Approach: canonicalise the early-return ⇄ guard duality that inlining
//! introduces (the inverse of `flatten_guards`), then structurally unify the
//! recovered body against a candidate statement-region — treating the function's
//! parameters and own locals as binding-holes and requiring everything else
//! (upvalues by pointer identity, globals, method/field names, literals,
//! operators, node kinds) to match exactly.

mod canonical;
mod targets;
mod unify;

pub(crate) use canonical::canon;
pub(crate) use unify::{Bindings, MatchCtx, rvalue_exact_eq, unify_local, unify_lvalue, unify_rvalue};

use canonical::{canon_recurse, canon_top, canon_top_len, canon_top_len_of, negate_canon};
use unify::{is_identity_producing, unify_block, unify_stmt};
use targets::{any_structural_target, collect_targets};

mod candidates;
mod statement_values;
pub(crate) use statement_values::{visit_stmt_rvalues, visit_stmt_rvalues_mut};

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Binary, BinaryOperation, Block, Call, Comment, Function, GenericFor, If,
    LValue, Literal, LocalRw, NumericFor, RValue, RcLocal, Reduce, Repeat, Return,
    Select, SideEffects, Statement, Traverse, Unary, UnaryOperation, While,
};

const DEF_MARKER: &str = "equivalent calls inferred from this helper; original call sites unknown";
// Trailing (same-line) marker appended to a reconstructed call: `f(args) -- ...`.
// No leading `^` caret (it no longer points up at a separate line above).
const CALL_MARKER: &str = "equivalent call inferred; original call site unknown";

type FnPtr = *const Mutex<Function>;

// ---- TEMPORARY PROFILING (env-gated, remove before ship) ----
#[doc(hidden)]
pub mod dprof {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    #[inline(always)]
    pub fn on() -> bool {
        *ON.get_or_init(|| std::env::var("MEDAL_PROF").is_ok())
    }
    #[inline(always)]
    pub fn inc(c: &AtomicU64, n: u64) {
        if on() {
            c.fetch_add(n, Ordering::Relaxed);
        }
    }
    pub static COLLECT_TARGETS_US: AtomicU64 = AtomicU64::new(0);
    pub static CANON_TOP_LEN_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static CANON_TOP_LEN_US: AtomicU64 = AtomicU64::new(0);
    pub static CANON_RECURSE_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static CANON_RECURSE_US: AtomicU64 = AtomicU64::new(0);
    pub static UNIFY_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static UNIFY_US: AtomicU64 = AtomicU64::new(0);
    pub static MATCH_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static MATCH_US: AtomicU64 = AtomicU64::new(0);
    pub static WIDTH_ITERS: AtomicU64 = AtomicU64::new(0);
    pub static COLLAPSE_US: AtomicU64 = AtomicU64::new(0);
    pub static ITERATIONS: AtomicU64 = AtomicU64::new(0);
    pub static BHR_CALLS: AtomicU64 = AtomicU64::new(0);
    pub static BHR_US: AtomicU64 = AtomicU64::new(0);
    pub fn dump() {
        if std::env::var("MEDAL_PROF").is_err() {
            return;
        }
        eprintln!("---- DEINLINE PROF (times in ns) ----");
        for (n, v) in [
            ("iterations", &ITERATIONS),
            ("collect_targets_us", &COLLECT_TARGETS_US),
            ("match_calls", &MATCH_CALLS),
            ("match_us", &MATCH_US),
            ("width_iters", &WIDTH_ITERS),
            ("canon_top_len_calls", &CANON_TOP_LEN_CALLS),
            ("canon_top_len_us", &CANON_TOP_LEN_US),
            ("canon_recurse_calls", &CANON_RECURSE_CALLS),
            ("canon_recurse_us", &CANON_RECURSE_US),
            ("unify_calls", &UNIFY_CALLS),
            ("unify_us", &UNIFY_US),
            ("block_has_return_calls", &BHR_CALLS),
            ("block_has_return_us", &BHR_US),
            ("collapse_us", &COLLAPSE_US),
        ] {
            eprintln!("{:<24} {:>12}", n, v.load(Ordering::Relaxed));
        }
    }
    pub struct T(Option<std::time::Instant>, &'static AtomicU64);
    impl T {
        #[inline(always)]
        pub fn new(c: &'static AtomicU64) -> Self {
            T(on().then(std::time::Instant::now), c)
        }
    }
    impl Drop for T {
        fn drop(&mut self) {
            if let Some(s) = self.0 {
                self.1
                    .fetch_add(s.elapsed().as_nanos() as u64, Ordering::Relaxed);
            }
        }
    }
}
// ---- END TEMPORARY PROFILING ----

#[derive(PartialEq, Clone, Copy)]
enum TKind {
    /// Void: every leaf falls through / returns no value. Inlined as a plain
    /// statement region; emitted as `f(args)`.
    Void,
    /// Value: every leaf returns exactly one SCALAR value. Inlined as
    /// `local RESULT; ...; RESULT = X`; emitted as `local RESULT = f(args)`.
    Value,
}

/// Where a Value target's init-less RESULT-register declaration sits at the call
/// site, relative to the start of the inlined body.
#[derive(PartialEq, Clone, Copy)]
enum ValueAnchor {
    /// The RESULT decl is the FIRST statement of the inlined region (the callee
    /// body has no leading non-branch statement). Matched by `match_value`. This
    /// is the only shape pre-§8, so it is byte-for-byte the original behaviour.
    AtResultDecl,
    /// The callee body has K leading non-branch statements (its own locals,
    /// computed before the value is produced). At the call site those K statements
    /// precede the interposed `local RESULT` decl: `<prefix…> ; local RESULT ;
    /// <value branch>`. Matched by `match_value_prefixed` (proposal §8). Scoped to
    /// K==1 (`prefix_len == 1`).
    AtPrefix,
}

struct Target {
    f_local: RcLocal,
    func_ptr: FnPtr,
    kind: TKind,
    pat: Vec<Statement>, // canon(body); for Value targets the leaves are `return X`
    pat_raw_len: usize,  // raw body length (Value window ceiling)
    /// Void window ceiling: `tail_spine_len(body)` (see there) — the raw body
    /// length plus the arms of every tail-spine `if` that inlining may lift into
    /// guard form at the site.
    pat_spine_len: usize,
    /// Node count of `pat`: the most one unification against it can walk.
    pat_nodes: usize,
    /// Tried at every position in this fixed-point iteration. After the first
    /// iteration only a target that can match anew is: its own body was
    /// rewritten, its pattern calls a helper that gained call sites, a site it
    /// matched was refused as ambiguous, or it compares against the rest of the
    /// block (`cps_loop_return`).
    focused: bool,
    /// For Value targets: where the RESULT-register decl sits (see `ValueAnchor`).
    /// `Void` targets always use `AtResultDecl` (unused for them).
    value_anchor: ValueAnchor,
    /// For `AtPrefix` Value targets: the number of leading callee-prefix pattern
    /// statements before the value-producing branch (the pattern's last
    /// statement). 0 for every other target. Scoped to 1 in the first cut.
    prefix_len: usize,
    /// Discriminant of `pat[0]` — the variant of the pattern's first statement.
    /// Cheap O(1) prefilter: `canon` drops leading `Empty`s and preserves the
    /// first surviving statement's variant (an unguarded `if` stays an `if`), so
    /// a Void pattern can only match at a position whose first non-`Empty`
    /// statement shares this variant. Void patterns never contain a `Return`
    /// (`block_has_return`-gated), so the variant is unambiguous for them.
    pat0_kind: std::mem::Discriminant<Statement>,
    /// Hash of `pat[0]`'s fixed-name anchor (method / global-call name), or `None`
    /// when it has none — a second O(1) prefilter dimension alongside `pat0_kind`,
    /// sound by `stmt_anchor_key`'s contract. Cuts per-position work when many
    /// same-variant targets differ only by name.
    pat0_anchor_key: Option<u64>,
    params: FxHashSet<RcLocal>, // P
    locals: FxHashSet<RcLocal>, // L (declared callee-locals)
    param_order: Vec<RcLocal>,
    /// Parameters the callee body WRITES (`p = p + 1`, …), in parameter order. The
    /// inliner cannot write through the caller's argument, so at every inlined site
    /// it materialises such a param as a fresh local initialised with the argument
    /// — `local L = ARG` immediately before the inlined body, which then writes `L`.
    /// These are therefore matched as callee LOCALS (they sit in `locals`, not
    /// `params`), and `match_void` consumes one leading `local L = ARG` declaration
    /// per written param, recovering `ARG` as the argument (`finish_unified`).
    written_params: Vec<RcLocal>,
    /// Parameters the callee body NEVER reads (F6a). On a non-variadic helper an
    /// unread param cannot be observed, so a call-site region that matches the body
    /// minus that param is still a valid de-inline: `try_unify_site` supplies `nil`
    /// for it (trailing such args are trimmed). Computed once via `collect_reads`
    /// over the body, nested blocks and closure bodies included. Empty for the
    /// overwhelmingly common all-params-read helper, so this changes nothing for
    /// those.
    unread: FxHashSet<RcLocal>,
    /// Parameters read exactly once, as the body's first observable step
    /// (`evaluation_order::block_reads_first`). Luau evaluated such an argument
    /// right before the inlined body, so one argument that runs code may still
    /// move back into the call (`SCurveTranform(toSCurveSpace(x))`).
    first_reads: Vec<RcLocal>,
    /// [`Target::first_reads`] for an argument that is a register local.
    first_register_reads: Vec<RcLocal>,
    /// Outer locals the body reads that a closure assigns. The helper
    /// fetches one as an upvalue where it stands; a site holding it in a
    /// register reads it when an operation runs, maybe after a call changed
    /// it (`x + change()`), so such a site is refused
    /// ([`crate::evaluation_order::region_late_read_conflict`]).
    free_cells: Vec<RcLocal>,
    /// At least one branch condition reads a parameter.  Only such targets can
    /// change statement shape after constant argument propagation, so this is a
    /// cold precomputed gate for the Tier-B partial-evaluation fallback.
    specializable: bool,
    /// Parameters read where only their truth matters
    /// ([`truth_tested_params`]), in parameter order. Luau folds such a read
    /// of a constant argument away, so a copy may keep no trace of it; Tier B
    /// then tries `true` and `false` for it.
    truth_params: Vec<RcLocal>,
    /// The [`Target::truth_params`] whose value is read too (`parent or
    /// root`, `if timeout then x.Value = timeout end`): an optional argument,
    /// left out rather than `false` where the copy says it was false.
    optional_params: Vec<RcLocal>,
    /// [`try_truth`]'s specializations of `pat`, built on first use: `None`
    /// where too little of the body is left.
    specializations: std::cell::RefCell<FxHashMap<(usize, InferredTruth), Option<std::rc::Rc<Vec<Statement>>>>>,
    /// A Value target with a path that falls off the end of the body instead
    /// of returning one value (`if c then return x end`). Exact only where the
    /// result is one nil-initialized local (`local r; if c then r = x end`):
    /// a call that falls off returns nothing, which `r` reads as nil.
    falls_off: bool,
    /// The pattern retains a void `return` inside a loop.  Such a return is
    /// lowered by inlining into a loop guard plus a cloned caller continuation;
    /// the CPS matcher verifies that continuation before refolding it.
    cps_loop_return: bool,
    /// A Value target returning from inside a loop ([`targets::loop_return_split`]):
    /// the index in `pat` of that loop, which is also the number of the
    /// helper's statements before it. Its sites store the value and leave
    /// through a flag and a `break` ([`match_value_loop`]). `None` for every
    /// other target.
    loop_exit_at: Option<usize>,
    /// The helper's own locals that its body ends by returning
    /// ([`targets::local_tuple_return`]). Its pattern is the body before that return,
    /// matched as a void region whose site declares the caller's locals in
    /// their place; rebuilt as `local set, key = f(args)`. Empty for every
    /// other target.
    returns: Vec<RcLocal>,
    captures: std::rc::Rc<crate::deinline_safety::CaptureSafety>,
    search: std::rc::Rc<crate::deinline_safety::SearchBudget>,
}

impl Target {
    fn ctx(&self) -> MatchCtx<'_> {
        MatchCtx {
            params: &self.params,
            locals: &self.locals,
        }
    }
}

// ===================================================================
// Entry point
// ===================================================================

/// What one fixed-point iteration rewrote: the helpers it spliced calls to,
/// and the function bodies it spliced into (`None` is the chunk). `contested`
/// holds the helpers of every site refused because two of them matched: a
/// rival whose pattern changes may leave the other one unambiguous.
#[derive(Default)]
struct Progress {
    binders: FxHashSet<RcLocal>,
    bodies: FxHashSet<Option<FnPtr>>,
    contested: FxHashSet<RcLocal>,
    /// The bodies the previous iteration rewrote. A rewrite at a site can
    /// make another helper match there (`local r = choose(c)` then gives
    /// `overwrite` the copy it starts with), so every active target is tried
    /// anew in these bodies, focused or not.
    revisit: FxHashSet<Option<FnPtr>>,
}

pub fn deinline(body: &mut Block) {
    // Every rewrite needs a target; the module-wide censuses below are only
    // worth building when some helper passes the per-declaration gates.
    if !any_structural_target(body) {
        crate::telemetry::count("skipped_without_targets", 1);
        return;
    }
    let captures = crate::deinline_safety::CaptureSafety::new(body);
    if !captures.complete() || captures.dynamic_environment() || captures.call_frames_untracked() {
        crate::telemetry::count("skipped_without_targets", 1);
        return;
    }
    // Helpers every call of which returns exactly one value, read off the
    // declarations before any rewrite can replace one (`local f = factory()`
    // no longer shows `f`'s body). Only their calls may move into a place
    // that takes every value (`return f()`); unknown arity keeps the local.
    let mut single_valued: FxHashSet<RcLocal> = FxHashSet::default();
    each_closure_decl(&body.0, &mut |binder, function| {
        if returns_exactly_one(&function.lock().body.0) {
            single_valued.insert(binder.clone());
        }
    });
    // The entry budget census describes the unchanged first iteration too.
    // Later iterations rebuild it after rewriting; no mutable-tree facts are
    // retained across a revision, and this summary owns only numeric IDs.
    let mut initial_captures = Some(std::rc::Rc::new(captures));
    let search = std::rc::Rc::new(crate::deinline_safety::SearchBudget::default());
    let mut converted: FxHashSet<RcLocal> = FxHashSet::default();
    // P4 perf: the write-once census is INVARIANT across fixed-point iterations for
    // the only thing we query — TARGET BINDERS (a `local f = function…end` local,
    // declared exactly once). De-inline emits reads of binders and writes to
    // result-registers, never a new write to a function-local binder, and a target
    // binder's own decl is never inside a removed region (`body_unsafe` refuses
    // closure-bearing bodies). So a binder's queried write count never changes;
    // compute the census ONCE here rather than re-traversing the whole module
    // inside `collect_targets` on every iteration (the +50% regression P4 caused).
    // Computing on the original body is also strictly CONSERVATIVE: de-inline only
    // removes statements, so the effective count can only drop — meaning the once-
    // map can over-refuse a pathological reassigned-then-inlined binder but can
    // NEVER wrongly admit one.
    let mut write_counts: FxHashMap<RcLocal, usize> = FxHashMap::default();
    {
        let _span = crate::telemetry::Span::new("D_WRITE_CENSUS");
        crate::expr_deinline::collect_write_counts(&body.0, &mut write_counts);
        crate::telemetry::count("bindings", write_counts.len() as u64);
    }
    // What the previous iteration rewrote: it decides which targets can match
    // anew (`Target::focused`).
    let mut previous: Option<Progress> = None;
    for _ in 0..64 {
        dprof::inc(&dprof::ITERATIONS, 1);
        crate::telemetry::count("iterations", 1);
        let targets = {
            let _t = dprof::T::new(&dprof::COLLECT_TARGETS_US);
            let _span = crate::telemetry::Span::new("D_COLLECT_TARGETS");
            let captures = initial_captures.take().unwrap_or_else(||
                std::rc::Rc::new(crate::deinline_safety::CaptureSafety::new(body)));
            let mut targets = collect_targets(body, &write_counts, captures);
            crate::telemetry::count("accepted_targets", targets.len() as u64);
            if targets.len() > 256 {
                crate::telemetry::count("target_budget_exhausted", 1);
                break;
            }
            for target in &mut targets {
                target.search = search.clone();
                if let Some(previous) = &previous {
                    target.focused = target.cps_loop_return
                        || previous.contested.contains(&target.f_local)
                        || previous.bodies.contains(&Some(target.func_ptr))
                        || previous.binders.iter().any(|binder| block_reads_local(&target.pat, binder));
                }
            }
            targets
        };
        if targets.is_empty() {
            break;
        }
        // f_local -> target index, so we can recognise each target's declaration
        // statement during the scan and only activate it for code in its scope.
        let decl_map: FxHashMap<RcLocal, usize> = targets
            .iter()
            .enumerate()
            .map(|(idx, t)| (t.f_local.clone(), idx))
            .collect();
        let mut newly = Progress {
            revisit: previous.as_ref().map(|previous| previous.bodies.clone()).unwrap_or_default(),
            ..Progress::default()
        };
        {
            let _span = crate::telemetry::Span::new("D_SCAN");
            deinline_block(
                &mut body.0,
                &targets,
                &decl_map,
                &[],
                &[],
                &FxHashSet::default(),
                None,
                true,
                true,
                &mut newly,
            );
            crate::telemetry::count("converted_binders", newly.binders.len() as u64);
        }
        // Fixed point is reached when an iteration rewrites NOTHING. `newly` gains a
        // binder on EVERY splice (`deinline_block` -> `newly.insert(hit.f_local)`),
        // so `newly.is_empty()` ⟺ zero sites rewritten this iteration ⟺ the AST is
        // stable. Terminate on that alone.
        //
        // The previous extra guard — `break` when `converted.len()` did not grow —
        // was UNSOUND as a termination test: `converted` is a SET of binders, so an
        // iteration that productively rewrites further sites of an ALREADY-converted
        // binder `f` (the chained / nested case: an inner de-inline in a prior
        // iteration exposed a fresh `f`-shaped region) mutates the AST yet leaves
        // `converted.len()` unchanged, stopping the loop one iteration too early and
        // silently leaving those regions un-reconstructed. Termination still holds
        // without it: every splice replaces an inlined region with a literal
        // `f(args)` call, which is never itself a re-matchable inline site — a bare
        // call to a `Local` callee with `Local`/literal args scores 0 anchors
        // (`anchors_in_rvalue`), below the `anchors_in_block(&pat) < 2` collection
        // floor, so it can never be re-collected as a target body. Thus the count of
        // matchable inlined regions strictly decreases each productive iteration and
        // is bounded by the initial AST size.
        if newly.binders.is_empty() {
            break;
        }
        converted.extend(newly.binders.iter().cloned());
        previous = Some(newly);
        if search.exhausted() { break; }
    }
    if !converted.is_empty() {
        {
            let _t = dprof::T::new(&dprof::COLLAPSE_US);
            let _span = crate::telemetry::Span::new("D_COLLAPSE_RESULTS");
            collapse_value_results(&mut body.0, &single_valued, &FxHashSet::default());
        }
        insert_def_markers(&mut body.0, &converted);
    }
    dprof::dump();
}

// ===================================================================
// Readability: collapse a single-use value de-inline
//   local v = f(args) -- inlined ...   (CALL_MARKER, a trailing comment)
//   if v then BODY end                 -- v used exactly once, as the whole condition
// into
//   -- [-O2 INLINED ...]
//   if f(args) then BODY end
// matching the original source. Only when `v` is read exactly once (in the
// immediately-following statement, anywhere incl. closures) so single-evaluation
// and ordering are preserved.
// ===================================================================

const COLLAPSE_MARKER: &str = "equivalent call inferred; original call site unknown";

/// One of the comments THIS pass itself injects (a reconstructed-call/def/collapse
/// marker). They are runtime no-ops. The fixed-point loop re-collects targets each
/// iteration: an inner de-inline can splice a `CALL_MARKER` into a callee body that
/// is ALSO a target, and a body carrying a (genuine source) comment is otherwise
/// refused by `body_unsafe`. Treating our own markers as no-ops — exempt in
/// `body_unsafe`, dropped symmetrically in `canon_top` — lets such a body stay a
/// valid target so chained/nested inlines keep collapsing, without ever matching on
/// the marker text. Genuine source comments still refuse the body.
fn is_internal_marker(c: &Comment) -> bool {
    c.text == CALL_MARKER || c.text == DEF_MARKER || c.text == COLLAPSE_MARKER
}

/// A statement that `canon_top` drops (an `Empty` placeholder or one of THIS
/// pass's own reconstruction markers) — i.e. a runtime no-op that does not
/// occupy a logical position in a candidate window. MUST stay in lock-step with
/// the filter in `canon_top` (it strips exactly `Empty` + `is_internal_marker`
/// comments): the candidate generator decides which raw index is the K-th
/// *effective* statement, and canon decides what the unifier actually sees, so
/// the two must agree on what counts as a no-op. A genuine SOURCE comment is NOT
/// trivia (it refuses the body via `body_unsafe`) and must never be skipped here.
fn is_match_trivia(s: &Statement) -> bool {
    matches!(s, Statement::Empty(_)) || matches!(s, Statement::Comment(c) if is_internal_marker(c))
}

/// `rest` (the statements following some statement) is exactly one unconditional
/// void `return`, ignoring trivia — so control never continues past it and the
/// statement before it sits in tail-control position (see `deinline_block`).
/// For each statement, whether the ones after it are a lone void `return`,
/// trivia aside: one backward pass.
fn void_return_tails(stmts: &[Statement]) -> Vec<bool> {
    #[derive(Clone, Copy, PartialEq)]
    enum Suffix {
        Nothing,
        VoidReturn,
        Other,
    }
    let mut tails = vec![false; stmts.len()];
    let mut after = Suffix::Nothing;
    for (j, statement) in stmts.iter().enumerate().rev() {
        tails[j] = after == Suffix::VoidReturn;
        after = match (statement, after) {
            (statement, after) if is_match_trivia(statement) => after,
            (Statement::Return(ret), Suffix::Nothing) if ret.values.is_empty() => Suffix::VoidReturn,
            _ => Suffix::Other,
        };
    }
    tails
}

/// `void_return_tails` for one suffix, scanning it whole.
#[cfg(test)]
fn continues_with_void_return_only(rest: &[Statement]) -> bool {
    let mut seen = false;
    for s in rest {
        if is_match_trivia(s) {
            continue;
        }
        match s {
            Statement::Return(r) if r.values.is_empty() && !seen => seen = true,
            _ => return false,
        }
    }
    seen
}

/// Upper bound on the EFFECTIVE statement count of a call-site window that can
/// canonicalise to `stmts` (the window ceiling for `match_void`). Inlining lowers a
/// callee's tail `if c then X else Y end` into guard form at the site —
/// `if not c then Y; return end; X` (or the mirror) — so the site's TOP-LEVEL
/// statement count exceeds the pattern's by up to the size of the arms that were
/// lifted out, recursively along the tail spine (the arms' own tail `if`s lift
/// too). A trailing void `return` is skipped when locating the spine `if` (it is
/// the N1 no-op); the caller adds one for a site-only trailing `return`. This only
/// widens the scan for patterns whose spine ends in an `if`; every width is still
/// gated by the exact `canon_top_len == kc` check, so a wider ceiling can only
/// admit windows the old flat `pat_raw_len + 1` bound wrongly cut short.
fn tail_spine_len(stmts: &[Statement]) -> usize {
    let n = stmts.len();
    let last = stmts
        .iter()
        .rev()
        .find(|s| !is_match_trivia(s) && !matches!(s, Statement::Return(r) if r.values.is_empty()));
    match last {
        Some(Statement::If(f)) => {
            n + tail_spine_len(&f.then_block.lock().0) + tail_spine_len(&f.else_block.lock().0)
        }
        _ => n,
    }
}

/// Absolute index of the `n`-th (0-based) NON-trivia statement at/after `from`
/// in `stmts`, or `None` if fewer than `n+1` effective statements remain.
///
/// Used by the Value-prefix matchers (P1/P6) to locate the interposed init-less
/// `local RESULT` declaration: an inner de-inline in an earlier fixed-point
/// iteration can splice a trailing `CALL_MARKER` (or the structurer an `Empty`)
/// between the callee-prefix statement(s) and that decl, so the fixed offset
/// `i + prefix_len` would point at the marker and `result_decl` would bail —
/// silently killing chained / nested AtPrefix reconstruction. Counting only
/// effective statements restores the match; the trivia is later removed by the
/// splice (which spans the absolute window `i..i+consume`).
fn nth_effective_index(stmts: &[Statement], from: usize, n: usize) -> Option<usize> {
    stmts
        .iter()
        .enumerate()
        .skip(from)
        .filter(|(_, s)| !is_match_trivia(s))
        .nth(n)
        .map(|(idx, _)| idx)
}

/// The raw window width at/after `from` that spans up to `max_eff` EFFECTIVE
/// (non-trivia) statements — the trivia-aware analogue of a flat `from + max_eff`
/// ceiling. Interposed `Empty`s / internal markers (an inner de-inline can splice
/// several of them into a nested candidate region across fixed-point iterations) do
/// NOT consume the budget, so a window whose *effective* length already equals the
/// pattern is never cut short by the raw ceiling (DeinlineReportNew §2 / F2). The
/// per-width matcher loops still gate each width by `canon_top_len == kc`, so the
/// only effect of a wider ceiling is to KEEP trying widths that the old raw bound
/// `pat_raw_len + 1` wrongly excluded once two or more trivia were interposed.
///
/// Returns the number of raw statements from `from` up to and INCLUDING the
/// `max_eff`-th effective statement, or all remaining statements when fewer than
/// `max_eff` effective statements remain. With no interposed trivia this equals
/// `min(stmts.len() - from, max_eff)` exactly — i.e. byte-identical to the old
/// ceiling in the common case, so non-nested corpus output cannot move.
fn raw_width_for_effective(stmts: &[Statement], from: usize, max_eff: usize) -> usize {
    // Defensive: every caller passes `pat_raw_len + 1 >= 1`, so this never fires in
    // practice (and a `kc..=0` width range would be empty anyway), but it keeps the
    // helper total for any future caller.
    if max_eff == 0 {
        return 0;
    }
    let mut eff = 0usize;
    for (off, s) in stmts[from..].iter().enumerate() {
        if !is_match_trivia(s) {
            eff += 1;
            if eff == max_eff {
                return off + 1;
            }
        }
    }
    stmts.len() - from
}

/// `live_out`: locals of `stmts` read after them (a `repeat` body's, by its
/// `until` condition).
fn collapse_value_results(stmts: &mut Vec<Statement>, single_valued: &FxHashSet<RcLocal>, live_out: &FxHashSet<RcLocal>) {
    // recurse into nested blocks and closure bodies first.
    let none = FxHashSet::default();
    for s in stmts.iter_mut() {
        match s {
            Statement::If(f) => {
                collapse_value_results(&mut f.then_block.lock().0, single_valued, &none);
                collapse_value_results(&mut f.else_block.lock().0, single_valued, &none);
            }
            Statement::While(w) => collapse_value_results(&mut w.block.lock().0, single_valued, &none),
            Statement::Repeat(r) => {
                let reads = condition_reads(&r.condition);
                collapse_value_results(&mut r.block.lock().0, single_valued, &reads);
            }
            Statement::NumericFor(nf) => collapse_value_results(&mut nf.block.lock().0, single_valued, &none),
            Statement::GenericFor(gf) => collapse_value_results(&mut gf.block.lock().0, single_valued, &none),
            _ => {}
        }
        visit_stmt_rvalues_mut(s, &mut |rv| {
            collapse_in_closures(rv, single_valued);
            true
        });
    }

    let taken = std::mem::take(stmts);
    let n = taken.len();
    // One linear pass recording, per local, the greatest top-level index that reads
    // / writes it (recursing into nested blocks + closures via `collect_reads` /
    // `collect_written`, the exact mirrors of `count_local_reads` and the old
    // per-tail write scan). This replaces the per-triple full-tail rescans
    // (`count_local_reads(&taken[i+3..])` and a `collect_written(&taken[i+2..])`
    // membership test), turning the dense-block Θ(N²) into Θ(N): "v not read
    // at/after j" ⟺ `last_read[v]` is absent or `< j`. (The bounded `== 1`
    // read-count below stays an exact scan — a max-index cannot express "exactly
    // one".)
    let mut last_read: FxHashMap<RcLocal, usize> = FxHashMap::default();
    let mut last_write: FxHashMap<RcLocal, usize> = FxHashMap::default();
    // Reuse the two scratch sets across statements (drain, don't reallocate),
    // mirroring `build_last_occ`. `k` ascends, and within a statement every drained
    // local is stored with the same `k`, so drain order is irrelevant.
    let mut rd: FxHashSet<RcLocal> = FxHashSet::default();
    let mut wr: FxHashSet<RcLocal> = FxHashSet::default();
    for (k, s) in taken.iter().enumerate() {
        collect_reads(std::slice::from_ref(s), &mut rd);
        for v in rd.drain() {
            last_read.insert(v, k);
        }
        collect_written(std::slice::from_ref(s), &mut wr);
        for v in wr.drain() {
            last_write.insert(v, k);
        }
    }
    let mut out: Vec<Statement> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        if i + 2 < n
            && let Statement::Assign(a) = &taken[i]
            && let Some((v, call)) = value_call_decl(a)
            && let Statement::Comment(cm) = &taken[i + 1]
            && cm.text == CALL_MARKER
            && count_local_reads(&taken[i + 2..i + 3], &v) == 1
            && last_read.get(&v).is_none_or(|&k| k < i + 3)
            && !live_out.contains(&v)
            // `v`'s declaration is about to be removed, so `v` must not be
            // *written* anywhere we keep either — a later `v = ...` (e.g. inside
            // the collapsed `if`) would otherwise be left with no declaration.
            && last_write.get(&v).is_none_or(|&k| k < i + 2)
            && let Some(collapsed) = collapse_use(&taken[i + 2], &v, call, single_valued)
        {
            // The reconstructed call now lives inside `collapsed`. For a
            // single-line `return f(args)` / `x = f(args)` the marker reads best
            // appended to that line; for the multi-line `if f(args) then … end`
            // shape it stays a leading header above the block.
            if matches!(collapsed, Statement::If(_)) {
                out.push(Statement::Comment(Comment::new(
                    COLLAPSE_MARKER.to_string(),
                )));
                out.push(collapsed);
            } else {
                out.push(collapsed);
                out.push(Statement::Comment(Comment::trailing(
                    COLLAPSE_MARKER.to_string(),
                )));
            }
            i += 3;
        } else {
            out.push(taken[i].clone());
            i += 1;
        }
    }
    *stmts = out;
}

/// `local v = <Call>` -> (v, the call rvalue).
fn value_call_decl(a: &Assign) -> Option<(RcLocal, &RValue)> {
    if a.prefix && !a.parallel && a.left.len() == 1 && a.right.len() == 1 {
        if let (LValue::Local(v), call @ RValue::Call(_)) = (&a.left[0], &a.right[0]) {
            return Some((v.clone(), call));
        }
    }
    None
}

/// If `s` uses `v` exactly in a leading, order-safe position (the whole `if`
/// condition `v`/`not v`, the whole `return v`, or the whole assign RHS `= v`),
/// returns `s` with `v` replaced by `call`. Otherwise `None`.
///
/// `single_valued` holds the binders of helpers proven to return exactly one
/// value. Any other call is refused the MULTI-VALUE-context arms — `return v`
/// -> `return f(args)` (a tail call spreads all values, or none) and a
/// MULTI-LHS `a, b = v` -> `a, b = f(args)` would expose values the original
/// single-LHS `local v = f(args)` had truncated away. Single-value contexts (an
/// `if` condition, a SINGLE-LHS assign) truncate to one value either way and stay
/// sound for every helper.
fn collapse_use(
    s: &Statement,
    v: &RcLocal,
    call: &RValue,
    single_valued: &FxHashSet<RcLocal>,
) -> Option<Statement> {
    let is_v = |rv: &RValue| matches!(rv, RValue::Local(x) if x == v);
    let is_not_v = |rv: &RValue| {
        matches!(rv, RValue::Unary(u)
            if u.operation == UnaryOperation::Not && is_v(&u.value))
    };
    // Only a call proven to return one value may move into a multi-value context.
    let exactly_one = call_callee_local(call).is_some_and(|l| single_valued.contains(l));
    match s {
        Statement::If(f) => {
            let cond = if is_v(&f.condition) {
                call.clone()
            } else if is_not_v(&f.condition) {
                RValue::Unary(Unary {
                    node_origin: Default::default(),
                    value: Box::new(call.clone()),
                    operation: UnaryOperation::Not,
                })
            } else {
                return None;
            };
            Some(Statement::If(If {
                node_origin: Default::default(),
                condition: cond,
                then_block: f.then_block.clone(),
                else_block: f.else_block.clone(),
            }))
        }
        // `return v` is a MULTI-VALUE (tail) context: `return f(args)` would
        // propagate ALL of a multi-value helper's values, where `local v = f(args)`
        // truncated to one. Refuse the collapse for such a helper (keep the sound
        // `local v = f(args); return v`); scalar helpers collapse as before.
        Statement::Return(r)
            if r.values.len() == 1 && is_v(&r.values[0]) && exactly_one =>
        {
            Some(Statement::Return(Return {
                node_origin: Default::default(),
                values: vec![call.clone()],
            }))
        }
        // A bare `Local`/`Global` target is always safe; an INDEXED target
        // (`t[k]`, `t.field`) only when the moved-in call provably cannot change
        // the prefix `t`/`k` (see `lvalue_safe_for_collapse`): `t[k] = f()`
        // evaluates the prefix relative to the RHS call differently from the
        // pre-collapse `local v = f(); t[k] = v`, so it is only sound when `f`
        // leaves `t`/`k` untouched.
        // A SINGLE-LHS `x = v` truncates the call to one value either way (sound for
        // any helper); a MULTI-LHS `a, b = v` is a multi-value context, so refuse it
        // for a multi-value helper (it would bind b/... to values the original
        // single-LHS `local v = f(args)` truncated away).
        Statement::Assign(a)
            if a.right.len() == 1
                && is_v(&a.right[0])
                && a.left.iter().all(lvalue_safe_for_collapse)
                && (a.left.len() == 1 || exactly_one) =>
        {
            Some(Statement::Assign(Assign {
                node_origin: Default::default(),
                left: a.left.clone(),
                right: vec![call.clone()],
                prefix: a.prefix,
                parallel: a.parallel,
                compound: false,
            }))
        }
        _ => None,
    }
}

/// A collapse-safe assignment target: a bare name binding (`x` / `GLOBAL`) whose
/// only effect is the store of the RHS value. An INDEXED target (`t[k]`, `t.f`) is
/// refused — collapsing `local v = f(); t[k] = v` into `t[k] = f()` would move the
/// target-prefix evaluation (`t`, `k`) to before the RHS call, so a call that
/// rebinds `t` or mutates `k` would write a different slot. Proving that absent
/// would need an effect summary of `f` (DeInlineReview §2); refusing every indexed
/// target is the simple sound choice and costs only a handful of cosmetic one-line
/// merges corpus-wide.
fn lvalue_safe_for_collapse(l: &LValue) -> bool {
    matches!(l, LValue::Local(_) | LValue::Global(_))
}

/// The single local a reconstructed `helper(args)` call targets (`f` in
/// `f(args)`), if the callee is a bare local — used to look the callee up in the
/// multi-value-helper set during collapse.
fn call_callee_local(call: &RValue) -> Option<&RcLocal> {
    if let RValue::Call(c) = call {
        if let RValue::Local(l) = c.value.as_ref() {
            return Some(l);
        }
    }
    None
}

/// Does any `return <expr>` in `stmts` (recursing into nested control-flow blocks
/// but NOT into nested closures — their returns are not this function's) return a
/// single NON-scalar value (a call / method-call / vararg / select)? Such a helper
/// can yield MORE than one value when tail-called.
///
/// P7-A introduced Value targets with a call/method leaf (`return process(x)`),
/// reconstructed soundly as the single-LHS, truncated `local RESULT = helper(args)`.
/// But the cosmetic `collapse_value_results` pass would then spread that helper's
/// values into a MULTI-VALUE context — `return RESULT` -> `return helper(args)` (a
/// tail call propagates ALL values) or `a, b = RESULT` -> `a, b = helper(args)` —
/// where the original truncated to exactly one. Pre-P7-A every Value helper was
/// scalar (1-value), so the collapse was always sound; flagging call-return helpers
/// here lets `collapse_use` refuse exactly the multi-value-context arms for them.
/// Whether every call of a function with this body returns exactly one
/// value: each `return` lists one value that is not a call's or `...`'s
/// results, and no path runs off the end of the body.
fn returns_exactly_one(body: &[Statement]) -> bool {
    fn one_value_returns(stmts: &[Statement]) -> bool {
        stmts.iter().all(|statement| match statement {
            Statement::Return(ret) => {
                matches!(ret.values.as_slice(), [value]
                    if !matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_)))
            }
            Statement::If(f) => one_value_returns(&f.then_block.lock().0) && one_value_returns(&f.else_block.lock().0),
            Statement::While(w) => one_value_returns(&w.block.lock().0),
            Statement::Repeat(r) => one_value_returns(&r.block.lock().0),
            Statement::NumericFor(nf) => one_value_returns(&nf.block.lock().0),
            Statement::GenericFor(gf) => one_value_returns(&gf.block.lock().0),
            _ => true,
        })
    }
    fn always_returns(stmts: &[Statement]) -> bool {
        match stmts.iter().rev().find(|statement| !is_match_trivia(statement)) {
            Some(Statement::Return(_)) => true,
            Some(Statement::If(f)) => always_returns(&f.then_block.lock().0) && always_returns(&f.else_block.lock().0),
            _ => false,
        }
    }
    one_value_returns(body) && always_returns(body)
}

pub(crate) fn count_local_reads(stmts: &[Statement], v: &RcLocal) -> usize {
    stmts
        .iter()
        .map(|s| {
            let mut n = s.values_read().iter().filter(|r| **r == v).count();
            match s {
                Statement::If(f) => {
                    n += count_local_reads(&f.then_block.lock().0, v);
                    n += count_local_reads(&f.else_block.lock().0, v);
                }
                Statement::While(w) => n += count_local_reads(&w.block.lock().0, v),
                Statement::Repeat(r) => n += count_local_reads(&r.block.lock().0, v),
                Statement::NumericFor(nf) => n += count_local_reads(&nf.block.lock().0, v),
                Statement::GenericFor(gf) => n += count_local_reads(&gf.block.lock().0, v),
                _ => {}
            }
            visit_stmt_rvalues(s, &mut |rv| {
                n += rvalue_closure_reads(rv, v);
                true
            });
            n
        })
        .sum()
}

/// Reads of `v` captured inside closure bodies within `rv` (the non-closure reads
/// are already counted via `values_read`).
fn rvalue_closure_reads(rv: &RValue, v: &RcLocal) -> usize {
    match rv {
        RValue::Closure(c) => count_local_reads(&c.function.0.lock().body.0, v),
        RValue::Call(c) => {
            rvalue_closure_reads(&c.value, v)
                + c.arguments
                    .iter()
                    .map(|a| rvalue_closure_reads(a, v))
                    .sum::<usize>()
        }
        RValue::MethodCall(m) => {
            rvalue_closure_reads(&m.value, v)
                + m.arguments
                    .iter()
                    .map(|a| rvalue_closure_reads(a, v))
                    .sum::<usize>()
        }
        RValue::Index(ix) => rvalue_closure_reads(&ix.left, v) + rvalue_closure_reads(&ix.right, v),
        RValue::Unary(u) => rvalue_closure_reads(&u.value, v),
        RValue::Binary(b) => rvalue_closure_reads(&b.left, v) + rvalue_closure_reads(&b.right, v),
        RValue::Table(t) => t
            .0
            .iter()
            .map(|(k, val)| {
                k.as_ref().map_or(0, |k| rvalue_closure_reads(k, v)) + rvalue_closure_reads(val, v)
            })
            .sum(),
        RValue::Select(Select::Call(c)) => {
            rvalue_closure_reads(&c.value, v)
                + c.arguments
                    .iter()
                    .map(|a| rvalue_closure_reads(a, v))
                    .sum::<usize>()
        }
        RValue::Select(Select::MethodCall(m)) => {
            rvalue_closure_reads(&m.value, v)
                + m.arguments
                    .iter()
                    .map(|a| rvalue_closure_reads(a, v))
                    .sum::<usize>()
        }
        RValue::IfExpression(e) => {
            rvalue_closure_reads(&e.condition, v)
                + rvalue_closure_reads(&e.then_value, v)
                + rvalue_closure_reads(&e.else_value, v)
        }
        _ => 0,
    }
}

// ---- Tail-liveness index --------------------------------------------------
//
// `any_local_live(&stmts[X..], set)` asks: does any local in `set` occur (read
// OR write, recursively into nested blocks AND closure bodies) at some top-level
// index >= X? That is exactly `∃ v ∈ set : last_occ[v] >= X`, where `last_occ[v]`
// is the greatest top-level index of `stmts` whose recursive contents read-or-
// write `v`:
//   any_local_live(&stmts[X..], set)
//     = ∃ v∈set : count_local_reads(&stmts[X..], v) > 0 ∨ v ∈ collect_written(&stmts[X..])
//     = ∃ v∈set : ∃ k>=X : v ∈ (reads(stmts[k]) ∪ written(stmts[k]))
//     = ∃ v∈set : last_occ[v] >= X.
// The driver (`deinline_block` step 2) scans O(N) positions and previously
// rescanned the whole tail at every position — O(N²), the #1 serial-tail cost.
// Computing `last_occ` once (and rebuilding only the cursor suffix on the rare
// accept) makes each query O(|set|).

/// Insert every local READ within `stmts` (recursively into nested blocks and
/// closure bodies). Mirrors `count_local_reads`/`rvalue_closure_reads` exactly
/// (set-presence ⟺ count > 0), but enumerates all read locals instead of
/// counting one.
pub(crate) fn collect_reads(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for s in stmts {
        s.visit_local_reads(&mut |r| {
            out.insert(r.clone());
            true
        });
        match s {
            Statement::If(f) => {
                collect_reads(&f.then_block.lock().0, out);
                collect_reads(&f.else_block.lock().0, out);
            }
            Statement::While(w) => collect_reads(&w.block.lock().0, out),
            Statement::Repeat(r) => collect_reads(&r.block.lock().0, out),
            Statement::NumericFor(nf) => collect_reads(&nf.block.lock().0, out),
            Statement::GenericFor(gf) => collect_reads(&gf.block.lock().0, out),
            _ => {}
        }
        visit_stmt_rvalues(s, &mut |rv| {
            collect_reads_in_closures(rv, out);
            true
        });
    }
}

/// Reads of locals captured inside closure bodies within `rv`. Structural mirror
/// of `rvalue_closure_reads`.
fn collect_reads_in_closures(rv: &RValue, out: &mut FxHashSet<RcLocal>) {
    match rv {
        RValue::Closure(c) => collect_reads(&c.function.0.lock().body.0, out),
        RValue::Call(c) => {
            collect_reads_in_closures(&c.value, out);
            for a in &c.arguments {
                collect_reads_in_closures(a, out);
            }
        }
        RValue::MethodCall(m) => {
            collect_reads_in_closures(&m.value, out);
            for a in &m.arguments {
                collect_reads_in_closures(a, out);
            }
        }
        RValue::Index(ix) => {
            collect_reads_in_closures(&ix.left, out);
            collect_reads_in_closures(&ix.right, out);
        }
        RValue::Unary(u) => collect_reads_in_closures(&u.value, out),
        RValue::Binary(b) => {
            collect_reads_in_closures(&b.left, out);
            collect_reads_in_closures(&b.right, out);
        }
        RValue::Table(t) => {
            for (k, val) in &t.0 {
                if let Some(k) = k {
                    collect_reads_in_closures(k, out);
                }
                collect_reads_in_closures(val, out);
            }
        }
        RValue::Select(Select::Call(c)) => {
            collect_reads_in_closures(&c.value, out);
            for a in &c.arguments {
                collect_reads_in_closures(a, out);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            collect_reads_in_closures(&m.value, out);
            for a in &m.arguments {
                collect_reads_in_closures(a, out);
            }
        }
        RValue::IfExpression(e) => {
            collect_reads_in_closures(&e.condition, out);
            collect_reads_in_closures(&e.then_value, out);
            collect_reads_in_closures(&e.else_value, out);
        }
        _ => {}
    }
}

/// Map each local to the greatest top-level index `k` (in `from..stmts.len()`)
/// whose statement reads-or-writes it. The read half mirrors `count_local_reads`
/// and the write half reuses `collect_written`, so membership is identical to the
/// per-statement predicate `any_local_live` tests. Rebuilt from the cursor on the
/// rare accept; because the cursor only advances, occurrences below `from` are
/// never queried (every future query uses `tail_start >= from`), so dropping them
/// is sound.
/// The locals a `repeat` condition reads, which its body's locals may be.
fn condition_reads(condition: &RValue) -> FxHashSet<RcLocal> {
    let mut reads = FxHashSet::default();
    collect_reads(&[Statement::Return(Return::new(vec![condition.clone()]))], &mut reads);
    reads
}

/// The tail-liveness index of a block whose `live_out` locals are read after
/// it, built now with those locals live past every statement; `None` (built
/// on first use) when nothing is.
fn live_out_index(
    stmts: &[Statement],
    from: usize,
    live_out: &FxHashSet<RcLocal>,
) -> Option<FxHashMap<RcLocal, usize>> {
    if live_out.is_empty() {
        return None;
    }
    let mut index = build_last_occ(stmts, from);
    index.extend(live_out.iter().map(|local| (local.clone(), usize::MAX)));
    Some(index)
}

fn build_last_occ(stmts: &[Statement], from: usize) -> FxHashMap<RcLocal, usize> {
    let mut last_occ: FxHashMap<RcLocal, usize> = FxHashMap::default();
    let mut occ: FxHashSet<RcLocal> = FxHashSet::default();
    for (k, s) in stmts.iter().enumerate().skip(from) {
        collect_reads(std::slice::from_ref(s), &mut occ);
        collect_written(std::slice::from_ref(s), &mut occ);
        for v in occ.drain() {
            last_occ.insert(v, k); // k ascending ⇒ final stored value is the max
        }
    }
    last_occ
}

/// `any_local_live(&stmts[tail_start..], set)` via the tail-liveness index, built
/// LAZILY on first use and cached in `last_occ` (the original `any_local_live`
/// only ran on a successful unify, so eager per-block construction would do work
/// for the many blocks that have a target in scope but never actually match).
/// The cache is invalidated (`= None`) by the driver whenever it splices, so it
/// is always consistent with the current `stmts`. (Empty `set` ⇒ false, matching
/// `any_local_live`, and without forcing a build.)
fn tail_has_live(
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
    stmts: &[Statement],
    from: usize,
    tail_start: usize,
    set: &FxHashSet<RcLocal>,
) -> bool {
    if set.is_empty() {
        return false;
    }
    // Build the index lazily from the scan cursor `from`. Every query in a block
    // uses `tail_start >= from` (the cursor only advances between accepts, and the
    // first query after each splice establishes `from`), so occurrences below
    // `from` are never inspected — see `build_last_occ`'s doc. Cheaper than from 0.
    let idx = last_occ.get_or_insert_with(|| build_last_occ(stmts, from));
    set.iter()
        .any(|v| idx.get(v).is_some_and(|&k| k >= tail_start))
}

fn collapse_in_closures(rv: &mut RValue, single_valued: &FxHashSet<RcLocal>) {
    // Find every closure within `rv` and run the collapse inside its body. Descent
    // uses the enum_dispatch `Traverse::rvalues_mut` (exhaustive by construction, so
    // it can never silently drop a new RValue variant — incl. `IfExpression`),
    // mirroring `expr_deinline::write_counts_in_closures`.
    if let RValue::Closure(c) = rv {
        collapse_value_results(&mut c.function.0.lock().body.0, single_valued, &FxHashSet::default());
        return;
    }
    rv.visit_rvalues_mut(&mut |child| {
        collapse_in_closures(child, single_valued);
        true
    });
}

// ===================================================================
// Canonical window cache
// ===================================================================

/// Per-position memo for the tail-canon of a contiguous candidate window,
/// keyed by `(absolute_start, raw_width)`. Values are `Rc`-shared so a cache hit
/// hands out a cheap handle instead of re-running the deep-clone `canon`. Only
/// tail-position contiguous windows (the void attempt-1 path and the `match_value`
/// region) use it — both compute the identical `canon_recurse(canon_top(win,true))`,
/// so they share one cache safely. The non-contiguous `match_value_prefixed` union
/// and the rewritten `value_tail_ret` window are NOT cached (they are rare and not
/// a plain slice). The cache is cleared per position (`stmts` mutates on splice, so
/// absolute indices are only valid within a single `try_match_at` call).
type CanonCache = FxHashMap<(usize, usize), std::rc::Rc<Vec<Statement>>>;

/// Tail-canon of the contiguous window `stmts[start..start+w]`, memoized in `cache`.
/// Byte-identical to `canon_recurse(canon_top(&stmts[start..start+w], true), true)`;
/// the only effect is that repeated requests for the same `(start, w)` — different
/// active targets with equal pattern length — pay the deep clone once.
fn canon_window(
    cache: &mut CanonCache,
    t: &Target,
    stmts: &[Statement],
    start: usize,
    w: usize,
) -> std::rc::Rc<Vec<Statement>> {
    if let Some(c) = cache.get(&(start, w)) {
        return c.clone();
    }
    dprof::inc(&dprof::CANON_RECURSE_CALLS, 1);
    crate::telemetry::count("canonicalize_calls", 1);
    let _t = dprof::T::new(&dprof::CANON_RECURSE_US);
    // An exhausted budget refuses the whole position (`try_match_at`); the
    // empty window built in its place matches no pattern.
    if !charge_window(t, &stmts[start..start + w]) {
        return std::rc::Rc::new(Vec::new());
    }
    let c = std::rc::Rc::new(canon_recurse(
        canon_top(&stmts[start..start + w], true),
        true,
    ));
    cache.insert((start, w), c.clone());
    c
}

/// A cheap hash prefilter key for the FIRST statement a Void / AtPrefix-Value
/// pattern unifies against — the fixed NAME the exact unifier requires to match: a
/// method name (`unify_method` compares `a.method == d.method`) or a global-call
/// callee name (`unify_call` -> `unify_rvalue` on a `Global`, compared by value).
/// `None` when the head has no such fixed name (a param-hole callee, an `Assign` /
/// `If` head, …); the key then cannot discriminate and the full match runs as
/// before. SOUNDNESS: if two heads carry DIFFERENT fixed names here, `unify_stmt`
/// MUST fail, so skipping on a key mismatch is false-negative-free. A hash COLLISION
/// only causes a missed skip (a redundant full match), never a wrong skip — so a
/// u64 digest is safe. Cuts the per-position work where many same-variant targets
/// (e.g. lots of `x:Method()` helpers) share `pat0_kind` but differ by name.
fn stmt_anchor_key(s: &Statement) -> Option<u64> {
    use core::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    match s {
        Statement::MethodCall(m) => {
            0u8.hash(&mut h);
            m.method.hash(&mut h);
            Some(h.finish())
        }
        Statement::Call(c) => match c.value.as_ref() {
            RValue::Global(g) => {
                1u8.hash(&mut h);
                g.0.hash(&mut h);
                Some(h.finish())
            }
            _ => None,
        },
        // Only the value's root: the unifier compares it structurally, and a
        // pattern parameter there yields no key. The assigned target may not
        // be used - a pattern `t[p] = v` also matches a site `t.Name = v`.
        Statement::Assign(a) if a.right.len() == 1 => match &a.right[0] {
            RValue::MethodCall(m) | RValue::Select(Select::MethodCall(m)) => {
                2u8.hash(&mut h);
                m.method.hash(&mut h);
                Some(h.finish())
            }
            RValue::Call(c) | RValue::Select(Select::Call(c)) => match c.value.as_ref() {
                RValue::Global(g) => {
                    3u8.hash(&mut h);
                    g.0.hash(&mut h);
                    Some(h.finish())
                }
                _ => None,
            },
            RValue::Index(index) => match index.right.as_ref() {
                RValue::Literal(Literal::String(key)) => {
                    4u8.hash(&mut h);
                    key.hash(&mut h);
                    Some(h.finish())
                }
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

// ===================================================================
// Region detection + replacement
// ===================================================================

/// If `s` is the declaration `local f = function ... end` of one of our targets,
/// returns that target's index. Used to activate the target only for statements
/// in its lexical scope (after this point in the block, plus nested blocks /
/// closures defined here).
fn target_decl_index(
    s: &Statement,
    decl_map: &FxHashMap<RcLocal, usize>,
    targets: &[Target],
) -> Option<usize> {
    if let Statement::Assign(a) = s {
        if a.prefix
            && a.left.len() == 1
            && a.right.len() == 1
            && let LValue::Local(l) = &a.left[0]
            && let RValue::Closure(c) = &a.right[0]
            && let Some(&idx) = decl_map.get(l)
            && Arc::as_ptr(&c.function.0) == targets[idx].func_ptr
        {
            return Some(idx);
        }
    }
    None
}

fn deinline_block(
    stmts: &mut Vec<Statement>,
    targets: &[Target],
    decl_map: &FxHashMap<RcLocal, usize>,
    outer_active: &[usize],
    outer_continuation: &[&[Statement]],
    // Locals of this block read after it: a `repeat` body's locals are still
    // in scope in its `until` condition.
    live_out: &FxHashSet<RcLocal>,
    current_func: Option<FnPtr>,
    is_func_tail: bool,
    is_func_body_top: bool,
    newly: &mut Progress,
) {
    // 1. recurse into nested statement-blocks and into closure bodies first.
    //    A child block/closure only sees targets whose declaration lexically
    //    precedes it — `active` grows as we pass each declaration in THIS block.
    // A child block is in tail-control position when nothing of the function runs
    // after it: it belongs to the block's LAST statement of a tail block, or — at
    // any nesting depth, loop bodies included — the statements after it are exactly
    // one unconditional void `return` (modulo trivia): `if c then A end; return`
    // runs A and then leaves the function, so a `return` inside A is a local exit
    // and the guard ⇄ nest canon (`unguard`) is sound there. `break`/`continue`
    // do NOT qualify (they leave the loop, not the function).
    let mut child_tails = void_return_tails(stmts);
    if is_func_tail && let Some(last) = child_tails.last_mut() {
        *last = true;
    }
    {
        let snapshot = (targets.iter().any(|target| target.cps_loop_return)
            && stmts.iter().any(|statement| matches!(statement, Statement::If(_))))
            .then(|| stmts.clone());
        let mut active: Vec<usize> = outer_active.to_vec();
        for (j, s) in stmts.iter_mut().enumerate() {
            let continuation = if matches!(s, Statement::If(_)) {
                snapshot.as_ref().map(|snapshot|
                    continuation_segments(&snapshot[j + 1..], outer_continuation)).unwrap_or_default()
            } else { Vec::new() };
            let child_tail = child_tails[j];
            match s {
                Statement::If(f) => {
                    deinline_block(
                        &mut f.then_block.lock().0,
                        targets,
                        decl_map,
                        &active,
                        &continuation,
                        &FxHashSet::default(),
                        current_func,
                        child_tail,
                        false,
                        newly,
                    );
                    deinline_block(
                        &mut f.else_block.lock().0,
                        targets,
                        decl_map,
                        &active,
                        &continuation,
                        &FxHashSet::default(),
                        current_func,
                        child_tail,
                        false,
                        newly,
                    );
                }
                Statement::While(w) => deinline_block(
                    &mut w.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &FxHashSet::default(),
                    current_func,
                    false,
                    false,
                    newly,
                ),
                Statement::Repeat(r) => deinline_block(
                    &mut r.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &condition_reads(&r.condition),
                    current_func,
                    false,
                    false,
                    newly,
                ),
                Statement::NumericFor(nf) => deinline_block(
                    &mut nf.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &FxHashSet::default(),
                    current_func,
                    false,
                    false,
                    newly,
                ),
                Statement::GenericFor(gf) => deinline_block(
                    &mut gf.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &FxHashSet::default(),
                    current_func,
                    false,
                    false,
                    newly,
                ),
                _ => {}
            }
            // closures can appear *anywhere* in a statement's rvalues (call/method
            // arguments, table values, ...), not just as a direct assign RHS — e.g.
            // `task.delay(8, function() ... end)` or `x:Connect(function() ... end)`.
            // A target in scope at the closure's definition is visible inside it
            // (as an upvalue), so we pass the current `active` set down.
            visit_stmt_rvalues_mut(s, &mut |rv| {
                recurse_into_closures(rv, targets, decl_map, &active, newly);
                true
            });
            if let Some(idx) = target_decl_index(s, decl_map, targets) {
                active.push(idx);
            }
        }
    }

    // 2. scan this block left to right, activating each target after its decl.
    let mut active: Vec<usize> = outer_active.to_vec();
    // A position tries the focused active targets in priority order, and the
    // rest only where one of those matches (two matches refuse the site). Both
    // depend only on this function and the active set: compute them when the
    // set grows, not at every position.
    let revisit = newly.revisit.contains(&current_func);
    let prioritize = |active: &[usize]| {
        let (focused, rivals): (Vec<usize>, Vec<usize>) =
            active.iter().partition(|&&i| revisit || targets[i].focused);
        let focused = crate::reconstruction_search::prioritize(&focused, current_func.map(|p| p as usize), |i| {
            targets[i].func_ptr as usize
        });
        (
            candidates::Candidates::new(focused, targets),
            candidates::Candidates::new(rivals, targets),
        )
    };
    let (mut ordered, mut rivals) = prioritize(&active);
    let mut i = 0;
    let mut anchor = 0;
    // Tail-liveness index, replacing the per-window O(N) `any_local_live` rescan
    // with an O(|set|) lookup. Built lazily on the first query (so target-free /
    // never-matching blocks pay nothing) and reused across positions; the driver
    // invalidates it after each splice, after which the next query rebuilds it.
    let mut last_occ: Option<FxHashMap<RcLocal, usize>> = live_out_index(stmts, 0, live_out);
    // Per-position canon cache, reused across the whole block scan (cleared at the
    // top of each `try_match_at`). Within one position the canon of a contiguous
    // tail-window `stmts[start..start+w]` depends ONLY on `(start, w)`, not on which
    // target requested it, yet several active targets that share a pattern length
    // recompute the very same deep-clone. Memoizing by `(start, w)` collapses those
    // to one canon per distinct window. Single-threaded (the serial tail), so `Rc`
    // is fine; the whole `deinline` pass never runs inside the parallel region.
    let mut canon_cache: CanonCache = FxHashMap::default();
    while i < stmts.len() {
        // The first statement at or after `i` that is not `Empty`: a cursor
        // that only moves forward between splices.
        if anchor < i {
            anchor = i;
        }
        while anchor < stmts.len() && matches!(stmts[anchor], Statement::Empty(_)) {
            anchor += 1;
        }
        if let Some(hit) = try_match_at(
            stmts,
            i,
            anchor,
            targets,
            &ordered,
            &rivals,
            &mut newly.contested,
            current_func,
            is_func_tail,
            is_func_body_top,
            outer_continuation,
            &mut last_occ,
            &mut canon_cache,
        ) {
            // A call rebuilt inside another statement's value carries no site
            // marker, as with the expression de-inliner: that statement may
            // itself fold into its use later, which would strand the marker.
            let embedded = hit.host.is_some();
            let call = Call::new(RValue::Local(hit.f_local.clone()), hit.args)
                .reconstructed(crate::call_origins::Kind::StatementDeinline);
            let stmt = match hit.host {
                Some(host) => host,
                None if hit.results.is_empty() => Statement::Call(call),
                None => Statement::Assign(Assign {
                    node_origin: Default::default(),
                    left: hit.results.into_iter().map(LValue::Local).collect(),
                    right: vec![RValue::Call(call)],
                    prefix: true,
                    parallel: false,
                    compound: false,
                }),
            };
            let marker = Statement::Comment(Comment::trailing(CALL_MARKER.to_string()));
            // A chained reconstruction (F1/F2) can consume an INNER reconstructed call
            // whose own trailing `CALL_MARKER` now sits just past the matched window
            // (the matcher keeps the smallest `w`, which need not span that trailing
            // marker). Orphaned, it would render as a duplicate ` -- inlined…` comment
            // on the new call's line. Swallow any immediately-following internal marker
            // — a runtime no-op that belonged to the consumed region's last statement —
            // into the splice. Only CALL_MARKERs exist here (DEF/COLLAPSE markers are
            // inserted after the loop); `Empty`s are deliberately left untouched so the
            // non-chained majority of corpus output stays byte-identical.
            let mut consume = hit.consume;
            while !embedded
                && i + consume < stmts.len()
                && matches!(&stmts[i + consume], Statement::Comment(c) if is_internal_marker(c))
            {
                consume += 1;
            }
            let mut replacement = if embedded { vec![stmt] } else { vec![stmt, marker] };
            if let Some(ret) = hit.tail_ret {
                replacement.push(Statement::Return(Return { node_origin: Default::default(), values: vec![ret] }));
            }
            let advance = replacement.len();
            stmts.splice(i..i + consume, replacement);
            newly.binders.insert(hit.f_local);
            newly.bodies.insert(current_func);
            i += advance;
            // The block changed; drop the cached index so the next query rebuilds
            // it against the spliced `stmts`.
            last_occ = live_out_index(stmts, i, live_out);
            anchor = i;
        } else {
            // A target declared inside a matched window went with it and is never
            // activated; one reached here unmatched is in scope from here on.
            if let Some(idx) = target_decl_index(&stmts[i], decl_map, targets) {
                active.push(idx);
                (ordered, rivals) = prioritize(&active);
            }
            i += 1;
        }
    }
}

pub(crate) fn stmt_rvalues_mut(s: &mut Statement) -> Vec<&mut RValue> {
    match s {
        Statement::Assign(a) => {
            let mut v: Vec<&mut RValue> = a.right.iter_mut().collect();
            for l in &mut a.left {
                if let LValue::Index(i) = l {
                    v.push(i.left.as_mut());
                    v.push(i.right.as_mut());
                }
            }
            v
        }
        Statement::Call(c) => {
            let mut v: Vec<&mut RValue> = vec![c.value.as_mut()];
            v.extend(c.arguments.iter_mut());
            v
        }
        Statement::MethodCall(m) => {
            let mut v: Vec<&mut RValue> = vec![m.value.as_mut()];
            v.extend(m.arguments.iter_mut());
            v
        }
        Statement::Return(r) => r.values.iter_mut().collect(),
        Statement::If(f) => vec![&mut f.condition],
        Statement::While(w) => vec![&mut w.condition],
        Statement::Repeat(r) => vec![&mut r.condition],
        Statement::NumericFor(nf) => vec![&mut nf.initial, &mut nf.limit, &mut nf.step],
        Statement::GenericFor(gf) => gf.right.iter_mut().collect(),
        Statement::SetList(sl) => {
            let mut v: Vec<&mut RValue> = sl.values.iter_mut().collect();
            if let Some(t) = &mut sl.tail {
                v.push(t);
            }
            v
        }
        _ => Vec::new(),
    }
}

pub(crate) fn stmt_rvalues(s: &Statement) -> Vec<&RValue> {
    match s {
        Statement::Assign(a) => {
            let mut v: Vec<&RValue> = a.right.iter().collect();
            for l in &a.left {
                if let LValue::Index(i) = l {
                    v.push(i.left.as_ref());
                    v.push(i.right.as_ref());
                }
            }
            v
        }
        Statement::Call(c) => {
            let mut v: Vec<&RValue> = vec![c.value.as_ref()];
            v.extend(c.arguments.iter());
            v
        }
        Statement::MethodCall(m) => {
            let mut v: Vec<&RValue> = vec![m.value.as_ref()];
            v.extend(m.arguments.iter());
            v
        }
        Statement::Return(r) => r.values.iter().collect(),
        Statement::If(f) => vec![&f.condition],
        Statement::While(w) => vec![&w.condition],
        Statement::Repeat(r) => vec![&r.condition],
        Statement::NumericFor(nf) => vec![&nf.initial, &nf.limit, &nf.step],
        Statement::GenericFor(gf) => gf.right.iter().collect(),
        Statement::SetList(sl) => {
            let mut v: Vec<&RValue> = sl.values.iter().collect();
            if let Some(t) = &sl.tail {
                v.push(t);
            }
            v
        }
        _ => Vec::new(),
    }
}

fn recurse_into_closures(
    rv: &mut RValue,
    targets: &[Target],
    decl_map: &FxHashMap<RcLocal, usize>,
    active: &[usize],
    newly: &mut Progress,
) {
    match rv {
        RValue::Closure(c) => {
            let fp = Arc::as_ptr(&c.function.0);
            deinline_block(
                &mut c.function.0.lock().body.0,
                targets,
                decl_map,
                active,
                &[],
                &FxHashSet::default(),
                Some(fp),
                true,
                true,
                newly,
            );
        }
        RValue::Call(c) => {
            recurse_into_closures(c.value.as_mut(), targets, decl_map, active, newly);
            for a in &mut c.arguments {
                recurse_into_closures(a, targets, decl_map, active, newly);
            }
        }
        RValue::MethodCall(m) => {
            recurse_into_closures(m.value.as_mut(), targets, decl_map, active, newly);
            for a in &mut m.arguments {
                recurse_into_closures(a, targets, decl_map, active, newly);
            }
        }
        RValue::Index(i) => {
            recurse_into_closures(i.left.as_mut(), targets, decl_map, active, newly);
            recurse_into_closures(i.right.as_mut(), targets, decl_map, active, newly);
        }
        RValue::Unary(u) => {
            recurse_into_closures(u.value.as_mut(), targets, decl_map, active, newly)
        }
        RValue::Binary(b) => {
            recurse_into_closures(b.left.as_mut(), targets, decl_map, active, newly);
            recurse_into_closures(b.right.as_mut(), targets, decl_map, active, newly);
        }
        RValue::Table(t) => {
            for (k, v) in &mut t.0 {
                if let Some(k) = k {
                    recurse_into_closures(k, targets, decl_map, active, newly);
                }
                recurse_into_closures(v, targets, decl_map, active, newly);
            }
        }
        RValue::Select(Select::Call(c)) => {
            recurse_into_closures(c.value.as_mut(), targets, decl_map, active, newly);
            for a in &mut c.arguments {
                recurse_into_closures(a, targets, decl_map, active, newly);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            recurse_into_closures(m.value.as_mut(), targets, decl_map, active, newly);
            for a in &mut m.arguments {
                recurse_into_closures(a, targets, decl_map, active, newly);
            }
        }
        RValue::IfExpression(e) => {
            recurse_into_closures(e.condition.as_mut(), targets, decl_map, active, newly);
            recurse_into_closures(e.then_value.as_mut(), targets, decl_map, active, newly);
            recurse_into_closures(e.else_value.as_mut(), targets, decl_map, active, newly);
        }
        _ => {}
    }
}

/// A matched site: window width, call arguments, and (Gap B arm-return form)
/// the tail return value to re-emit after the call.
/// A matched width, its arguments, the re-emitted tail return, the call's
/// result locals and whether a constant argument was inferred.
type Site = (usize, Vec<RValue>, Option<RValue>, Vec<RcLocal>, Option<RcLocal>);

/// Two widths matching with different calls make the site ambiguous, except
/// that a match inferring a constant argument ([`try_inferred_constant`])
/// yields to one that does not: the constant would have removed the code
/// the wider copy still has (`startFlipbook(nil)` is a prefix of
/// `startFlipbook(time)`).
fn record_site(
    site: &mut Option<Site>,
    ambiguous: &mut bool,
    w: usize,
    u: &Unified,
    tail_ret: Option<&RValue>,
) {
    let found = || Some((w, u.args.clone(), tail_ret.cloned(), u.returned.clone(), u.inferred.clone()));
    match site {
        None => *site = found(),
        // Only inferred matches were seen, so only they made it ambiguous.
        Some((.., Some(_))) if u.inferred.is_none() => {
            *site = found();
            *ambiguous = false;
        }
        Some((.., None)) if u.inferred.is_some() => {}
        Some((_, prev, prev_ret, prev_results, _)) => {
            let same_ret = match (prev_ret.as_ref(), tail_ret) {
                (None, None) => true,
                (Some(a), Some(b)) => rvalue_exact_eq(a, b),
                _ => false,
            };
            if !args_vec_eq(prev, &u.args) || !same_ret || *prev_results != u.returned {
                *ambiguous = true;
            }
        }
    }
}

/// Gap B: is the window `stmts[i..i+w]` a void callee inlined at the function's
/// value-returning tail? Returns the tail return value `RET` if so. Valid only
/// when the window is immediately followed by exactly `return RET` at the tail,
/// RET is a stable scalar, and every return inside the window is `return RET`.
fn value_tail_ret(stmts: &[Statement], i: usize, w: usize, is_func_tail: bool) -> Option<RValue> {
    if !is_func_tail || i + w != stmts.len().checked_sub(1)? {
        return None;
    }
    let ret = match &stmts[i + w] {
        Statement::Return(r) if r.values.len() == 1 => r.values[0].clone(),
        _ => return None,
    };
    if matches!(
        ret,
        RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_)
    ) || ret.has_side_effects()
    {
        return None;
    }
    let raw = &stmts[i..i + w];
    if !block_has_return(raw) {
        return None;
    }
    tail_ret_for_window(raw, ret)
}

/// Gap B, arm-return form: the window `stmts[i..i+w]` ENDS a tail block and every
/// path through it terminates with `return RET` (the structurer cloned the caller's
/// tail `return RET` into each arm of the callee's lifted guard `if`, so no single
/// `return RET` follows the window for `value_tail_ret` to find). Returns RET.
/// `rewrite_return_to_void` then yields the same void shape as the plain form and
/// the splice re-emits `return RET` after the call — sound because the window
/// never falls through (`block_always_returns`), so `f(args); return RET` runs
/// exactly the paths the window did.
fn arm_tail_ret(stmts: &[Statement], i: usize, w: usize, is_func_tail: bool) -> Option<RValue> {
    if !is_func_tail || i + w != stmts.len() {
        return None;
    }
    let raw = &stmts[i..i + w];
    // The window must end in a branch (a bare trailing `return RET` window is the
    // plain form's job, and a non-branching window has nothing to lift).
    if !matches!(
        raw.iter().rev().find(|s| !is_match_trivia(s)),
        Some(Statement::If(_))
    ) || !block_always_returns(raw)
    {
        return None;
    }
    let ret = first_return_value(raw)?;
    if matches!(
        ret,
        RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_)
    ) || ret.has_side_effects()
    {
        return None;
    }
    tail_ret_for_window(raw, ret)
}

/// Shared Gap B gate: every return in `raw` is exactly `return RET`, and RET reads
/// no local the window writes (early-path vs tail-path value would differ).
fn tail_ret_for_window(raw: &[Statement], ret: RValue) -> Option<RValue> {
    if !all_returns_are(raw, &ret) {
        return None;
    }
    let mut written: FxHashSet<RcLocal> = FxHashSet::default();
    collect_written(raw, &mut written);
    for r in ret.values_read() {
        if written.contains(r) {
            return None;
        }
    }
    Some(ret)
}

/// The single value of the first one-value `return` in `stmts` (nested blocks
/// included, source order), if any.
fn first_return_value(stmts: &[Statement]) -> Option<RValue> {
    for s in stmts {
        match s {
            Statement::Return(r) if r.values.len() == 1 => return Some(r.values[0].clone()),
            Statement::Return(_) => return None,
            Statement::If(f) => {
                if let Some(v) = first_return_value(&f.then_block.lock().0) {
                    return Some(v);
                }
                if let Some(v) = first_return_value(&f.else_block.lock().0) {
                    return Some(v);
                }
            }
            Statement::While(w) => {
                if let Some(v) = first_return_value(&w.block.lock().0) {
                    return Some(v);
                }
            }
            Statement::Repeat(r) => {
                if let Some(v) = first_return_value(&r.block.lock().0) {
                    return Some(v);
                }
            }
            Statement::NumericFor(nf) => {
                if let Some(v) = first_return_value(&nf.block.lock().0) {
                    return Some(v);
                }
            }
            Statement::GenericFor(gf) => {
                if let Some(v) = first_return_value(&gf.block.lock().0) {
                    return Some(v);
                }
            }
            _ => {}
        }
    }
    None
}

/// Control never falls off the end of `stmts`: its last effective statement is a
/// `return`, or an `if` whose BOTH arms always return.
fn block_always_returns(stmts: &[Statement]) -> bool {
    match stmts.iter().rev().find(|s| !is_match_trivia(s)) {
        Some(Statement::Return(_)) => true,
        Some(Statement::If(f)) => {
            block_always_returns(&f.then_block.lock().0)
                && block_always_returns(&f.else_block.lock().0)
        }
        _ => false,
    }
}

fn all_returns_are(stmts: &[Statement], ret: &RValue) -> bool {
    stmts.iter().all(|s| match s {
        Statement::Return(r) => r.values.len() == 1 && rvalue_exact_eq(&r.values[0], ret),
        Statement::If(f) => {
            all_returns_are(&f.then_block.lock().0, ret)
                && all_returns_are(&f.else_block.lock().0, ret)
        }
        Statement::While(w) => all_returns_are(&w.block.lock().0, ret),
        Statement::Repeat(r) => all_returns_are(&r.block.lock().0, ret),
        Statement::NumericFor(nf) => all_returns_are(&nf.block.lock().0, ret),
        Statement::GenericFor(gf) => all_returns_are(&gf.block.lock().0, ret),
        _ => true,
    })
}

fn rewrite_return_to_void(stmts: &[Statement], ret: &RValue) -> Vec<Statement> {
    stmts
        .iter()
        .map(|s| match s {
            Statement::Return(r) if r.values.len() == 1 && rvalue_exact_eq(&r.values[0], ret) => {
                Statement::Return(Return::default())
            }
            Statement::If(f) => Statement::If(If::new(
                f.condition.clone(),
                Block(rewrite_return_to_void(&f.then_block.lock().0, ret)),
                Block(rewrite_return_to_void(&f.else_block.lock().0, ret)),
            )),
            Statement::While(w) => Statement::While(While::new(
                w.condition.clone(),
                Block(rewrite_return_to_void(&w.block.lock().0, ret)),
            )),
            Statement::Repeat(r) => Statement::Repeat(Repeat::new(
                r.condition.clone(),
                Block(rewrite_return_to_void(&r.block.lock().0, ret)),
            )),
            Statement::NumericFor(nf) => Statement::NumericFor(Box::new(NumericFor {
                initial: nf.initial.clone(),
                limit: nf.limit.clone(),
                step: nf.step.clone(),
                counter: nf.counter.clone(),
                block: Arc::new(Mutex::new(Block(rewrite_return_to_void(
                    &nf.block.lock().0,
                    ret,
                )))),
            })),
            Statement::GenericFor(gf) => Statement::GenericFor(GenericFor {
                res_locals: gf.res_locals.clone(),
                right: gf.right.clone(),
                block: Arc::new(Mutex::new(Block(rewrite_return_to_void(
                    &gf.block.lock().0,
                    ret,
                )))),
                origin: gf.origin,
            }),
            other => other.clone(),
        })
        .collect()
}

struct Hit {
    f_local: RcLocal,
    consume: usize, // statements to replace, starting at i
    args: Vec<RValue>,
    /// The locals the call declares: none emits `f(args)`, else `local results = f(args)`.
    results: Vec<RcLocal>,
    /// Gap B (arm-return form): the consumed window ended the block with EVERY
    /// path `return RET`; the splice re-emits `return RET` after the call.
    tail_ret: Option<RValue>,
    /// The statement that uses the value, with the call already in its place
    /// (`match_embedded_value`); emitted instead of a result declaration.
    host: Option<Statement>,
    /// The parameter a constant argument was inferred for.
    inferred: Option<RcLocal>,
}

fn try_match_at(
    stmts: &[Statement],
    i: usize,
    // The first statement at or after `i` that is not `Empty` (`stmts.len()`
    // if none).
    anchor: usize,
    targets: &[Target],
    // The targets in scope here, in `reconstruction_search` priority order.
    ordered: &candidates::Candidates,
    // The other targets in scope: consulted only where an `ordered` one matches.
    rivals: &candidates::Candidates,
    // Receives the helpers of a site refused because two of them match.
    contested: &mut FxHashSet<RcLocal>,
    current_func: Option<FnPtr>,
    is_func_tail: bool,
    is_func_body_top: bool,
    outer_continuation: &[&[Statement]],
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
    canon_cache: &mut CanonCache,
) -> Option<Hit> {
    if ordered.is_empty() {
        return None; // no targets in scope here — nothing to match (skip the scan)
    }
    // Fresh per-position: `stmts` mutates on each splice, so absolute (start, w)
    // keys from a prior position no longer describe the same window content.
    canon_cache.clear();
    dprof::inc(&dprof::MATCH_CALLS, 1);
    crate::telemetry::count("match_calls", 1);
    let _mt = dprof::T::new(&dprof::MATCH_US);
    // Cheap O(1) prefilter anchor: the first non-`Empty` statement at/after `i`.
    // Every candidate window is `canon`'d before unification, and `canon` drops
    // leading `Empty`s while preserving the first surviving statement's variant,
    // so a Void target can only match here if its `pat[0]` shares this variant.
    // This skips the expensive canon()/unify window scan for the (position,
    // target) pairs the unifier would reject on the very first statement — the
    // large majority, since ~16 targets are active per position but typically
    // only one matches the anchor's variant. (Void only: Value targets keep their
    // existing `result_decl(stmts[i])` gate, and a Value pattern's `pat[0]` may be
    // a leaf `return X` unified against an `Assign`, so the variant check would be
    // unsound there.)
    // Skip only leading `Empty` (NOT internal markers) for this O(1) variant
    // prefilter. Skipping markers here was tried and reverted: it let `match_void`
    // attempt windows that START at a reconstruction marker, and on a chained void
    // site that silently dropped the trailing `-- inlined` marker from an already-
    // reconstructed call (the call stayed correct, but lost its UNHOOKABLE
    // annotation). Nothing legitimately begins a match at a marker position, so
    // the canon-alignment was cosmetic-negative; keep the original predicate.
    let anchor_stmt = stmts.get(anchor);
    let anchor_disc = anchor_stmt.map(std::mem::discriminant);
    // Second prefilter dimension: the fixed-name anchor of that first statement
    // (method / global-call name). Computed once per position; compared to each
    // candidate target's `pat0_anchor_key`. Same sound domain as `pat0_kind`.
    let anchor_key = anchor_stmt.and_then(stmt_anchor_key);
    let anchor_is_if = matches!(anchor_stmt, Some(Statement::If(_)));
    let assign_kind = std::mem::discriminant(&Statement::Assign(Assign::new(Vec::new(), Vec::new())));
    // only targets whose local function is in scope here (declared earlier, in a
    // visible block) are candidates — emitting a call to an out-of-scope local
    // would be invalid.
    // One target's attempt at this position; `Err` when the fuel ran out.
    let mut attempt = |ti: usize| -> Result<Option<Hit>, ()> {
        let t = &targets[ti];
        if current_func == Some(t.func_ptr) {
            return Ok(None); // never match a function against its own definition body
        }
        if t.pat.is_empty() {
            return Ok(None);
        }
        // O(1) variant prefilter: the first non-`Empty` statement at `i` must share
        // `pat[0]`'s variant. Applies to Void targets and to `AtPrefix` Value
        // targets (whose `pat[0]` is the leading callee-prefix `Assign`, which
        // `canon` preserves). NOT to `AtResultDecl` Value targets — their `pat[0]`
        // may be a leaf `return X` unified against an `Assign`, so the variant
        // check would be unsound there.
        // A constant argument can remove a leading `if` of a specializable
        // pattern (Tier B), leaving any statement first at the site.
        let head_may_vanish = t.specializable && matches!(t.pat[0], Statement::If(_));
        let use_disc = !head_may_vanish
            && (t.kind == TKind::Void || (t.kind == TKind::Value && t.value_anchor == ValueAnchor::AtPrefix));
        // Canon may fuse a site's select `if` into the assignment a pattern
        // starts with (N5).
        let fused_head = anchor_is_if && t.pat0_kind == assign_kind;
        if use_disc && anchor_disc != Some(t.pat0_kind) && !fused_head {
            return Ok(None); // first-statement variant cannot match this pattern
        }
        // Name prefilter (same targets as the variant check): when BOTH the position
        // and the pattern have a fixed-name head anchor and they differ, the exact
        // unify of `pat[0]` would fail — skip without entering the window scan.
        if use_disc
            && let (Some(ak), Some(tk)) = (anchor_key, t.pat0_anchor_key)
            && ak != tk
        {
            return Ok(None);
        }
        // One unit per width the non-allocating length check scans; the deep
        // canon/unify work is charged where it happens (`charge_unify`).
        if !t.search.spend(t.pat_spine_len.saturating_add(2)) { return Err(()); }
        let hit = match (t.kind, t.value_anchor) {
            (TKind::Void, _) => match_void(
                stmts,
                i,
                t,
                is_func_tail,
                is_func_body_top,
                outer_continuation,
                last_occ,
                canon_cache,
                current_func,
            ),
            (TKind::Value, _) if t.loop_exit_at.is_some() => {
                match_value_loop(stmts, i, t, is_func_body_top, last_occ, current_func)
            }
            (TKind::Value, ValueAnchor::AtResultDecl) => {
                match_value(stmts, i, t, is_func_body_top, last_occ, canon_cache, current_func)
            }
            (TKind::Value, ValueAnchor::AtPrefix) => {
                match_value_prefixed(stmts, i, t, current_func, is_func_body_top, last_occ)
            }
        };
        Ok(hit.filter(|hit| !hit.inferred.as_ref().is_some_and(|param| continues_pruned_branch(t, param, stmts, i + hit.consume))))
    };
    // Where several helpers match, the one covering the most statements wins:
    // each rebuild is exact, and a shorter one would leave the rest pasted
    // (`cancel()` matches only the first statement of `purchase(nil)`). Two
    // that cover the same statements are ambiguous: refuse.
    fn offer(found: &mut Option<Hit>, tied: &mut Vec<RcLocal>, hit: Hit) {
        match found {
            Some(best) if best.consume > hit.consume => {}
            Some(best) if best.consume == hit.consume => tied.push(hit.f_local),
            _ => {
                tied.clear();
                *found = Some(hit);
            }
        }
    }
    let mut found: Option<Hit> = None;
    let mut tied: Vec<RcLocal> = Vec::new();
    for ti in ordered.matching(anchor_disc, anchor_key, anchor_is_if) {
        let Ok(hit) = attempt(ti) else { return None };
        if let Some(h) = hit {
            offer(&mut found, &mut tied, h);
        }
    }
    // A target outside this iteration's focus cannot match anew, but where a
    // focused one matches, it may still cover more or make the site ambiguous.
    if found.is_some() {
        for ti in rivals.matching(anchor_disc, anchor_key, anchor_is_if) {
            match attempt(ti) {
                Ok(None) => {}
                Ok(Some(rival)) => offer(&mut found, &mut tied, rival),
                Err(()) => return None,
            }
        }
    }
    if !tied.is_empty() {
        contested.extend(tied);
        contested.extend(found.map(|best| best.f_local));
        return None;
    }
    // A width or target skipped for fuel may have been a competing match.
    if ordered.iter().chain(rivals.iter()).next().is_some_and(|&ti| targets[ti].search.exhausted()) {
        return None;
    }
    found
}

fn match_void(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    is_func_tail: bool,
    is_func_body_top: bool,
    outer_continuation: &[&[Statement]],
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let kc = t.pat.len();
    // Written-param targets (`Target::written_params`): the site starts with one
    // `local L = ARG` copy per written param (any order; each is matched to its
    // param by the injective local binding, and `finish_unified` recovers `ARG`).
    // Consume them here; the body window starts right after.
    let k = t.written_params.len();
    let mut prefix: Vec<(RcLocal, RValue)> = Vec::with_capacity(k);
    let mut start = i;
    while prefix.len() < k {
        let s = stmts.get(start)?;
        if is_match_trivia(s) {
            start += 1;
            continue;
        }
        match s {
            Statement::Assign(a)
                if a.prefix && !a.parallel && a.left.len() == 1 && a.right.len() == 1 =>
            {
                let LValue::Local(l) = &a.left[0] else {
                    return None;
                };
                prefix.push((l.clone(), a.right[0].clone()));
                start += 1;
            }
            _ => return None,
        }
    }
    // F2: an effective-count ceiling (trivia don't consume the budget) so a nested
    // candidate region carrying 2+ interposed markers is still reachable. The
    // ceiling is the pattern's tail-spine length (guard-form expansion of every
    // tail `if`, see `tail_spine_len`) plus one for a site-only trailing `return`.
    let max_w = raw_width_for_effective(stmts, start, t.pat_spine_len + 1);
    let mut site: Option<Site> = None;
    let mut ambiguous = false;
    // A constant argument can remove a branch of the body (Tier B), so a
    // specializable helper's copy may be shorter than its body.
    let min_w = if t.specializable { 1 } else { kc };
    for w in min_w..=max_w {
        dprof::inc(&dprof::WIDTH_ITERS, 1);
        crate::telemetry::count("width_candidates", 1);
        let raw = &stmts[start..start + w];
        // Never replace a function's ENTIRE top-level body with a single call:
        // the ambiguous thin-wrapper case (`B(x)=A(x)`).
        if is_func_body_top && i == 0 && start + w == stmts.len() {
            continue;
        }
        if w < kc || canon_top_len(raw, true) < kc {
            let shorter = canon_top_len(raw, true) < kc
                && !(block_has_return(raw) && !(is_func_tail && start + w == stmts.len()));
            if shorter {
                let plain = canon_window(canon_cache, t, stmts, start, w);
                if charge_unify(t, &plain)
                    && let Some(u) = try_unify_specialized_site(t, &plain, &prefix, current_func)
                    && !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals)
                {
                    record_site(&mut site, &mut ambiguous, w, &u, None);
                }
            }
            continue;
        }
        // Attempt 1 — plain canon, with tail-safety for consuming a caller return.
        // Order the pure gates cheapest-reject-first: the non-allocating top-level
        // canon-length check rejects the large majority of widths, so evaluate it
        // BEFORE the recursive `block_has_return` return-safety scan (both are
        // side-effect-free, so this reordering is byte-identical — it only avoids
        // computing `block_has_return` for windows whose length already can't match).
        if canon_top_len(raw, true) == kc {
            let plain_blocked = {
                dprof::inc(&dprof::BHR_CALLS, 1);
                crate::telemetry::count("return_scan_calls", 1);
                let _t = dprof::T::new(&dprof::BHR_US);
                block_has_return(raw) && !(is_func_tail && start + w == stmts.len())
            };
            if !plain_blocked && plain_kinds_may_match(t, raw) {
                let plain = canon_window(canon_cache, t, stmts, start, w);
                if let Some(u) = try_unify_site_any(t, &plain, &prefix, current_func) {
                    // every callee-temp must be dead after the consumed window, else
                    // a later use would reference a now-removed declaration.
                    if !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals) {
                        record_site(&mut site, &mut ambiguous, w, &u, None);
                    }
                }
            }
            // Tier C/CPS: a return from inside an inlined loop becomes a loop
            // guard followed by a cloned caller continuation.  Verify the clone
            // against the actual suffix before replacing only the helper prefix.
            if t.cps_loop_return && (start + w < stmts.len() || !outer_continuation.is_empty()) {
                let plain = canon_window(canon_cache, t, stmts, start, w);
                let continuation = semantic_continuation(&stmts[start + w..], outer_continuation);
                if let Some(u) = try_unify_cps_site(t, raw, &plain, &continuation, &prefix, current_func) {
                    let live = tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals);
                    if !live {
                        record_site(&mut site, &mut ambiguous, w, &u, None);
                    }
                }
            }
        }
        // Attempt 4 — a loop return inlined where code follows it: the
        // structurer lowers it to a flag and a `break` (`unflag_loop_exits`).
        if t.cps_loop_return
            && let Some((unflagged, flags)) = unflag_loop_exits(raw)
            && canon_top_len(&unflagged, true) == kc
            && charge_window(t, &unflagged)
        {
            let folded = canon_recurse(canon_top(&unflagged, true), true);
            if let Some(u) = try_unify_site_any(t, &folded, &prefix, current_func) {
                let mut dead = |set: &FxHashSet<RcLocal>| !tail_has_live(last_occ, stmts, i, start + w, set);
                if dead(&u.callee_locals) && dead(&flags) {
                    record_site(&mut site, &mut ambiguous, w, &u, None);
                }
            }
        }
        // Attempt 2 (Gap B) — void callee inlined at a value-returning caller's
        // tail, its void early-returns lowered to the caller's tail `return RET`.
        // (A helper returning its locals has no early return.)
        if t.returns.is_empty()
            && let Some(ret) = value_tail_ret(stmts, start, w, is_func_tail)
        {
            let rewritten = rewrite_return_to_void(raw, &ret);
            if canon_top_len(&rewritten, true) == kc && charge_window(t, &rewritten) {
                let folded = canon_recurse(canon_top(&rewritten, true), true);
                if let Some(u) = try_unify_site_any(t, &folded, &prefix, current_func) {
                    if !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals) {
                        record_site(&mut site, &mut ambiguous, w, &u, None);
                    }
                }
            }
        }
        // Attempt 3 (Gap B, arm-return form) — same lowering, but the caller's
        // tail `return RET` was cloned into every arm of the window's final `if`
        // and nothing follows the window. Re-emit `return RET` after the call.
        if t.returns.is_empty()
            && let Some(ret) = arm_tail_ret(stmts, start, w, is_func_tail)
        {
            let rewritten = rewrite_return_to_void(raw, &ret);
            if canon_top_len(&rewritten, true) == kc && charge_window(t, &rewritten) {
                let folded = canon_recurse(canon_top(&rewritten, true), true);
                if let Some(u) = try_unify_site_any(t, &folded, &prefix, current_func) {
                    if !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals) {
                        record_site(&mut site, &mut ambiguous, w, &u, Some(&ret));
                    }
                }
            }
        }
    }
    if ambiguous {
        return None;
    }
    let (w, args, tail_ret, results, inferred) = site?;
    Some(Hit {
        f_local: t.f_local.clone(),
        consume: (start - i) + w,
        args,
        results,
        tail_ret,
        host: None,
        inferred,
    })
}

/// A value-returning callee inlines as `local RESULT; <region writing RESULT>`.
/// `stmts[i]` must be the (init-less) `local RESULT` declaration; the region that
/// follows it computes RESULT on every path. We match the value pattern (whose
/// leaves are `return X`) against that region (whose leaves are `RESULT = X`).
fn match_value(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let Some(r) = result_decl(&stmts[i]) else {
        return match_declared_value(stmts, i, i, t, is_func_body_top, last_occ, current_func);
    };
    let kc = t.pat.len();
    let body_start = i + 1;
    // F2: effective-count ceiling (interposed trivia don't consume the budget).
    let max_w = raw_width_for_effective(stmts, body_start, t.pat_raw_len + 1);
    let mut site: Option<Site> = None;
    let mut ambiguous = false;
    for w in kc..=max_w {
        // whole-body gate: the decl + region must not be the function's entire body.
        if is_func_body_top && i == 0 && body_start + w == stmts.len() {
            continue;
        }
        let region = &stmts[body_start..body_start + w];
        // Cheapest-reject-first: the non-allocating top-level canon-length check
        // rejects most widths, so run it BEFORE the recursive `block_has_return`
        // return-safety scan. Both are pure `continue` gates, so the order is
        // byte-identical; it just avoids scanning returns for wrong-length regions.
        if canon_top_len(region, true) != kc {
            continue;
        }
        // the region assigns RESULT; it must not itself contain returns.
        if block_has_return(region) {
            continue;
        }
        // An empty window stands for a plain form whose kinds cannot match.
        let cwin = if plain_kinds_may_match(t, region) {
            canon_window(canon_cache, t, stmts, body_start, w)
        } else {
            std::rc::Rc::new(Vec::new())
        };
        // Plain form first; then the result-alias form (`alias_result_leaves`):
        // a leaf that writes RESULT early and keeps using it (`RESULT = E;
        // S(RESULT)…`) is rewritten to `local T = E; S(T)…; RESULT = T` — the
        // shape the callee's `local L = E; S(L)…; return L` unifies against.
        let attempts: [(std::rc::Rc<Vec<Statement>>, Option<Vec<Statement>>); 2] = [
            (cwin, None),
            match alias_result_leaves(region, &r) {
                Some(rw) if !block_has_return(&rw) && canon_top_len(&rw, true) == kc && charge_window(t, &rw) => (
                    std::rc::Rc::new(canon_recurse(canon_top(&rw, true), true)),
                    Some(rw),
                ),
                _ => (std::rc::Rc::new(Vec::new()), None),
            },
        ];
        for (idx, (cw, rewritten)) in attempts.iter().enumerate() {
            if (idx == 0 && cw.is_empty()) || (idx == 1 && rewritten.is_none()) {
                continue;
            }
            let region_eff: &[Statement] = rewritten.as_deref().unwrap_or(region);
            if let Some(u) = try_unify_site_any(t, cw, &[], current_func) {
                // RESULT must be exactly the declared local and only written (never
                // read) inside the region, so the region is its full computation.
                // A later reassignment of RESULT is FINE: the replacement re-declares
                // it as `local RESULT = f(args)`, so subsequent writes stay valid —
                // we must NOT reject on that. Every OTHER callee-temp, though, must be
                // dead after the region (its declaration is being removed).
                if u.result.as_ref() == Some(&r)
                    // The RESULT register must not ALSO be a region-internal callee temp
                    // (a re-declaration / for-counter shadowing it). On genuine -O2 the
                    // result register and the callee's internal temps are distinct, so
                    // this changes no real match; it mirrors `match_value_prefixed`'s
                    // identical conjunct (the getOwnerId reassignment-collision class)
                    // and keeps the two value matchers symmetric (F10b hardening).
                    && !u.callee_locals.contains(&r)
                    && !block_reads_local(region_eff, &r)
                    && !tail_has_live(last_occ, stmts, i, body_start + w, &u.callee_locals)
                {
                    record_site(&mut site, &mut ambiguous, w, &u, None);
                }
            }
        }
    }
    if ambiguous {
        return None;
    }
    let (w, args, _, _, inferred) = site?;
    Some(Hit {
        f_local: t.f_local.clone(),
        consume: 1 + w,
        args,
        results: vec![r],
        tail_ret: None,
        host: None,
        inferred,
    })
}

/// A value helper returning from inside a loop (`Target::loop_exit_at`),
/// inlined as `local r = f(args)`: `PRE; local r; local ok = true; for … do
/// if c then r = x; ok = false; break end end; if ok then REST end`, the two
/// declarations in either order ([`unflag_value_loop`]). The window ends with
/// the loop, or with the flag's `if` right after it.
fn match_value_loop(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let pre_len = t.loop_exit_at?;
    // Cheapest rejects first: the loop of the pattern's kind at its place,
    // after the helper's leading statements and the two declarations.
    let looped = nth_effective_index(stmts, i, pre_len + 2)?;
    if std::mem::discriminant(&stmts[looped]) != std::mem::discriminant(&t.pat[pre_len]) {
        return None;
    }
    let first = nth_effective_index(stmts, i, pre_len)?;
    let second = nth_effective_index(stmts, first + 1, 0)?;
    let (result, flag) = match (result_decl(&stmts[first]), flag_decl(&stmts[second])) {
        (Some(result), Some(flag)) => (result, flag),
        _ => (result_decl(&stmts[second])?, flag_decl(&stmts[first])?),
    };
    let guard = nth_effective_index(stmts, looped + 1, 0).filter(|&g| {
        matches!(&stmts[g], Statement::If(branch) if matches!(&branch.condition, RValue::Local(read) if *read == flag))
    });
    let end = guard.map_or(looped, |g| g) + 1;
    if is_func_body_top && i == 0 && end == stmts.len() {
        return None;
    }
    // The only `return`s of the window are the ones given back.
    if block_has_return(&stmts[i..end]) {
        return None;
    }
    let window = unflag_value_loop(&stmts[i..first], &stmts[looped], guard.map(|g| &stmts[g]), &result, &flag)?;
    if canon_top_len(&window, true) != t.pat.len() || !charge_window(t, &window) {
        return None;
    }
    let u = try_unify_site_any(t, &canon_recurse(canon_top(&window, true), true), &[], current_func)?;
    let mut dead = |set: &FxHashSet<RcLocal>| !tail_has_live(last_occ, stmts, i, end, set);
    let complete = u.result.as_ref() == Some(&result)
        && !u.callee_locals.contains(&result)
        && !u.callee_locals.contains(&flag)
        && !block_reads_local(&window, &result)
        && dead(&u.callee_locals)
        && dead(&FxHashSet::from_iter([flag]));
    complete.then(|| Hit {
        f_local: t.f_local.clone(),
        consume: end - i,
        args: u.args,
        results: vec![result],
        tail_ret: None,
        host: None,
        inferred: u.inferred,
    })
}

/// `local flag = true` -> `flag`.
fn flag_decl(statement: &Statement) -> Option<RcLocal> {
    let Statement::Assign(assign) = statement else { return None };
    match (assign.left.as_slice(), assign.right.as_slice()) {
        ([LValue::Local(flag)], [RValue::Literal(Literal::Boolean(true))]) if assign.prefix && !assign.parallel => {
            Some(flag.clone())
        }
        _ => None,
    }
}

/// `local RESULT = E` at `d`: SSA already fused the value branch into the
/// RESULT declaration (`c and K or B`), and canon fuses the helper's return
/// diamonds the same way. `stmts[i..d]` are the callee's leading statements
/// (`AtPrefix`, none for `AtResultDecl`). The window unifies as `<prefix>;
/// local RESULT; RESULT = E`, under the gates of the two value matchers.
fn match_declared_value(
    stmts: &[Statement],
    i: usize,
    d: usize,
    t: &Target,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let Statement::Assign(decl) = &stmts[d] else { return None };
    if !decl.prefix || decl.parallel || decl.left.len() != 1 || decl.right.len() != 1 {
        return None;
    }
    let LValue::Local(r) = &decl.left[0] else { return None };
    if is_func_body_top && i == 0 && d + 1 == stmts.len() {
        return None;
    }
    let prefix = &stmts[i..d];
    let mut window = prefix.to_vec();
    window.push(Statement::Assign(Assign { prefix: false, ..decl.clone() }));
    if canon_top_len(&window, true) != t.pat.len() || block_has_return(prefix) || !charge_window(t, &window) {
        return None;
    }
    let u = try_unify_site_any(t, &canon_recurse(canon_top(&window, true), true), &[], current_func)?;
    let complete = u.result.as_ref() == Some(r)
        && !u.callee_locals.contains(r)
        && !block_reads_local(prefix, r)
        && !tail_has_live(last_occ, stmts, i, d + 1, &u.callee_locals);
    complete.then(|| Hit { f_local: t.f_local.clone(), consume: d + 1 - i, args: u.args, results: vec![r.clone()], tail_ret: None, host: None, inferred: u.inferred })
}

/// `<prefix>` computing a local the helper returns, then read under its own
/// name: `local conn; conn = signal:Connect(function() conn:Disconnect() end)`
/// before `trove:Add(conn)`, for a helper ending `return conn`. The call
/// declares that local: `local conn = helper(args)`, or stands alone when
/// nothing reads it. The helper's closures keep their own `conn` then, so
/// nothing may write the local once the prefix defines it: no later
/// statement, and no closure, the prefix's included.
fn match_returned_local(
    stmts: &[Statement],
    i: usize,
    d: usize,
    t: &Target,
    current_func: Option<FnPtr>,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
) -> Option<Hit> {
    let prefix = &stmts[i..d];
    if t.falls_off || prefix.is_empty() || block_has_return(prefix) {
        return None;
    }
    let Some(Statement::Return(ret)) = t.pat.last() else { return None };
    if !matches!(ret.values.as_slice(), [RValue::Local(_)]) {
        return None;
    }
    let mut written_later = FxHashSet::default();
    collect_written(&stmts[d..], &mut written_later);
    closure_writes(prefix, &mut written_later);
    let declared = prefix.iter().filter_map(|statement| match statement {
        Statement::Assign(assign) if assign.prefix => Some(assign.left.iter()),
        _ => None,
    }).flatten().filter_map(|left| left.as_local());
    for local in declared {
        if written_later.contains(local) {
            continue;
        }
        let read_later = tail_has_live(last_occ, stmts, i, d, &FxHashSet::from_iter([local.clone()]));
        let result = RcLocal::default();
        let mut window = prefix.to_vec();
        window.push(Assign::new(vec![result.clone().into()], vec![RValue::Local(local.clone())]).into());
        if canon_top_len(&window, true) != t.pat.len() || !charge_window(t, &window) {
            continue;
        }
        let Some(u) = try_unify_site_any(t, &canon_recurse(canon_top(&window, true), true), &[], current_func) else {
            continue;
        };
        let mut others = u.callee_locals.clone();
        others.remove(local);
        if u.result.as_ref() != Some(&result) || tail_has_live(last_occ, stmts, i, d, &others) {
            continue;
        }
        let results = if read_later { vec![local.clone()] } else { Vec::new() };
        return Some(Hit { f_local: t.f_local.clone(), consume: d - i, args: u.args, results, tail_ret: None, host: None, inferred: u.inferred });
    }
    None
}

/// The locals the closures `stmts` create write, at any depth.
fn closure_writes(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for statement in stmts {
        statement.traverse_rvalues_ref(&mut |value| {
            if let RValue::Closure(closure) = value {
                collect_written(&closure.function.0.lock().body.0, out);
            }
        });
        match statement {
            Statement::If(branch) => {
                closure_writes(&branch.then_block.lock().0, out);
                closure_writes(&branch.else_block.lock().0, out);
            }
            Statement::While(node) => closure_writes(&node.block.lock().0, out),
            Statement::Repeat(node) => closure_writes(&node.block.lock().0, out),
            Statement::NumericFor(node) => closure_writes(&node.block.lock().0, out),
            Statement::GenericFor(node) => closure_writes(&node.block.lock().0, out),
            _ => {}
        }
    }
}

/// `<prefix>; S` where SSA folded the value branch into the one statement
/// `S` that uses it: `local n = tonumber(x); obj:SetAttribute("K", not n
/// and 4 or math.clamp(...))`. The helper's leading statements may run where
/// the value is evaluated only when nothing observable precedes it in `S`
/// and it is evaluated on every path (`visit_leading_values`); the prefix
/// locals must then be read nowhere else. `S` gets the call in that place.
fn match_embedded_value(
    stmts: &[Statement],
    i: usize,
    d: usize,
    t: &Target,
    current_func: Option<FnPtr>,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
) -> Option<Hit> {
    let prefix = &stmts[i..d];
    if t.falls_off || prefix.is_empty() || block_has_return(prefix) || (is_func_body_top && i == 0 && d + 1 == stmts.len()) {
        return None;
    }
    // Only a value of the kind the pattern returns can match it. A helper
    // handing back a parameter it reads nowhere else constrains nothing
    // there: any value would match (`mark(label, value)` making the callee
    // `mark` itself `(mark("x", mark))(...)`).
    let Some(Statement::Return(ret)) = t.pat.last() else { return None };
    let [pattern_value] = ret.values.as_slice() else { return None };
    if let RValue::Local(local) = pattern_value
        && t.params.contains(local)
        && count_local_reads(&t.pat, local) == 1
    {
        return None;
    }
    let root = value_kind(pattern_value);
    let result = RcLocal::default();
    let mut window = prefix.to_vec();
    window.push(Assign::new(vec![result.clone().into()], vec![RValue::Literal(Literal::Nil)]).into());
    if canon_top_len(&window, true) != t.pat.len() {
        return None;
    }
    let mut host = stmts[d].clone();
    let mut found = None;
    // The call runs the helper's statements after every value `S` evaluated
    // before its place, which the prefix ran before: none of those may see a
    // difference. A local the prefix writes, a cell a call in it may write,
    // and a global or field that code may change once the prefix runs any
    // (`dispatch = new; dispatch(f())` is not `dispatch(helper())`). A method
    // lookup is no effect, as everywhere in evaluation order.
    let mut prefix_writes = FxHashSet::default();
    collect_written(prefix, &mut prefix_writes);
    let prefix_runs_code = may_run_code(prefix);
    let function = current_func.map(|function| function as usize);
    let register = |local: &RcLocal| t.captures.register_of(local, function);
    let changed_by_prefix = |read: &Earlier| match read {
        Earlier::Value(RValue::Literal(_)) => false,
        Earlier::Value(value @ RValue::Local(local)) => {
            prefix_writes.contains(local) || (prefix_runs_code && !t.captures.stable_at(value, function))
        }
        Earlier::Value(value) => prefix_runs_code && !t.captures.constant_import(value),
    };
    visit_leading_values(&mut host, &register, &mut |value, evaluated_before, spread| {
        if value_kind(value) != root || evaluated_before.iter().any(&changed_by_prefix) {
            return false;
        }
        // Where every result is taken, the call must give as many as the
        // value it replaces: a helper returning `(find(...))` is one value,
        // the inlined `find(...)` in `local a, b = ...` two.
        if spread.takes_all(value) != (spread != Spread::One && is_multiple(pattern_value)) {
            return false;
        }
        if let Some(Statement::Assign(store)) = window.last_mut() {
            store.right[0] = value.clone();
        }
        if !charge_window(t, &window) {
            return false;
        }
        let Some(u) = try_unify_site_any(t, &canon_recurse(canon_top(&window, true), true), &[], current_func) else {
            return false;
        };
        if u.result.as_ref() != Some(&result) || u.callee_locals.contains(&result) {
            return false;
        }
        let call = Call::new(RValue::Local(t.f_local.clone()), u.args.clone())
            .reconstructed(crate::call_origins::Kind::StatementDeinline);
        *value = RValue::Call(call);
        found = Some(u);
        true
    });
    let u = found?;
    // The helper's locals are gone with the prefix: `S` may neither read nor
    // write them anywhere, its nested blocks included.
    let mut host_locals = FxHashSet::default();
    collect_reads(std::slice::from_ref(&host), &mut host_locals);
    collect_written(std::slice::from_ref(&host), &mut host_locals);
    if !host_locals.is_disjoint(&u.callee_locals) || tail_has_live(last_occ, stmts, i, d + 1, &u.callee_locals) {
        return None;
    }
    Some(Hit { f_local: t.f_local.clone(), consume: d + 1 - i, args: u.args, results: Vec::new(), tail_ret: None, host: Some(host), inferred: u.inferred })
}

/// Offers `visit` each value of `statement` that Lua evaluates on every path
/// before anything observable happens in it, outermost first and in
/// evaluation order, until `visit` takes one (and returns `true`). Local
/// reads, literals and import paths are not observable; a store's address
/// comes before the values it stores, except a `register` local, which the
/// store reads when it runs. `visit` also gets the reads already evaluated at
/// that point (`Earlier`).
fn visit_leading_values(
    statement: &mut Statement,
    register: &dyn Fn(&RcLocal) -> bool,
    visit: &mut impl FnMut(&mut RValue, &[Earlier], Spread) -> bool,
) -> bool {
    #[derive(PartialEq)]
    enum Flow {
        Taken,
        Clear,
        Blocked,
    }
    fn walk(
        value: &mut RValue,
        spread: Spread,
        visit: &mut impl FnMut(&mut RValue, &[Earlier], Spread) -> bool,
        before: &mut Vec<Earlier>,
    ) -> Flow {
        if visit(value, before, spread) {
            return Flow::Taken;
        }
        match value {
            RValue::Literal(_) | RValue::Local(_) | RValue::Global(_) => {
                before.push(Earlier::Value(value.clone()));
                Flow::Clear
            }
            RValue::Index(index) if is_import_path(&index.left) && matches!(*index.right, RValue::Literal(_)) => {
                before.push(Earlier::Value(value.clone()));
                Flow::Clear
            }
            RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
                match walk(&mut binary.left, Spread::One, visit, before) {
                    Flow::Taken => Flow::Taken,
                    _ => Flow::Blocked,
                }
            }
            RValue::IfExpression(select) => match walk(&mut select.condition, Spread::One, visit, before) {
                Flow::Taken => Flow::Taken,
                _ => Flow::Blocked,
            },
            RValue::Unary(unary) if unary.operation == UnaryOperation::Not => walk(&mut unary.value, Spread::One, visit, before),
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                match walk_method(&mut call.value, &mut call.arguments, visit, before) {
                    Flow::Taken => Flow::Taken,
                    _ => Flow::Blocked,
                }
            }
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                match walk_call(&mut call.value, &mut call.arguments, visit, before) {
                    Flow::Taken => Flow::Taken,
                    _ => Flow::Blocked,
                }
            }
            RValue::Select(_) | RValue::Index(_) | RValue::Binary(_) | RValue::Unary(_) => {
                let mut flow = Flow::Clear;
                value.visit_rvalues_mut(&mut |child| {
                    flow = walk(child, Spread::One, visit, before);
                    flow == Flow::Clear
                });
                if flow == Flow::Taken { Flow::Taken } else { Flow::Blocked }
            }
            _ => Flow::Blocked,
        }
    }
    // A list whose last value stands at `last`, the others at `Spread::One`.
    fn walk_all<'a>(
        values: impl IntoIterator<Item = &'a mut RValue>,
        last: Spread,
        visit: &mut impl FnMut(&mut RValue, &[Earlier], Spread) -> bool,
        before: &mut Vec<Earlier>,
    ) -> Flow {
        let mut values = values.into_iter().peekable();
        while let Some(value) = values.next() {
            let spread = if values.peek().is_none() { last } else { Spread::One };
            match walk(value, spread, visit, before) {
                Flow::Clear => {}
                flow => return flow,
            }
        }
        Flow::Clear
    }
    fn walk_call(
        callee: &mut RValue,
        arguments: &mut [RValue],
        visit: &mut impl FnMut(&mut RValue, &[Earlier], Spread) -> bool,
        before: &mut Vec<Earlier>,
    ) -> Flow {
        match walk(callee, Spread::One, visit, before) {
            Flow::Clear => walk_all(arguments, Spread::Values, visit, before),
            flow => flow,
        }
    }
    fn walk_method(
        receiver: &mut RValue,
        arguments: &mut [RValue],
        visit: &mut impl FnMut(&mut RValue, &[Earlier], Spread) -> bool,
        before: &mut Vec<Earlier>,
    ) -> Flow {
        walk_call(receiver, arguments, visit, before)
    }
    let mut before = Vec::new();
    let flow = match statement {
        Statement::Assign(assign) if !assign.parallel => {
            let last = if assign.left.len() > assign.right.len() { Spread::Store } else { Spread::One };
            let mut addresses = Vec::new();
            for lhs in &mut assign.left {
                if let LValue::Index(index) = lhs {
                    for address in [&mut *index.left, &mut *index.right] {
                        if !matches!(address, RValue::Local(local) if register(local)) {
                            addresses.push(address);
                        }
                    }
                }
            }
            match walk_all(addresses, Spread::One, visit, &mut before) {
                Flow::Clear => walk_all(&mut assign.right, last, visit, &mut before),
                flow => flow,
            }
        }
        Statement::Call(call) => walk_call(&mut call.value, &mut call.arguments, visit, &mut before),
        Statement::MethodCall(call) => walk_method(&mut call.value, &mut call.arguments, visit, &mut before),
        Statement::Return(ret) => walk_all(&mut ret.values, Spread::Values, visit, &mut before),
        Statement::If(branch) => walk(&mut branch.condition, Spread::One, visit, &mut before),
        _ => Flow::Blocked,
    };
    flow == Flow::Taken
}

/// The kind of a value, a call the same whether it gives one result
/// (`x = f()`) or all of them (`return f()`): `Spread` tells them apart.
#[derive(PartialEq)]
enum ValueKind {
    Call,
    MethodCall,
    Other(std::mem::Discriminant<RValue>),
}

fn value_kind(value: &RValue) -> ValueKind {
    match value {
        RValue::Call(_) | RValue::Select(Select::Call(_)) => ValueKind::Call,
        RValue::MethodCall(_) | RValue::Select(Select::MethodCall(_)) => ValueKind::MethodCall,
        _ => ValueKind::Other(std::mem::discriminant(value)),
    }
}

/// A value giving all the results of a call or of `...` where they are
/// taken.
fn is_multiple(value: &RValue) -> bool {
    matches!(value, RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_))
}

/// How many results a statement takes from a value ([`visit_leading_values`]).
#[derive(Clone, Copy, PartialEq)]
enum Spread {
    /// One: an operand, a condition, or a value before the last of a list.
    One,
    /// All, as the last argument of a call or value of a `return`, where
    /// `(f())` keeps only the first.
    Values,
    /// As many as a store to more targets than values still needs, the last
    /// value being a call (`local a, b = f()`, also written as a fixed-result
    /// select).
    Store,
}

impl Spread {
    /// Whether `value` gives more than one result here.
    fn takes_all(self, value: &RValue) -> bool {
        match self {
            Spread::One => false,
            Spread::Values => is_multiple(value),
            Spread::Store => is_multiple(value) || matches!(value, RValue::Select(Select::Call(_) | Select::MethodCall(_) | Select::VarArg(_))),
        }
    }
}

/// A read a statement performs before one of its values is evaluated: a
/// local, literal, global or import path.
enum Earlier {
    Value(RValue),
}

/// Whether `stmts` may run code other than their own: a call, a store to a
/// global or a field, or an operation a metamethod can take over.
fn may_run_code(stmts: &[Statement]) -> bool {
    stmts.iter().any(|statement| match statement {
        Statement::Assign(assign) => {
            assign.left.iter().any(|left| !matches!(left, LValue::Local(_)))
                || assign.right.iter().any(|value| {
                    !crate::effects::summarize(value, &|_| false).effects.is_total_pure()
                })
        }
        Statement::Comment(_) | Statement::Empty(_) => false,
        _ => true,
    })
}

fn is_import_path(value: &RValue) -> bool {
    match value {
        RValue::Global(_) => true,
        RValue::Index(index) => matches!(*index.right, RValue::Literal(Literal::String(_))) && is_import_path(&index.left),
        _ => false,
    }
}

/// §8: a value-returning callee with a leading non-branch statement (its own
/// local, computed before the value is produced) inlines at the call site as
/// `<prefix> ; local RESULT ; <value branch>` — the RESULT-register decl is
/// INTERPOSED *after* the callee-prefix statement, not at the window start
/// (`match_value`'s assumption). This sibling matches that shape, scoped to
/// exactly one prefix statement (`t.prefix_len == 1`, set in `collect_targets`).
///
/// `stmts[i]` is the callee-prefix statement; `stmts[i+1]` is the init-less
/// `local RESULT`; `stmts[i+2..]` is the value region. We unify the canon'd
/// pattern against the UNION `prefix ++ region` (the RESULT decl spliced out): the
/// prefix statement binds as an ordinary callee-local via the existing injective
/// map, exactly as if it were at the window start. Crucially every whole-window
/// analysis (arg hoist-safety + region writes inside `try_unify_site`, the RESULT
/// read-check, callee-temp liveness) runs over the UNION, so a prefix statement
/// cannot smuggle in an unsafe reorder or hide a RESULT read.
fn match_value_prefixed(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    current_func: Option<FnPtr>,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
) -> Option<Hit> {
    let p = t.prefix_len; // effective callee-prefix statement count (>= 1)
    // P1: the interposed init-less `local RESULT` decl is the p-th EFFECTIVE
    // statement at/after i — `i + p` (the old fixed offset) would land on a
    // CALL_MARKER/`Empty` an inner de-inline spliced between the prefix and the
    // decl, making `result_decl` bail and silently killing chained AtPrefix
    // reconstruction. Count only non-trivia statements instead.
    let d = nth_effective_index(stmts, i, p)?;
    let Some(r) = result_decl(&stmts[d]) else {
        return match_declared_value(stmts, i, d, t, is_func_body_top, last_occ, current_func)
            .or_else(|| match_embedded_value(stmts, i, d, t, current_func, is_func_body_top, last_occ))
            .or_else(|| match_returned_local(stmts, i, d, t, current_func, last_occ));
    };
    let kc = t.pat.len();
    let region_start = d + 1;
    // F2: effective-count ceiling (interposed trivia don't consume the budget).
    let max_w = raw_width_for_effective(stmts, region_start, t.pat_raw_len + 1);
    // The prefix (the callee's leading statement) is loop-invariant — neither it
    // nor its return-freeness depends on `w` — so resolve it once up front. A
    // return in the prefix means a different shape (the value branch must be the
    // last, sole returning statement), so bail before the per-width scan.
    let prefix = &stmts[i..d];
    if block_has_return(prefix) {
        return None;
    }
    let mut site: Option<Site> = None;
    let mut ambiguous = false;
    for w in kc.saturating_sub(p)..=max_w {
        if w == 0 {
            continue;
        }
        // whole-body gate (shifted bounds): refuse if the window covers the entire
        // top-level function body — the thin-wrapper / mutual-clone hazard.
        if is_func_body_top && i == 0 && region_start + w == stmts.len() {
            continue;
        }
        let region = &stmts[region_start..region_start + w];
        // Cheapest reject first: the non-allocating top-level canon length of
        // the UNION window (prefix + region, RESULT decl spliced out) rejects
        // most widths before the return scan and the deep window copy.
        if canon_top_len_of(prefix.iter().chain(region), true) != kc {
            continue;
        }
        // the value region must not contain a return: it assigns RESULT on every
        // path; a return would be a different shape.
        if block_has_return(region) {
            continue;
        }
        let mut union: Vec<Statement> = Vec::with_capacity(p + w);
        union.extend_from_slice(prefix);
        union.extend_from_slice(region);
        if !charge_window(t, &union) {
            break;
        }
        let cwin = {
            dprof::inc(&dprof::CANON_RECURSE_CALLS, 1);
            crate::telemetry::count("canonicalize_calls", 1);
            let _t = dprof::T::new(&dprof::CANON_RECURSE_US);
            canon_recurse(canon_top(&union, true), true)
        };
        if let Some(u) = try_unify_site_any(t, &cwin, &[], current_func) {
            // RESULT must be exactly the interposed decl, written-only inside the
            // union (its full computation), NOT also a callee-prefix binder (the
            // getOwnerId reassignment-collision class), and every OTHER callee temp
            // — including the consumed prefix local — must be dead after.
            if u.result.as_ref() == Some(&r)
                && !u.callee_locals.contains(&r)
                && !block_reads_local(prefix, &r)
                && !block_reads_local(region, &r)
                && !tail_has_live(last_occ, stmts, i, region_start + w, &u.callee_locals)
            {
                record_site(&mut site, &mut ambiguous, w, &u, None);
            }
        }
    }
    if ambiguous {
        return None;
    }
    let (w, args, _, _, inferred) = site?;
    Some(Hit {
        f_local: t.f_local.clone(),
        // Absolute span from i: prefix + any interposed trivia + the RESULT decl
        // (at d) + the w-statement region. `(d - i)` counts the prefix and trivia
        // so the splice removes the interposed marker along with the window.
        consume: (d - i) + 1 + w,
        args,
        results: vec![r],
        tail_ret: None,
        host: None,
        inferred,
    })
}

/// Result-alias form of a value site (Shape A). The callee's value leaf
/// `local L = E; S(L)…; return L` is lowered at the site with the RESULT register
/// standing in for `L`: `RESULT = E; S(RESULT)…` — the result is written EARLY and
/// then read by the rest of the leaf, so it is not the `RESULT = X` terminal write
/// `match_value` expects. Rewrite each such leaf of `region` (along the tail spine
/// of `if`s whose arms write RESULT) to `local T = E; S(T)…; RESULT = T` with a
/// fresh `T`, which is observably the same (RESULT is written exactly once in the
/// leaf, never read before that write, never read from a closure, and the leaf
/// cannot leave early: no `return` — the caller checks — and no depth-zero
/// `break`/`continue`) and is exactly the shape the pattern unifies against. Leaves
/// already ending in a RESULT write are left alone. `None` when nothing changed.
fn alias_result_leaves(region: &[Statement], r: &RcLocal) -> Option<Vec<Statement>> {
    let mut out = region.to_vec();
    let mut changed = false;
    alias_leaf_block(&mut out, r, &mut changed);
    changed.then_some(out)
}

fn is_plain_local_write(a: &Assign, r: &RcLocal) -> bool {
    !a.prefix
        && !a.parallel
        && a.left.len() == 1
        && a.right.len() == 1
        && matches!(&a.left[0], LValue::Local(x) if x == r)
}

fn alias_leaf_block(stmts: &mut Vec<Statement>, r: &RcLocal, changed: &mut bool) {
    let Some(last) = stmts.iter().rposition(|s| !is_match_trivia(s)) else {
        return;
    };
    // A tail `if` whose arms write RESULT: the leaves are inside its arms. Rebuild
    // the `if` with fresh blocks (the region is a clone sharing the original's
    // block Arcs, which must not be mutated).
    if let Statement::If(f) = &stmts[last] {
        let mut then = f.then_block.lock().0.clone();
        let mut els = f.else_block.lock().0.clone();
        let mut w: FxHashSet<RcLocal> = FxHashSet::default();
        collect_written(&then, &mut w);
        collect_written(&els, &mut w);
        if w.contains(r) {
            alias_leaf_block(&mut then, r, changed);
            alias_leaf_block(&mut els, r, changed);
            let cond = f.condition.clone();
            stmts[last] = Statement::If(If::new(cond, Block(then), Block(els)));
            return;
        }
    }
    if matches!(&stmts[last], Statement::Assign(a) if is_plain_local_write(a, r))
        || matches!(
            &stmts[last],
            Statement::Return(_) | Statement::Break(_) | Statement::Continue(_)
        )
    {
        return;
    }
    let writes: Vec<usize> = stmts
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, Statement::Assign(a) if is_plain_local_write(a, r)))
        .map(|(j, _)| j)
        .collect();
    if writes.len() != 1 {
        return;
    }
    let m = writes[0];
    for (j, s) in stmts.iter().enumerate() {
        if j == m {
            continue;
        }
        let mut w: FxHashSet<RcLocal> = FxHashSet::default();
        collect_written(std::slice::from_ref(s), &mut w);
        if w.contains(r) {
            return;
        }
    }
    if block_reads_local(&stmts[..m], r)
        || has_depth_zero_loop_control(&stmts[m + 1..], 0)
        || stmts[m + 1..]
            .iter()
            .any(|s| stmt_rvalues(s).into_iter().any(|rv| rvalue_closure_reads(rv, r) > 0))
    {
        return;
    }
    let t = RcLocal::default();
    let Statement::Assign(a) = &mut stmts[m] else {
        unreachable!()
    };
    a.left[0] = LValue::Local(t.clone());
    a.prefix = true;
    for s in stmts.drain(m + 1..).collect::<Vec<_>>() {
        let s = subst_reads_owned(s, r, &t);
        stmts.push(s);
    }
    stmts.push(Statement::Assign(Assign {
        node_origin: Default::default(),
        left: vec![LValue::Local(r.clone())],
        right: vec![RValue::Local(t)],
        prefix: false,
        parallel: false,
        compound: false,
    }));
    *changed = true;
}

/// `s` with every (non-closure) read of `old` replaced by `new`, nested blocks
/// included. Block-bearing statements are REBUILT with fresh blocks so a shared
/// original block Arc is never mutated (mirrors `canon_children_owned`).
fn subst_reads_owned(s: Statement, old: &RcLocal, new: &RcLocal) -> Statement {
    let sub_block = |b: &Arc<Mutex<Block>>| -> Block {
        Block(
            b.lock()
                .0
                .iter()
                .cloned()
                .map(|st| subst_reads_owned(st, old, new))
                .collect(),
        )
    };
    match s {
        Statement::If(mut f) => {
            f.condition.replace_values_read(old, new);
            let then = sub_block(&f.then_block);
            let els = sub_block(&f.else_block);
            Statement::If(If::new(f.condition, then, els))
        }
        Statement::While(mut w) => {
            w.condition.replace_values_read(old, new);
            let block = sub_block(&w.block);
            Statement::While(While::new(w.condition, block))
        }
        Statement::Repeat(mut rp) => {
            rp.condition.replace_values_read(old, new);
            let block = sub_block(&rp.block);
            Statement::Repeat(Repeat::new(rp.condition, block))
        }
        Statement::NumericFor(mut nf) => {
            nf.initial.replace_values_read(old, new);
            nf.limit.replace_values_read(old, new);
            nf.step.replace_values_read(old, new);
            let block = sub_block(&nf.block);
            Statement::NumericFor(Box::new(NumericFor {
                block: Arc::new(Mutex::new(block)),
                initial: nf.initial,
                limit: nf.limit,
                step: nf.step,
                counter: nf.counter,
            }))
        }
        Statement::GenericFor(mut gf) => {
            for rv in &mut gf.right {
                rv.replace_values_read(old, new);
            }
            let block = sub_block(&gf.block);
            Statement::GenericFor(GenericFor {
                res_locals: gf.res_locals,
                right: gf.right,
                block: Arc::new(Mutex::new(block)),
                origin: gf.origin,
            })
        }
        mut other => {
            other.replace_values_read(old, new);
            other
        }
    }
}

struct Unified {
    args: Vec<RValue>,
    result: Option<RcLocal>,
    /// The caller-side locals the callee's own (non-result) locals mapped onto —
    /// the region's internal temps. They cease to exist once the region becomes a
    /// call, so the site is only valid if none is live afterwards.
    callee_locals: FxHashSet<RcLocal>,
    /// The caller locals the helper's returned locals (`Target::returns`)
    /// mapped onto, in return order: the call's results.
    returned: Vec<RcLocal>,
    /// The parameter a constant argument was inferred for
    /// ([`try_inferred_constant`]).
    inferred: Option<RcLocal>,
}

/// The leading `local L = ARG` copies a written-param site starts with (see
/// `Target::written_params`), as `(L, ARG)` pairs in source order. Empty for every
/// target without written params.
type Prefix = [(RcLocal, RValue)];

fn try_unify_site(t: &Target, cwin: &[Statement], prefix: &Prefix, current_func: Option<FnPtr>) -> Option<Unified> {
    dprof::inc(&dprof::UNIFY_CALLS, 1);
    crate::telemetry::count("unify_calls", 1);
    let _t = dprof::T::new(&dprof::UNIFY_US);
    let mut b = Bindings::default();
    if unify_block(t, &t.pat, cwin, &mut b).is_err() {
        return None;
    }
    finish_unified(t, cwin, b, prefix, current_func)
}

fn finish_unified(
    t: &Target,
    cwin: &[Statement],
    b: Bindings,
    prefix: &Prefix,
    // The function the site is in: its registers read alike across calls.
    current_func: Option<FnPtr>,
) -> Option<Unified> {
    let mut args = Vec::with_capacity(t.param_order.len());
    // Every consumed prefix copy must feed exactly one written param — an unbound
    // copy would be a caller declaration the splice silently deletes.
    let mut prefix_used = 0usize;
    for p in &t.param_order {
        if t.written_params.contains(p) {
            // Written param: matched as a callee local bound to the site's copy `L`;
            // the argument is that copy's initialiser. Evaluated at the copy's own
            // position (= the call position), so any initialiser is order-safe; a
            // closure is still refused (identity). Never `nil`-supplied.
            let l = b.locals.get(p)?;
            let (_, arg) = prefix.iter().find(|(x, _)| x == l)?;
            if matches!(arg, RValue::Closure(_)) {
                return None;
            }
            prefix_used += 1;
            args.push(arg.clone());
            continue;
        }
        match b.params.get(p) {
            Some(e) => args.push(e.clone()),
            // A parameter never bound during unification. If the body NEVER reads it
            // (F6a), the region legitimately matched the body minus that param: on a
            // non-variadic helper (`is_variadic` is refused in `collect_targets`) an
            // unread param is unobservable, so supply `nil` — exactly what Luau passes
            // for a missing/dropped argument. A read param that failed to bind is a
            // genuine mismatch and still refuses the whole site. (A write-only param
            // never reaches here: it fails `unify_local`'s identity branch first.)
            None if t.unread.contains(p) => args.push(RValue::Literal(Literal::Nil)),
            None => return None,
        }
    }
    // Trim trailing `nil`s we supplied for unread params: `f(a, nil)` ≡ `f(a)` for a
    // non-variadic helper, and the shorter form reads better. Only OUR inserted nils
    // are trimmed (the position's param is in `unread`); a real trailing `nil` the
    // caller passed to a READ param is kept (its param is not in `unread`).
    while args
        .last()
        .is_some_and(|a| matches!(a, RValue::Literal(Literal::Nil)))
        && t.unread.contains(&t.param_order[args.len() - 1])
    {
        args.pop();
    }
    // Argument hoist-safety. Turning the region back into `f(args)` evaluates
    // every argument at the call site (before the body), in parameter order.
    // That is sound only when each argument can be moved to the front without
    // changing observable behaviour:
    //   * total and effect-free — Luau operators can invoke metamethods or throw,
    //     even though the generic `SideEffects` trait only propagates operand
    //     effects, so admit only atomic Local/Literal snapshots;
    //   * value stability — it must read no local the region writes, so its value
    //     can't change between the front and its in-body use point.
    // A genuine inlined arg with side effects survives in the copy as a `local`
    // temp (the per-function inliner won't hoist it past an effect), which binds
    // here as a side-effect-free `Local` and is accepted. Everything else: REFUSE.
    //
    // Callback/metamethod writes are not syntactic region writes. A module
    // census protects reference-captured cells independently of upstream SSA
    // cleanup, so the proof holds even when matching a handwritten shape.
    if prefix_used != prefix.len() {
        return None;
    }
    if t.free_cells.iter().any(|local| {
        t.captures.register_of(local, current_func.map(|function| function as usize))
            && crate::evaluation_order::region_late_read_conflict(cwin, local, &t.captures.may_change(local))
    }) {
        return None;
    }
    let mut region_writes: FxHashSet<RcLocal> = FxHashSet::default();
    collect_written(cwin, &mut region_writes);
    // Written-parameter copies may have effects. Their original order must
    // match parameter order, and no argument may refer to a copy we remove.
    // One other argument may run code when the body reads its parameter first
    // (`Target::first_reads`): it ran right before the inlined body, after
    // every copy, so no written parameter may follow it.
    let mut prefix_index = 0;
    let mut moved = false;
    for (idx, a) in args.iter().enumerate() {
        if a.values_read().iter().any(|read| prefix.iter().any(|(l, _)| l == *read)) {
            return None;
        }
        if t.written_params.contains(&t.param_order[idx]) {
            if moved {
                return None;
            }
            let bound = b.locals.get(&t.param_order[idx])?;
            if prefix.get(prefix_index).map(|(l, _)| l) != Some(bound) { return None; }
            prefix_index += 1;
            // A prefix-copy initialiser is not hoisted (see above); it only must not
            // read a local the region writes — the site evaluated it before the
            // region, and so does `f(args)`.
            for r in a.values_read() {
                if region_writes.contains(r) {
                    return None;
                }
            }
            continue;
        }
        if !t.captures.stable_at(a, current_func.map(|function| function as usize)) {
            let first = if matches!(a, RValue::Local(_)) { &t.first_register_reads } else { &t.first_reads };
            if moved || !first.contains(&t.param_order[idx]) {
                return None;
            }
            moved = true;
        }
        for r in a.values_read() {
            if region_writes.contains(r) {
                return None;
            }
        }
    }
    // Each returned local is declared by the pattern, so the site's matching
    // declaration bound it. The caller's local outlives the region.
    let returned = t.returns.iter().map(|l| b.locals.get(l).cloned()).collect::<Option<Vec<_>>>()?;
    let mut callee_locals: FxHashSet<RcLocal> = b.locals.into_values().collect();
    for l in &returned {
        callee_locals.remove(l);
    }
    Some(Unified {
        // Every target is non-variadic, so a trailing call's extra results
        // fill no parameter the body reads: `helper(f())` needs no `(f())`,
        // whether the site or a written parameter's copy adjusted it.
        args: args.into_iter().map(crate::untruncated).collect(),
        result: b.result,
        callee_locals,
        returned,
        inferred: None,
    })
}

/// Exact Tier-A match first; when a parameter controls a branch, fall back to a
/// verified partial evaluation.  The fallback never trusts the partial match:
/// it only uses it to seed arguments, specializes a deep copy of the recovered
/// definition, then requires a full structural unification against the site.
fn try_unify_site_any(t: &Target, cwin: &[Statement], prefix: &Prefix, current_func: Option<FnPtr>) -> Option<Unified> {
    if !charge_unify(t, cwin) {
        return None;
    }
    try_unify_site(t, cwin, prefix, current_func).or_else(|| try_unify_specialized_site(t, cwin, prefix, current_func))
}

/// Fuel for unifying one candidate window: the pattern's node count, the most
/// a lockstep unification walks. Building the window is charged once, where
/// it is built (`canon_window`), however many targets then compare against it.
fn charge_unify(t: &Target, _cwin: &[Statement]) -> bool {
    t.search.spend(t.pat_nodes)
}

/// Whether `window` can canonicalize to statements of `t.pat`'s kinds, checked
/// before building the canonical copy. Canon keeps each top-level kind except
/// that an `if` may fuse into an assignment or a return (N4/N5), and a value
/// pattern's `return` stands for the site's RESULT assignment. A specializable
/// target changes shape under partial evaluation and a written-param target
/// matches after a consumed prefix, so both skip the gate.
fn plain_kinds_may_match(t: &Target, window: &[Statement]) -> bool {
    if t.specializable || !t.written_params.is_empty() {
        return true;
    }
    let mut real = window.iter().filter(|s| !is_match_trivia(s));
    t.pat.iter().all(|pattern| {
        real.next().is_some_and(|site| {
            std::mem::discriminant(site) == std::mem::discriminant(pattern)
                || matches!(
                    (pattern, site),
                    (Statement::Assign(_) | Statement::Return(_), Statement::If(_))
                        | (Statement::Return(_), Statement::Assign(_))
                )
        })
    })
}

/// Fuel for building a canonical window: its node count. Once the fuel is
/// gone, refuses without counting.
fn charge_window(t: &Target, window: &[Statement]) -> bool {
    !t.search.exhausted() && t.search.spend(window.iter().map(dbg_stmt_node_count).sum())
}

fn try_unify_cps_site(
    t: &Target,
    window: &[Statement],
    cwin: &[Statement],
    continuation: &[Statement],
    prefix: &Prefix,
    current_func: Option<FnPtr>,
) -> Option<Unified> {
    if !t.cps_loop_return
        || continuation.is_empty()
        || !sequence_has_return_tail(continuation)
        || has_depth_zero_loop_control(continuation, 0)
        || !charge_unify(t, cwin)
    {
        return None;
    }
    let continuation = canon(continuation);
    if tail_copy_falls_through(window, &continuation) {
        return None;
    }
    let mut bindings = Bindings::default();
    if !cps_unify_block(t, &t.pat, cwin, &continuation, true, &mut bindings) {
        return None;
    }
    finish_unified(t, cwin, bindings, prefix, current_func)
}

fn cps_unify_block(
    t: &Target,
    pattern: &[Statement],
    candidate: &[Statement],
    continuation: &[Statement],
    allow_fallthrough_continuation: bool,
    bindings: &mut Bindings,
) -> bool {
    if candidate.len() < pattern.len() {
        return false;
    }

    // The inliner copies the caller continuation not only at an early-return
    // edge, but also after the callee's ordinary fallthrough.  Consequently a
    // structured branch can be `CALLEE_PREFIX; CONTINUATION` while the recovered
    // helper branch is just `CALLEE_PREFIX`.  Match the helper prefix first, then
    // accept only an empty suffix (fall through to the continuation outside the
    // current structured block) or a fully verified cloned continuation.
    let mut trial = bindings.clone();
    for (index, (left, right)) in pattern.iter().zip(&candidate[..pattern.len()]).enumerate() {
        let statement_reaches_callee_end =
            allow_fallthrough_continuation && index + 1 == pattern.len();
        if !cps_unify_stmt(
            t,
            left,
            right,
            continuation,
            statement_reaches_callee_end,
            &mut trial,
        ) {
            return false;
        }
    }

    let suffix = &candidate[pattern.len()..];
    if !suffix.is_empty() {
        if !allow_fallthrough_continuation {
            return false;
        }
        let suffix = canon(suffix);
        if !crate::factor_common_tails::block_alpha_eq(continuation, &suffix) {
            return false;
        }
    }
    *bindings = trial;
    true
}

fn cps_unify_stmt(
    t: &Target,
    pattern: &Statement,
    candidate: &Statement,
    continuation: &[Statement],
    reaches_callee_end: bool,
    bindings: &mut Bindings,
) -> bool {
    let ctx = t.ctx();
    match (pattern, candidate) {
        (Statement::If(left), Statement::If(right)) => {
            unify_rvalue(&ctx, &left.condition, &right.condition, bindings).is_ok()
                && cps_unify_block(
                    t,
                    &left.then_block.lock().0,
                    &right.then_block.lock().0,
                    continuation,
                    reaches_callee_end,
                    bindings,
                )
                && cps_unify_block(
                    t,
                    &left.else_block.lock().0,
                    &right.else_block.lock().0,
                    continuation,
                    reaches_callee_end,
                    bindings,
                )
        }
        (Statement::GenericFor(left), Statement::GenericFor(right)) => {
            left.res_locals.len() == right.res_locals.len()
                && left.right.len() == right.right.len()
                && left
                    .res_locals
                    .iter()
                    .zip(&right.res_locals)
                    .all(|(pl, cl)| unify_local(&ctx, pl, cl, bindings).is_ok())
                && left
                    .right
                    .iter()
                    .zip(&right.right)
                    .all(|(pl, cl)| unify_rvalue(&ctx, pl, cl, bindings).is_ok())
                && cps_unify_loop_body(
                    t,
                    &left.block.lock().0,
                    &right.block.lock().0,
                    continuation,
                    bindings,
                )
        }
        (Statement::NumericFor(left), Statement::NumericFor(right)) => {
            unify_rvalue(&ctx, &left.initial, &right.initial, bindings).is_ok()
                && unify_rvalue(&ctx, &left.limit, &right.limit, bindings).is_ok()
                && unify_rvalue(&ctx, &left.step, &right.step, bindings).is_ok()
                && unify_local(&ctx, &left.counter, &right.counter, bindings).is_ok()
                && cps_unify_loop_body(
                    t,
                    &left.block.lock().0,
                    &right.block.lock().0,
                    continuation,
                    bindings,
                )
        }
        (Statement::While(left), Statement::While(right)) => {
            unify_rvalue(&ctx, &left.condition, &right.condition, bindings).is_ok()
                && cps_unify_loop_body(
                    t,
                    &left.block.lock().0,
                    &right.block.lock().0,
                    continuation,
                    bindings,
                )
        }
        (Statement::Repeat(left), Statement::Repeat(right)) => {
            unify_rvalue(&ctx, &left.condition, &right.condition, bindings).is_ok()
                && cps_unify_loop_body(
                    t,
                    &left.block.lock().0,
                    &right.block.lock().0,
                    continuation,
                    bindings,
                )
        }
        (Statement::Return(_), _) => false,
        _ => unify_stmt(t, pattern, candidate, bindings).is_ok(),
    }
}

fn cps_unify_loop_body(
    t: &Target,
    pattern: &[Statement],
    candidate: &[Statement],
    continuation: &[Statement],
    bindings: &mut Bindings,
) -> bool {
    // An exact `return` here would exit the caller and skip K; after refolding it
    // exits only the helper and the caller executes K. Every loop-return edge must
    // therefore go through the continuation-proving rule below.
    if !block_has_return(pattern) {
        let mut exact = bindings.clone();
        if cps_unify_block(t, pattern, candidate, continuation, false, &mut exact) {
            *bindings = exact;
            return true;
        }
    }
    cps_unify_loop_exit(t, pattern, candidate, continuation, bindings)
}

/// Match the canonical lowering of `if C then return end` inside an inlined
/// loop.  The caller cannot jump out of the helper directly, so the structurer
/// emits `if not C then continue end; CONTINUATION`.  We accept it only when the
/// copied suffix is alpha-equivalent to the actual parent continuation.
fn cps_unify_loop_exit(
    t: &Target,
    pattern: &[Statement],
    candidate: &[Statement],
    continuation: &[Statement],
    bindings: &mut Bindings,
) -> bool {
    let [Statement::If(pattern_guard)] = pattern else {
        return false;
    };
    let Some((Statement::If(candidate_guard), copied_continuation)) = candidate.split_first()
    else {
        return false;
    };
    let pattern_then = pattern_guard.then_block.lock();
    let pattern_else = pattern_guard.else_block.lock();
    let candidate_then = candidate_guard.then_block.lock();
    let candidate_else = candidate_guard.else_block.lock();
    if !pattern_else.0.is_empty()
        || !matches!(pattern_then.0.as_slice(), [Statement::Return(ret)] if ret.values.is_empty())
        || !candidate_else.0.is_empty()
    {
        return false;
    }

    let ctx = t.ctx();
    // Before `recover_guard_continue`, the structurer represents the callee's
    // loop return directly as `if C then CONTINUATION end`.  This is the shape
    // present when de-inline runs, and the copied branch must equal K in full.
    // A copy inside the loop must leave the caller as the continuation does,
    // checked before `canon` drops its final `return`: without one it would
    // keep looping (`if C then K end` is not `if C then K; return end`).
    if copied_continuation.is_empty() && sequence_has_return_tail(&candidate_then.0) {
        let candidate_then = canon(&candidate_then.0);
        let continuation_equal =
            crate::factor_common_tails::block_alpha_eq(continuation, &candidate_then);
        if continuation_equal {
            let mut trial = bindings.clone();
            if unify_rvalue(
                &ctx,
                &pattern_guard.condition,
                &candidate_guard.condition,
                &mut trial,
            )
            .is_ok()
            {
                *bindings = trial;
                return true;
            }
        }
    }

    // Also recognize the normalized form produced later by
    // `recover_guard_continue`: `if not C then continue end; CONTINUATION`.
    if !matches!(candidate_then.0.as_slice(), [Statement::Continue(_)]) {
        return false;
    }
    let mut trial = bindings.clone();
    if unify_rvalue(
        &ctx,
        &pattern_guard.condition,
        &negate_canon(candidate_guard.condition.clone()),
        &mut trial,
    )
    .is_err()
    {
        return false;
    }
    if !sequence_has_return_tail(copied_continuation) {
        return false;
    }
    let copied = canon(copied_continuation);
    let equal = crate::factor_common_tails::block_alpha_eq(&continuation, &copied);
    if equal {
        *bindings = trial;
    }
    equal
}

fn try_unify_specialized_site(t: &Target, cwin: &[Statement], prefix: &Prefix, current_func: Option<FnPtr>) -> Option<Unified> {
    try_seeded_specialization(t, cwin, prefix, current_func).or_else(|| try_inferred_constant(t, cwin, prefix, current_func))
}

fn try_seeded_specialization(t: &Target, cwin: &[Statement], prefix: &Prefix, current_func: Option<FnPtr>) -> Option<Unified> {
    if !t.specializable || t.params.is_empty() {
        return None;
    }

    let mut seed = Bindings::default();
    seed_unify_block(t, &t.pat, cwin, &mut seed);
    if !seed
        .params
        .values()
        .any(|value| matches!(value, RValue::Literal(_)))
    {
        return None;
    }
    if t.param_order
        .iter()
        .any(|param| !seed.params.contains_key(param) && !t.unread.contains(param))
    {
        return None;
    }
    // Seeding intentionally tolerates structural mismatches, but substituting a
    // repeated identity-producing argument (`{}` / closure-valued if-expression)
    // would erase the exact unifier's per-occurrence identity guard: two separate
    // constructors in the candidate could collapse to one `f({})` argument.
    if seed
        .params
        .iter()
        .any(|(param, value)| is_identity_producing(value) && count_local_reads(&t.pat, param) > 1)
    {
        return None;
    }

    // `canon` deep-copies every block-bearing statement, avoiding mutation of
    // the recovered function body's shared Arcs while we prune constant arms.
    let mut specialized = canon(&t.pat);
    specialize_block(&mut specialized, &seed.params);
    let specialized = canon(&specialized);

    let mut verified = Bindings {
        params: seed.params,
        ..Bindings::default()
    };
    if unify_block(t, &specialized, cwin, &mut verified).is_err() {
        return None;
    }
    finish_unified(t, cwin, verified, prefix, current_func)
}

/// The most truth-tested parameters a constant is inferred for.
const MAX_TRUTH_PARAMS: usize = 2;

/// A constant argument whose every read Luau folded (`visible and 0 or 1`
/// with `false` is `1`) leaves nothing at the site to bind. Its truth is what
/// the folding read, so `true` or a false value stands for it, whichever
/// specializes the body into exactly the copy, the other parameters bound by
/// the exact unification; where a true and a false value both do, the copy
/// says nothing. A false value is `nil` (left out when last) for a parameter
/// whose value is read too (`parent or root`), as for an argument left out,
/// else `false` (a flag); the other one where that fails (`p and x` keeps
/// it). One parameter per call, from targets with at most
/// [`MAX_TRUTH_PARAMS`]; the specializations are built once per target.
fn try_inferred_constant(t: &Target, cwin: &[Statement], prefix: &Prefix, current_func: Option<FnPtr>) -> Option<Unified> {
    if t.truth_params.is_empty() || t.truth_params.len() > MAX_TRUTH_PARAMS {
        return None;
    }
    for (index, param) in t.truth_params.iter().enumerate() {
        let optional = t.optional_params.contains(param);
        let truthy = try_truth(t, cwin, prefix, current_func, index, InferredTruth::True);
        let preferred = if optional { InferredTruth::Nil } else { InferredTruth::False };
        let other = if optional { InferredTruth::False } else { InferredTruth::Nil };
        let falsy = try_truth(t, cwin, prefix, current_func, index, preferred)
            .or_else(|| try_truth(t, cwin, prefix, current_func, index, other));
        match (truthy, falsy) {
            (Some(_), Some(_)) => return None,
            (Some(unified), None) | (None, Some(unified)) => return Some(unified),
            (None, None) => {}
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum InferredTruth {
    True,
    False,
    Nil,
}

impl InferredTruth {
    fn literal(self) -> Literal {
        match self {
            InferredTruth::True => Literal::Boolean(true),
            InferredTruth::False => Literal::Boolean(false),
            InferredTruth::Nil => Literal::Nil,
        }
    }
}

/// `t`'s body with truth parameter `index` given `truth`, unified exactly
/// against `cwin`. Nothing of the site vouched for that constant: the body
/// it leaves must still pass the collection floor (`anchor_score`, arguments
/// not counted), or a helper doing nothing for it would match anywhere.
fn try_truth(
    t: &Target,
    cwin: &[Statement],
    prefix: &Prefix,
    current_func: Option<FnPtr>,
    index: usize,
    truth: InferredTruth,
) -> Option<Unified> {
    let param = &t.truth_params[index];
    let specialized = t
        .specializations
        .borrow_mut()
        .entry((index, truth))
        .or_insert_with(|| {
            // `canon` deep-copies every block-bearing statement, avoiding
            // mutation of the recovered function body's shared Arcs.
            let mut specialized = canon(&t.pat);
            let bindings = FxHashMap::from_iter([(param.clone(), RValue::Literal(truth.literal()))]);
            specialize_block(&mut specialized, &bindings);
            let specialized = canon(&specialized);
            (!specialized.is_empty() && anchor_score(&specialized, &t.param_order) >= 2)
                .then(|| std::rc::Rc::new(specialized))
        })
        .clone()?;
    if specialized.len() != cwin.len() || !charge_unify(t, cwin) {
        return None;
    }
    let mut bindings = Bindings::default();
    unify_block(t, &specialized, cwin, &mut bindings).ok()?;
    bindings.params.insert(param.clone(), RValue::Literal(truth.literal()));
    let mut unified = finish_unified(t, cwin, bindings, prefix, current_func)?;
    unified.inferred = Some(param.clone());
    // A `nil` supplied last is an argument left out.
    if truth == InferredTruth::Nil
        && matches!(unified.args.last(), Some(RValue::Literal(Literal::Nil)))
        && t.param_order.get(unified.args.len() - 1) == Some(param)
    {
        unified.args.pop();
    }
    Some(unified)
}

/// The statement after a window matched with a constant inferred for `param`
/// is one that constant removed from the body, with another condition:
/// `if thread then … end; if now then thread = task.defer(…) end` for a
/// helper `startFlipbook(time)` ending `if time then thread = task.defer(…)
/// end`. The copy goes on past the window; it is a call with another
/// argument that the exact match missed, not a call with the constant.
fn continues_pruned_branch(t: &Target, param: &RcLocal, stmts: &[Statement], end: usize) -> bool {
    fn same_shape(a: &[Statement], b: &[Statement]) -> bool {
        let mut a = a.iter().filter(|s| !is_match_trivia(s));
        let mut b = b.iter().filter(|s| !is_match_trivia(s));
        loop {
            match (a.next(), b.next()) {
                (None, None) => return true,
                (Some(x), Some(y)) if std::mem::discriminant(x) == std::mem::discriminant(y) => {}
                _ => return false,
            }
        }
    }
    let Some(Statement::If(site)) = (end..stmts.len()).find(|&k| !is_match_trivia(&stmts[k])).map(|k| &stmts[k]) else {
        return false;
    };
    t.pat.iter().any(|statement| {
        matches!(statement, Statement::If(pruned)
            if reads_local(&pruned.condition, param)
                && same_shape(&pruned.then_block.lock().0, &site.then_block.lock().0)
                && same_shape(&pruned.else_block.lock().0, &site.else_block.lock().0))
    })
}

/// Parameters `pattern` reads where only their truth matters: a branch or
/// `if`-expression condition, an `and`/`or` left operand, `not`'s operand.
/// Luau's constant folding removes such a read of a constant argument.
/// Also the parameters whose value is read: the left of an `or`, or any
/// other read. In `parameters` order.
fn truth_tested_params(
    pattern: &[Statement],
    params: &FxHashSet<RcLocal>,
    parameters: &[RcLocal],
) -> (Vec<RcLocal>, Vec<RcLocal>) {
    struct Found {
        truth: FxHashSet<RcLocal>,
        valued: FxHashSet<RcLocal>,
    }
    fn truth(value: &RValue, params: &FxHashSet<RcLocal>, found: &mut Found) {
        match value {
            RValue::Local(local) if params.contains(local) => {
                found.truth.insert(local.clone());
            }
            RValue::Unary(unary) if unary.operation == UnaryOperation::Not => truth(&unary.value, params, found),
            RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
                truth(&binary.left, params, found);
                truth(&binary.right, params, found);
            }
            other => operands(other, params, found),
        }
    }
    // `p or default` gives `p` itself where it is true.
    fn defaulted(binary: &Binary, params: &FxHashSet<RcLocal>, found: &mut Found) {
        if binary.operation == BinaryOperation::Or
            && let RValue::Local(local) = &*binary.left
            && params.contains(local)
        {
            found.valued.insert(local.clone());
        }
    }
    fn operands(value: &RValue, params: &FxHashSet<RcLocal>, found: &mut Found) {
        match value {
            RValue::Local(local) if params.contains(local) => {
                found.valued.insert(local.clone());
            }
            RValue::Unary(unary) if unary.operation == UnaryOperation::Not => truth(&unary.value, params, found),
            RValue::Binary(binary) if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) => {
                defaulted(binary, params, found);
                truth(&binary.left, params, found);
                operands(&binary.right, params, found);
            }
            RValue::IfExpression(select) => {
                truth(&select.condition, params, found);
                operands(&select.then_value, params, found);
                operands(&select.else_value, params, found);
            }
            other => {
                other.visit_rvalues(&mut |child| {
                    operands(child, params, found);
                    true
                });
            }
        }
    }
    fn block(stmts: &[Statement], params: &FxHashSet<RcLocal>, found: &mut Found) {
        for statement in stmts {
            match statement {
                Statement::If(branch) => {
                    truth(&branch.condition, params, found);
                    block(&branch.then_block.lock().0, params, found);
                    block(&branch.else_block.lock().0, params, found);
                }
                Statement::While(node) => {
                    truth(&node.condition, params, found);
                    block(&node.block.lock().0, params, found);
                }
                Statement::Repeat(node) => {
                    block(&node.block.lock().0, params, found);
                    truth(&node.condition, params, found);
                }
                Statement::NumericFor(node) => {
                    for value in [&node.initial, &node.limit, &node.step] {
                        operands(value, params, found);
                    }
                    block(&node.block.lock().0, params, found);
                }
                Statement::GenericFor(node) => {
                    for value in &node.right {
                        operands(value, params, found);
                    }
                    block(&node.block.lock().0, params, found);
                }
                other => {
                    visit_stmt_rvalues(other, &mut |value| {
                        operands(value, params, found);
                        true
                    });
                }
            }
        }
    }
    let mut found = Found { truth: FxHashSet::default(), valued: FxHashSet::default() };
    block(pattern, params, &mut found);
    let in_order = |set: &FxHashSet<RcLocal>| parameters.iter().filter(|param| set.contains(*param)).cloned().collect();
    (in_order(&found.truth), in_order(&found.valued))
}

/// Harvest parameter bindings from structurally corresponding prefixes.  A
/// mismatch is expected for a specialized site, so failures are ignored here;
/// every harvested binding is re-checked by `unify_block` after specialization.
fn seed_unify_block(t: &Target, pattern: &[Statement], candidate: &[Statement], b: &mut Bindings) {
    for (left, right) in pattern.iter().zip(candidate) {
        seed_unify_stmt(t, left, right, b);
    }
    // A removed leading branch shifts the rest: align from the end too.
    if pattern.len() != candidate.len() {
        for (left, right) in pattern.iter().rev().zip(candidate.iter().rev()) {
            seed_unify_stmt(t, left, right, b);
        }
    }
}

fn seed_unify_values(ctx: &MatchCtx, pattern: &[RValue], candidate: &[RValue], b: &mut Bindings) {
    for (left, right) in pattern.iter().zip(candidate) {
        let _ = unify_rvalue(ctx, left, right, b);
    }
}

fn seed_unify_stmt(t: &Target, pattern: &Statement, candidate: &Statement, b: &mut Bindings) {
    let ctx = t.ctx();
    match (pattern, candidate) {
        (Statement::Assign(left), Statement::Assign(right)) => {
            if left.prefix != right.prefix || left.parallel != right.parallel {
                return;
            }
            for (pl, cl) in left.left.iter().zip(&right.left) {
                let _ = unify_lvalue(&ctx, pl, cl, b);
            }
            seed_unify_values(&ctx, &left.right, &right.right, b);
        }
        (Statement::Call(left), Statement::Call(right)) => {
            let _ = unify_rvalue(&ctx, &left.value, &right.value, b);
            seed_unify_values(&ctx, &left.arguments, &right.arguments, b);
        }
        (Statement::MethodCall(left), Statement::MethodCall(right))
            if left.method == right.method =>
        {
            let _ = unify_rvalue(&ctx, &left.value, &right.value, b);
            seed_unify_values(&ctx, &left.arguments, &right.arguments, b);
        }
        (Statement::If(left), Statement::If(right)) => {
            let _ = unify_rvalue(&ctx, &left.condition, &right.condition, b);
            seed_unify_block(t, &left.then_block.lock().0, &right.then_block.lock().0, b);
            seed_unify_block(t, &left.else_block.lock().0, &right.else_block.lock().0, b);
        }
        (Statement::While(left), Statement::While(right)) => {
            let _ = unify_rvalue(&ctx, &left.condition, &right.condition, b);
            seed_unify_block(t, &left.block.lock().0, &right.block.lock().0, b);
        }
        (Statement::Repeat(left), Statement::Repeat(right)) => {
            let _ = unify_rvalue(&ctx, &left.condition, &right.condition, b);
            seed_unify_block(t, &left.block.lock().0, &right.block.lock().0, b);
        }
        (Statement::NumericFor(left), Statement::NumericFor(right)) => {
            let _ = unify_rvalue(&ctx, &left.initial, &right.initial, b);
            let _ = unify_rvalue(&ctx, &left.limit, &right.limit, b);
            let _ = unify_rvalue(&ctx, &left.step, &right.step, b);
            let _ = unify_local(&ctx, &left.counter, &right.counter, b);
            seed_unify_block(t, &left.block.lock().0, &right.block.lock().0, b);
        }
        (Statement::GenericFor(left), Statement::GenericFor(right)) => {
            for (pl, cl) in left.res_locals.iter().zip(&right.res_locals) {
                let _ = unify_local(&ctx, pl, cl, b);
            }
            seed_unify_values(&ctx, &left.right, &right.right, b);
            seed_unify_block(t, &left.block.lock().0, &right.block.lock().0, b);
        }
        (Statement::Return(left), Statement::Return(right)) => {
            seed_unify_values(&ctx, &left.values, &right.values, b);
        }
        (Statement::SetList(left), Statement::SetList(right)) => {
            let _ = unify_local(&ctx, &left.object_local, &right.object_local, b);
            seed_unify_values(&ctx, &left.values, &right.values, b);
            if let (Some(pl), Some(cl)) = (&left.tail, &right.tail) {
                let _ = unify_rvalue(&ctx, pl, cl, b);
            }
        }
        _ => {}
    }
}

fn specialize_rvalue(value: &mut RValue, bindings: &FxHashMap<RcLocal, RValue>) {
    if let RValue::Local(local) = value
        && let Some(replacement) = bindings.get(local)
    {
        *value = replacement.clone();
        return;
    }
    value.visit_rvalues_mut(&mut |child| {
        specialize_rvalue(child, bindings);
        true
    });
    let owned = std::mem::replace(value, RValue::Literal(Literal::Nil));
    *value = match owned {
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) && matches!(
                (&*binary.left, &*binary.right),
                (RValue::Literal(_), RValue::Literal(_))
            ) =>
        {
            let (RValue::Literal(left), RValue::Literal(right)) = (&*binary.left, &*binary.right)
            else {
                unreachable!()
            };
            if let Some(equal) = runtime_literal_equal(left, right) {
                RValue::Literal(Literal::Boolean(
                    if binary.operation == BinaryOperation::Equal {
                        equal
                    } else {
                        !equal
                    },
                ))
            } else {
                RValue::Binary(binary)
            }
        }
        RValue::Binary(binary)
            if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or)
                && matches!(&*binary.left, RValue::Literal(_)) =>
        {
            let truthy = specialized_truth(&binary.left).unwrap();
            match binary.operation {
                BinaryOperation::And if truthy => *binary.right,
                BinaryOperation::And => *binary.left,
                BinaryOperation::Or if truthy => *binary.left,
                BinaryOperation::Or => *binary.right,
                _ => unreachable!(),
            }
        }
        other => other.reduce(),
    };
}

fn runtime_literal_equal(left: &Literal, right: &Literal) -> Option<bool> {
    match (left, right) {
        (Literal::Number(left), Literal::Number(right)) => Some(left == right),
        (Literal::Nil, Literal::Nil) => Some(true),
        (Literal::Boolean(left), Literal::Boolean(right)) => Some(left == right),
        (Literal::String(left), Literal::String(right)) => Some(left == right),
        // Keep vector/other literal kinds out of the partial evaluator: their
        // runtime equality representation is VM-specific, while cross-kind Lua
        // primitives are always unequal.
        (left, right) if std::mem::discriminant(left) != std::mem::discriminant(right) => {
            Some(false)
        }
        _ => None,
    }
}

fn specialized_truth(value: &RValue) -> Option<bool> {
    match value {
        RValue::Literal(Literal::Nil) => Some(false),
        RValue::Literal(Literal::Boolean(value)) => Some(*value),
        RValue::Literal(_) => Some(true),
        _ => None,
    }
}

fn specialize_block(stmts: &mut Vec<Statement>, bindings: &FxHashMap<RcLocal, RValue>) {
    let mut output = Vec::with_capacity(stmts.len());
    for mut statement in std::mem::take(stmts) {
        statement.visit_rvalues_mut(&mut |value| {
            specialize_rvalue(value, bindings);
            true
        });

        match &mut statement {
            Statement::If(node) => {
                specialize_block(&mut node.then_block.lock().0, bindings);
                specialize_block(&mut node.else_block.lock().0, bindings);
                if let Some(take_then) = specialized_truth(&node.condition) {
                    let selected = if take_then {
                        std::mem::take(&mut node.then_block.lock().0)
                    } else {
                        std::mem::take(&mut node.else_block.lock().0)
                    };
                    output.extend(selected);
                    if sequence_has_terminal_tail(&output) {
                        break;
                    }
                    continue;
                }
            }
            Statement::While(node) => specialize_block(&mut node.block.lock().0, bindings),
            Statement::Repeat(node) => specialize_block(&mut node.block.lock().0, bindings),
            Statement::NumericFor(node) => specialize_block(&mut node.block.lock().0, bindings),
            Statement::GenericFor(node) => specialize_block(&mut node.block.lock().0, bindings),
            _ => {}
        }

        let terminal = matches!(
            statement,
            Statement::Return(_) | Statement::Break(_) | Statement::Continue(_)
        );
        output.push(statement);
        if terminal {
            break;
        }
    }
    *stmts = output;
}

fn sequence_has_terminal_tail(stmts: &[Statement]) -> bool {
    stmts
        .iter()
        .rev()
        .find(|statement| !matches!(statement, Statement::Empty(_) | Statement::Comment(_)))
        .is_some_and(|statement| match statement {
            Statement::Return(_) | Statement::Break(_) | Statement::Continue(_) => true,
            Statement::If(node) => {
                sequence_has_terminal_tail(&node.then_block.lock().0)
                    && sequence_has_terminal_tail(&node.else_block.lock().0)
            }
            _ => false,
        })
}

/// CPS loop-return refolding copies a caller continuation into a callee loop.
/// Only `return` has the same target at both locations; `break`/`continue` are
/// lexically bound to different loops and cannot be compared structurally.
/// Whether the window ends (in its last statement, or an arm of a final `if`
/// at any depth) with a copy of the canonical `continuation` that then falls
/// through, so that the copy and the real continuation both run. A copy the
/// inliner makes ends by leaving the function, which the window's canonical
/// form drops at its end; the raw window still shows it.
fn tail_copy_falls_through(window: &[Statement], continuation: &[Statement]) -> bool {
    if continuation.is_empty() {
        // The continuation is a bare `return`: falling into it is returning.
        return false;
    }
    let ends_with_copy = |block: &[Statement]| {
        let canonical = canon(block);
        canonical.len() >= continuation.len()
            && crate::factor_common_tails::block_alpha_eq(
                &canonical[canonical.len() - continuation.len()..],
                continuation,
            )
    };
    if !sequence_has_return_tail(window) && ends_with_copy(window) {
        return true;
    }
    match window.iter().rev().find(|s| !matches!(s, Statement::Empty(_) | Statement::Comment(_))) {
        Some(Statement::If(branch)) => {
            tail_copy_falls_through(&branch.then_block.lock().0, continuation)
                || tail_copy_falls_through(&branch.else_block.lock().0, continuation)
        }
        _ => false,
    }
}

fn sequence_has_return_tail(stmts: &[Statement]) -> bool {
    stmts
        .iter()
        .rev()
        .find(|statement| !matches!(statement, Statement::Empty(_) | Statement::Comment(_)))
        .is_some_and(|statement| match statement {
            Statement::Return(_) => true,
            Statement::If(node) => {
                sequence_has_return_tail(&node.then_block.lock().0)
                    && sequence_has_return_tail(&node.else_block.lock().0)
            }
            _ => false,
        })
}

fn has_depth_zero_loop_control(stmts: &[Statement], loop_depth: usize) -> bool {
    stmts.iter().any(|statement| match statement {
        Statement::Break(_) | Statement::Continue(_) => loop_depth == 0,
        Statement::If(node) => {
            has_depth_zero_loop_control(&node.then_block.lock().0, loop_depth)
                || has_depth_zero_loop_control(&node.else_block.lock().0, loop_depth)
        }
        Statement::While(node) => has_depth_zero_loop_control(&node.block.lock().0, loop_depth + 1),
        Statement::Repeat(node) => {
            has_depth_zero_loop_control(&node.block.lock().0, loop_depth + 1)
        }
        Statement::NumericFor(node) => {
            has_depth_zero_loop_control(&node.block.lock().0, loop_depth + 1)
        }
        Statement::GenericFor(node) => {
            has_depth_zero_loop_control(&node.block.lock().0, loop_depth + 1)
        }
        _ => false,
    })
}

/// Compose the statements executed after a site in the current block with the
/// continuation inherited from enclosing `if` arms.  The outer segment is
/// unreachable when the local segment definitely transfers control.
fn continuation_segments<'a>(local: &'a [Statement], outer: &[&'a [Statement]]) -> Vec<&'a [Statement]> {
    let mut result = Vec::with_capacity(outer.len() + 1);
    if !local.is_empty() { result.push(local); }
    if !sequence_has_terminal_tail(local) { result.extend_from_slice(outer); }
    result
}

fn semantic_continuation(local: &[Statement], outer: &[&[Statement]]) -> Vec<Statement> {
    continuation_segments(local, outer).into_iter().flat_map(|segment| segment.iter().cloned()).collect()
}

/// `stmts[i]` is an init-less `local R` declaration -> returns R.
fn result_decl(s: &Statement) -> Option<RcLocal> {
    if let Statement::Assign(a) = s {
        if a.prefix && !a.parallel && a.left.len() == 1 {
            if let LValue::Local(r) = &a.left[0] {
                if a.right.is_empty()
                    || (a.right.len() == 1 && matches!(a.right[0], RValue::Literal(Literal::Nil)))
                {
                    return Some(r.clone());
                }
            }
        }
    }
    None
}

fn block_reads_local(stmts: &[Statement], target: &RcLocal) -> bool {
    count_local_reads(stmts, target) > 0
}

fn args_vec_eq(a: &[RValue], b: &[RValue]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| rvalue_exact_eq(x, y))
}

fn branch_conditions_read_any(stmts: &[Statement], params: &FxHashSet<RcLocal>) -> bool {
    stmts.iter().any(|statement| match statement {
        Statement::If(node) => {
            node.condition
                .values_read()
                .into_iter()
                .any(|local| params.contains(local))
                || branch_conditions_read_any(&node.then_block.lock().0, params)
                || branch_conditions_read_any(&node.else_block.lock().0, params)
        }
        Statement::While(node) => branch_conditions_read_any(&node.block.lock().0, params),
        Statement::Repeat(node) => branch_conditions_read_any(&node.block.lock().0, params),
        Statement::NumericFor(node) => branch_conditions_read_any(&node.block.lock().0, params),
        Statement::GenericFor(node) => branch_conditions_read_any(&node.block.lock().0, params),
        _ => false,
    })
}

/// A helper's `return` from inside a loop, inlined where code follows the
/// call, becomes a flag and a `break`: `local ok = true; for … do if c then
/// ok = false; break end end; if ok then REST end`. Gives back the helper's
/// `for … do if c then return end end; REST` where that ends `window` (or
/// an arm of its last `if`, at any depth): a `return` skips everything after
/// it, the flag only REST. Every `break` of that loop must clear the flag,
/// which nothing else reads or writes. Also returns the flags, which the
/// rebuilt call no longer declares.
fn unflag_loop_exits(window: &[Statement]) -> Option<(Vec<Statement>, FxHashSet<RcLocal>)> {
    let mut flags = FxHashSet::default();
    let block = unflag_block(window, &mut flags)?;
    Some((block, flags))
}

fn unflag_block(block: &[Statement], flags: &mut FxHashSet<RcLocal>) -> Option<Vec<Statement>> {
    let real: Vec<usize> = (0..block.len()).filter(|&k| !is_match_trivia(&block[k])).collect();
    if let [.., declaration, looped, guarded] = real[..]
        && let Statement::Assign(assign) = &block[declaration]
        && assign.prefix
        && let ([LValue::Local(flag)], [RValue::Literal(Literal::Boolean(true))]) =
            (assign.left.as_slice(), assign.right.as_slice())
        && let Statement::If(guard) = &block[guarded]
        && matches!(&guard.condition, RValue::Local(read) if read == flag)
        && guard.else_block.lock().0.is_empty()
        && count_local_reads(&guard.then_block.lock().0, flag) == 0
        && !block_writes_local(&guard.then_block.lock().0, flag)
        && let Some(exits) = return_from_loop(&block[looped], flag, None)
    {
        flags.insert(flag.clone());
        let mut out: Vec<Statement> = block[..declaration].to_vec();
        out.extend(block[declaration + 1..looped].iter().cloned());
        out.push(exits);
        out.extend(guard.then_block.lock().0.iter().cloned());
        return Some(out);
    }
    let &last = real.last()?;
    let Statement::If(branch) = &block[last] else { return None };
    let then_block = unflag_block(&branch.then_block.lock().0, flags);
    let else_block = unflag_block(&branch.else_block.lock().0, flags);
    if then_block.is_none() && else_block.is_none() {
        return None;
    }
    let rebuilt = If::new(
        branch.condition.clone(),
        Block(then_block.unwrap_or_else(|| branch.then_block.lock().0.clone())),
        Block(else_block.unwrap_or_else(|| branch.else_block.lock().0.clone())),
    );
    let mut out = block.to_vec();
    out[last] = rebuilt.into();
    Some(out)
}

/// `looped` with each `flag = false; break` of its own turned into `return`,
/// when those are its only `break`s and its only uses of `flag`. With a
/// `result`, each exit is `result = x; flag = false; break`, turned into
/// `return x`.
fn return_from_loop(looped: &Statement, flag: &RcLocal, result: Option<&RcLocal>) -> Option<Statement> {
    fn exits(stmts: &[Statement], flag: &RcLocal, result: Option<&RcLocal>, replaced: &mut usize) -> Option<Vec<Statement>> {
        let clears_flag = |statement: &Statement| matches!(statement, Statement::Assign(assign)
            if !assign.prefix
                && matches!(assign.left.as_slice(), [LValue::Local(written)] if written == flag)
                && matches!(assign.right.as_slice(), [RValue::Literal(Literal::Boolean(false))]));
        let next_real = |from: usize| (from..stmts.len()).find(|&n| !is_match_trivia(&stmts[n]));
        let mut out = Vec::with_capacity(stmts.len());
        let mut k = 0;
        while k < stmts.len() {
            let statement = &stmts[k];
            // A value exit: the store, then the flag, then the `break`.
            if let Some(result) = result
                && let Statement::Assign(store) = statement
                && is_plain_local_write(store, result)
                && let Some(clear) = next_real(k + 1)
                && clears_flag(&stmts[clear])
            {
                let next = next_real(clear + 1)?;
                if !matches!(stmts[next], Statement::Break(_)) || reads_local(&store.right[0], flag) {
                    return None;
                }
                out.push(Return::new(vec![store.right[0].clone()]).into());
                *replaced += 1;
                k = next + 1;
                continue;
            }
            if clears_flag(statement) {
                let next = next_real(k + 1)?;
                // A value exit stores its result first.
                if result.is_some() || !matches!(stmts[next], Statement::Break(_)) {
                    return None;
                }
                out.push(Return::new(Vec::new()).into());
                *replaced += 1;
                k = next + 1;
                continue;
            }
            match statement {
                Statement::Break(_) => return None,
                Statement::If(branch) => {
                    if reads_local(&branch.condition, flag) {
                        return None;
                    }
                    let then_block = exits(&branch.then_block.lock().0, flag, result, replaced)?;
                    let else_block = exits(&branch.else_block.lock().0, flag, result, replaced)?;
                    out.push(If::new(branch.condition.clone(), Block(then_block), Block(else_block)).into());
                }
                // A nested loop's `break`s are its own; it may not touch the flag.
                other => {
                    let single = std::slice::from_ref(other);
                    if count_local_reads(single, flag) > 0 || block_writes_local(single, flag) {
                        return None;
                    }
                    out.push(other.clone());
                }
            }
            k += 1;
        }
        Some(out)
    }
    let mut replaced = 0;
    let rebuilt: Statement = match looped {
        Statement::GenericFor(node) => {
            if node.right.iter().any(|value| reads_local(value, flag)) {
                return None;
            }
            let body = exits(&node.block.lock().0, flag, result, &mut replaced)?;
            Statement::GenericFor(GenericFor {
                res_locals: node.res_locals.clone(),
                right: node.right.clone(),
                block: Arc::new(Mutex::new(Block(body))),
                origin: node.origin.clone(),
            })
        }
        Statement::NumericFor(node) => {
            if [&node.initial, &node.limit, &node.step].into_iter().any(|value| reads_local(value, flag)) {
                return None;
            }
            let body = exits(&node.block.lock().0, flag, result, &mut replaced)?;
            Statement::NumericFor(Box::new(NumericFor {
                block: Arc::new(Mutex::new(Block(body))),
                initial: node.initial.clone(),
                limit: node.limit.clone(),
                step: node.step.clone(),
                counter: node.counter.clone(),
            }))
        }
        Statement::While(node) => {
            if reads_local(&node.condition, flag) {
                return None;
            }
            let body = exits(&node.block.lock().0, flag, result, &mut replaced)?;
            While::new(node.condition.clone(), Block(body)).into()
        }
        _ => return None,
    };
    (replaced > 0).then_some(rebuilt)
}

/// A value helper's `return x` from inside a loop, inlined into `local r =
/// f(args)`: Luau stores `x` into `r` and jumps past the rest of the helper,
/// which the structurer writes as a flag and a `break`: `PRE; local r; local
/// ok = true; for … do if c then r = x; ok = false; break end end; if ok then
/// REST end`. The helper's own `return nil` after the loop leaves no store
/// (the declaration already holds `nil`), so the flag's `if` may be missing.
/// Gives back `PRE; for … do if c then return x end end; REST` with `r = nil`
/// closing a REST that never stores `r`: the helper's body with its returns
/// after the loop as stores to `r`, the shape a value pattern unifies
/// against. A `return` skips the rest of the helper, the flag only REST, so
/// every `break` of the loop must store `r` and clear the flag, which nothing
/// else reads or writes, and the loop may not write `r` otherwise.
fn unflag_value_loop(
    pre: &[Statement],
    looped: &Statement,
    guard: Option<&Statement>,
    result: &RcLocal,
    flag: &RcLocal,
) -> Option<Vec<Statement>> {
    let exits = return_from_loop(looped, flag, Some(result))?;
    if block_writes_local(std::slice::from_ref(&exits), result) {
        return None;
    }
    let mut rest = Vec::new();
    if let Some(guard) = guard {
        let Statement::If(guard) = guard else { return None };
        let then_block = guard.then_block.lock();
        if !guard.else_block.lock().0.is_empty()
            || count_local_reads(&then_block.0, flag) > 0
            || block_writes_local(&then_block.0, flag)
        {
            return None;
        }
        rest.extend(then_block.0.iter().cloned());
    }
    if !block_writes_local(&rest, result) {
        rest.push(Assign::new(vec![LValue::Local(result.clone())], vec![RValue::Literal(Literal::Nil)]).into());
    }
    let mut window = Vec::with_capacity(pre.len() + 1 + rest.len());
    window.extend(pre.iter().cloned());
    window.push(exits);
    window.extend(rest);
    Some(window)
}

fn reads_local(value: &RValue, local: &RcLocal) -> bool {
    count_local_reads(&[Statement::Return(Return::new(vec![value.clone()]))], local) > 0
}

fn block_writes_local(stmts: &[Statement], local: &RcLocal) -> bool {
    let mut written = FxHashSet::default();
    collect_written(stmts, &mut written);
    written.contains(local)
}

fn is_void_return_guard(statement: &Statement) -> bool {
    let Statement::If(node) = statement else {
        return false;
    };
    node.else_block.lock().0.is_empty()
        && matches!(node.then_block.lock().0.last(), Some(Statement::Return(ret)) if ret.values.is_empty())
}

fn has_loop_void_return(stmts: &[Statement], inside_loop: bool) -> bool {
    stmts.iter().any(|statement| {
        (inside_loop && is_void_return_guard(statement))
            || match statement {
                Statement::If(node) => {
                    has_loop_void_return(&node.then_block.lock().0, inside_loop)
                        || has_loop_void_return(&node.else_block.lock().0, inside_loop)
                }
                Statement::While(node) => has_loop_void_return(&node.block.lock().0, true),
                Statement::Repeat(node) => has_loop_void_return(&node.block.lock().0, true),
                Statement::NumericFor(node) => has_loop_void_return(&node.block.lock().0, true),
                Statement::GenericFor(node) => has_loop_void_return(&node.block.lock().0, true),
                _ => false,
            }
    })
}

// ===================================================================
// Shared AST facts and traversal
// ===================================================================

pub(crate) fn is_scalar_return_value(rv: &RValue) -> bool {
    !matches!(
        rv,
        RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_)
    )
}

pub(crate) fn each_closure_decl(
    stmts: &[Statement],
    f: &mut impl FnMut(&RcLocal, &Arc<Mutex<Function>>),
) {
    for s in stmts {
        // Register a target only for a direct `local x = function ... end`.
        if let Statement::Assign(a) = s {
            if a.prefix
                && a.left.len() == 1
                && a.right.len() == 1
                && let LValue::Local(l) = &a.left[0]
                && let RValue::Closure(c) = &a.right[0]
            {
                f(l, &c.function.0);
            }
        }
        // Recurse into nested statement blocks ...
        match s {
            Statement::If(fi) => {
                each_closure_decl(&fi.then_block.lock().0, f);
                each_closure_decl(&fi.else_block.lock().0, f);
            }
            Statement::While(w) => each_closure_decl(&w.block.lock().0, f),
            Statement::Repeat(r) => each_closure_decl(&r.block.lock().0, f),
            Statement::NumericFor(nf) => each_closure_decl(&nf.block.lock().0, f),
            Statement::GenericFor(gf) => each_closure_decl(&gf.block.lock().0, f),
            _ => {}
        }
        // ... and descend into EVERY closure body, wherever it appears (call
        // arguments, table values, ...), to find local closure declarations
        // nested inside — `deinline_block` likewise recurses into those bodies.
        visit_stmt_rvalues(s, &mut |rv| {
            each_closure_in_rvalue(rv, f);
            true
        });
    }
}

fn each_closure_in_rvalue(rv: &RValue, f: &mut impl FnMut(&RcLocal, &Arc<Mutex<Function>>)) {
    match rv {
        RValue::Closure(c) => each_closure_decl(&c.function.0.lock().body.0, f),
        RValue::Call(c) => {
            each_closure_in_rvalue(&c.value, f);
            for a in &c.arguments {
                each_closure_in_rvalue(a, f);
            }
        }
        RValue::MethodCall(m) => {
            each_closure_in_rvalue(&m.value, f);
            for a in &m.arguments {
                each_closure_in_rvalue(a, f);
            }
        }
        RValue::Index(ix) => {
            each_closure_in_rvalue(&ix.left, f);
            each_closure_in_rvalue(&ix.right, f);
        }
        RValue::Unary(u) => each_closure_in_rvalue(&u.value, f),
        RValue::Binary(b) => {
            each_closure_in_rvalue(&b.left, f);
            each_closure_in_rvalue(&b.right, f);
        }
        RValue::Table(t) => {
            for (k, v) in &t.0 {
                if let Some(k) = k {
                    each_closure_in_rvalue(k, f);
                }
                each_closure_in_rvalue(v, f);
            }
        }
        RValue::Select(Select::Call(c)) => {
            each_closure_in_rvalue(&c.value, f);
            for a in &c.arguments {
                each_closure_in_rvalue(a, f);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            each_closure_in_rvalue(&m.value, f);
            for a in &m.arguments {
                each_closure_in_rvalue(a, f);
            }
        }
        RValue::IfExpression(e) => {
            each_closure_in_rvalue(&e.condition, f);
            each_closure_in_rvalue(&e.then_value, f);
            each_closure_in_rvalue(&e.else_value, f);
        }
        _ => {}
    }
}

/// A body we refuse to treat as a de-inline pattern: contains gotos/labels,
/// comments, upvalue-close, lifter-internal for-nodes, or a nested closure whose
/// bytecode provenance is unavailable.
/// Return shape is handled separately by `classify_returns`.
pub(crate) fn body_unsafe(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| {
        // A nested closure is matchable only when the lifter retained its exact
        // bytecode prototype id. `unify_closure` additionally proves the ordered
        // capture modes and mapped upvalues. Synthetic/unknown closures remain
        // refused; structural identity guessing would be unsound.
        if stmt_rvalues(s)
            .iter()
            .any(|rv| rvalue_has_unproven_closure(rv))
        {
            return true;
        }
        match s {
            // Our own reconstruction markers are runtime no-ops. A shared callee
            // body that gained one from an inner de-inline in an earlier
            // fixed-point iteration must stay a valid target, else chained/nested
            // inlines never re-collapse; `canon_top` drops them symmetrically from
            // pattern and candidate, so matching is unaffected. A genuine source
            // comment still refuses the body.
            Statement::Comment(c) => !is_internal_marker(c),
            Statement::Goto(_)
            | Statement::Label(_)
            | Statement::Close(_)
            | Statement::NumForInit(_)
            | Statement::NumForNext(_)
            | Statement::GenericForInit(_)
            | Statement::GenericForNext(_) => true,
            // return shape (void/value/reject) is decided by `classify_returns`.
            Statement::If(f) => {
                body_unsafe(&f.then_block.lock().0) || body_unsafe(&f.else_block.lock().0)
            }
            Statement::While(w) => body_unsafe(&w.block.lock().0),
            Statement::Repeat(r) => body_unsafe(&r.block.lock().0),
            Statement::NumericFor(nf) => body_unsafe(&nf.block.lock().0),
            Statement::GenericFor(gf) => body_unsafe(&gf.block.lock().0),
            _ => false,
        }
    })
}

fn rvalue_has_unproven_closure(rv: &RValue) -> bool {
    match rv {
        RValue::Closure(closure) => closure.function.0.lock().bytecode_proto_id.is_none(),
        RValue::Index(i) => {
            rvalue_has_unproven_closure(&i.left) || rvalue_has_unproven_closure(&i.right)
        }
        RValue::Unary(u) => rvalue_has_unproven_closure(&u.value),
        RValue::Binary(b) => {
            rvalue_has_unproven_closure(&b.left) || rvalue_has_unproven_closure(&b.right)
        }
        RValue::Call(c) => {
            rvalue_has_unproven_closure(&c.value)
                || c.arguments.iter().any(rvalue_has_unproven_closure)
        }
        RValue::MethodCall(m) => {
            rvalue_has_unproven_closure(&m.value)
                || m.arguments.iter().any(rvalue_has_unproven_closure)
        }
        RValue::Table(t) => t.0.iter().any(|(k, v)| {
            k.as_ref().is_some_and(rvalue_has_unproven_closure) || rvalue_has_unproven_closure(v)
        }),
        RValue::Select(Select::Call(c)) => {
            rvalue_has_unproven_closure(&c.value)
                || c.arguments.iter().any(rvalue_has_unproven_closure)
        }
        RValue::Select(Select::MethodCall(m)) => {
            rvalue_has_unproven_closure(&m.value)
                || m.arguments.iter().any(rvalue_has_unproven_closure)
        }
        RValue::IfExpression(e) => {
            rvalue_has_unproven_closure(&e.condition)
                || rvalue_has_unproven_closure(&e.then_value)
                || rvalue_has_unproven_closure(&e.else_value)
        }
        _ => false,
    }
}

fn block_has_return(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Return(_) => true,
        Statement::If(f) => {
            block_has_return(&f.then_block.lock().0) || block_has_return(&f.else_block.lock().0)
        }
        Statement::While(w) => block_has_return(&w.block.lock().0),
        Statement::Repeat(r) => block_has_return(&r.block.lock().0),
        Statement::NumericFor(nf) => block_has_return(&nf.block.lock().0),
        Statement::GenericFor(gf) => block_has_return(&gf.block.lock().0),
        _ => false,
    })
}

pub(crate) fn collect_declared_locals(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for s in stmts {
        match s {
            Statement::Assign(a) if a.prefix => {
                for l in &a.left {
                    if let LValue::Local(x) = l {
                        out.insert(x.clone());
                    }
                }
            }
            Statement::If(f) => {
                collect_declared_locals(&f.then_block.lock().0, out);
                collect_declared_locals(&f.else_block.lock().0, out);
            }
            Statement::While(w) => collect_declared_locals(&w.block.lock().0, out),
            Statement::Repeat(r) => collect_declared_locals(&r.block.lock().0, out),
            Statement::NumericFor(nf) => {
                out.insert(nf.counter.clone());
                collect_declared_locals(&nf.block.lock().0, out);
            }
            Statement::GenericFor(gf) => {
                out.extend(gf.res_locals.iter().cloned());
                collect_declared_locals(&gf.block.lock().0, out);
            }
            _ => {}
        }
    }
}

pub(crate) fn collect_written(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for s in stmts {
        match s {
            Statement::Assign(a) => {
                for l in &a.left {
                    if let LValue::Local(x) = l {
                        out.insert(x.clone());
                    }
                }
            }
            Statement::NumericFor(nf) => {
                out.insert(nf.counter.clone());
                collect_written(&nf.block.lock().0, out);
            }
            Statement::GenericFor(gf) => {
                for x in &gf.res_locals {
                    out.insert(x.clone());
                }
                collect_written(&gf.block.lock().0, out);
            }
            Statement::SetList(sl) => {
                out.insert(sl.object_local.clone());
            }
            Statement::If(f) => {
                collect_written(&f.then_block.lock().0, out);
                collect_written(&f.else_block.lock().0, out);
            }
            Statement::While(w) => collect_written(&w.block.lock().0, out),
            Statement::Repeat(r) => collect_written(&r.block.lock().0, out),
            _ => {}
        }
        // also any writes performed inside closures in this statement's rvalues
        // (a closure that captures and writes an upvalue) — by `RcLocal` identity
        // these are the same locals after `link_upvalues`.
        visit_stmt_rvalues(s, &mut |rv| {
            collect_written_in_closures(rv, out);
            true
        });
    }
}

fn collect_written_in_closures(rv: &RValue, out: &mut FxHashSet<RcLocal>) {
    match rv {
        RValue::Closure(c) => collect_written(&c.function.0.lock().body.0, out),
        RValue::Call(c) => {
            collect_written_in_closures(&c.value, out);
            for a in &c.arguments {
                collect_written_in_closures(a, out);
            }
        }
        RValue::MethodCall(m) => {
            collect_written_in_closures(&m.value, out);
            for a in &m.arguments {
                collect_written_in_closures(a, out);
            }
        }
        RValue::Index(i) => {
            collect_written_in_closures(&i.left, out);
            collect_written_in_closures(&i.right, out);
        }
        RValue::Unary(u) => collect_written_in_closures(&u.value, out),
        RValue::Binary(b) => {
            collect_written_in_closures(&b.left, out);
            collect_written_in_closures(&b.right, out);
        }
        RValue::Table(t) => {
            for (k, val) in &t.0 {
                if let Some(k) = k {
                    collect_written_in_closures(k, out);
                }
                collect_written_in_closures(val, out);
            }
        }
        RValue::Select(Select::Call(c)) => {
            collect_written_in_closures(&c.value, out);
            for a in &c.arguments {
                collect_written_in_closures(a, out);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            collect_written_in_closures(&m.value, out);
            for a in &m.arguments {
                collect_written_in_closures(a, out);
            }
        }
        RValue::IfExpression(e) => {
            collect_written_in_closures(&e.condition, out);
            collect_written_in_closures(&e.then_value, out);
            collect_written_in_closures(&e.else_value, out);
        }
        _ => {}
    }
}

/// How specific a helper's pattern is: its fixed names
/// ([`anchors_in_block`]) and the outer locals it uses. An outer local
/// (`particles` in `if p then particles:AbsoluteEmit(p) end`) unifies only
/// with itself, like a global; a parameter or a local of the body binds to
/// anything.
fn anchor_score(pattern: &[Statement], parameters: &[RcLocal]) -> usize {
    let mut used = FxHashSet::default();
    collect_reads(pattern, &mut used);
    collect_written(pattern, &mut used);
    let mut own = FxHashSet::default();
    collect_declared_locals(pattern, &mut own);
    let outer = used.iter().filter(|l| !own.contains(*l) && !parameters.contains(l)).count();
    anchors_in_block(pattern) + outer
}

fn anchors_in_block(stmts: &[Statement]) -> usize {
    let mut n = 0;
    for s in stmts {
        anchors_in_stmt(s, &mut n);
    }
    n
}

// Instrumentation only: count rvalue nodes + nested statements in a pattern stmt.
pub(crate) fn dbg_stmt_node_count(s: &Statement) -> usize {
    let mut n = 1usize;
    crate::deinline::visit_stmt_rvalues(s, &mut |rv| {
        n += dbg_rvalue_node_count(rv);
        true
    });
    match s {
        Statement::If(f) => {
            n += f
                .then_block
                .lock()
                .0
                .iter()
                .map(dbg_stmt_node_count)
                .sum::<usize>();
            n += f
                .else_block
                .lock()
                .0
                .iter()
                .map(dbg_stmt_node_count)
                .sum::<usize>();
        }
        Statement::While(w) => {
            n += w
                .block
                .lock()
                .0
                .iter()
                .map(dbg_stmt_node_count)
                .sum::<usize>()
        }
        Statement::Repeat(r) => {
            n += r
                .block
                .lock()
                .0
                .iter()
                .map(dbg_stmt_node_count)
                .sum::<usize>()
        }
        Statement::NumericFor(nf) => {
            n += nf
                .block
                .lock()
                .0
                .iter()
                .map(dbg_stmt_node_count)
                .sum::<usize>()
        }
        Statement::GenericFor(gf) => {
            n += gf
                .block
                .lock()
                .0
                .iter()
                .map(dbg_stmt_node_count)
                .sum::<usize>()
        }
        _ => {}
    }
    n
}

fn dbg_rvalue_node_count(rv: &RValue) -> usize {
    1 + rv
        .rvalues()
        .iter()
        .map(|c| dbg_rvalue_node_count(c))
        .sum::<usize>()
}

fn anchors_in_stmt(s: &Statement, n: &mut usize) {
    match s {
        Statement::Assign(a) => {
            for l in &a.left {
                anchors_in_lvalue(l, n);
            }
            for r in &a.right {
                anchors_in_rvalue(r, n);
            }
        }
        Statement::Call(c) => {
            anchors_in_rvalue(&c.value, n);
            for a in &c.arguments {
                anchors_in_rvalue(a, n);
            }
        }
        Statement::MethodCall(m) => {
            *n += 1;
            anchors_in_rvalue(&m.value, n);
            for a in &m.arguments {
                anchors_in_rvalue(a, n);
            }
        }
        Statement::If(f) => {
            anchors_in_rvalue(&f.condition, n);
            for s in &f.then_block.lock().0 {
                anchors_in_stmt(s, n);
            }
            for s in &f.else_block.lock().0 {
                anchors_in_stmt(s, n);
            }
        }
        Statement::While(w) => {
            anchors_in_rvalue(&w.condition, n);
            for s in &w.block.lock().0 {
                anchors_in_stmt(s, n);
            }
        }
        Statement::Repeat(r) => {
            anchors_in_rvalue(&r.condition, n);
            for s in &r.block.lock().0 {
                anchors_in_stmt(s, n);
            }
        }
        Statement::NumericFor(nf) => {
            anchors_in_rvalue(&nf.initial, n);
            anchors_in_rvalue(&nf.limit, n);
            anchors_in_rvalue(&nf.step, n);
            for s in &nf.block.lock().0 {
                anchors_in_stmt(s, n);
            }
        }
        Statement::GenericFor(gf) => {
            for r in &gf.right {
                anchors_in_rvalue(r, n);
            }
            for s in &gf.block.lock().0 {
                anchors_in_stmt(s, n);
            }
        }
        Statement::SetList(sl) => {
            for v in &sl.values {
                anchors_in_rvalue(v, n);
            }
            if let Some(tail) = &sl.tail {
                anchors_in_rvalue(tail, n);
            }
        }
        Statement::Return(r) => {
            for v in &r.values {
                anchors_in_rvalue(v, n);
            }
        }
        _ => {}
    }
}

fn anchors_in_lvalue(l: &LValue, n: &mut usize) {
    match l {
        LValue::Global(_) => *n += 1,
        LValue::Index(i) => {
            anchors_in_rvalue(&i.left, n);
            anchors_in_rvalue(&i.right, n);
        }
        LValue::Local(_) => {}
    }
}

pub(crate) fn anchors_in_rvalue(rv: &RValue, n: &mut usize) {
    match rv {
        RValue::Global(_) | RValue::Literal(Literal::String(_)) => *n += 1,
        // A closure of a known prototype matches only another instance of
        // that prototype (`unify_closure`): as specific as any pattern gets.
        RValue::Closure(c) if c.function.0.lock().bytecode_proto_id.is_some() => *n += 2,
        RValue::Index(i) => {
            anchors_in_rvalue(&i.left, n);
            anchors_in_rvalue(&i.right, n);
        }
        RValue::Unary(u) => anchors_in_rvalue(&u.value, n),
        RValue::Binary(b) => {
            anchors_in_rvalue(&b.left, n);
            anchors_in_rvalue(&b.right, n);
        }
        // Luau `if c then a else b` expression. Without this arm the `_ => {}`
        // below would count ZERO anchors for an `IfExpression`-rooted body — the
        // exact shape the §7 expression de-inliner extracts pre-`normalize_conditions`
        // (`if C then V else false`) — and silently fail its `anchors >= 2` cost gate.
        RValue::IfExpression(e) => {
            anchors_in_rvalue(&e.condition, n);
            anchors_in_rvalue(&e.then_value, n);
            anchors_in_rvalue(&e.else_value, n);
        }
        RValue::Call(c) => {
            anchors_in_rvalue(&c.value, n);
            for a in &c.arguments {
                anchors_in_rvalue(a, n);
            }
        }
        RValue::MethodCall(m) => {
            *n += 1;
            anchors_in_rvalue(&m.value, n);
            for a in &m.arguments {
                anchors_in_rvalue(a, n);
            }
        }
        RValue::Table(t) => {
            for (k, v) in &t.0 {
                if let Some(k) = k {
                    anchors_in_rvalue(k, n);
                }
                anchors_in_rvalue(v, n);
            }
        }
        RValue::Select(Select::Call(c)) => {
            anchors_in_rvalue(&c.value, n);
            for a in &c.arguments {
                anchors_in_rvalue(a, n);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            *n += 1;
            anchors_in_rvalue(&m.value, n);
            for a in &m.arguments {
                anchors_in_rvalue(a, n);
            }
        }
        _ => {}
    }
}

// ===================================================================
// Definition markers
// ===================================================================

pub(crate) fn insert_def_markers(stmts: &mut Vec<Statement>, converted: &FxHashSet<RcLocal>) {
    insert_def_markers_with_text(stmts, converted, DEF_MARKER);
}

pub(crate) fn insert_def_markers_with_text(
    stmts: &mut Vec<Statement>,
    converted: &FxHashSet<RcLocal>,
    marker: &str,
) {
    if converted.is_empty() {
        return;
    }
    for s in stmts.iter_mut() {
        match s {
            Statement::If(f) => {
                insert_def_markers_with_text(&mut f.then_block.lock().0, converted, marker);
                insert_def_markers_with_text(&mut f.else_block.lock().0, converted, marker);
            }
            Statement::While(w) => insert_def_markers_with_text(&mut w.block.lock().0, converted, marker),
            Statement::Repeat(r) => insert_def_markers_with_text(&mut r.block.lock().0, converted, marker),
            Statement::NumericFor(nf) => insert_def_markers_with_text(&mut nf.block.lock().0, converted, marker),
            Statement::GenericFor(gf) => insert_def_markers_with_text(&mut gf.block.lock().0, converted, marker),
            _ => {}
        }
        // recover definitions inside ANY closure body (call arguments, table
        // values, ...), matching where `deinline_block` recovers the calls.
        visit_stmt_rvalues_mut(s, &mut |rv| {
            markers_in_closures(rv, converted, marker);
            true
        });
    }

    let mut out: Vec<Statement> = Vec::with_capacity(stmts.len());
    for s in std::mem::take(stmts) {
        if let Statement::Assign(a) = &s {
            if a.prefix
                && a.left.len() == 1
                && a.right.len() == 1
                && let LValue::Local(l) = &a.left[0]
                && matches!(&a.right[0], RValue::Closure(_))
                && converted.contains(l)
                // Idempotent: if this decl already carries the marker (e.g. the
                // statement de-inliner converted the same helper earlier, or this
                // pass already ran), do not emit this marker a second time.
                && !matches!(out.last(), Some(Statement::Comment(c)) if c.text == marker)
            {
                out.push(Statement::Comment(Comment::new(marker.to_string())));
            }
        }
        out.push(s);
    }
    *stmts = out;
}

fn markers_in_closures(rv: &mut RValue, converted: &FxHashSet<RcLocal>, marker: &str) {
    match rv {
        RValue::Closure(c) => insert_def_markers_with_text(&mut c.function.0.lock().body.0, converted, marker),
        RValue::Call(c) => {
            markers_in_closures(c.value.as_mut(), converted, marker);
            for a in &mut c.arguments {
                markers_in_closures(a, converted, marker);
            }
        }
        RValue::MethodCall(m) => {
            markers_in_closures(m.value.as_mut(), converted, marker);
            for a in &mut m.arguments {
                markers_in_closures(a, converted, marker);
            }
        }
        RValue::Index(ix) => {
            markers_in_closures(ix.left.as_mut(), converted, marker);
            markers_in_closures(ix.right.as_mut(), converted, marker);
        }
        RValue::Unary(u) => markers_in_closures(u.value.as_mut(), converted, marker),
        RValue::Binary(b) => {
            markers_in_closures(b.left.as_mut(), converted, marker);
            markers_in_closures(b.right.as_mut(), converted, marker);
        }
        RValue::Table(t) => {
            for (k, v) in &mut t.0 {
                if let Some(k) = k {
                    markers_in_closures(k, converted, marker);
                }
                markers_in_closures(v, converted, marker);
            }
        }
        RValue::Select(Select::Call(c)) => {
            markers_in_closures(c.value.as_mut(), converted, marker);
            for a in &mut c.arguments {
                markers_in_closures(a, converted, marker);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            markers_in_closures(m.value.as_mut(), converted, marker);
            for a in &mut m.arguments {
                markers_in_closures(a, converted, marker);
            }
        }
        RValue::IfExpression(e) => {
            markers_in_closures(e.condition.as_mut(), converted, marker);
            markers_in_closures(e.then_value.as_mut(), converted, marker);
            markers_in_closures(e.else_value.as_mut(), converted, marker);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
