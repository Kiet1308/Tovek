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

mod statement_values;
pub(crate) use statement_values::{visit_stmt_rvalues, visit_stmt_rvalues_mut};
mod pattern_cache;
use pattern_cache::{CompiledPattern, CompiledPatterns, PreparedPattern};
#[cfg(test)]
mod canonical_tests;

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Binary, BinaryOperation, Block, Call, Closure, Comment, Function, GenericFor, If,
    LValue, Literal, LocalRw, MethodCall, NumericFor, RValue, RcLocal, Reduce, Repeat, Return,
    Select, SideEffects, Statement, Table, Traverse, Unary, UnaryOperation, Upvalue, While,
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
    /// A Value target returning from inside a loop ([`loop_return_split`]):
    /// the index in `pat` of that loop, which is also the number of the
    /// helper's statements before it. Its sites store the value and leave
    /// through a flag and a `break` ([`match_value_loop`]). `None` for every
    /// other target.
    loop_exit_at: Option<usize>,
    /// The helper's own locals that its body ends by returning
    /// ([`local_tuple_return`]). Its pattern is the body before that return,
    /// matched as a void region whose site declares the caller's locals in
    /// their place; rebuilt as `local set, key = f(args)`. Empty for every
    /// other target.
    returns: Vec<RcLocal>,
    captures: std::rc::Rc<crate::deinline_safety::CaptureSafety>,
    search: std::rc::Rc<crate::deinline_safety::SearchBudget>,
}

#[derive(Default, Clone)]
pub(crate) struct Bindings {
    pub(crate) params: FxHashMap<RcLocal, RValue>,
    locals: FxHashMap<RcLocal, RcLocal>,
    locals_rev: FxHashMap<RcLocal, RcLocal>,
    /// For Value targets: the single caller local that every `return X` in the
    /// pattern maps to (i.e. the inlined result local `RESULT`).
    result: Option<RcLocal>,
}

impl Bindings {
    pub(crate) fn local_binding(&self, pattern: &RcLocal) -> Option<&RcLocal> {
        self.locals.get(pattern)
    }
}

/// The only target context the *expression*-level unifier (`unify_rvalue` and the
/// `unify_*` helpers it calls) actually reads: the binding-hole sets. `params` are
/// bind-once holes (each binds to one caller argument expression, and every later
/// occurrence must match it); `locals` are the callee's own declared locals,
/// matched as an injective renaming. Everything else (globals, literals, operators,
/// upvalues by `RcLocal` identity, ...) must match exactly.
///
/// Factoring this out of `Target` lets the §7 expression de-inliner
/// (`crate::expr_deinline`) reuse the exact same battle-tested structural unifier
/// (bit-exact literals, closure-arg refusal, param consistency, local injectivity)
/// without depending on the statement-only `Target` fields (`pat`, `kind`,
/// `value_anchor`, ...). The statement matcher builds one via [`Target::ctx`].
pub(crate) struct MatchCtx<'a> {
    pub(crate) params: &'a FxHashSet<RcLocal>,
    pub(crate) locals: &'a FxHashSet<RcLocal>,
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
    deinline_with_patterns(body, CompiledPatterns::default());
}

fn deinline_with_patterns(body: &mut Block, mut patterns: CompiledPatterns) {
    // Every rewrite needs a target; the module-wide censuses below are only
    // worth building when some helper passes the per-declaration gates.
    if !any_structural_target(body, &mut patterns) {
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
            let mut targets = collect_targets(body, &write_counts, captures, &mut patterns);
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
        // Canonical pattern syntax is reusable only for helpers whose owned
        // body was not edited. Capture/call-graph and parameter-motion proofs
        // are rebuilt above on every iteration, even for a reused pattern.
        patterns.retain_unchanged(targets.into_iter().map(|target| (target.func_ptr,
            CompiledPattern { kind: target.kind, falls_off: target.falls_off, pat: target.pat })), &newly.bodies);
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
// Canonicalisation (collapse the return ⇄ guard duality)
// ===================================================================

fn negate_canon(cond: RValue) -> RValue {
    match cond {
        RValue::Unary(u) if u.operation == UnaryOperation::Not => *u.value,
        RValue::Binary(b)
            if matches!(
                b.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            let operation = if b.operation == BinaryOperation::Equal {
                BinaryOperation::NotEqual
            } else {
                BinaryOperation::Equal
            };
            RValue::Binary(Binary {
                node_origin: Default::default(),
                left: b.left,
                right: b.right,
                operation,
            })
        }
        other => RValue::Unary(Unary {
            node_origin: Default::default(),
            value: Box::new(other),
            operation: UnaryOperation::Not,
        }),
    }
}

/// P9: EVERY condition has an exact boolean inverse for the §8 guard-polarity
/// flip. The Lua identity `if C then A else B ≡ if not C then B else A` holds for
/// ANY expression C — C is still evaluated exactly once, in the same place, with
/// the same short-circuit behaviour; only the branch order swaps. `negate_canon`
/// realises this inverse losslessly: it strips a leading `not`, swaps `==`/`~=`,
/// and for everything else (relational `< <= > >=`, `and`/`or`, calls, …) simply
/// WRAPS in `not`. That wrap is purely structural — it does NOT push the `not`
/// inward (no De Morgan) and does NOT turn `not (a < b)` into the NaN-unsafe
/// `a >= b`. The pattern side is already in this wrapped form (`unguard` applies
/// `negate_canon` to every guard condition unconditionally), so the wrapped
/// candidate condition unifies with it EXACTLY. Hence the flip is value-exact and
/// NaN-safe for all conditions, and gating it added nothing but missed matches —
/// so it now admits every condition.
fn cond_exact_invertible(_c: &RValue) -> bool {
    true
}

pub(crate) fn canon(stmts: &[Statement]) -> Vec<Statement> {
    canon_tail(stmts, true)
}

/// Per-position memo for the tail-canon of a contiguous candidate window,
/// keyed by `(absolute_start, raw_width)`. Values are `Rc`-shared so a cache hit
/// hands out a cheap handle instead of re-running the deep-clone `canon`. Only
/// tail-position contiguous windows (the void attempt-1 path and the `match_value`
/// region) use it — both compute the identical `canon_recurse(canon_top(win,true))`,
/// so they share one cache safely. The non-contiguous `match_value_prefixed` union
/// and the rewritten `value_tail_ret` window are NOT cached (they are rare and not
/// a plain slice). The cache is cleared per position (`stmts` mutates on splice, so
/// absolute indices are only valid within a single `try_match_at` call).
#[derive(Default)]
struct CanonCache {
    windows: FxHashMap<(usize, usize), std::rc::Rc<Vec<Statement>>>,
    lengths: WindowLengths,
}

impl CanonCache {
    fn clear(&mut self) {
        self.windows.clear();
        self.lengths.clear();
    }

    fn top_len(&mut self, stmts: &[Statement], start: usize, width: usize) -> usize {
        self.lengths.get(stmts, start, width)
    }
}

/// Tiny windows keep the allocation-free length predicate. Repeated or growing
/// windows pay at most 64 raw statements of direct scans before switching to a
/// prefix summary. All keys belong to one immutable try_match_at invocation;
/// clearing at the next position also invalidates every splice/child revision.
struct WindowLengths {
    remaining_scan: usize,
    last: Option<(usize, usize, usize)>,
    prefixes: FxHashMap<usize, PrefixLengths>,
}

impl Default for WindowLengths {
    fn default() -> Self { Self { remaining_scan: 64, last: None, prefixes: Default::default() } }
}

impl WindowLengths {
    fn clear(&mut self) {
        self.remaining_scan = 64;
        self.last = None;
        self.prefixes.clear();
    }

    fn get(&mut self, stmts: &[Statement], start: usize, width: usize) -> usize {
        if let Some((previous_start, previous_width, length)) = self.last
            && (previous_start, previous_width) == (start, width)
        {
            return length;
        }
        let length = if !self.prefixes.contains_key(&start) && width <= self.remaining_scan {
            self.remaining_scan -= width;
            #[cfg(test)]
            canonical_tests::record_direct(width);
            canon_top_len(&stmts[start..start + width], true)
        } else {
            self.prefixes.entry(start).or_default().get(&stmts[start..], width)
        };
        self.last = Some((start, width, length));
        length
    }
}

/// Top-level canon length for every requested prefix of one start position.
/// The first foldable guard and non-trivia count completely describe N1/N2/N3.
/// Later guard bodies are never visited once the first foldable one is known.
#[derive(Default)]
struct PrefixLengths {
    lengths: Vec<usize>,
    effective: usize,
    first_guard: Option<usize>,
    last_void: bool,
}

impl PrefixLengths {
    fn get(&mut self, stmts: &[Statement], width: usize) -> usize {
        let from = self.lengths.len();
        for statement in &stmts[from..width.max(from)] {
            if !is_match_trivia(statement) {
                if self.first_guard.is_none() && is_foldable_guard(statement) {
                    self.first_guard = Some(self.effective);
                }
                self.effective += 1;
                self.last_void = matches!(statement, Statement::Return(ret) if ret.values.is_empty());
            }
            let effective = self.effective - usize::from(self.last_void);
            let length = self.first_guard.filter(|&guard| guard + 1 < effective)
                .map_or(effective, |guard| guard + 1);
            self.lengths.push(length);
        }
        if width > from {
            crate::telemetry::count("canonical_length_prefix_statements", (width - from) as u64);
            #[cfg(test)]
            canonical_tests::record_indexed(width - from);
        }
        let length = width.checked_sub(1).map_or(0, |index| self.lengths[index]);
        debug_assert_eq!(length, canon_top_len(&stmts[..width], true));
        length
    }
}

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
    if let Some(c) = cache.windows.get(&(start, w)) {
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
    cache.windows.insert((start, w), c.clone());
    c
}

/// `tail` = whether this block sits in the function's tail-control position, i.e.
/// a `return` here exits with *nothing* of the function left to run after it.
///
/// Stripping a trailing void `return` (N1) and folding guards (N3) are sound
/// ONLY at tail position. A `return` inside a non-tail nested block is non-local:
/// it skips not just the rest of *this* block but every statement after the
/// enclosing block too. Folding `if c then return end; REST` into
/// `if not c then REST end` there would wrongly let that outer continuation run
/// when the guard fires — conflating two different control flows. So below tail
/// position we leave returns intact; a surviving void return then trips the
/// `block_has_return` refusal gate (for patterns) / `plain_blocked` gate (for
/// candidates), keeping detection consistent and sound.
fn canon_tail(stmts: &[Statement], tail: bool) -> Vec<Statement> {
    let canonical = canon_recurse(canon_top(stmts, tail), tail);
    if tail { canonical } else { nest_value_return_guards(canonical) }
}

/// Below the function's tail a `return` still ends every path it is on, so
/// the statements after an `if` that returns a value on all its paths but
/// one belong on that path: `if a then return x end; if b then return y
/// end; REST` is `if a then return x elseif b then return y else REST end`.
/// A value helper's loop body returns in the first shape, its inlined copy
/// leaves through `break`s structured in the second (`match_value_loop`).
/// Blocks with a void `return` are left as written: the CPS matcher reads
/// their guards as they are.
fn nest_value_return_guards(stmts: Vec<Statement>) -> Vec<Statement> {
    fn returns(stmts: &[Statement], void: &mut bool, value: &mut bool) {
        for statement in stmts {
            match statement {
                Statement::Return(ret) if ret.values.is_empty() => *void = true,
                Statement::Return(_) => *value = true,
                Statement::If(branch) => {
                    returns(&branch.then_block.lock().0, void, value);
                    returns(&branch.else_block.lock().0, void, value);
                }
                _ => {}
            }
        }
    }
    let at = stmts.iter().position(|statement| {
        let Statement::If(branch) = statement else { return false };
        let (mut void, mut value) = (false, false);
        returns(std::slice::from_ref(statement), &mut void, &mut value);
        value && !void && is_open_guard(branch)
    });
    let Some(at) = at else { return stmts };
    if stmts[at + 1..].iter().all(is_match_trivia) {
        return stmts;
    }
    let mut stmts = stmts;
    let rest = nest_value_return_guards(stmts.split_off(at + 1));
    let guard = stmts.pop().expect("the guard");
    stmts.extend(graft(vec![guard], rest));
    stmts
}

/// The *top-level* (non-recursive) half of canon: the only edits that change the
/// statement count — N2 (drop `Empty`), and at tail N1 (drop a trailing void
/// `return`) and N3 (un-guard). It does NOT descend into nested blocks, so
/// `canon_top(w, tail).len() == canon_tail(w, tail).len()` exactly (the recursive
/// half is a 1:1 statement map). Callers use this to reject a candidate window by
/// length *before* paying for the deep nested-block rebuild in `canon_recurse`.
fn canon_top(stmts: &[Statement], tail: bool) -> Vec<Statement> {
    // N2: drop Empty placeholders AND our own reconstruction markers (runtime
    // no-ops). An inner de-inline may have spliced a CALL_MARKER into a shared
    // callee body mid-fixed-point; dropping it here keeps a re-collected pattern
    // length-aligned with its candidate windows (both stripped symmetrically), and
    // a stripped marker contributes 0 anchors / does not perturb the length gates.
    let mut s: Vec<Statement> = stmts
        .iter()
        .filter(|st| {
            !matches!(st, Statement::Empty(_))
                && !matches!(st, Statement::Comment(c) if is_internal_marker(c))
        })
        .cloned()
        .collect();
    if tail {
        // N1: drop a trailing void return at THIS (tail) level (implicit-return no-op).
        if matches!(s.last(), Some(Statement::Return(r)) if r.values.is_empty()) {
            s.pop();
        }
        // N3: un-guard at THIS (tail) level — the guards' `[return]` then-blocks
        // are still intact here (we have NOT yet recursed into child blocks).
        s = unguard(s);
    }
    s
}

/// Replace `if c then x = a else x = b end` with `x = <select_value(c, a, b)>`,
/// the form SSA gives a register select when it can. Matching fuses it on both
/// sides, so a helper and its inlined copy agree whichever of them SSA fused.
fn fuse_assign_diamond(statement: &mut Statement) {
    fn single_store(block: &Block) -> Option<(RcLocal, RValue)> {
        let mut real = block.0.iter().filter(|s| !matches!(s, Statement::Empty(_)));
        match (real.next(), real.next()) {
            (Some(Statement::Assign(a)), None)
                if !a.prefix && !a.parallel && !a.compound && a.left.len() == 1 && a.right.len() == 1 =>
            {
                let LValue::Local(local) = &a.left[0] else { return None };
                Some((local.clone(), a.right[0].clone()))
            }
            _ => None,
        }
    }
    let Statement::If(f) = &*statement else {
        return;
    };
    let (Some((then_local, then_value)), Some((else_local, else_value))) =
        (single_store(&f.then_block.lock()), single_store(&f.else_block.lock()))
    else {
        return;
    };
    if then_local != else_local {
        return;
    }
    let mut condition = f.condition.clone();
    if let Some(value) = crate::select_value(&mut condition, then_value, else_value) {
        *statement = Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(then_local)],
            right: vec![value],
            prefix: false,
            parallel: false,
            compound: false,
        });
    }
}

/// `local u = E; return u or b` (or `u and b`) as `return E or b`: the step SSA
/// takes after fusing a select that read `u` twice into one that reads it
/// once, which is how the copy Luau inlined into a caller reads. `u` must be
/// read only there, first, and not by `E`. Nested blocks only: the top-level
/// statement count of a window is what the length gates measure.
fn fold_select_temp(stmts: &mut Vec<Statement>) {
    let real: Vec<usize> = (0..stmts.len()).filter(|&i| !is_match_trivia(&stmts[i])).collect();
    let [.., decl_at, ret_at] = real[..] else { return };
    let (Statement::Assign(decl), Statement::Return(ret)) = (&stmts[decl_at], &stmts[ret_at]) else {
        return;
    };
    let ([LValue::Local(temp)], [init], [RValue::Binary(select)]) =
        (decl.left.as_slice(), decl.right.as_slice(), ret.values.as_slice())
    else {
        return;
    };
    if !decl.prefix
        || decl.parallel
        || !matches!(select.operation, BinaryOperation::Or | BinaryOperation::And)
        || !matches!(select.left.as_ref(), RValue::Local(read) if read == temp)
        || select.right.any_local_read(&mut |read| read == temp)
        || init.any_local_read(&mut |read| read == temp)
        || !is_scalar_return_value(init)
    {
        return;
    }
    let folded = Binary::new(init.clone(), select.right.as_ref().clone(), select.operation).into();
    stmts[ret_at] = Statement::Return(Return { node_origin: Default::default(), values: vec![folded] });
    stmts.remove(decl_at);
}

/// Replace a trailing `if c then return a else return b end` (one scalar value
/// each) with `return <select_value(c, a, b)>`. One statement for one, so the
/// canonical length is unchanged.
fn fuse_return_diamond(stmts: &mut [Statement]) {
    // One value each: a scalar, or a call adjusted to one result
    // (`return (f())`), which the select reads as an operand.
    fn single_value(block: &Block) -> Option<RValue> {
        let mut real = block.0.iter().filter(|s| !matches!(s, Statement::Empty(_)));
        match (real.next(), real.next()) {
            (Some(Statement::Return(r)), None)
                if r.values.len() == 1
                    && (is_scalar_return_value(&r.values[0]) || matches!(r.values[0], RValue::Select(_))) =>
            {
                Some(r.values[0].clone())
            }
            _ => None,
        }
    }
    let Some(Statement::If(f)) = stmts.last() else {
        return;
    };
    let (Some(then_value), Some(else_value)) =
        (single_value(&f.then_block.lock()), single_value(&f.else_block.lock()))
    else {
        return;
    };
    let mut condition = f.condition.clone();
    if let Some(value) = crate::select_value(&mut condition, then_value, else_value) {
        *stmts.last_mut().unwrap() = Statement::Return(Return {
            node_origin: Default::default(),
            values: vec![value],
        });
    }
}

/// The recursive half of canon: canonicalise each statement's child blocks.
/// A 1:1 statement map, so it never changes the count produced by `canon_top`.
/// Tail-position only propagates to the LAST statement's branches (and never
/// into loop bodies, where a `return` exits across iterations + the loop).
fn canon_recurse(s: Vec<Statement>, tail: bool) -> Vec<Statement> {
    let n = s.len();
    // Consume the owned Vec by value: a leaf statement (Assign/Call/Return/…) is
    // MOVED through untouched — no second deep clone — since `canon_top` already
    // produced this owned Vec. Only the block-bearing statements are rebuilt (their
    // child blocks are re-canon'd into fresh `Block`s; the shared original block Arc
    // must not be mutated). This halves the per-leaf clone cost in the hot matcher.
    let mut s: Vec<Statement> = s
        .into_iter()
        .enumerate()
        .map(|(j, st)| canon_children_owned(st, tail && j + 1 == n))
        .collect();
    // Select diamonds are matched in their `and`/`or` form, the one SSA gives
    // the copy Luau inlined into a caller where the value lands in a register
    // (`if c then return a end return b` ~ `v = c and a or b`). Post-order, so
    // a chain of them fuses inside out as SSA does. One statement for one:
    // `canon_top_len` still describes the result.
    for statement in &mut s {
        fuse_assign_diamond(statement); // N5, any level
    }
    if tail {
        fuse_return_diamond(&mut s); // N4, a trailing return diamond
    }
    s
}

fn canon_children_owned(s: Statement, tail: bool) -> Statement {
    let arm = |block: &Block| {
        let mut stmts = canon_tail(&block.0, tail);
        if tail {
            fold_select_temp(&mut stmts);
        }
        stmts
    };
    match s {
        Statement::If(f) => Statement::If(If::new(
            f.condition,
            Block(arm(&f.then_block.lock())),
            Block(arm(&f.else_block.lock())),
        )),
        Statement::While(w) => Statement::While(While::new(
            w.condition,
            Block(canon_tail(&w.block.lock().0, false)),
        )),
        Statement::Repeat(r) => Statement::Repeat(Repeat::new(
            r.condition,
            Block(canon_tail(&r.block.lock().0, false)),
        )),
        Statement::NumericFor(nf) => Statement::NumericFor(Box::new(NumericFor {
            block: Arc::new(Mutex::new(Block(canon_tail(&nf.block.lock().0, false)))),
            initial: nf.initial,
            limit: nf.limit,
            step: nf.step,
            counter: nf.counter,
        })),
        Statement::GenericFor(gf) => Statement::GenericFor(GenericFor {
            res_locals: gf.res_locals,
            right: gf.right,
            block: Arc::new(Mutex::new(Block(canon_tail(&gf.block.lock().0, false)))),
            origin: gf.origin,
        }),
        other => other,
    }
}

/// `canon_top` already cloned these statements, including their expression
/// origins. Consume that owned copy: cloning it again only repeats payload and
/// operand allocations (Origin::clone's `cloned` flag is already set). Child
/// block Arcs remain shared, so their early-return prefix still needs a clone.
fn unguard(stmts: Vec<Statement>) -> Vec<Statement> {
    unguard_owned(stmts.into_iter())
}

fn unguard_owned(mut stmts: std::vec::IntoIter<Statement>) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::new();
    while let Some(statement) = stmts.next() {
        if let Statement::If(f) = &statement {
            // A guard may do work before returning:
            // `if cond then PREFIX; return [X] end; REST`.  Re-nest the shared
            // continuation into the exact structured form produced at inlined
            // sites: `if not cond then REST else PREFIX; return X end`.  For a
            // void return the terminal return is omitted; tail fall-through is
            // equivalent and `PREFIX` remains in the else arm.
            let guard: Option<(Vec<Statement>, Option<RValue>)> = {
                let then = f.then_block.lock();
                let els = f.else_block.lock();
                if els.0.is_empty() {
                    match then.0.split_last() {
                        Some((Statement::Return(r), prefix)) if r.values.is_empty() => {
                            Some((prefix.to_vec(), None))
                        }
                        Some((Statement::Return(r), prefix)) if r.values.len() == 1 => {
                            Some((prefix.to_vec(), Some(r.values[0].clone())))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            };
            let open = guard.is_none() && is_open_guard(f);
            if let Some((mut early_prefix, ret_val)) = guard {
                if !stmts.as_slice().is_empty() {
                    let Statement::If(f) = statement else { unreachable!() };
                    let cond = f.condition;
                    let folded = unguard_owned(stmts);
                    if let Some(x) = ret_val {
                        early_prefix.push(Statement::Return(Return { node_origin: Default::default(), values: vec![x] }));
                    }
                    out.push(Statement::If(If::new(
                        negate_canon(cond),
                        Block(folded),
                        Block(early_prefix),
                    )));
                    return out;
                }
            } else if open && !stmts.as_slice().is_empty() {
                let folded = unguard_owned(stmts);
                out.extend(graft(vec![statement], folded));
                return out;
            }
        }
        out.push(statement);
    }
    out
}

/// Paths through `stmts` that fall off its end (0 when every one returns), or
/// `None` for a leaf canon does not fold across: a multi-value return, or
/// control that leaves the block another way.
fn open_ends(stmts: &[Statement]) -> Option<usize> {
    match stmts.iter().rev().find(|s| !is_match_trivia(s)) {
        None => Some(1),
        Some(Statement::Return(r)) => (r.values.len() <= 1).then_some(0),
        Some(Statement::If(f)) => {
            let then = open_ends(&f.then_block.lock().0)?;
            let els = open_ends(&f.else_block.lock().0)?;
            Some(then + els)
        }
        Some(Statement::Break(_) | Statement::Continue(_) | Statement::Goto(_) | Statement::Label(_)) => None,
        Some(_) => Some(1),
    }
}

/// An `if` that returns on every path but one, beyond the single guard: what
/// follows it runs only on that path, so it belongs at that path's end
/// (`if a then return x elseif b then return y end REST` is `if a then return
/// x elseif b then return y else REST end`).
fn is_open_guard(f: &If) -> bool {
    let then = open_ends(&f.then_block.lock().0);
    let els = open_ends(&f.else_block.lock().0);
    matches!((then, els), (Some(a), Some(b)) if a + b == 1)
}

/// `stmts` with `rest` placed at its one open end (see `open_ends`). Blocks
/// are shared, so a grafted `if` is rebuilt rather than changed in place.
fn graft(mut stmts: Vec<Statement>, rest: Vec<Statement>) -> Vec<Statement> {
    if let Some(at) = stmts.iter().rposition(|s| !is_match_trivia(s))
        && let Statement::If(f) = &stmts[at]
    {
        let then = f.then_block.lock().0.clone();
        let els = f.else_block.lock().0.clone();
        let (then, els) = if open_ends(&then) == Some(1) {
            (graft(then, rest), els)
        } else {
            (then, graft(els, rest))
        };
        stmts[at] = If::new(f.condition.clone(), Block(then), Block(els)).into();
        return stmts;
    }
    stmts.extend(rest);
    stmts
}

/// The foldable-guard shape `unguard` collapses: `if cond then return [X] end`
/// (empty else, then-block a single 0-or-1-value return). Factored out so
/// `canon_top_len` computes the post-unguard length using the EXACT same predicate
/// `unguard` folds on (no drift); `canon_top_len`'s debug_assert cross-checks the
/// whole length against the real `canon_top`.
fn is_foldable_guard(s: &Statement) -> bool {
    if let Statement::If(f) = s {
        let guard = {
            let then = f.then_block.lock();
            let els = f.else_block.lock();
            els.0.is_empty()
                && matches!(then.0.last(), Some(Statement::Return(r)) if r.values.len() <= 1)
        };
        guard || is_open_guard(f)
    } else {
        false
    }
}

/// `canon_top(stmts, tail).len()` WITHOUT allocating the canon'd Vec — a hot-path
/// pre-filter so the per-width matcher loops pay the `canon_top` + `canon_recurse`
/// allocations only on a window whose canon'd length actually equals the pattern
/// length (the common case in the width scan is a NON-match). Mirrors `canon_top`
/// exactly: count non-trivia (N2); at tail, drop a trailing void return (N1) then
/// apply `unguard`'s length effect (N3 — the FIRST foldable guard that has a
/// following effective statement folds everything after it into one `If`, so the
/// length becomes that guard's index + 1; otherwise no change). The debug_assert
/// pins it to the real `canon_top` length in debug / test builds.
fn canon_top_len(stmts: &[Statement], tail: bool) -> usize {
    let n = canon_top_len_of(stmts.iter(), tail);
    debug_assert_eq!(
        n,
        canon_top(stmts, tail).len(),
        "canon_top_len must mirror canon_top length exactly"
    );
    n
}

/// `canon_top_len` over statements that need not be contiguous (a callee
/// prefix followed by a region, without building the joined window).
fn canon_top_len_of<'a, I>(stmts: I, tail: bool) -> usize
where
    I: DoubleEndedIterator<Item = &'a Statement> + Clone,
{
    dprof::inc(&dprof::CANON_TOP_LEN_CALLS, 1);
    crate::telemetry::count("canonical_top_length_calls", 1);
    let _t = dprof::T::new(&dprof::CANON_TOP_LEN_US);
    let total = stmts.clone().filter(|s| !is_match_trivia(s)).count();
    if !tail || total == 0 {
        return total;
    }
    // N1: drop a trailing void return (the LAST non-trivia statement).
    let last_void = stmts
        .clone()
        .rev()
        .find(|s| !is_match_trivia(s))
        .is_some_and(|s| matches!(s, Statement::Return(r) if r.values.is_empty()));
    let effective = total - usize::from(last_void);
    // N3: unguard folds at the first foldable guard that has a following
    // (within-`effective`) statement -> top-level length is its index + 1.
    let mut len = effective;
    for (idx, s) in stmts
        .filter(|s| !is_match_trivia(s))
        .take(effective)
        .enumerate()
    {
        if idx + 1 < effective && is_foldable_guard(s) {
            len = idx + 1;
            break;
        }
    }
    len
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
// Unifier
// ===================================================================

fn unify_block(
    t: &Target,
    pat: &[Statement],
    cand: &[Statement],
    b: &mut Bindings,
) -> Result<(), ()> {
    if pat.len() != cand.len() {
        return Err(());
    }
    for (p, c) in pat.iter().zip(cand) {
        unify_stmt(t, p, c, b)?;
    }
    Ok(())
}

fn unify_stmt(t: &Target, p: &Statement, c: &Statement, b: &mut Bindings) -> Result<(), ()> {
    // The expression-level unifier only needs the binding-hole sets; build the
    // shared context once. `unify_block` (statement-level) still needs `t.kind`,
    // so it keeps taking `t`.
    let ctx = t.ctx();
    match (p, c) {
        (Statement::Assign(pa), Statement::Assign(ca)) => {
            // `prefix` distinguishes a `local x = ...` declaration from a plain
            // `x = ...` reassignment — they are NOT interchangeable. Matching a
            // declaration against a reassignment (or vice versa) would erase a
            // write to a caller-visible local. An inlined copy preserves the
            // callee's `local`, so genuine matches keep equal prefixes.
            // `t[k] += v` evaluates `t` and `k` once, `t[k] = t[k] + v` twice.
            if pa.left.len() != ca.left.len()
                || pa.right.len() != ca.right.len()
                || pa.parallel != ca.parallel
                || pa.prefix != ca.prefix
                || pa.compound != ca.compound
            {
                return Err(());
            }
            for (pl, cl) in pa.left.iter().zip(&ca.left) {
                unify_lvalue(&ctx, pl, cl, b)?;
            }
            for (pr, cr) in pa.right.iter().zip(&ca.right) {
                unify_rvalue(&ctx, pr, cr, b)?;
            }
            Ok(())
        }
        (Statement::Call(pc), Statement::Call(cc)) => unify_call(&ctx, pc, cc, b),
        (Statement::MethodCall(pm), Statement::MethodCall(cm)) => unify_method(&ctx, pm, cm, b),
        // §8 guard-polarity flip (Value targets only). The call-site value
        // lowering keeps a guard's if/else in POSITIVE polarity
        // (`if C then RESULT=early else REST`), whereas `canon` un-guards the
        // callee body into the NEGATED form (`if not C then REST else return
        // early`). These are the exact Lua identity `if C then A else B ≡
        // if not C then B else A`. Try a DIRECT unify first (on a clone, so a
        // partial failure does not pollute `b`); on failure, retry with the
        // condition negated and the then/else branches swapped. P9: this is now
        // attempted for EVERY candidate condition — `negate_canon` realises the
        // inverse losslessly by structural `not`-wrap (NO De Morgan, NO `<`→`>=`),
        // and the pattern side is already wrapped the same way by `unguard`, so the
        // wrapped candidate condition unifies EXACTLY. Value-exact and NaN-safe for
        // all conditions; `cond_exact_invertible` (now always true) is kept as the
        // documented gate point. Gated to Value targets so the void matches keep
        // their original clone-free path.
        (Statement::If(pf), Statement::If(cf)) if t.kind == TKind::Value => {
            let mut bd = b.clone();
            let direct = unify_rvalue(&ctx, &pf.condition, &cf.condition, &mut bd)
                .and_then(|_| {
                    unify_block(t, &pf.then_block.lock().0, &cf.then_block.lock().0, &mut bd)
                })
                .and_then(|_| {
                    unify_block(t, &pf.else_block.lock().0, &cf.else_block.lock().0, &mut bd)
                });
            if direct.is_ok() {
                *b = bd;
                return Ok(());
            }
            if cond_exact_invertible(&cf.condition) {
                unify_rvalue(&ctx, &pf.condition, &negate_canon(cf.condition.clone()), b)?;
                unify_block(t, &pf.then_block.lock().0, &cf.else_block.lock().0, b)?;
                unify_block(t, &pf.else_block.lock().0, &cf.then_block.lock().0, b)
            } else {
                Err(())
            }
        }
        (Statement::If(pf), Statement::If(cf)) => {
            unify_rvalue(&ctx, &pf.condition, &cf.condition, b)?;
            unify_block(t, &pf.then_block.lock().0, &cf.then_block.lock().0, b)?;
            unify_block(t, &pf.else_block.lock().0, &cf.else_block.lock().0, b)
        }
        (Statement::While(pw), Statement::While(cw)) => {
            unify_rvalue(&ctx, &pw.condition, &cw.condition, b)?;
            unify_block(t, &pw.block.lock().0, &cw.block.lock().0, b)
        }
        (Statement::Repeat(pr), Statement::Repeat(cr)) => {
            unify_rvalue(&ctx, &pr.condition, &cr.condition, b)?;
            unify_block(t, &pr.block.lock().0, &cr.block.lock().0, b)
        }
        (Statement::NumericFor(pn), Statement::NumericFor(cn)) => {
            unify_rvalue(&ctx, &pn.initial, &cn.initial, b)?;
            unify_rvalue(&ctx, &pn.limit, &cn.limit, b)?;
            unify_rvalue(&ctx, &pn.step, &cn.step, b)?;
            unify_local(&ctx, &pn.counter, &cn.counter, b)?;
            unify_block(t, &pn.block.lock().0, &cn.block.lock().0, b)
        }
        (Statement::GenericFor(pg), Statement::GenericFor(cg)) => {
            if pg.res_locals.len() != cg.res_locals.len() || pg.right.len() != cg.right.len() {
                return Err(());
            }
            for (pl, cl) in pg.res_locals.iter().zip(&cg.res_locals) {
                unify_local(&ctx, pl, cl, b)?;
            }
            for (pr, cr) in pg.right.iter().zip(&cg.right) {
                unify_rvalue(&ctx, pr, cr, b)?;
            }
            unify_block(t, &pg.block.lock().0, &cg.block.lock().0, b)
        }
        (Statement::Return(pr), Statement::Return(cr)) => match (pr.values.as_slice(), cr.values.as_slice()) {
            ([], []) => Ok(()),
            // A value helper's `return x` from inside its loop, which
            // `unflag_value_loop` gives back from the site's `r = x; flag =
            // false; break` (a site window has no `return` of its own).
            ([pattern], [site]) if t.loop_exit_at.is_some() => unify_returned_value(&ctx, pattern, site, b),
            _ => Err(()),
        },
        // Value target: the callee's `return X` was lowered to `RESULT = X` in
        // the inlined copy. Bind the single result local and unify the value.
        // The result-write leaf is always a PLAIN reassignment (`RESULT = X`):
        // `RESULT` is the init-less decl pinned by `result_decl` (prefix=true), and
        // LocalDeclarer's single-declaration invariant (local_declarations.rs) gives
        // each local exactly ONE prefix=true decl, so every in-region write to it is
        // prefix=false / parallel=false. A `local RESULT = X` redeclaration or a
        // parallel phi-copy here would change scope when spliced, so refuse it —
        // mirrors the sibling (Assign, Assign) arm's prefix/parallel equality and
        // `result_decl`'s own `prefix && !parallel` gate (F10a hardening).
        (Statement::Return(pr), Statement::Assign(ca))
            if t.kind == TKind::Value
                && pr.values.len() == 1
                && ca.left.len() == 1
                && ca.right.len() == 1
                && !ca.prefix
                && !ca.parallel =>
        {
            let r = match &ca.left[0] {
                LValue::Local(r) => r,
                _ => return Err(()),
            };
            match &b.result {
                Some(prev) if prev == r => {}
                Some(_) => return Err(()), // two different result locals
                None => b.result = Some(r.clone()),
            }
            unify_returned_value(&ctx, &pr.values[0], &ca.right[0], b)
        }
        (Statement::Break(_), Statement::Break(_)) => Ok(()),
        (Statement::Continue(_), Statement::Continue(_)) => Ok(()),
        (Statement::SetList(ps), Statement::SetList(cs)) => {
            if ps.index != cs.index || ps.values.len() != cs.values.len() {
                return Err(());
            }
            unify_local(&ctx, &ps.object_local, &cs.object_local, b)?;
            for (x, y) in ps.values.iter().zip(&cs.values) {
                unify_rvalue(&ctx, x, y, b)?;
            }
            match (&ps.tail, &cs.tail) {
                (Some(x), Some(y)) => unify_rvalue(&ctx, x, y, b),
                (None, None) => Ok(()),
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

/// The value a helper returns against the one its inlined copy stores into
/// the result local, which takes one value: a call leaf (P7-A) is the call
/// the store adjusts to one result.
fn unify_returned_value(ctx: &MatchCtx, pattern: &RValue, site: &RValue, b: &mut Bindings) -> Result<(), ()> {
    match (pattern, site) {
        (RValue::Call(x), RValue::Select(Select::Call(y))) => unify_call(ctx, x, y, b),
        (RValue::MethodCall(x), RValue::Select(Select::MethodCall(y))) => unify_method(ctx, x, y, b),
        (pattern, site) => unify_rvalue(ctx, pattern, site, b),
    }
}

pub(crate) fn unify_lvalue(
    ctx: &MatchCtx,
    p: &LValue,
    c: &LValue,
    b: &mut Bindings,
) -> Result<(), ()> {
    match (p, c) {
        (LValue::Local(pl), LValue::Local(cl)) => unify_local(ctx, pl, cl, b),
        (LValue::Global(a), LValue::Global(d)) => {
            if a == d {
                Ok(())
            } else {
                Err(())
            }
        }
        (LValue::Index(a), LValue::Index(d)) => {
            unify_rvalue(ctx, &a.left, &d.left, b)?;
            unify_rvalue(ctx, &a.right, &d.right, b)
        }
        _ => Err(()),
    }
}

/// Structural expression unifier. Treats `ctx.params` as bind-once holes
/// (mapping each callee parameter to one caller argument expression) and
/// `ctx.locals` as an injective callee-local renaming; everything else (globals,
/// literals bit for bit, operators, method/field names, upvalues by
/// `RcLocal` identity) must match EXACTLY — no commutativity, associativity, or
/// boolean rewriting. Shared by the statement de-inliner and the §7 expression
/// de-inliner (`crate::expr_deinline`).
pub(crate) fn unify_rvalue(
    ctx: &MatchCtx,
    p: &RValue,
    c: &RValue,
    b: &mut Bindings,
) -> Result<(), ()> {
    if let RValue::Local(pl) = p {
        if ctx.params.contains(pl) {
            if let Some(prev) = b.params.get(pl) {
                // Repeated occurrence: it must be the SAME value as the first bind
                // (NaN/sign-of-zero-exact, not derived `f64` eq), AND not an
                // identity-producing expression — two `{}`/closures are distinct
                // values, so sharing one across occurrences (emitting `f({})`
                // once) would diverge from the region's per-use construction.
                // Single-use params never reach here (they bind below).
                return if rvalue_exact_eq(prev, c) && !is_identity_producing(c) {
                    Ok(())
                } else {
                    Err(())
                };
            }
            // closures as arguments are refused (identity matching is unsound)
            if matches!(c, RValue::Closure(_)) {
                return Err(());
            }
            // Argument hoist-safety (side effects / value stability) is enforced
            // once, after unification, by the caller (`try_unify_site` for the
            // statement pass; the arg-safety gate for the expression pass).
            b.params.insert(pl.clone(), c.clone());
            return Ok(());
        }
        if ctx.locals.contains(pl) {
            return match c {
                RValue::Local(cl) => unify_local(ctx, pl, cl, b),
                _ => Err(()),
            };
        }
        // external / upvalue: must be the exact same local (pointer identity)
        return match c {
            RValue::Local(cl) if cl == pl => Ok(()),
            _ => Err(()),
        };
    }
    match (p, c) {
        (RValue::Global(a), RValue::Global(d)) => {
            if a == d {
                Ok(())
            } else {
                Err(())
            }
        }
        (RValue::Literal(a), RValue::Literal(d)) => {
            if a == d {
                Ok(())
            } else {
                Err(())
            }
        }
        (RValue::Index(a), RValue::Index(d)) => {
            unify_rvalue(ctx, &a.left, &d.left, b)?;
            unify_rvalue(ctx, &a.right, &d.right, b)
        }
        (RValue::Unary(a), RValue::Unary(d)) => {
            if a.operation == d.operation {
                unify_rvalue(ctx, &a.value, &d.value, b)
            } else {
                Err(())
            }
        }
        (RValue::Binary(a), RValue::Binary(d)) => {
            if a.operation == d.operation {
                unify_rvalue(ctx, &a.left, &d.left, b)?;
                unify_rvalue(ctx, &a.right, &d.right, b)
            } else {
                Err(())
            }
        }
        // Luau `if c then a else b` EXPRESSION. Positional only — NO branch swap,
        // NO polarity flip (the §8 `cond_exact_invertible` flip is statement-only
        // and NaN-unsafe to reuse here). Required by the §7 expression de-inliner;
        // inert for the statement pass (its patterns are pre-reconstruct and never
        // contain an `IfExpression`).
        (RValue::IfExpression(a), RValue::IfExpression(d)) => {
            unify_rvalue(ctx, &a.condition, &d.condition, b)?;
            unify_rvalue(ctx, &a.then_value, &d.then_value, b)?;
            unify_rvalue(ctx, &a.else_value, &d.else_value, b)
        }
        (RValue::Call(a), RValue::Call(d)) => unify_call(ctx, a, d, b),
        (RValue::MethodCall(a), RValue::MethodCall(d)) => unify_method(ctx, a, d, b),
        (RValue::Table(a), RValue::Table(d)) => unify_table(ctx, a, d, b),
        (RValue::Closure(a), RValue::Closure(d)) => unify_closure(ctx, a, d, b),
        (RValue::VarArg(_), RValue::VarArg(_)) => Ok(()),
        (RValue::Select(a), RValue::Select(d)) => unify_select(ctx, a, d, b),
        _ => Err(()),
    }
}

/// Match one nested closure constructor by bytecode provenance and captures.
///
/// Luau can instantiate the same child prototype at both the original helper
/// definition and an `-O2`-inlined call site. Those instances have different
/// AST pointers but execute the same bytecode. Matching the prototype id alone
/// would still be insufficient: a different capture mode or captured value can
/// change mutation/snapshot semantics. Requiring the ordered `Copy`/`Ref` list
/// to unify under the enclosing helper's parameter/local mapping supplies the
/// missing proof without recursively comparing large callback bodies.
fn unify_closure(
    ctx: &MatchCtx,
    pattern: &Closure,
    candidate: &Closure,
    bindings: &mut Bindings,
) -> Result<(), ()> {
    let same_function = Arc::ptr_eq(&pattern.function.0, &candidate.function.0);
    if !same_function {
        let pattern_proto = pattern.function.0.lock().bytecode_proto_id;
        let candidate_proto = candidate.function.0.lock().bytecode_proto_id;
        if pattern_proto.is_none() || pattern_proto != candidate_proto {
            return Err(());
        }
    }

    if pattern.upvalues.len() != candidate.upvalues.len() {
        return Err(());
    }
    for (pattern_upvalue, candidate_upvalue) in pattern.upvalues.iter().zip(&candidate.upvalues) {
        let (pattern_local, candidate_local) = match (pattern_upvalue, candidate_upvalue) {
            (Upvalue::Copy(pattern), Upvalue::Copy(candidate))
            | (Upvalue::Ref(pattern), Upvalue::Ref(candidate)) => (pattern, candidate),
            _ => return Err(()),
        };
        unify_rvalue(
            ctx,
            &RValue::Local(pattern_local.clone()),
            &RValue::Local(candidate_local.clone()),
            bindings,
        )?;
    }
    Ok(())
}

/// A parameter holds one value. A call or `...` where all its values are
/// taken (a last argument or list item) passes more than one, which the call
/// to the helper would adjust away: `print("tag", produce())` is not
/// `helper(produce())` for a helper `print("tag", p)`.
fn spreads_into_parameter(ctx: &MatchCtx, pattern: Option<&RValue>, candidate: Option<&RValue>) -> bool {
    matches!(pattern, Some(RValue::Local(p)) if ctx.params.contains(p))
        && matches!(candidate, Some(RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_)))
}

fn unify_call(ctx: &MatchCtx, a: &Call, d: &Call, b: &mut Bindings) -> Result<(), ()> {
    if a.arguments.len() != d.arguments.len() || spreads_into_parameter(ctx, a.arguments.last(), d.arguments.last()) {
        return Err(());
    }
    unify_rvalue(ctx, &a.value, &d.value, b)?;
    for (x, y) in a.arguments.iter().zip(&d.arguments) {
        unify_rvalue(ctx, x, y, b)?;
    }
    Ok(())
}

fn unify_method(
    ctx: &MatchCtx,
    a: &MethodCall,
    d: &MethodCall,
    b: &mut Bindings,
) -> Result<(), ()> {
    if a.method != d.method
        || a.arguments.len() != d.arguments.len()
        || spreads_into_parameter(ctx, a.arguments.last(), d.arguments.last())
    {
        return Err(());
    }
    unify_rvalue(ctx, &a.value, &d.value, b)?;
    for (x, y) in a.arguments.iter().zip(&d.arguments) {
        unify_rvalue(ctx, x, y, b)?;
    }
    Ok(())
}

fn unify_table(ctx: &MatchCtx, a: &Table, d: &Table, b: &mut Bindings) -> Result<(), ()> {
    fn last_item(table: &Table) -> Option<&RValue> {
        match table.0.last() {
            Some((None, value)) => Some(value),
            _ => None,
        }
    }
    if a.0.len() != d.0.len() || spreads_into_parameter(ctx, last_item(a), last_item(d)) {
        return Err(());
    }
    for ((ak, av), (dk, dv)) in a.0.iter().zip(&d.0) {
        match (ak, dk) {
            (Some(x), Some(y)) => unify_rvalue(ctx, x, y, b)?,
            (None, None) => {}
            _ => return Err(()),
        }
        unify_rvalue(ctx, av, dv, b)?;
    }
    Ok(())
}

fn unify_select(ctx: &MatchCtx, a: &Select, d: &Select, b: &mut Bindings) -> Result<(), ()> {
    match (a, d) {
        (Select::Call(x), Select::Call(y)) => unify_call(ctx, x, y, b),
        (Select::MethodCall(x), Select::MethodCall(y)) => unify_method(ctx, x, y, b),
        (Select::VarArg(_), Select::VarArg(_)) => Ok(()),
        _ => Err(()),
    }
}

pub(crate) fn unify_local(
    ctx: &MatchCtx,
    pl: &RcLocal,
    cl: &RcLocal,
    b: &mut Bindings,
) -> Result<(), ()> {
    if ctx.locals.contains(pl) {
        if let Some(prev) = b.locals.get(pl) {
            return if prev == cl { Ok(()) } else { Err(()) };
        }
        if let Some(other) = b.locals_rev.get(cl) {
            if other != pl {
                return Err(()); // injectivity
            }
        }
        b.locals.insert(pl.clone(), cl.clone());
        b.locals_rev.insert(cl.clone(), pl.clone());
        return Ok(());
    }
    // param-as-binder or external: identity
    if pl == cl { Ok(()) } else { Err(()) }
}

/// Structural equality for the correctness gates: literals bit for bit, and
/// closures by `Function` pointer identity only — two `{}`/closures are distinct values and must never
/// be treated as equal by structure. Use this anywhere a gate's soundness depends
/// on two reconstructed expressions being the SAME value (return-folding,
/// repeated-argument consistency, recorded-site agreement).
pub(crate) fn rvalue_exact_eq(a: &RValue, b: &RValue) -> bool {
    match (a, b) {
        (RValue::Local(x), RValue::Local(y)) => x == y,
        (RValue::Global(x), RValue::Global(y)) => x == y,
        (RValue::Literal(x), RValue::Literal(y)) => x == y,
        (RValue::VarArg(_), RValue::VarArg(_)) => true,
        (RValue::Unary(x), RValue::Unary(y)) => {
            x.operation == y.operation && rvalue_exact_eq(&x.value, &y.value)
        }
        (RValue::Binary(x), RValue::Binary(y)) => {
            x.operation == y.operation
                && rvalue_exact_eq(&x.left, &y.left)
                && rvalue_exact_eq(&x.right, &y.right)
        }
        (RValue::Index(x), RValue::Index(y)) => {
            rvalue_exact_eq(&x.left, &y.left) && rvalue_exact_eq(&x.right, &y.right)
        }
        (RValue::IfExpression(x), RValue::IfExpression(y)) => {
            rvalue_exact_eq(&x.condition, &y.condition)
                && rvalue_exact_eq(&x.then_value, &y.then_value)
                && rvalue_exact_eq(&x.else_value, &y.else_value)
        }
        (RValue::Call(x), RValue::Call(y)) => {
            call_exact_eq(&x.value, &x.arguments, &y.value, &y.arguments)
        }
        (RValue::MethodCall(x), RValue::MethodCall(y)) => {
            x.method == y.method && call_exact_eq(&x.value, &x.arguments, &y.value, &y.arguments)
        }
        (RValue::Table(x), RValue::Table(y)) => {
            x.0.len() == y.0.len()
                && x.0.iter().zip(&y.0).all(|((kx, vx), (ky, vy))| {
                    (match (kx, ky) {
                        (Some(kx), Some(ky)) => rvalue_exact_eq(kx, ky),
                        (None, None) => true,
                        _ => false,
                    }) && rvalue_exact_eq(vx, vy)
                })
        }
        // The same function capturing the same cells the same way.
        (RValue::Closure(x), RValue::Closure(y)) => {
            Arc::as_ptr(&x.function.0) == Arc::as_ptr(&y.function.0) && x.upvalues == y.upvalues
        }
        // Select wraps Call/MethodCall/VarArg: recurse so a closure argument
        // still compares by identity.
        (RValue::Select(x), RValue::Select(y)) => match (x, y) {
            (Select::Call(x), Select::Call(y)) => {
                call_exact_eq(&x.value, &x.arguments, &y.value, &y.arguments)
            }
            (Select::MethodCall(x), Select::MethodCall(y)) => {
                x.method == y.method
                    && call_exact_eq(&x.value, &x.arguments, &y.value, &y.arguments)
            }
            (Select::VarArg(_), Select::VarArg(_)) => true,
            _ => false,
        },
        _ => false,
    }
}

fn call_exact_eq(av: &RValue, aa: &[RValue], bv: &RValue, ba: &[RValue]) -> bool {
    rvalue_exact_eq(av, bv)
        && aa.len() == ba.len()
        && aa.iter().zip(ba).all(|(p, q)| rvalue_exact_eq(p, q))
}

/// An expression whose every evaluation yields a FRESH, distinct value: a table
/// constructor or a closure. Such an argument must never be shared across multiple
/// parameter occurrences — the inlined region built a separate value at each use,
/// whereas `f(arg)` constructs one and passes it to all of them (an identity
/// divergence). The condition of an `if-expr` is evaluated once, so only the
/// produced branch's identity matters.
fn is_identity_producing(rv: &RValue) -> bool {
    match rv {
        RValue::Table(_) | RValue::Closure(_) => true,
        RValue::IfExpression(e) => {
            is_identity_producing(&e.then_value) || is_identity_producing(&e.else_value)
        }
        _ => false,
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
        (focused, rivals)
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
    let mut canon_cache = CanonCache::default();
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
    ordered: &[usize],
    // The other targets in scope: consulted only where an `ordered` one matches.
    rivals: &[usize],
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
    for &ti in ordered {
        let Ok(hit) = attempt(ti) else { return None };
        if let Some(h) = hit {
            offer(&mut found, &mut tied, h);
        }
    }
    // A target outside this iteration's focus cannot match anew, but where a
    // focused one matches, it may still cover more or make the site ambiguous.
    if found.is_some() {
        for &ti in rivals {
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
    if ordered.iter().chain(rivals).next().is_some_and(|&ti| targets[ti].search.exhausted()) {
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
        let canonical_length = canon_cache.top_len(stmts, start, w);
        if w < kc || canonical_length < kc {
            let shorter = canonical_length < kc
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
        if canonical_length == kc {
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
        if canon_cache.top_len(stmts, body_start, w) != kc {
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
// Target collection + per-function gates
// ===================================================================

/// Whether some `local f = function ... end` passes the gates of
/// [`collect_targets`] that depend only on the helper itself (a necessary
/// condition for any target).
fn any_structural_target(body: &Block, patterns: &mut CompiledPatterns) -> bool {
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
        let Some(prepared) = PreparedPattern::new(&body) else { return; };
        let CompiledPattern { kind, falls_off, pat: pattern } = prepared.finish(&body);
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
        if found && matches!(&body, std::borrow::Cow::Borrowed(_)) {
            // No mutation occurs between this gate and the first collection.
            // Owned tuple lowering mints fresh locals, so it keeps its original
            // independent construction and local-allocation order.
            patterns.seed(Arc::as_ptr(function), CompiledPattern { kind, falls_off, pat: pattern });
        }
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
fn branch_tuple_return(body: &[Statement], parameters: &[RcLocal]) -> Option<(Vec<Statement>, Vec<RcLocal>)> {
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

fn collect_targets(
    body: &Block,
    write_counts: &FxHashMap<RcLocal, usize>,
    captures: std::rc::Rc<crate::deinline_safety::CaptureSafety>,
    patterns: &mut CompiledPatterns,
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
        let prepared = match patterns.take(Arc::as_ptr(&func)).map(PreparedPattern::from)
            .or_else(|| PreparedPattern::new(&body)) {
            Some(prepared) => prepared,
            None => {
                // multi-return / mixed / bare-vararg leaf / non-terminal value return
                deinline_reject!(
                    RejectReason::UnsupportedReturnShape,
                    g.name.as_deref().unwrap_or("<anon>")
                );
                continue;
            }
        };
        let (kind, falls_off) = (prepared.kind, prepared.falls_off);
        let shape = crate::deinline_safety::CaptureSafety::new(&g.body);
        if !shape.complete() || shape.nodes() > 2048 {
            deinline_reject!(RejectReason::ShapeBudget, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        let pat = prepared.finish(&body).pat;
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
        patterns.remember(func_ptr, &g.body.0, matches!(&body, std::borrow::Cow::Borrowed(_)));
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
#[cfg(test)]
fn classify_returns(body: &[Statement]) -> Option<(TKind, bool)> {
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
/// store the value and leave the loop through a flag ([`match_value_loop`]).
fn loop_return_split(pattern: &[Statement]) -> Option<usize> {
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
fn local_tuple_return<'a>(body: &'a [Statement], parameters: &[RcLocal]) -> Option<(&'a [Statement], Vec<RcLocal>)> {
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
fn returning_nil(body: &[Statement]) -> Vec<Statement> {
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

pub(crate) fn is_scalar_return_value(rv: &RValue) -> bool {
    !matches!(
        rv,
        RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_)
    )
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
fn value_leaf_shape(stmts: &[Statement]) -> bool {
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
mod tests {
    use super::*;
    use crate::{
        Break, Closure, Empty, ForOrigin, ForPrepKind, Function, Global, Index, Local,
        VmProfileId,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use rustc_hash::FxHashSet;

    // Original owned-input implementation, retained as an independent oracle.
    fn unguard_reference(mut stmts: Vec<Statement>) -> Vec<Statement> {
        let mut out: Vec<Statement> = Vec::new();
        let mut i = 0;
        while i < stmts.len() {
            if let Statement::If(f) = &stmts[i] {
                // A guard may do work before returning:
                // `if cond then PREFIX; return [X] end; REST`.  Re-nest the shared
                // continuation into the exact structured form produced at inlined
                // sites: `if not cond then REST else PREFIX; return X end`.  For a
                // void return the terminal return is omitted; tail fall-through is
                // equivalent and `PREFIX` remains in the else arm.
                let guard: Option<(Vec<Statement>, Option<RValue>)> = {
                    let then = f.then_block.lock();
                    let els = f.else_block.lock();
                    if els.0.is_empty() {
                        match then.0.split_last() {
                            Some((Statement::Return(r), prefix)) if r.values.is_empty() => {
                                Some((prefix.to_vec(), None))
                            }
                            Some((Statement::Return(r), prefix)) if r.values.len() == 1 => {
                                Some((prefix.to_vec(), Some(r.values[0].clone())))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    }
                };
                if let Some((mut early_prefix, ret_val)) = guard {
                    if i + 1 < stmts.len() {
                        let cond = f.condition.clone();
                        let suffix: Vec<Statement> = stmts.split_off(i + 1);
                        let folded = unguard_reference(suffix);
                        if let Some(x) = ret_val {
                            early_prefix.push(Statement::Return(Return { node_origin: Default::default(), values: vec![x] }));
                        }
                        out.push(Statement::If(If::new(
                            negate_canon(cond),
                            Block(folded),
                            Block(early_prefix),
                        )));
                        return out;
                    }
                } else if i + 1 < stmts.len() && is_open_guard(f) {
                    let suffix: Vec<Statement> = stmts.split_off(i + 1);
                    let folded = unguard_reference(suffix);
                    out.extend(graft(vec![stmts[i].clone()], folded));
                    return out;
                }
            }
            out.push(stmts[i].clone());
            i += 1;
        }
        out
    }


    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn local_value(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn add_one(local: &RcLocal) -> RValue {
        RValue::Binary(Binary::new(
            local_value(local),
            number(1.0),
            BinaryOperation::Add,
        ))
    }

    fn assign_local(local: &RcLocal, value: RValue, prefix: bool) -> Statement {
        Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(local.clone())],
            right: vec![value],
            prefix,
            parallel: false, compound: false,
        })
    }

    fn return_one(value: RValue) -> Statement {
        Statement::Return(Return::new(vec![value]))
    }

    fn print_x() -> Statement {
        Statement::Call(Call::new(global("print"), vec![string("x")]))
    }

    fn void_target(pat: Vec<Statement>, locals: FxHashSet<RcLocal>) -> Target {
        let pat0 = pat.first().expect("test pat must be non-empty");
        let pat0_kind = std::mem::discriminant(pat0);
        let pat0_anchor_key = stmt_anchor_key(pat0);
        Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            pat_raw_len: pat.len(),
            pat_spine_len: tail_spine_len(&pat),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: FxHashSet::default(),
            locals,
            param_order: Vec::new(),
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        }
    }

    #[test]
    fn written_upvalues_are_not_local_binders() {
        let state = local("state");
        let other = local("other");
        let pat = canon(&[print_x(), assign_local(&state, add_one(&state), false)]);

        let mut declared = FxHashSet::default();
        collect_declared_locals(&pat, &mut declared);
        assert!(!declared.contains(&state));

        let target = void_target(pat, declared);
        let cand = canon(&[print_x(), assign_local(&other, add_one(&other), false)]);

        assert!(try_unify_site(&target, &cand, &[], None).is_none());
    }

    #[test]
    fn written_argument_copies_keep_order_dependencies_without_truncation() {
        let p = local("p"); let q = local("q"); let a = local("a"); let b = local("b");
        let mut target = void_target(vec![print_x()], [p.clone(), q.clone()].into_iter().collect());
        target.param_order = vec![p.clone(), q.clone()];
        target.written_params = target.param_order.clone();
        let bindings = Bindings { locals: [(p, a.clone()), (q, b.clone())].into_iter().collect(), ..Default::default() };
        let first: RValue = Call::new(global("first"), vec![]).into();
        let last: RValue = Call::new(global("last"), vec![]).into();
        let prefix = vec![(a.clone(), first.clone()), (b.clone(), last.clone())];
        let hit = finish_unified(&target, &[], bindings.clone(), &prefix, None).unwrap();
        // A non-variadic helper drops a trailing call's extra results itself.
        assert!(hit.args.iter().all(|v| matches!(v, RValue::Call(_))));
        assert!(finish_unified(&target, &[], bindings.clone(), &[(b.clone(), last), (a.clone(), first.clone())], None).is_none());
        assert!(finish_unified(&target, &[], bindings, &[(a.clone(), first), (b, a.into())], None).is_none());
    }

    /// `local name = function() BODY end`
    fn helper_decl(name: &RcLocal, body: Vec<Statement>) -> Statement {
        let function = Function { body: Block(body), ..Function::default() };
        assign_local(
            name,
            RValue::Closure(Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(function))),
                upvalues: Vec::new(),
            }),
            true,
        )
    }

    fn global_call(name: &str, arguments: Vec<RValue>) -> Call {
        Call::new(global(name), arguments)
    }

    #[test]
    fn a_flagged_loop_exit_is_the_helpers_return_only_at_the_end() {
        // local ok = true
        // while c do if d then ok = false; break end end
        // if ok then print("x") end
        let ok = local("ok");
        let exit = |flag: &RcLocal, tail: Vec<Statement>| {
            let mut body = vec![Statement::If(If::new(
                global("d"),
                Block(vec![
                    assign_local(flag, boolean(false), false),
                    Statement::Break(Break {}),
                ]),
                Block::default(),
            ))];
            body.extend(tail);
            vec![
                assign_local(flag, boolean(true), true),
                Statement::While(While::new(global("c"), Block(body))),
                Statement::If(If::new(local_value(flag), Block(vec![print_x()]), Block::default())),
            ]
        };
        let (unflagged, flags) = unflag_loop_exits(&exit(&ok, Vec::new())).expect("flag lowered");
        assert!(flags.contains(&ok));
        assert_eq!(
            Block(unflagged).to_string(),
            "while c do\n\tif d then\n\t\treturn\n\tend\nend\n\nprint(\"x\")"
        );

        // Something after the flagged `if` runs even when the flag is cleared.
        let mut followed = exit(&ok, Vec::new());
        followed.push(print_x());
        assert!(unflag_loop_exits(&followed).is_none());
        // A `break` that keeps the flag reaches REST, which a `return` skips.
        let plain_break = exit(&ok, vec![Statement::Break(Break {})]);
        assert!(unflag_loop_exits(&plain_break).is_none());
        // REST reads the flag.
        let reads = exit(&ok, Vec::new());
        if let Statement::If(guard) = &reads[2] {
            guard.then_block.lock().0.push(Statement::Call(Call::new(global("print"), vec![local_value(&ok)])));
        }
        assert!(unflag_loop_exits(&reads).is_none());
    }

    /// `local function findItem(items, name) for _, item in items do if
    /// item.Name == name then return item end end return nil end`, inlined
    /// into `local found = findItem(list, key)`: the store, a flag and a
    /// `break` stand for the `return`; the helper's `return nil` is the
    /// declaration's `nil`, its `return fallback` the flag's `if`.
    #[test]
    fn a_value_returned_from_inside_a_loop_rebuilds_from_its_flagged_exit() {
        let (helper, items, name, item) = (local("findItem"), local("items"), local("name"), local("item"));
        let named = |of: &RcLocal, name: RValue| {
            RValue::Binary(Binary::new(
                RValue::Index(crate::Index::new(local_value(of), string("Name"))),
                name,
                BinaryOperation::Equal,
            ))
        };
        let helper_body = |tail: RValue| {
            vec![
                Statement::GenericFor(GenericFor::new(
                    vec![local("_"), item.clone()],
                    vec![local_value(&items)],
                    Block(vec![Statement::If(If::new(
                        named(&item, local_value(&name)),
                        Block(vec![return_one(local_value(&item))]),
                        Block::default(),
                    ))]),
                )),
                return_one(tail),
            ]
        };
        let declare = |helper_body: Vec<Statement>| {
            let declaration = helper_decl(&helper, helper_body);
            if let Statement::Assign(assign) = &declaration
                && let RValue::Closure(closure) = &assign.right[0]
            {
                closure.function.lock().parameters = vec![items.clone(), name.clone()];
            }
            declaration
        };
        let (list, key, found, ok, x) = (local("list"), local("key"), local("found"), local("ok"), local("x"));
        // `exit` is the loop body's leaving arm; `after` follows the loop.
        let site = |exit: Vec<Statement>, after: Vec<Statement>| {
            let mut found_declaration = Assign::new(vec![LValue::Local(found.clone())], vec![RValue::Literal(Literal::Nil)]);
            found_declaration.prefix = true;
            let mut block = vec![
                found_declaration.into(),
                assign_local(&ok, boolean(true), true),
                Statement::GenericFor(GenericFor::new(
                    vec![local("_"), x.clone()],
                    vec![local_value(&list)],
                    Block(vec![Statement::If(If::new(named(&x, local_value(&key)), Block(exit), Block::default()))]),
                )),
            ];
            block.extend(after);
            block.push(Statement::Call(global_call("print", vec![local_value(&found)])));
            block
        };
        let leave = || {
            vec![
                assign_local(&found, local_value(&x), false),
                assign_local(&ok, boolean(false), false),
                Statement::Break(Break {}),
            ]
        };
        let rebuilt = |helper_body: Vec<Statement>, site: Vec<Statement>| {
            let mut block = Block(vec![declare(helper_body)]);
            block.0.extend(site);
            deinline(&mut block);
            block.to_string()
        };

        let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(leave(), Vec::new()));
        assert!(output.contains("local found = findItem(list, key)"), "{output}");
        assert!(!output.contains("break"), "{output}");

        // The helper's `return fallback` runs where the flag is still set.
        let fallback = |found: &RcLocal| {
            Statement::If(If::new(
                local_value(&ok),
                Block(vec![assign_local(found, string("none"), false)]),
                Block::default(),
            ))
        };
        let output = rebuilt(helper_body(string("none")), site(leave(), vec![fallback(&found)]));
        assert!(output.contains("local found = findItem(list, key)"), "{output}");

        // A store that keeps looping is not a `return`.
        let keeps_looping = vec![assign_local(&found, local_value(&x), false)];
        let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(keeps_looping, Vec::new()));
        assert!(!output.contains("findItem(list"), "{output}");
        // A `break` leaving without the flag reaches what the flag guards.
        let mut plain_break = leave();
        plain_break.remove(1);
        let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(plain_break, Vec::new()));
        assert!(!output.contains("findItem(list"), "{output}");
        // The flag is read after the loop.
        let read_flag = vec![Statement::Call(global_call("print", vec![local_value(&ok)]))];
        let output = rebuilt(helper_body(RValue::Literal(Literal::Nil)), site(leave(), read_flag));
        assert!(!output.contains("findItem(list"), "{output}");
        // The fallback stores something else than the helper returns.
        let output = rebuilt(helper_body(string("none")), site(leave(), Vec::new()));
        assert!(!output.contains("findItem(list"), "{output}");
    }

    /// A numeric `for` returning from two guards (`if a then return "x" end;
    /// if b then return "y" end`), copied with `break` exits the structurer
    /// writes as `if a then … elseif b then … end`, and a helper falling off
    /// its end (the declaration's `nil`), both rebuild.
    #[test]
    fn a_numeric_loop_with_two_returns_or_none_after_it_rebuilds() {
        let (helper, list, i, n) = (local("classify"), local("list"), local("i"), local("n"));
        let numeric = |counter: &RcLocal, body: Vec<Statement>| {
            Statement::NumericFor(Box::new(NumericFor::new(
                number(1.0),
                RValue::Unary(Unary::new(local_value(&list), UnaryOperation::Length)),
                number(1.0),
                counter.clone(),
                Block(body),
            )))
        };
        let test = |name: &str, of: &RcLocal| RValue::Call(global_call(name, vec![local_value(of)]));
        let guard = |condition: RValue, then_block: Vec<Statement>, else_block: Vec<Statement>| {
            Statement::If(If::new(condition, Block(then_block), Block(else_block)))
        };
        let run = |helper_body: Vec<Statement>, site: Vec<Statement>| {
            let declaration = helper_decl(&helper, helper_body);
            if let Statement::Assign(assign) = &declaration
                && let RValue::Closure(closure) = &assign.right[0]
            {
                closure.function.lock().parameters = vec![list.clone()];
            }
            let mut block = Block(vec![declaration]);
            block.0.extend(site);
            deinline(&mut block);
            block.to_string()
        };
        let (r, ok) = (local("r"), local("ok"));
        let declare_r = || {
            let mut declaration = Assign::new(vec![LValue::Local(r.clone())], vec![RValue::Literal(Literal::Nil)]);
            declaration.prefix = true;
            Statement::from(declaration)
        };
        let leave = |value: RValue| vec![assign_local(&r, value, false), assign_local(&ok, boolean(false), false), Statement::Break(Break {})];
        let use_r = || Statement::Call(global_call("print", vec![local_value(&r)]));

        // for i = 1, #list do if a(i) then return "x" end if b(i) then return "y" end end return "none"
        let helper_body = vec![
            numeric(&i, vec![
                guard(test("a", &i), vec![return_one(string("x"))], vec![]),
                guard(test("b", &i), vec![return_one(string("y"))], vec![]),
            ]),
            return_one(string("none")),
        ];
        let site = vec![
            declare_r(),
            assign_local(&ok, boolean(true), true),
            numeric(&n, vec![guard(test("a", &n), leave(string("x")), vec![guard(test("b", &n), leave(string("y")), vec![])])]),
            guard(local_value(&ok), vec![assign_local(&r, string("none"), false)], vec![]),
            use_r(),
        ];
        let output = run(helper_body, site);
        assert!(output.contains("local r = classify(list)"), "{output}");

        // for i = 1, #list do if a(i) then return i end end (nothing after)
        let helper_body = vec![numeric(&i, vec![guard(test("a", &i), vec![return_one(local_value(&i))], vec![])])];
        let site = vec![
            declare_r(),
            assign_local(&ok, boolean(true), true),
            numeric(&n, vec![guard(test("a", &n), leave(local_value(&n)), vec![])]),
            use_r(),
        ];
        let output = run(helper_body, site);
        assert!(output.contains("local r = classify(list)"), "{output}");
    }

    /// Luau folds the reads of a constant argument whose truth alone is
    /// tested (`visible and 0 or 1` with `false` is `1`), leaving the copy no
    /// trace of it: the constant that specializes the body into exactly the
    /// copy is passed (`nil`, left out, for a parameter given a default).
    #[test]
    fn a_constant_argument_folded_away_is_inferred_from_its_truth() {
        let declare = |helper: &RcLocal, parameters: Vec<RcLocal>, body: Vec<Statement>| {
            let declaration = helper_decl(helper, body);
            if let Statement::Assign(assign) = &declaration
                && let RValue::Closure(closure) = &assign.right[0]
            {
                closure.function.lock().parameters = parameters;
            }
            declaration
        };
        let store = |object: &RcLocal, field: &str, value: RValue| {
            Statement::Assign(Assign::new(
                vec![LValue::Index(crate::Index::new(local_value(object), string(field)))],
                vec![value],
            ))
        };
        let method = |object: &RcLocal, name: &str| {
            Statement::MethodCall(MethodCall::new(local_value(object), name.to_string(), Vec::new()))
        };
        let select = |condition: RValue, yes: RValue, no: RValue| {
            RValue::Binary(Binary::new(RValue::Binary(Binary::new(condition, yes, BinaryOperation::And)), no, BinaryOperation::Or))
        };
        let run = |block: Vec<Statement>| {
            let mut block = Block(block);
            deinline(&mut block);
            block.to_string()
        };

        // local function fade(frame, visible) frame.Transparency = visible and 0 or 1; frame:Play() end
        let (fade, frame, visible, part) = (local("fade"), local("frame"), local("visible"), local("part"));
        let fade_body = vec![
            store(&frame, "Transparency", select(local_value(&visible), number(0.0), number(1.0))),
            method(&frame, "Play"),
        ];
        let output = run(vec![
            declare(&fade, vec![frame.clone(), visible.clone()], fade_body.clone()),
            store(&part, "Transparency", number(1.0)),
            method(&part, "Play"),
            store(&part, "Transparency", number(0.0)),
            method(&part, "Play"),
        ]);
        assert!(output.contains("fade(part, false)") && output.contains("fade(part, true)"), "{output}");
        // A value neither constant gives keeps the copy.
        let output = run(vec![
            declare(&fade, vec![frame.clone(), visible.clone()], fade_body),
            store(&part, "Transparency", number(0.5)),
            method(&part, "Play"),
        ]);
        assert!(!output.contains("fade(part"), "{output}");

        // local function attach(item, parent) item.Parent = parent or root; item:Init() end
        let (attach, item, parent, root) = (local("attach"), local("item"), local("parent"), local("root"));
        let output = run(vec![
            declare(&attach, vec![item.clone(), parent.clone()], vec![
                store(&item, "Parent", RValue::Binary(Binary::new(local_value(&parent), local_value(&root), BinaryOperation::Or))),
                method(&item, "Init"),
            ]),
            store(&part, "Parent", local_value(&root)),
            method(&part, "Init"),
        ]);
        assert!(output.contains("attach(part)"), "{output}");

        // local function add(item, timeout) item:Init(); item.Ready = true; if timeout then item.Value = timeout end end:
        // an optional value is left out, a flag is passed `false`.
        let (add, timeout) = (local("add"), local("timeout"));
        let output = run(vec![
            declare(&add, vec![item.clone(), timeout.clone()], vec![
                method(&item, "Init"),
                store(&item, "Ready", RValue::Literal(Literal::Boolean(true))),
                Statement::If(If::new(local_value(&timeout), Block(vec![store(&item, "Value", local_value(&timeout))]), Block::default())),
            ]),
            method(&part, "Init"),
            store(&part, "Ready", RValue::Literal(Literal::Boolean(true))),
        ]);
        assert!(output.contains("add(part)"), "{output}");

        // local function emit(enabled) if enabled then work("a") end end:
        // `false` leaves nothing, which no statement may stand for.
        let (emit, enabled) = (local("emit"), local("enabled"));
        let work = || Statement::Call(global_call("work", vec![string("a")]));
        let output = run(vec![
            declare(&emit, vec![enabled.clone()], vec![Statement::If(If::new(local_value(&enabled), Block(vec![work()]), Block::default()))]),
            print_x(),
            work(),
        ]);
        assert!(output.contains("emit(true)") && !output.contains("emit(false)"), "{output}");

        // local function start(time) cancel("x"); if time then work(time) end end:
        // the whole copy `start(t)` wins over its prefix, `start(nil)`.
        let (start, time, t) = (local("start"), local("time"), local("t"));
        let cancel = || Statement::Call(global_call("cancel", vec![string("x")]));
        let work = |of: &RcLocal| Statement::Call(global_call("work", vec![local_value(of)]));
        let body = |time: &RcLocal| {
            vec![cancel(), Statement::If(If::new(local_value(time), Block(vec![work(time)]), Block::default()))]
        };
        let mut site = body(&t);
        site.insert(0, declare(&start, vec![time.clone()], body(&time)));
        let output = run(site);
        assert!(output.contains("start(t)") && output.matches("cancel(").count() == 1, "{output}");
        // Where the copy goes on with the branch the constant removed (here
        // under another condition), the prefix is no call with `nil`.
        let now = local("now");
        let mut site = vec![
            cancel(),
            Statement::If(If::new(local_value(&now), Block(vec![Statement::Call(global_call("work", vec![local_value(&now), number(1.0)]))]), Block::default())),
        ];
        site.insert(0, declare(&start, vec![time.clone()], body(&time)));
        let output = run(site);
        assert!(output.matches("cancel(").count() == 2, "{output}");
    }

    #[test]
    fn a_local_the_until_condition_reads_is_not_the_helpers() {
        // local helper = function() local ready = make("ready"); print(ready) end
        // repeat local ready = make("ready"); print(ready) until ready
        let body = |ready: &RcLocal| {
            vec![
                assign_local(ready, RValue::Call(global_call("make", vec![string("ready")])), true),
                Statement::Call(global_call("print", vec![local_value(ready)])),
            ]
        };
        let site = |condition: RValue, ready: &RcLocal| {
            Statement::Repeat(Repeat::new(condition, Block(body(ready))))
        };
        let helper = local("helper");
        let ready = local("ready");
        let mut block = Block(vec![
            helper_decl(&helper, body(&local("ready"))),
            site(local_value(&ready), &ready),
        ]);
        deinline(&mut block);
        assert_eq!(block.to_string().matches("helper()").count(), 1, "{block}");

        // The same body is rebuilt where the condition reads something else.
        let other = local("ready");
        let mut block = Block(vec![
            helper_decl(&helper, body(&local("ready"))),
            site(global("done"), &other),
        ]);
        deinline(&mut block);
        assert_eq!(block.to_string().matches("helper()").count(), 2, "{block}");
    }

    #[test]
    fn a_rebuilt_value_keeps_the_locals_its_statement_still_uses() {
        // local helper = function() local temp = source("key"); return temp ~= nil end
        // local temp = source("key"); if temp ~= nil then print(temp) end
        let helper = local("helper");
        let pattern_temp = local("temp");
        let source = |temp: &RcLocal| {
            assign_local(temp, RValue::Call(global_call("source", vec![string("key")])), true)
        };
        let not_nil = |temp: &RcLocal| {
            RValue::Binary(Binary::new(local_value(temp), RValue::Literal(Literal::Nil), BinaryOperation::NotEqual))
        };
        let helper_body = vec![source(&pattern_temp), return_one(not_nil(&pattern_temp))];
        let temp = local("temp");
        let use_temp = |then_block: Vec<Statement>| {
            Statement::If(If::new(not_nil(&temp), Block(then_block), Block::default()))
        };
        let mut block = Block(vec![
            helper_decl(&helper, helper_body.clone()),
            source(&temp),
            use_temp(vec![Statement::Call(global_call("print", vec![local_value(&temp)]))]),
        ]);
        deinline(&mut block);
        assert_eq!(block.to_string().matches("helper()").count(), 1, "{block}");

        let mut block = Block(vec![
            helper_decl(&helper, helper_body),
            source(&temp),
            use_temp(vec![Statement::Call(global_call("print", vec![string("present")]))]),
        ]);
        deinline(&mut block);
        assert!(block.to_string().contains("if helper() then"), "{block}");
    }

    /// `local at = find(value); if at then return sub(value, at), at end;
    /// return value, 1` stores into the caller's locals, `at` sharing the
    /// second result's register; the rebuilt call declares both.
    #[test]
    fn a_tuple_returned_through_branches_rebuilds_into_its_results() {
        let (value, at) = (local("value"), local("at"));
        let ret = |values: Vec<RValue>| Statement::Return(Return::new(values));
        let find = |of: &RcLocal| RValue::Call(global_call("find", vec![local_value(of)]));
        let sub = |of: &RcLocal, at: &RcLocal| RValue::Call(global_call("sub", vec![local_value(of), local_value(at)]));
        let body = vec![
            assign_local(&at, find(&value), true),
            Statement::If(If::new(local_value(&at), Block(vec![ret(vec![sub(&value, &at), local_value(&at)])]), Block::default())),
            ret(vec![local_value(&value), number(1.0)]),
        ];
        let (lowered, results) = branch_tuple_return(&body, &[value.clone()]).expect("branch tuple");
        assert_eq!(results.len(), 2);
        assert_eq!(results[1], at, "the returned local is the second result");
        assert_eq!(
            Block(lowered).to_string().replace(&results[0].to_string(), "name"),
            "local at = find(value)\nlocal name\n\nif at then\n\tname = sub(value, at)\nelse\n\tname = value\n\tat = 1\nend"
        );

        // The site Luau inlines for `local name, position = split(text)`,
        // with `name` and `position` read afterwards.
        let helper = local("split");
        let (text, name, position) = (local("text"), local("name"), local("position"));
        let mut empty = Assign::new(vec![LValue::Local(name.clone())], Vec::new());
        empty.prefix = true;
        let mut block = Block(vec![
            helper_decl(&helper, body.clone()),
            assign_local(&position, find(&text), true),
            empty.into(),
            Statement::If(If::new(
                local_value(&position),
                Block(vec![assign_local(&name, sub(&text, &position), false)]),
                Block(vec![assign_local(&name, local_value(&text), false), assign_local(&position, number(1.0), false)]),
            )),
            Statement::Call(global_call("print", vec![local_value(&name), local_value(&position)])),
        ]);
        // The helper's parameter is `value`.
        if let Statement::Assign(declaration) = &block.0[0]
            && let RValue::Closure(closure) = &declaration.right[0]
        {
            closure.function.lock().parameters = vec![value.clone()];
        }
        deinline(&mut block);
        assert!(block.to_string().contains("local name, position = split(text)"), "{block}");

        // A later result reading the local keeps it apart from the results:
        // its store would overwrite what the later value reads.
        let later = vec![
            assign_local(&at, find(&value), true),
            Statement::If(If::new(
                local_value(&at),
                Block(vec![ret(vec![local_value(&at), RValue::Binary(Binary::new(local_value(&at), number(1.0), BinaryOperation::Add))])]),
                Block::default(),
            )),
            ret(vec![number(0.0), local_value(&value)]),
        ];
        let (_, results) = branch_tuple_return(&later, &[value.clone()]).expect("branch tuple");
        assert_ne!(results[0], at);

        // Every path must return the same number of values.
        let refused = |body: Vec<Statement>| branch_tuple_return(&body, &[value.clone()]).is_none();
        assert!(refused(vec![
            Statement::If(If::new(local_value(&at), Block(vec![ret(vec![number(1.0), number(2.0)])]), Block::default())),
            ret(vec![number(1.0)]),
        ]));
        // A path running off the end returns nothing.
        assert!(refused(vec![Statement::If(If::new(
            local_value(&at),
            Block(vec![ret(vec![number(1.0), number(2.0)])]),
            Block::default(),
        ))]));
        // The last value of a call spreads.
        assert!(refused(vec![
            Statement::If(If::new(local_value(&at), Block(vec![ret(vec![number(1.0), find(&value)])]), Block::default())),
            ret(vec![number(1.0), number(2.0)]),
        ]));
    }

    #[test]
    fn a_helper_returning_its_own_locals_matches_its_body() {
        // `local a = 1; local b = {}; return a, b`
        let (a, b) = (local("a"), local("b"));
        let body = vec![
            assign_local(&a, number(1.0), true),
            assign_local(&b, RValue::Table(Table::default()), true),
            Statement::Return(Return::new(vec![local_value(&a), local_value(&b)])),
        ];
        let (rest, returned) = local_tuple_return(&body, &[]).expect("local tuple");
        assert_eq!(rest.len(), 2);
        assert_eq!(returned, vec![a.clone(), b.clone()]);

        let refused = |body: Vec<Statement>| local_tuple_return(&body, &[]).is_none();
        let ret = |values: Vec<RValue>| Statement::Return(Return::new(values));
        // One value is a value target; a non-local is not the caller's local.
        assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a)])]));
        assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a), number(2.0)])]));
        // A parameter, or the same local twice, is not one declaration each.
        assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a), local_value(&b)])]));
        assert!(refused(vec![assign_local(&a, number(1.0), true), ret(vec![local_value(&a), local_value(&a)])]));
        // Another return means a call may not reach this one.
        let early = Statement::If(If::new(
            local_value(&local("c")),
            Block(vec![Statement::Return(Return::new(vec![]))]),
            Block::default(),
        ));
        assert!(refused(vec![
            early,
            assign_local(&a, number(1.0), true),
            assign_local(&b, number(2.0), true),
            ret(vec![local_value(&a), local_value(&b)]),
        ]));
        // A closure capturing `a` would share it with the caller's code.
        let capturing = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: vec![Upvalue::Ref(a.clone())],
        });
        assert!(refused(vec![
            assign_local(&a, number(1.0), true),
            assign_local(&b, capturing, true),
            ret(vec![local_value(&b), local_value(&a)]),
        ]));
    }

    #[test]
    fn a_value_helper_that_falls_off_returns_nil_there() {
        // `if c then return "a" end` and `if c then return "a" else return end`
        let c = local("c");
        let falls = vec![Statement::If(If::new(local_value(&c), Block(vec![return_one(string("a"))]), Block::default()))];
        let void = vec![Statement::If(If::new(
            local_value(&c),
            Block(vec![return_one(string("a"))]),
            Block(vec![Statement::Return(Return::new(vec![]))]),
        ))];
        for body in [falls, void] {
            assert!(matches!(classify_returns(&body), Some((TKind::Value, true))));
            assert_eq!(
                Block(returning_nil(&body)).to_string(),
                "if c then\n\treturn \"a\"\nelse\n\treturn nil\nend"
            );
        }
    }

    #[test]
    fn value_return_inside_loop_is_not_a_terminal_leaf() {
        let cond = local("cond");
        let pred = local("pred");
        let body = vec![
            Statement::While(While::new(
                local_value(&cond),
                Block(vec![
                    Statement::If(If::new(
                        local_value(&pred),
                        Block(vec![return_one(string("a"))]),
                        Block::default(),
                    )),
                    Statement::Break(Break {}),
                ]),
            )),
            return_one(string("b")),
        ];

        let pat = canon(&body);
        assert!(!value_leaf_shape(&pat));
        // Matched only where the copy leaves the loop through a flag
        // (`match_value_loop`), never as a plain value region.
        assert!(matches!(classify_returns(&body), Some((TKind::Value, false))));
        assert_eq!(loop_return_split(&pat), Some(0));

        // A `return` two loops deep leaves through two flags: refused.
        let nested = vec![
            Statement::While(While::new(local_value(&cond), Block(vec![body[0].clone()]))),
            return_one(string("b")),
        ];
        assert!(loop_return_split(&canon(&nested)).is_none());
        assert!(classify_returns(&nested).is_none());
    }

    #[test]
    fn value_guard_return_canonicalizes_to_terminal_leaves() {
        let pred = local("pred");
        let body = vec![
            Statement::If(If::new(
                local_value(&pred),
                Block(vec![return_one(string("a"))]),
                Block::default(),
            )),
            return_one(string("b")),
        ];

        let pat = canon(&body);
        assert!(value_leaf_shape(&pat));
        assert!(matches!(classify_returns(&body), Some((TKind::Value, false))));
    }

    #[test]
    fn void_guard_with_prefix_canonicalizes_all_early_returns() {
        let body = vec![
            Statement::If(If::new(
                global("disabled"),
                Block(vec![Statement::Return(Return::default())]),
                Block::default(),
            )),
            print_x(),
            Statement::If(If::new(
                global("created"),
                Block(vec![
                    Statement::Call(Call::new(global("markCreated"), vec![])),
                    Statement::Return(Return::default()),
                ]),
                Block::default(),
            )),
            Statement::If(If::new(
                global("notDestroyed"),
                Block(vec![
                    Statement::If(If::new(
                        global("disconnect"),
                        Block(vec![Statement::Call(Call::new(
                            global("markDisconnect"),
                            vec![],
                        ))]),
                        Block::default(),
                    )),
                    Statement::Return(Return::default()),
                ]),
                Block::default(),
            )),
            Statement::Call(Call::new(global("markDestroyed"), vec![])),
        ];

        let canonical = canon(&body);
        assert!(
            !block_has_return(&canonical),
            "all void early returns should become structured branches:\n{}",
            Block(canonical)
        );
    }

    #[test]
    fn constant_specialized_void_site_refolds_after_verified_partial_evaluation() {
        let debug = local("debug");
        let event = local("event");
        let key = local("key");
        let raw = vec![
            Statement::If(If::new(
                RValue::Unary(Unary::new(local_value(&debug), UnaryOperation::Not)),
                Block(vec![Statement::Return(Return::default())]),
                Block::default(),
            )),
            Statement::Call(Call::new(
                global("emit"),
                vec![local_value(&event), local_value(&key)],
            )),
            Statement::If(If::new(
                RValue::Binary(Binary::new(
                    local_value(&event),
                    string("CREATED"),
                    BinaryOperation::Equal,
                )),
                Block(vec![
                    Statement::Call(Call::new(global("markCreated"), vec![local_value(&key)])),
                    Statement::Return(Return::default()),
                ]),
                Block::default(),
            )),
            Statement::Call(Call::new(global("markOther"), vec![local_value(&key)])),
        ];
        let pat = canon(&raw);
        let mut params = FxHashSet::default();
        params.insert(event.clone());
        params.insert(key.clone());
        let target = Target {
            f_local: local("logEvent"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            pat_raw_len: raw.len(),
            pat_spine_len: raw.len(),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind: std::mem::discriminant(&pat[0]),
            pat0_anchor_key: stmt_anchor_key(&pat[0]),
            pat,
            params,
            locals: FxHashSet::default(),
            param_order: vec![event, key],
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: true,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        };

        let caller_key = local("callerKey");
        let candidate = canon(&[Statement::If(If::new(
            local_value(&debug),
            Block(vec![
                Statement::Call(Call::new(
                    global("emit"),
                    vec![string("OTHER"), local_value(&caller_key)],
                )),
                Statement::Call(Call::new(
                    global("markOther"),
                    vec![local_value(&caller_key)],
                )),
            ]),
            Block::default(),
        ))]);

        let unified = try_unify_site_any(&target, &candidate, &[], None)
            .expect("literal-specialized branch must refold only after exact verification");
        assert_eq!(unified.args.len(), 2);
        assert!(rvalue_exact_eq(&unified.args[0], &string("OTHER")));
        assert!(rvalue_exact_eq(&unified.args[1], &local_value(&caller_key)));

        let mut wrong = candidate.clone();
        let Statement::If(node) = &mut wrong[0] else {
            panic!()
        };
        node.then_block.lock().0[1] = Statement::Call(Call::new(
            global("differentEffect"),
            vec![local_value(&caller_key)],
        ));
        assert!(
            try_unify_site_any(&target, &wrong, &[], None).is_none(),
            "a non-specialization body difference must remain refused"
        );
    }

    #[test]
    fn specialized_site_refuses_repeated_table_identity() {
        let flag = local("flag");
        let value = local("value");
        let raw = vec![Statement::Call(Call::new(
            global("consume"),
            vec![local_value(&flag), local_value(&value), local_value(&value)],
        ))];
        let pat = canon(&raw);
        let mut params = FxHashSet::default();
        params.insert(flag.clone());
        params.insert(value.clone());
        let target = Target {
            f_local: local("consumeTwice"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            pat_raw_len: raw.len(),
            pat_spine_len: raw.len(),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind: std::mem::discriminant(&pat[0]),
            pat0_anchor_key: stmt_anchor_key(&pat[0]),
            pat,
            params,
            locals: FxHashSet::default(),
            param_order: vec![flag, value],
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: true,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        };
        let candidate = canon(&[Statement::Call(Call::new(
            global("consume"),
            vec![
                string("enabled"),
                RValue::Table(Table::default()),
                RValue::Table(Table::default()),
            ],
        ))]);

        assert!(
            try_unify_specialized_site(&target, &candidate, &[], None).is_none(),
            "two fresh tables must never collapse into one reconstructed argument"
        );
    }

    #[test]
    fn vector_equality_is_not_partially_evaluated() {
        let vector = Literal::Vector(1.0, 2.0, 3.0);
        assert_eq!(runtime_literal_equal(&vector, &vector), None);

        let mut expression = RValue::Binary(Binary::new(
            RValue::Literal(vector.clone()),
            RValue::Literal(vector),
            BinaryOperation::Equal,
        ));
        specialize_rvalue(&mut expression, &FxHashMap::default());
        assert!(matches!(expression, RValue::Binary(_)));
    }

    #[test]
    fn statement_deinline_refuses_metamethod_risk_argument() {
        let parameter = local("parameter");
        let raw = vec![
            print_x(),
            Statement::Call(Call::new(global("consume"), vec![local_value(&parameter)])),
        ];
        let pat = canon(&raw);
        let mut params = FxHashSet::default();
        params.insert(parameter.clone());
        let target = Target {
            f_local: local("effectThenConsume"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            pat_raw_len: raw.len(),
            pat_spine_len: raw.len(),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind: std::mem::discriminant(&pat[0]),
            pat0_anchor_key: stmt_anchor_key(&pat[0]),
            pat,
            params,
            locals: FxHashSet::default(),
            param_order: vec![parameter],
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        };
        let left = local("left");
        let right = local("right");
        let candidate = canon(&[
            print_x(),
            Statement::Call(Call::new(
                global("consume"),
                vec![RValue::Binary(Binary::new(
                    local_value(&left),
                    local_value(&right),
                    BinaryOperation::Add,
                ))],
            )),
        ]);

        assert!(
            try_unify_site(&target, &candidate, &[], None).is_none(),
            "moving a potentially metamethod-backed operator before print is unsound"
        );
    }

    #[test]
    fn loop_return_cps_site_requires_exact_caller_continuation() {
        let frames = local("frames");
        let frame = local("frame");
        let existing = local("existing");
        let loop_guard = Statement::If(If::new(
            RValue::Binary(Binary::new(
                local_value(&existing),
                local_value(&frame),
                BinaryOperation::Equal,
            )),
            Block(vec![Statement::Return(Return::default())]),
            Block::default(),
        ));
        let raw = vec![Statement::If(If::new(
            local_value(&frame),
            Block(vec![
                Statement::GenericFor(GenericFor::new(
                    vec![existing.clone()],
                    vec![local_value(&frames)],
                    Block(vec![loop_guard]),
                )),
                Statement::Call(Call::new(
                    global("insertFrame"),
                    vec![local_value(&frames), local_value(&frame)],
                )),
            ]),
            Block::default(),
        ))];
        let pat = canon(&raw);
        let mut params = FxHashSet::default();
        params.insert(frame.clone());
        let mut locals = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        let target = Target {
            f_local: local("addFrame"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            pat_raw_len: raw.len(),
            pat_spine_len: raw.len(),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind: std::mem::discriminant(&pat[0]),
            pat0_anchor_key: stmt_anchor_key(&pat[0]),
            pat,
            params,
            locals,
            param_order: vec![frame],
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: true,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        };

        let actual = local("actual");
        let caller_existing = local("callerExisting");
        let continuation = vec![
            Statement::Call(Call::new(global("afterHelper"), vec![local_value(&actual)])),
            Statement::Return(Return::default()),
        ];
        let mut loop_body = vec![Statement::If(If::new(
            RValue::Binary(Binary::new(
                local_value(&caller_existing),
                local_value(&actual),
                BinaryOperation::NotEqual,
            )),
            Block(vec![Statement::Continue(crate::Continue {})]),
            Block::default(),
        ))];
        loop_body.extend(continuation.clone());
        let mut normal_path = vec![
            Statement::GenericFor(GenericFor::new(
                vec![caller_existing.clone()],
                vec![local_value(&frames)],
                Block(loop_body),
            )),
            Statement::Call(Call::new(
                global("insertFrame"),
                vec![local_value(&frames), local_value(&actual)],
            )),
        ];
        // Luau clones K after both the loop-return edge and the helper's normal
        // fallthrough.  The enclosing empty arm reaches the external K directly.
        normal_path.extend(continuation.clone());
        let window = vec![Statement::If(If::new(local_value(&actual), Block(normal_path), Block::default()))];
        let candidate = canon(&window);

        let unified = try_unify_cps_site(&target, &window, &candidate, &continuation, &[], None)
            .expect("verified cloned continuation should recover the loop-return helper");
        assert!(rvalue_exact_eq(&unified.args[0], &local_value(&actual)));

        let structured_loop_body = vec![Statement::If(If::new(
            RValue::Binary(Binary::new(
                local_value(&caller_existing),
                local_value(&actual),
                BinaryOperation::Equal,
            )),
            Block(continuation.clone()),
            Block::default(),
        ))];
        let mut structured_normal_path = vec![
            Statement::GenericFor(GenericFor::new(
                vec![caller_existing],
                vec![local_value(&frames)],
                Block(structured_loop_body),
            )),
            Statement::Call(Call::new(
                global("insertFrame"),
                vec![local_value(&frames), local_value(&actual)],
            )),
        ];
        structured_normal_path.extend(continuation.clone());
        let structured_window =
            vec![Statement::If(If::new(local_value(&actual), Block(structured_normal_path), Block::default()))];
        let structured_candidate = canon(&structured_window);
        let structured = try_unify_cps_site(&target, &structured_window, &structured_candidate, &continuation, &[], None)
            .expect("pre-guard-continue structured loop exit should also refold");
        assert!(rvalue_exact_eq(&structured.args[0], &local_value(&actual)));

        let wrong_continuation = vec![
            Statement::Call(Call::new(
                global("differentAfter"),
                vec![local_value(&actual)],
            )),
            Statement::Return(Return::default()),
        ];
        assert!(
            try_unify_cps_site(&target, &window, &candidate, &wrong_continuation, &[], None).is_none(),
            "a different caller continuation must refuse CPS refolding"
        );
    }

    #[test]
    fn cps_continuation_is_only_accepted_at_true_tail_positions() {
        let target = void_target(vec![print_x()], FxHashSet::default());
        let continuation = vec![
            Statement::Call(Call::new(global("after"), Vec::new())),
            Statement::Return(Return::default()),
        ];
        let pattern = vec![
            Statement::If(If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![Statement::Call(Call::new(
                    global("inside"),
                    Vec::new(),
                ))]),
                Block::default(),
            )),
            Statement::Call(Call::new(global("laterInCallee"), Vec::new())),
        ];
        let mut nested = vec![Statement::Call(Call::new(global("inside"), Vec::new()))];
        nested.extend(continuation.clone());
        let candidate = vec![
            Statement::If(If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(nested),
                Block::default(),
            )),
            Statement::Call(Call::new(global("laterInCallee"), Vec::new())),
        ];

        assert!(!cps_unify_block(
            &target,
            &pattern,
            &candidate,
            &continuation,
            true,
            &mut Bindings::default(),
        ));
    }

    #[test]
    fn cps_exact_loop_return_cannot_skip_caller_continuation() {
        let target = void_target(vec![print_x()], FxHashSet::default());
        let continuation = vec![Statement::Return(Return::default())];
        let pattern = vec![Statement::While(While::new(
            RValue::Literal(Literal::Boolean(true)),
            Block(vec![Statement::Return(Return::default())]),
        ))];
        let candidate = canon(&pattern);

        assert!(!cps_unify_block(
            &target,
            &pattern,
            &candidate,
            &continuation,
            true,
            &mut Bindings::default(),
        ));
    }

    #[test]
    fn cps_exact_repeat_return_cannot_skip_caller_continuation() {
        let target = void_target(vec![print_x()], FxHashSet::default());
        let continuation = vec![Statement::Return(Return::default())];
        let pattern = vec![Statement::Repeat(Repeat::new(
            RValue::Literal(Literal::Boolean(false)),
            Block(vec![Statement::Return(Return::default())]),
        ))];
        let candidate = canon(&pattern);

        assert!(!cps_unify_block(
            &target,
            &pattern,
            &candidate,
            &continuation,
            true,
            &mut Bindings::default(),
        ));
    }

    #[test]
    fn cps_continuation_requires_return_not_loop_control() {
        assert!(!sequence_has_return_tail(&[Statement::Break(Break {})]));
        assert!(!sequence_has_return_tail(&[Statement::Continue(
            crate::Continue {},
        )]));
        assert!(sequence_has_return_tail(&[Statement::Return(
            Return::default(),
        )]));

        let mixed = vec![
            Statement::If(If::new(
                RValue::Literal(Literal::Boolean(true)),
                Block(vec![Statement::Break(Break {})]),
                Block::default(),
            )),
            Statement::Return(Return::default()),
        ];
        assert!(sequence_has_return_tail(&mixed));
        assert!(has_depth_zero_loop_control(&mixed, 0));
    }

    /// P7-A: a call/method-call leaf (`return g(x)`) is an admissible Value leaf —
    /// the single-LHS `RESULT = g(x)` inlined site truncates it to one value, so
    /// the candidate shape proves the arity. (Refused pre-P7-A.)
    #[test]
    fn call_leaf_is_an_admissible_value_leaf_p7a() {
        let c = local("c");
        let body = vec![Statement::If(If::new(
            local_value(&c),
            Block(vec![return_one(RValue::Call(Call::new(
                global("g"),
                vec![local_value(&c)],
            )))]),
            Block(vec![return_one(RValue::Literal(Literal::Nil))]),
        ))];
        let pat = canon(&body);
        assert!(value_leaf_shape(&pat), "call leaf must be admissible");
        assert!(matches!(classify_returns(&body), Some((TKind::Value, false))));
    }

    /// P7-A boundary: a bare `...` (vararg) leaf is STILL refused — its multi-value
    /// spread has no provable single-value truncation point. Likewise a 2-value
    /// `return a, b` stays refused (returns_bad's multi-value arm).
    #[test]
    fn vararg_and_multivalue_leaves_still_refused_p7a() {
        let vararg_body = vec![Statement::Return(Return::new(vec![RValue::VarArg(
            crate::VarArg,
        )]))];
        assert!(
            classify_returns(&vararg_body).is_none(),
            "a bare vararg return must stay refused"
        );

        let multi_body = vec![Statement::Return(Return::new(vec![
            string("a"),
            string("b"),
        ]))];
        assert!(
            classify_returns(&multi_body).is_none(),
            "a 2-value return must stay refused"
        );
    }

    // === Soundness-boundary tripwires (lock in the SKIP decisions; guard the
    //     P6/P7 widenings from ever matching an unsound shape) ===

    /// P3: the `anchors_in_block < 2` readability gate keeps a trivial body
    /// (`return x + 1`, 0 anchors) out — de-inlining it to `f(x)` would be LESS
    /// readable than the inlined form. Lowering this gate is the report's
    /// largest-recall idea but is refused on readability grounds.
    #[test]
    fn anchor_gate_refuses_trivial_body_p3() {
        let x = local("x");
        let trivial = canon(&[return_one(add_one(&x))]); // `return x + 1`
        assert!(
            anchors_in_block(&trivial) < 2,
            "a trivial add-one helper must stay below the anchor floor"
        );
    }

    /// P12: `unify_local` injectivity must refuse mapping TWO distinct callee
    /// locals onto ONE caller local — coalescing two simultaneously-live locals
    /// into one would assert shared storage the original did not have.
    #[test]
    fn injectivity_two_locals_one_caller_refused_p12() {
        let a = local("a");
        let b = local("b");
        let mut locals = FxHashSet::default();
        locals.insert(a.clone());
        locals.insert(b.clone());
        let pat = vec![
            Statement::Call(Call::new(global("print"), vec![local_value(&a)])),
            Statement::Call(Call::new(global("print"), vec![local_value(&b)])),
        ];
        let t = void_target(pat, locals);

        let c = local("c");
        let cand = vec![
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        ];
        assert!(
            try_unify_site(&t, &cand, &[], None).is_none(),
            "two callee locals mapping to one caller local must be refused"
        );
    }

    /// P11-A: a window covering a function's ENTIRE top-level body is refused (the
    /// thin-wrapper / mutual-clone hazard — a whole-body structural match is the
    /// least-evidential match for the -O2 marker). The same window matches fine
    /// when it is NOT the whole body.
    #[test]
    fn whole_body_wrapper_refused_p11a() {
        let pat = vec![print_x(), Statement::Call(Call::new(global("foo"), vec![]))];
        let t = void_target(pat, FxHashSet::default());
        let cand = vec![print_x(), Statement::Call(Call::new(global("foo"), vec![]))];
        let mut canon_cache = CanonCache::default();

        // is_func_body_top = true AND the window is the whole body -> refused.
        assert!(
            match_void(&cand, 0, &t, false, true, &[], &mut None, &mut canon_cache, None).is_none(),
            "replacing a function's entire body with one call must be refused"
        );
        // Not the whole body (is_func_body_top = false) -> matches.
        assert!(
            match_void(&cand, 0, &t, false, false, &[], &mut None, &mut canon_cache, None).is_some(),
            "the same region matches when it is not the whole body"
        );
    }

    /// P8: a mutable-parameter accumulator (`p = math.max(p, 0)`, p used as LHS)
    /// must NOT de-inline. Register coalescing makes it `arg = math.max(arg, 0)` on
    /// a caller-visible local in place — `f(arg)` would be wrong (the call does not
    /// write arg). `unify_local`'s param-identity requirement refuses it, which the
    /// P6 prefix widening must not loosen.
    #[test]
    fn mutable_param_accumulator_refused_p8() {
        let p = local("p");
        let math_max = |v: RValue| {
            RValue::Call(Call::new(
                RValue::Index(Index::new(global("math"), string("max"))),
                vec![v, number(0.0)],
            ))
        };
        // helper body: `p = math.max(p, 0) ; return p` (p is a PARAMETER).
        let pat = canon(&[
            assign_local(&p, math_max(local_value(&p)), false),
            return_one(local_value(&p)),
        ]);
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        let mut params = FxHashSet::default();
        params.insert(p.clone());
        let t = Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Value,
            pat_raw_len: 2,
            pat_spine_len: 2,
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtPrefix,
            prefix_len: 1,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params,
            locals: FxHashSet::default(),
            param_order: vec![p.clone()],
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        };

        let arg = local("arg");
        let v = local("v");
        let cand = vec![
            assign_local(&arg, math_max(local_value(&arg)), false),
            init_less_decl(&v),
            assign_local(&v, local_value(&arg), false),
            print_x(),
        ];
        assert!(
            match_value_prefixed(&cand, 0, &t, None, false, &mut None).is_none(),
            "an in-place accumulator with a param-LHS must not de-inline"
        );
    }

    /// F2: a candidate region carrying TWO interposed `CALL_MARKER`s (two inner
    /// de-inlines in a chained reconstruction) must still match. The old raw ceiling
    /// `pat_raw_len + 1` capped the window at 4 raw statements — one short of the 5
    /// needed (3 calls + 2 markers) — silently missing the outer reconstruction; the
    /// effective-count ceiling (trivia don't consume the budget) reaches it.
    #[test]
    fn void_region_with_two_interposed_markers_matches_f2() {
        let mk = || Statement::Comment(Comment::trailing(CALL_MARKER.to_string()));
        let call = |s: &str| Statement::Call(Call::new(global("print"), vec![string(s)]));
        let pat = vec![call("a"), call("b"), call("c")];
        let t = void_target(pat, FxHashSet::default());
        let cand = vec![
            call("a"),
            mk(),
            call("b"),
            mk(),
            call("c"),
            print_x(), // trailing real stmt: the window must stop before it (canon != kc)
        ];
        let mut canon_cache = CanonCache::default();
        let hit = match_void(&cand, 0, &t, false, false, &[], &mut None, &mut canon_cache, None)
            .expect("two interposed markers must not exceed the effective window ceiling");
        assert_eq!(
            hit.consume, 5,
            "window spans the 3 calls + 2 interior markers"
        );
    }

    /// Build a Void target with the given param order and a set of NEVER-read params
    /// (F6a). `print(read_param)` twice is the body; the read param binds, the unread
    /// ones are supplied as `nil` (trailing ones trimmed) by `try_unify_site`.
    fn unused_param_void_target(
        param_order: Vec<RcLocal>,
        read: &RcLocal,
        unread: &[RcLocal],
    ) -> Target {
        let pat = vec![
            Statement::Call(Call::new(global("print"), vec![local_value(read)])),
            Statement::Call(Call::new(global("print"), vec![local_value(read)])),
        ];
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        let params: FxHashSet<RcLocal> = param_order.iter().cloned().collect();
        let unread_set: FxHashSet<RcLocal> = unread.iter().cloned().collect();
        Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            // These F6a tests call `try_unify_site` directly (which never reads
            // `pat_raw_len`); canon len == raw len == 2 here, so the nominal value is
            // fine. A window-scan (match_void) test would need the RAW body length.
            pat_raw_len: pat.len(),
            pat_spine_len: tail_spine_len(&pat),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params,
            locals: FxHashSet::default(),
            param_order,
            written_params: Vec::new(),
            unread: unread_set,
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        }
    }

    /// F6a: a TRAILING never-read parameter no longer blocks de-inline; the dropped
    /// arg is trimmed (`f(a, unused)` called `f(x, 999)` reconstructs as `f(x)`).
    #[test]
    fn unused_trailing_param_de_inlines_with_trimmed_arg_f6a() {
        let a = local("a");
        let unused = local("u");
        let t = unused_param_void_target(vec![a.clone(), unused.clone()], &a, &[unused.clone()]);
        let c = local("c");
        let cand = vec![
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        ];
        let u = try_unify_site(&t, &cand, &[], None).expect("unused trailing param must not block de-inline");
        assert_eq!(
            u.args.len(),
            1,
            "trailing nil for the unused param is trimmed"
        );
        assert!(matches!(&u.args[0], RValue::Local(l) if l == &c));
    }

    /// F6a: an INTERIOR never-read parameter is preserved as `nil` to keep positions
    /// (`f(unused, b)` called `f(999, x)` reconstructs as `f(nil, x)`).
    #[test]
    fn unused_interior_param_de_inlines_with_nil_f6a() {
        let unused = local("u");
        let b = local("b");
        let t = unused_param_void_target(vec![unused.clone(), b.clone()], &b, &[unused.clone()]);
        let c = local("c");
        let cand = vec![
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
        ];
        let u = try_unify_site(&t, &cand, &[], None).expect("interior unused param must not block de-inline");
        assert_eq!(u.args.len(), 2);
        assert!(
            matches!(&u.args[0], RValue::Literal(Literal::Nil)),
            "interior unused param -> nil placeholder"
        );
        assert!(matches!(&u.args[1], RValue::Local(l) if l == &c));
    }

    /// F6a soundness boundary: a param that the body READS but that fails to bind
    /// (genuine mismatch) still refuses the whole site — only NEVER-read params get
    /// the nil treatment.
    #[test]
    fn read_param_that_fails_to_bind_still_refused_f6a() {
        let a = local("a");
        // `a` IS read by the body but is NOT in `unread`.
        let t = unused_param_void_target(vec![a.clone()], &a, &[]);
        // candidate whose second print reads a DIFFERENT local than the first ->
        // `a` binds to the first, the second occurrence mismatches -> refuse.
        let c = local("c");
        let d = local("d");
        let cand = vec![
            Statement::Call(Call::new(global("print"), vec![local_value(&c)])),
            Statement::Call(Call::new(global("print"), vec![local_value(&d)])),
        ];
        assert!(
            try_unify_site(&t, &cand, &[], None).is_none(),
            "a read param with inconsistent bindings must refuse, never default to nil"
        );
    }

    // === §8: call-site value de-inline with an interposed RESULT decl ===

    fn boolean(b: bool) -> RValue {
        RValue::Literal(Literal::Boolean(b))
    }

    fn field(obj: RValue, name: &str) -> RValue {
        RValue::Index(Index::new(obj, string(name)))
    }

    fn bin(left: RValue, op: BinaryOperation, right: RValue) -> RValue {
        RValue::Binary(Binary::new(left, right, op))
    }

    fn call1(callee: RValue, arg: RValue) -> RValue {
        RValue::Call(Call::new(callee, vec![arg]))
    }

    fn not_rv(v: RValue) -> RValue {
        RValue::Unary(Unary {
            node_origin: Default::default(),
            value: Box::new(v),
            operation: UnaryOperation::Not,
        })
    }

    fn if_stmt(cond: RValue, then_b: Vec<Statement>, else_b: Vec<Statement>) -> Statement {
        Statement::If(If::new(cond, Block(then_b), Block(else_b)))
    }

    /// init-less `local l` (a RESULT-register declaration).
    fn init_less_decl(l: &RcLocal) -> Statement {
        Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(l.clone())],
            right: vec![],
            prefix: true,
            parallel: false, compound: false,
        })
    }

    fn void_return() -> Statement {
        Statement::Return(Return::default())
    }

    /// Build an `AtPrefix` Value target directly from a callee body whose canon is
    /// `[<one prefix Assign>, <value branch>]` (mirrors `collect_targets`).
    fn value_prefix_target(body: &[Statement]) -> Target {
        let pat = canon(body);
        assert_eq!(pat.len(), 2, "test body must canon to [prefix, branch]");
        assert!(
            matches!(pat[0], Statement::Assign(_)),
            "prefix must be an Assign"
        );
        let mut locals = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Value,
            pat_raw_len: body.len(),
            pat_spine_len: body.len(),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtPrefix,
            prefix_len: 1,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: FxHashSet::default(),
            locals,
            param_order: Vec::new(),
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        }
    }

    /// P6: build an `AtPrefix` Value target with K>=1 leading non-branch prefix
    /// statements (`prefix_len == pat.len() - 1`), mirroring `collect_targets`.
    fn value_prefix_target_k(body: &[Statement]) -> Target {
        let pat = canon(body);
        let k = pat.len() - 1;
        assert!(k >= 1, "need at least one prefix statement");
        assert!(
            pat[..k].iter().all(|s| matches!(
                s,
                Statement::Assign(_) | Statement::Call(_) | Statement::MethodCall(_)
            )),
            "prefix statements must be non-branch"
        );
        let mut locals = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Value,
            pat_raw_len: body.len(),
            pat_spine_len: body.len(),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtPrefix,
            prefix_len: k,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: FxHashSet::default(),
            locals,
            param_order: Vec::new(),
            written_params: Vec::new(),
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        }
    }

    /// The flagship AfkClient `isAfkEnabled` case: a guard-leading value callee with
    /// a callee-prefix local before the interposed RESULT decl. Exercises BOTH §8
    /// changes — the prefix-aware window AND the guard-polarity flip (the inline
    /// copy's `if Enabled == false then v2=false …` is the NEGATED+SWAPPED mirror of
    /// the canon'd pattern's `if Enabled ~= false then … else return false`).
    #[test]
    fn afk_value_prefix_guard_flip_matches() {
        let afk = local("afkConfig"); // external/upvalue — same RcLocal both sides
        let place_id = local("placeId");
        let body = vec![
            assign_local(
                &place_id,
                call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
                true,
            ),
            if_stmt(
                bin(
                    field(local_value(&afk), "Enabled"),
                    BinaryOperation::Equal,
                    boolean(false),
                ),
                vec![return_one(boolean(false))],
                vec![],
            ),
            if_stmt(
                bin(
                    local_value(&place_id),
                    BinaryOperation::And,
                    bin(
                        local_value(&place_id),
                        BinaryOperation::GreaterThan,
                        number(0.0),
                    ),
                ),
                vec![return_one(bin(
                    field(global("game"), "PlaceId"),
                    BinaryOperation::Equal,
                    local_value(&place_id),
                ))],
                vec![return_one(bin(
                    field(global("game"), "PlaceId"),
                    BinaryOperation::Equal,
                    number(0.0),
                ))],
            ),
        ];
        let t = value_prefix_target(&body);

        let v = local("v");
        let v2 = local("v2");
        let candidate = vec![
            assign_local(
                &v,
                call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
                true,
            ),
            init_less_decl(&v2),
            if_stmt(
                bin(
                    field(local_value(&afk), "Enabled"),
                    BinaryOperation::Equal,
                    boolean(false),
                ),
                vec![assign_local(&v2, boolean(false), false)],
                vec![if_stmt(
                    bin(
                        local_value(&v),
                        BinaryOperation::And,
                        bin(local_value(&v), BinaryOperation::GreaterThan, number(0.0)),
                    ),
                    vec![assign_local(
                        &v2,
                        bin(
                            field(global("game"), "PlaceId"),
                            BinaryOperation::Equal,
                            local_value(&v),
                        ),
                        false,
                    )],
                    vec![assign_local(
                        &v2,
                        bin(
                            field(global("game"), "PlaceId"),
                            BinaryOperation::Equal,
                            number(0.0),
                        ),
                        false,
                    )],
                )],
            ),
            if_stmt(not_rv(local_value(&v2)), vec![void_return()], vec![]),
        ];

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
            .expect("isAfkEnabled prefix + guard-polarity flip should match");
        assert_eq!(hit.consume, 3, "consume prefix + decl + value branch");
        assert_eq!(hit.results, vec![v2]);
        assert!(hit.args.is_empty(), "isAfkEnabled has no parameters");
    }

    /// An if/else value callee (non-empty else, NOT a guard) keeps the SAME polarity
    /// on both sides, so the prefix fix alone suffices and the flip is a no-op.
    #[test]
    fn value_prefix_if_else_matches_without_flip() {
        let obj = local("obj");
        let k = local("k");
        let body = vec![
            assign_local(&k, field(local_value(&obj), "Field"), true),
            if_stmt(
                bin(local_value(&k), BinaryOperation::Equal, number(1.0)),
                vec![return_one(string("a"))],
                vec![return_one(string("b"))],
            ),
        ];
        let t = value_prefix_target(&body);

        let k2 = local("k2");
        let v = local("v");
        let candidate = vec![
            assign_local(&k2, field(local_value(&obj), "Field"), true),
            init_less_decl(&v),
            if_stmt(
                bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
                vec![assign_local(&v, string("a"), false)],
                vec![assign_local(&v, string("b"), false)],
            ),
            print_x(), // trailing stmt: window isn't whole-body; doesn't read k2
        ];

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
            .expect("if/else value prefix should match without a flip");
        assert_eq!(hit.consume, 3);
        assert_eq!(hit.results, vec![v]);
    }

    /// F10a hardening: a value-return result-write leaf carrying `prefix = true` (a
    /// `local v = X` redeclaration rather than the plain `v = X` reassignment the
    /// single-declaration invariant guarantees) must NOT unify as the result lane —
    /// splicing it would change `v`'s scope. The (Return, Assign) arm now requires
    /// `!prefix && !parallel`, mirroring `result_decl` and the (Assign, Assign) arm.
    #[test]
    fn value_leaf_with_prefix_redeclaration_refused_f10a() {
        let obj = local("obj");
        let k = local("k");
        let body = vec![
            assign_local(&k, field(local_value(&obj), "Field"), true),
            if_stmt(
                bin(local_value(&k), BinaryOperation::Equal, number(1.0)),
                vec![return_one(string("a"))],
                vec![return_one(string("b"))],
            ),
        ];
        let t = value_prefix_target(&body);

        let k2 = local("k2");
        let v = local("v");
        let candidate = vec![
            assign_local(&k2, field(local_value(&obj), "Field"), true),
            init_less_decl(&v),
            if_stmt(
                bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
                // result-write leaves as `local v = …` (prefix=true) -> refused.
                vec![assign_local(&v, string("a"), true)],
                vec![assign_local(&v, string("b"), true)],
            ),
            print_x(),
        ];

        assert!(
            match_value_prefixed(&candidate, 0, &t, None, false, &mut None).is_none(),
            "a prefix=true result-write leaf must not unify as the result lane (F10a)"
        );
    }

    /// P1 regression: a `CALL_MARKER` an inner de-inline spliced between the
    /// callee-prefix statement and the interposed `local RESULT` decl must NOT
    /// break the AtPrefix match. The old `d = i + p` offset pointed at the marker
    /// (`result_decl` -> None -> bail), silently killing chained reconstruction;
    /// `nth_effective_index` skips the marker and still finds the decl, and the
    /// `consume` span removes the marker along with the window.
    #[test]
    fn value_prefix_marker_between_prefix_and_result_decl_still_matches() {
        let obj = local("obj");
        let k = local("k");
        let body = vec![
            assign_local(&k, field(local_value(&obj), "Field"), true),
            if_stmt(
                bin(local_value(&k), BinaryOperation::Equal, number(1.0)),
                vec![return_one(string("a"))],
                vec![return_one(string("b"))],
            ),
        ];
        let t = value_prefix_target(&body);

        let k2 = local("k2");
        let v = local("v");
        let marker = Statement::Comment(Comment::trailing(CALL_MARKER.to_string()));
        let candidate = vec![
            assign_local(&k2, field(local_value(&obj), "Field"), true),
            marker, // interposed by an inner de-inline of the prefix
            init_less_decl(&v),
            if_stmt(
                bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
                vec![assign_local(&v, string("a"), false)],
                vec![assign_local(&v, string("b"), false)],
            ),
            print_x(), // trailing stmt: window isn't whole-body; doesn't read k2
        ];

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
            .expect("interposed marker must not break the AtPrefix match");
        // span = prefix(0) + marker(1) + decl(2) + region-if(3): removes 4 stmts,
        // leaving the trailing print.
        assert_eq!(hit.consume, 4);
        assert_eq!(hit.results, vec![v]);
    }

    /// P6: a Value helper with TWO leading non-branch prefix statements (K==2)
    /// inlines as `<prefix1> ; <prefix2> ; local RESULT ; <value branch>`. The
    /// generalised `prefix_len = pat.len()-1` + logical-index RESULT lookup must
    /// match it (the old K==1 scope refused everything but a single prefix stmt).
    #[test]
    fn value_prefix_k2_matches() {
        let obj = local("obj");
        let a = local("a");
        let b = local("b");
        let body = vec![
            assign_local(&a, field(local_value(&obj), "A"), true),
            assign_local(&b, field(local_value(&obj), "B"), true),
            if_stmt(
                bin(
                    local_value(&a),
                    BinaryOperation::GreaterThan,
                    local_value(&b),
                ),
                vec![return_one(local_value(&a))],
                vec![return_one(local_value(&b))],
            ),
        ];
        let t = value_prefix_target_k(&body);
        assert_eq!(t.prefix_len, 2);

        let a2 = local("a2");
        let b2 = local("b2");
        let v = local("v");
        let candidate = vec![
            assign_local(&a2, field(local_value(&obj), "A"), true),
            assign_local(&b2, field(local_value(&obj), "B"), true),
            init_less_decl(&v),
            if_stmt(
                bin(
                    local_value(&a2),
                    BinaryOperation::GreaterThan,
                    local_value(&b2),
                ),
                vec![assign_local(&v, local_value(&a2), false)],
                vec![assign_local(&v, local_value(&b2), false)],
            ),
            print_x(), // trailing: window isn't whole-body
        ];

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
            .expect("K==2 value prefix should match");
        // span = prefix a2(0) + prefix b2(1) + RESULT decl(2) + value-if(3).
        assert_eq!(hit.consume, 4);
        assert_eq!(hit.results, vec![v]);
    }

    /// P9: a guard whose condition is RELATIONAL (`<`) IS now polarity-flipped.
    /// The flip is the value-exact, NaN-safe identity `if C then A else B ≡
    /// if not C then B else A` realised by a structural `not`-wrap — it never
    /// rewrites `not (k < 0)` into the NaN-unsafe `k >= 0`, so it is sound for any
    /// condition. (Was `refuse_relational_guard_not_flipped` pre-P9.)
    #[test]
    fn relational_guard_is_polarity_flipped() {
        let obj = local("obj");
        let k = local("k");
        let body = vec![
            assign_local(&k, field(local_value(&obj), "Field"), true),
            if_stmt(
                bin(local_value(&k), BinaryOperation::LessThan, number(0.0)),
                vec![return_one(boolean(false))],
                vec![],
            ),
            return_one(local_value(&k)),
        ];
        let t = value_prefix_target(&body); // pat = [assign k, If(not(k<0), [return k], [return false])]

        let k2 = local("k2");
        let v = local("v");
        let candidate = vec![
            assign_local(&k2, field(local_value(&obj), "Field"), true),
            init_less_decl(&v),
            if_stmt(
                bin(local_value(&k2), BinaryOperation::LessThan, number(0.0)),
                vec![assign_local(&v, boolean(false), false)],
                vec![assign_local(&v, local_value(&k2), false)],
            ),
            print_x(),
        ];

        // f(obj) = k=obj.Field; if k<0 then return false end; return k. The
        // candidate computes v = (k2<0) ? false : k2 == f(obj). The flip negates
        // the candidate's `k2<0` to `not (k2<0)` (NOT `k2>=0`) and swaps branches.
        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut None)
            .expect("relational guard condition IS polarity-flipped under P9");
        assert_eq!(hit.consume, 3); // prefix k2(0) + RESULT decl(1) + value-if(2)
        assert_eq!(hit.results, vec![v]);
    }

    /// Red-team: the polarity flip lines the diamond up correctly, but a leaf value
    /// DIVERGES from the pattern. Exact unification must still refuse — the flip is
    /// only a structural re-orientation, never a relaxation of value equality.
    #[test]
    fn flip_with_divergent_leaf_is_refused() {
        let afk = local("afkConfig");
        let place_id = local("placeId");
        let body = vec![
            assign_local(
                &place_id,
                call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
                true,
            ),
            if_stmt(
                bin(
                    field(local_value(&afk), "Enabled"),
                    BinaryOperation::Equal,
                    boolean(false),
                ),
                vec![return_one(boolean(false))],
                vec![],
            ),
            if_stmt(
                bin(
                    local_value(&place_id),
                    BinaryOperation::And,
                    bin(
                        local_value(&place_id),
                        BinaryOperation::GreaterThan,
                        number(0.0),
                    ),
                ),
                vec![return_one(bin(
                    field(global("game"), "PlaceId"),
                    BinaryOperation::Equal,
                    local_value(&place_id),
                ))],
                vec![return_one(bin(
                    field(global("game"), "PlaceId"),
                    BinaryOperation::Equal,
                    number(0.0),
                ))],
            ),
        ];
        let t = value_prefix_target(&body);

        let v = local("v");
        let v2 = local("v2");
        let candidate = vec![
            assign_local(
                &v,
                call1(global("tonumber"), field(local_value(&afk), "PlaceId")),
                true,
            ),
            init_less_decl(&v2),
            if_stmt(
                bin(
                    field(local_value(&afk), "Enabled"),
                    BinaryOperation::Equal,
                    boolean(false),
                ),
                // DIVERGENT: pattern's early-return value is `false`, here it is `true`.
                vec![assign_local(&v2, boolean(true), false)],
                vec![if_stmt(
                    bin(
                        local_value(&v),
                        BinaryOperation::And,
                        bin(local_value(&v), BinaryOperation::GreaterThan, number(0.0)),
                    ),
                    vec![assign_local(
                        &v2,
                        bin(
                            field(global("game"), "PlaceId"),
                            BinaryOperation::Equal,
                            local_value(&v),
                        ),
                        false,
                    )],
                    vec![assign_local(
                        &v2,
                        bin(
                            field(global("game"), "PlaceId"),
                            BinaryOperation::Equal,
                            number(0.0),
                        ),
                        false,
                    )],
                )],
            ),
            if_stmt(not_rv(local_value(&v2)), vec![void_return()], vec![]),
        ];

        assert!(
            match_value_prefixed(&candidate, 0, &t, None, false, &mut None).is_none(),
            "a divergent leaf literal must be refused even when the flip aligns the diamond"
        );
    }

    // === DeInlineReview fixes ===

    /// F4 FIX 1: `rvalue_exact_eq` is sign-of-zero / NaN bit-exact, unlike the
    /// derived `==` the return-folding and arg-consistency gates previously used.
    #[test]
    fn rvalue_exact_eq_signed_zero_and_nan() {
        assert!(!rvalue_exact_eq(&number(0.0), &number(-0.0)));
        assert!(rvalue_exact_eq(&number(0.0), &number(0.0)));
        // derived `f64` eq says `NaN != NaN`; bit-exact (same payload) says equal —
        // this only RE-ENABLES correct de-inlines, never an unsound one.
        assert!(rvalue_exact_eq(&number(f64::NAN), &number(f64::NAN)));
        // recursion still distinguishes a nested ±0.0.
        assert!(!rvalue_exact_eq(
            &bin(number(1.0), BinaryOperation::Add, number(0.0)),
            &bin(number(1.0), BinaryOperation::Add, number(-0.0)),
        ));
    }

    /// F4 FIX 1 in the return-folding gate: an early `return +0.0` must not be
    /// treated as equal to a tail `return -0.0` (they differ as `1/x`).
    #[test]
    fn value_tail_signed_zero_returns_refused() {
        let body = vec![if_stmt(
            local_value(&local("pred")),
            vec![return_one(number(0.0))],
            vec![],
        )];
        assert!(!all_returns_are(&body, &number(-0.0)));
        assert!(all_returns_are(&body, &number(0.0)));
    }

    /// F4 FIX 2: a parameter occurring twice, bound to two DISTINCT table
    /// constructors, is refused (different table identities); a repeated bare local
    /// (same value) is fine — and a single-use table never reaches the repeat path.
    #[test]
    fn repeated_identity_arg_refused_local_ok() {
        let p = local("p");
        let mut params = FxHashSet::default();
        params.insert(p.clone());
        let locals = FxHashSet::default();
        let ctx = MatchCtx {
            params: &params,
            locals: &locals,
        };

        let mut b = Bindings::default();
        assert!(
            unify_rvalue(
                &ctx,
                &local_value(&p),
                &RValue::Table(Table::default()),
                &mut b
            )
            .is_ok()
        );
        assert!(
            unify_rvalue(
                &ctx,
                &local_value(&p),
                &RValue::Table(Table::default()),
                &mut b
            )
            .is_err(),
            "two distinct `{{}}` arguments must not be shared across param occurrences"
        );

        let x = local("x");
        let mut b2 = Bindings::default();
        assert!(unify_rvalue(&ctx, &local_value(&p), &local_value(&x), &mut b2).is_ok());
        assert!(unify_rvalue(&ctx, &local_value(&p), &local_value(&x), &mut b2).is_ok());
    }

    /// F3 boundary: a synthetic closure has no bytecode provenance, including
    /// when hidden in an if-expression, and therefore remains unsafe.
    #[test]
    fn body_unsafe_sees_closure_inside_if_expression() {
        let closure = RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function::default()))),
            upvalues: Vec::new(),
        });
        let unsafe_body = vec![Statement::Return(Return::new(vec![RValue::IfExpression(
            crate::IfExpression::new(local_value(&local("c")), closure, boolean(false)),
        )]))];
        assert!(body_unsafe(&unsafe_body));

        let safe_body = vec![Statement::Return(Return::new(vec![RValue::IfExpression(
            crate::IfExpression::new(local_value(&local("c")), number(1.0), boolean(false)),
        )]))];
        assert!(!body_unsafe(&safe_body));
    }

    #[test]
    fn bytecode_closure_unifies_by_proto_capture_mode_and_mapping() {
        let parameter = local("parameter");
        let argument = local("argument");
        let make = |proto, upvalue| Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                bytecode_proto_id: Some(proto),
                ..Function::default()
            }))),
            upvalues: vec![upvalue],
        };
        let pattern = RValue::Closure(make(41, Upvalue::Copy(parameter.clone())));
        let candidate = RValue::Closure(make(41, Upvalue::Copy(argument.clone())));
        let mut params = FxHashSet::default();
        params.insert(parameter.clone());
        let locals = FxHashSet::default();
        let ctx = MatchCtx {
            params: &params,
            locals: &locals,
        };
        let mut bindings = Bindings::default();

        unify_rvalue(&ctx, &pattern, &candidate, &mut bindings)
            .expect("same bytecode proto and Copy capture must unify");
        assert!(rvalue_exact_eq(
            bindings.params.get(&parameter).unwrap(),
            &local_value(&argument)
        ));

        let wrong_mode = RValue::Closure(make(41, Upvalue::Ref(argument.clone())));
        assert!(unify_rvalue(&ctx, &pattern, &wrong_mode, &mut Bindings::default()).is_err());

        let wrong_proto = RValue::Closure(make(42, Upvalue::Copy(argument)));
        assert!(unify_rvalue(&ctx, &pattern, &wrong_proto, &mut Bindings::default()).is_err());
    }

    #[test]
    fn body_unsafe_allows_only_bytecode_proven_nested_closures() {
        let callback = |proto| {
            RValue::Closure(Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(Function {
                    bytecode_proto_id: proto,
                    ..Function::default()
                }))),
                upvalues: Vec::new(),
            })
        };
        let body = |value| vec![Statement::Call(Call::new(global("spawn"), vec![value]))];

        assert!(!body_unsafe(&body(callback(Some(7)))));
        assert!(body_unsafe(&body(callback(None))));
    }

    /// F2: an indexed-LHS value collapse is refused (it would reorder the target
    /// prefix relative to the moved-in call); a bare-local LHS still collapses.
    #[test]
    fn collapse_refuses_indexed_lhs_keeps_local_lhs() {
        let v = local("v");
        let t = local("t");
        let call = call1(global("f"), number(1.0));

        let indexed = Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Index(Index::new(local_value(&t), string("field")))],
            right: vec![local_value(&v)],
            prefix: false,
            parallel: false, compound: false,
        });
        let empty = FxHashSet::default();
        assert!(collapse_use(&indexed, &v, &call, &empty).is_none());

        let x = local("x");
        let local_lhs = assign_local(&x, local_value(&v), false);
        match collapse_use(&local_lhs, &v, &call, &empty).expect("local LHS must collapse") {
            Statement::Assign(a) => assert!(matches!(a.right[0], RValue::Call(_))),
            _ => panic!("expected an Assign"),
        }
    }

    /// P7-A regression: only a helper proven to return exactly one value (its
    /// binder is in `single_valued`) is collapsed into a multi-value context.
    /// `local v = helper(args); return v` keeps its form otherwise (a bare
    /// `return helper(args)` would propagate ALL of the helper's values, or none,
    /// where `local v =` adjusted to one); same for a MULTI-LHS `a, b = v`.
    /// Single-value contexts (`if v`, single-LHS `x = v`) collapse for any helper.
    #[test]
    fn multivalue_helper_not_spread_into_multivalue_context_p7a() {
        let helper = local("helper");
        let v = local("v");
        let call = call1(local_value(&helper), number(1.0));
        let mut proven = FxHashSet::default();
        proven.insert(helper.clone());
        let unknown = FxHashSet::default();

        // `return v` — multi-value context: only a proven single-value helper.
        let ret = Statement::Return(Return::new(vec![local_value(&v)]));
        assert!(collapse_use(&ret, &v, &call, &unknown).is_none(), "return v must NOT collapse an unproven helper");
        assert!(collapse_use(&ret, &v, &call, &proven).is_some(), "return v DOES collapse a single-value helper");

        // MULTI-LHS `a, b = v` — multi-value context.
        let a = local("a");
        let b = local("b");
        let multi_lhs = Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Local(a.clone()), LValue::Local(b.clone())],
            right: vec![local_value(&v)],
            prefix: false,
            parallel: false, compound: false,
        });
        assert!(collapse_use(&multi_lhs, &v, &call, &unknown).is_none(), "multi-LHS a,b = v must NOT collapse an unproven helper");

        // SINGLE-LHS `x = v` and `if v` truncate to one value for any helper.
        let x = local("x");
        let single_lhs = assign_local(&x, local_value(&v), false);
        assert!(collapse_use(&single_lhs, &v, &call, &unknown).is_some(), "single-LHS x = v collapses any helper (truncates)");
        let if_v = if_stmt(local_value(&v), vec![print_x()], vec![]);
        assert!(collapse_use(&if_v, &v, &call, &unknown).is_some(), "if v collapses any helper (single-value condition)");
    }

    /// Second review, item 1: the arity proof is read off the declarations
    /// before any rewrite. A declaration rebuilt into `local f = factory()` no
    /// longer shows `f`'s body, which returns `produce(...)`'s results; a
    /// collapse after it must not turn `local r = f(); return r` (one value)
    /// into `return f()`.
    #[test]
    fn closures_are_equal_only_with_the_same_captures() {
        let function = ByAddress(Arc::new(Mutex::new(Function::default())));
        let (a, b) = (local("a"), local("b"));
        let closure = |upvalue: Upvalue| {
            RValue::Closure(Closure { node_origin: Default::default(), function: function.clone(), upvalues: vec![upvalue] })
        };
        assert!(rvalue_exact_eq(&closure(Upvalue::Copy(a.clone())), &closure(Upvalue::Copy(a.clone()))));
        assert!(!rvalue_exact_eq(&closure(Upvalue::Copy(a.clone())), &closure(Upvalue::Copy(b))));
        assert!(!rvalue_exact_eq(&closure(Upvalue::Copy(a.clone())), &closure(Upvalue::Ref(a))));
    }

    #[test]
    fn a_helper_returning_a_call_is_never_proven_single_valued() {
        let produce = |tag: &str| RValue::Call(Call::new(global("produce"), vec![string(tag)]));
        assert!(!returns_exactly_one(&[Statement::Return(Return::new(vec![produce("tag")]))]));
        let one = RValue::Select(crate::Select::Call(Call::new(global("produce"), vec![string("tag")])));
        assert!(returns_exactly_one(&[Statement::Return(Return::new(vec![one]))]));
        // Falls off the end when `c` fails: no value at all.
        let c = local("c");
        let falls = Statement::If(If::new(local_value(&c), Block(vec![return_one(number(1.0))]), Block::default()));
        assert!(!returns_exactly_one(&[falls]));
        assert!(returns_exactly_one(&[return_one(number(1.0))]));
    }

    /// F5: `body_unsafe` exempts our own reconstruction markers (so a callee body
    /// that gained a CALL_MARKER from an inner de-inline stays a valid target),
    /// while still refusing a genuine source comment; `canon_top` drops the marker
    /// so a re-collected pattern stays length-aligned with its candidates.
    #[test]
    fn internal_markers_exempted_in_body_unsafe_and_canon() {
        let marked = vec![
            print_x(),
            Statement::Comment(Comment::trailing(CALL_MARKER.to_string())),
        ];
        assert!(!body_unsafe(&marked));

        let real_comment = vec![
            print_x(),
            Statement::Comment(Comment::new(" a real source comment".to_string())),
        ];
        assert!(body_unsafe(&real_comment));

        assert_eq!(
            canon_top(&marked, true).len(),
            1,
            "marker dropped by canon_top"
        );
    }

    #[test]
    fn canon_preserves_generic_for_provenance() {
        let origin = ForOrigin {
            prep_pc: 10,
            step_pc: 20,
            body_pc: 21,
            follow_pc: 22,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        let statement = GenericFor {
            res_locals: vec![local("value")],
            right: vec![global("items")],
            block: Arc::new(Mutex::new(Block::default())),
            origin: Some(origin),
        }
        .into();

        let canonical = canon(&[statement]);
        assert_eq!(
            canonical[0]
                .as_generic_for()
                .expect("canonicalization retains the loop")
                .origin,
            Some(origin)
        );
    }

    #[test]
    fn consuming_unguard_matches_reference_shape_origins_and_owners() {
        use crate::{node_origins, Traverse, Upvalue};
        type OriginSnapshot = Option<(Vec<std::sync::Arc<node_origins::Input>>, bool, bool, bool, Option<&'static str>)>;
        fn origin(origin: &node_origins::Origin, out: &mut Vec<OriginSnapshot>) {
            out.push(origin.0.as_ref().map(|data| (data.inputs.clone(), data.inlined,
                data.cloned, data.incomplete, data.synthesized)));
        }
        fn snapshot_value(value: &RValue, tags: &mut Vec<OriginSnapshot>, numbers: &mut Vec<u64>) {
            if let Some(value) = node_origins::value(value) { origin(value, tags); }
            if let RValue::Literal(Literal::Number(value)) = value { numbers.push(value.to_bits()); }
            if let RValue::Closure(closure) = value {
                snapshot(&closure.function.0.lock().body.0, tags, numbers);
            }
            value.visit_rvalues(&mut |value| { snapshot_value(value, tags, numbers); true });
        }
        fn snapshot(statements: &[Statement], tags: &mut Vec<OriginSnapshot>, numbers: &mut Vec<u64>) {
            for statement in statements {
                if let Some(value) = node_origins::statement(statement) { origin(value, tags); }
                for value in stmt_rvalues(statement) { snapshot_value(value, tags, numbers); }
                match statement {
                    Statement::If(node) => {
                        snapshot(&node.then_block.lock().0, tags, numbers);
                        snapshot(&node.else_block.lock().0, tags, numbers);
                    }
                    Statement::While(node) => snapshot(&node.block.lock().0, tags, numbers),
                    Statement::Repeat(node) => snapshot(&node.block.lock().0, tags, numbers),
                    Statement::NumericFor(node) => snapshot(&node.block.lock().0, tags, numbers),
                    Statement::GenericFor(node) => snapshot(&node.block.lock().0, tags, numbers),
                    _ => {}
                }
            }
        }
        fn new_origin(index: &mut usize) -> node_origins::Origin {
            *index += 1;
            let mut origin = node_origins::Origin::input(node_origins::Input {
                function: "unguard_differential".into(), block: *index / 4,
                statement: *index, value: Some(*index % 4),
            });
            let data = origin.0.as_mut().unwrap();
            data.inlined = *index & 1 != 0;
            data.cloned = *index & 2 != 0;
            data.incomplete = *index & 4 != 0;
            if *index & 8 != 0 { data.synthesized = Some("test_origin"); }
            origin
        }
        fn annotate_value(value: &mut RValue, index: &mut usize) {
            if let Some(origin) = node_origins::value_mut(value) { *origin = new_origin(index); }
            if let RValue::Closure(closure) = value {
                annotate(&mut closure.function.0.lock().body.0, index);
            }
            value.visit_rvalues_mut(&mut |value| { annotate_value(value, index); true });
        }
        fn annotate(statements: &mut [Statement], index: &mut usize) {
            for statement in statements {
                if let Some(origin) = node_origins::statement_mut(statement) { *origin = new_origin(index); }
                for value in stmt_rvalues_mut(statement) { annotate_value(value, index); }
                match statement {
                    Statement::If(node) => {
                        annotate(&mut node.then_block.lock().0, index);
                        annotate(&mut node.else_block.lock().0, index);
                    }
                    Statement::While(node) => annotate(&mut node.block.lock().0, index),
                    Statement::Repeat(node) => annotate(&mut node.block.lock().0, index),
                    _ => {}
                }
            }
        }
        let binding = local("value");
        for seed in 0..1024usize {
            let function = Arc::new(Mutex::new(Function {
                bytecode_proto_id: Some(7),
                body: Block(vec![return_one(add_one(&binding))]),
                ..Default::default()
            }));
            let closure = || RValue::Closure(Closure {
                node_origin: Default::default(), function: ByAddress(function.clone()),
                upvalues: vec![Upvalue::Ref(binding.clone()), Upvalue::Copy(binding.clone())],
            });
            let condition = |choice: usize| {
                let comparison: RValue = Binary::new(local_value(&binding),
                    number(f64::from_bits(0x7ff8_0000_0000_1234)),
                    [BinaryOperation::Equal, BinaryOperation::NotEqual,
                        BinaryOperation::LessThan, BinaryOperation::LessThanOrEqual][choice % 4]).into();
                if choice & 4 == 0 { comparison }
                else { Unary::new(comparison, UnaryOperation::Not).into() }
            };
            let mut source = Vec::new();
            let mut choices = seed;
            for at in 0..7 {
                let choice = (choices + at) % 10;
                choices = choices / 7 + 3;
                source.push(match choice {
                    0 => print_x(),
                    1 => Statement::Call(Call::new(global("consume"), vec![closure(), string("a\0\u{ff}7")])),
                    2 => void_return(),
                    3 => return_one(number(-0.0)),
                    4 => if_stmt(condition(seed + at), vec![void_return()], vec![]),
                    5 => if_stmt(condition(seed + at), vec![print_x(), return_one(add_one(&binding))], vec![]),
                    6 => if_stmt(condition(seed + at), vec![return_one(closure())], vec![]),
                    7 => if_stmt(condition(seed + at), vec![void_return()], vec![print_x()]),
                    8 => if_stmt(condition(seed + at), vec![return_one(number(2.0)), print_x()], vec![]),
                    _ => Statement::While(While::new(condition(seed + at), Block(vec![
                        if_stmt(condition(seed), vec![void_return()], vec![]), print_x()]))),
                });
            }
            annotate(&mut source, &mut 0);
            let (mut source_tags, mut source_numbers) = (Vec::new(), Vec::new());
            snapshot(&source, &mut source_tags, &mut source_numbers);
            // This is unguard's exact production precondition: canon_top has
            // already cloned retained statements, but still shares block/body Arcs.
            let actual = unguard(source.clone());
            let shape = format!("{actual:?}");
            let rendered = Block(actual.clone()).to_string();
            let (mut tags, mut numbers) = (Vec::new(), Vec::new());
            snapshot(&actual, &mut tags, &mut numbers);
            let owners = (Arc::count(&binding.0.0), Arc::strong_count(&function));
            drop(actual);
            let expected = unguard_reference(source.clone());
            assert_eq!(format!("{expected:?}"), shape, "seed {seed}: shape");
            assert_eq!(Block(expected.clone()).to_string(), rendered, "seed {seed}: source");
            let (mut expected_tags, mut expected_numbers) = (Vec::new(), Vec::new());
            snapshot(&expected, &mut expected_tags, &mut expected_numbers);
            assert_eq!(expected_tags, tags, "seed {seed}: full origins");
            assert_eq!(expected_numbers, numbers, "seed {seed}: float bits");
            assert_eq!((Arc::count(&binding.0.0), Arc::strong_count(&function)), owners,
                "seed {seed}: local and closure ownership");
            let (mut after_tags, mut after_numbers) = (Vec::new(), Vec::new());
            snapshot(&source, &mut after_tags, &mut after_numbers);
            assert_eq!(after_tags, source_tags, "seed {seed}: shared inputs unchanged");
            assert_eq!(after_numbers, source_numbers);
        }
    }

    #[test]
    fn consuming_unguard_reuses_already_cloned_operand_and_literal_storage() {
        fn storage(statements: &[Statement]) -> Vec<(usize, usize)> {
            statements.iter().map(|statement| {
                let Statement::Call(call) = statement else { unreachable!() };
                let RValue::Literal(Literal::String(bytes)) = &call.arguments[0] else { unreachable!() };
                (call.value.as_ref() as *const RValue as usize, bytes.as_ptr() as usize)
            }).collect()
        }
        for count in [64, 256, 1024] {
            let source: Vec<_> = (0..count).map(|_| Statement::Call(Call::new(
                global("observe"), vec![Literal::String(vec![0xff; 128]).into()]))).collect();
            let prepared = source.clone();
            let before = storage(&prepared);
            assert_eq!(storage(&unguard(prepared)), before);
            let reference_input = source.clone();
            let before = storage(&reference_input);
            let after = storage(&unguard_reference(reference_input));
            assert!(before.iter().zip(&after).all(|(a, b)| a.0 != b.0 && a.1 != b.1));
        }
    }

    /// Exhaustive equivalence: `canon_top_len(stmts, tail) == canon_top(stmts, tail).len()`
    /// over EVERY sequence of length 0..=4 from a canon-relevant alphabet (Empty /
    /// internal-marker / source-comment trivia; plain / void-return / value-return
    /// statements; foldable + several non-foldable guard shapes; a 2-value return), for
    /// both tail values — ~41k cases. Computes the real length via `canon_top` directly
    /// (independent of the in-function debug_assert), pinning the non-allocating length
    /// mirror to `canon_top` even for release builds where the debug_assert is gone.
    #[test]
    fn canon_top_len_mirrors_canon_top_exhaustively() {
        let make = |sym: u8| -> Statement {
            match sym {
                0 => Statement::Empty(Empty {}),
                1 => Statement::Comment(Comment::trailing(CALL_MARKER.to_string())), // internal trivia
                2 => Statement::Comment(Comment::new(" source".to_string())),        // NOT trivia
                3 => print_x(),                                                      // plain stmt
                4 => void_return(),                                                  // void return
                5 => return_one(number(1.0)),                                        // value return
                6 => if_stmt(global("c"), vec![void_return()], vec![]), // foldable void guard
                7 => if_stmt(global("c"), vec![return_one(number(2.0))], vec![]), // foldable value guard
                8 => if_stmt(global("c"), vec![void_return()], vec![print_x()]),  // else nonempty
                9 => if_stmt(global("c"), vec![print_x(), void_return()], vec![]), // then len 2
                10 => if_stmt(global("c"), vec![print_x()], vec![]),              // then non-return
                _ => if_stmt(
                    global("c"),
                    vec![Statement::Return(Return::new(vec![
                        number(1.0),
                        number(2.0),
                    ]))],
                    vec![],
                ), // 2-value return then-block
            }
        };
        const ALPHA: u8 = 12;
        for len in 0..=4usize {
            let mut idx = vec![0u8; len];
            loop {
                let stmts: Vec<Statement> = idx.iter().map(|&s| make(s)).collect();
                for &tail in &[false, true] {
                    assert_eq!(
                        canon_top_len(&stmts, tail),
                        canon_top(&stmts, tail).len(),
                        "canon_top_len mismatch: tail={} seq={:?}",
                        tail,
                        idx
                    );
                }
                if len == 0 {
                    break;
                }
                let mut p = len - 1;
                loop {
                    idx[p] += 1;
                    if idx[p] < ALPHA {
                        break;
                    }
                    idx[p] = 0;
                    if p == 0 {
                        break;
                    }
                    p -= 1;
                }
                if idx.iter().all(|&x| x == 0) {
                    break;
                }
            }
        }
    }

    /// P-perf prefilter: `stmt_anchor_key` must give EQUAL keys for equal fixed names
    /// and DISTINCT keys for distinct names (a method name, or a global-call callee),
    /// and `None` where there is no fixed name (a local-callee call, a non-call). This
    /// is the contract the name prefilter relies on for false-negative freedom.
    #[test]
    fn stmt_anchor_key_contract() {
        let recv = local("o");
        let mc = |m: &str| {
            Statement::MethodCall(MethodCall {
                node_origin: Default::default(),
                value: Box::new(local_value(&recv)),
                method: m.to_string(),
                arguments: vec![],
            })
        };
        assert!(stmt_anchor_key(&mc("Foo")).is_some());
        assert_eq!(stmt_anchor_key(&mc("Foo")), stmt_anchor_key(&mc("Foo")));
        assert_ne!(stmt_anchor_key(&mc("Foo")), stmt_anchor_key(&mc("Bar")));

        let gc = |g: &str| Statement::Call(Call::new(global(g), vec![]));
        assert!(stmt_anchor_key(&gc("foo")).is_some());
        assert_ne!(stmt_anchor_key(&gc("foo")), stmt_anchor_key(&gc("bar")));
        // method vs global with same text are distinct (kind-tagged).
        assert_ne!(stmt_anchor_key(&mc("foo")), stmt_anchor_key(&gc("foo")));

        // no fixed name -> None (the prefilter then never skips).
        assert!(stmt_anchor_key(&Statement::Call(Call::new(local_value(&recv), vec![]))).is_none());
        assert!(stmt_anchor_key(&void_return()).is_none());
        assert!(stmt_anchor_key(&print_x()).is_some()); // print(...) is a global call
    }

    // ---- ROADMAP C: canon/shape extensions ----

    fn print_local(l: &RcLocal) -> Statement {
        Statement::Call(Call::new(global("print"), vec![local_value(l)]))
    }


    #[test]
    fn tail_spine_len_counts_lifted_arms() {
        // `a; if c then x; y end; return` -> at a site: `a; if not c then return end; x; y; return`
        let c = local("c");
        let body = vec![
            print_x(),
            if_stmt(local_value(&c), vec![print_x(), print_x()], vec![]),
            Statement::Return(Return::default()),
        ];
        assert_eq!(tail_spine_len(&body), 3 + 2);
        // nested tail `if`s lift recursively
        let nested = vec![if_stmt(
            local_value(&c),
            vec![print_x(), if_stmt(local_value(&c), vec![print_x()], vec![print_x()])],
            vec![],
        )];
        assert_eq!(tail_spine_len(&nested), 1 + 2 + 1 + 1);
    }

    #[test]
    fn void_return_after_statement_makes_it_tail() {
        assert!(continues_with_void_return_only(&[Statement::Return(Return::default())]));
        assert!(continues_with_void_return_only(&[
            Statement::Empty(Empty {}),
            Statement::Return(Return::default()),
        ]));
        assert!(!continues_with_void_return_only(&[]));
        assert!(!continues_with_void_return_only(&[print_x(), Statement::Return(Return::default())]));
        assert!(!continues_with_void_return_only(&[return_one(number(1.0))]));
        assert!(!continues_with_void_return_only(&[Statement::Break(Break {})]));
        // The backward pass agrees with scanning every suffix.
        let empty = || Statement::Empty(Empty {});
        let void = || Statement::Return(Return::default());
        let blocks = [
            vec![print_x(), empty(), void(), empty()],
            vec![void(), empty(), void()],
            vec![empty(), empty(), print_x()],
            vec![print_x(), return_one(number(1.0)), empty()],
        ];
        for block in blocks {
            let expected: Vec<bool> = (0..block.len()).map(|j| continues_with_void_return_only(&block[j + 1..])).collect();
            assert_eq!(void_return_tails(&block), expected);
        }
    }

    #[test]
    fn arm_tail_ret_requires_every_path_to_return_the_value() {
        let clone = local("clone");
        let c = local("c");
        let window = vec![if_stmt(
            local_value(&c),
            vec![print_x(), return_one(local_value(&clone))],
            vec![return_one(local_value(&clone))],
        )];
        let ret = arm_tail_ret(&window, 0, 1, true).expect("both arms return `clone`");
        assert!(rvalue_exact_eq(&ret, &local_value(&clone)));
        // not at the block end / not func tail -> refused
        assert!(arm_tail_ret(&window, 0, 1, false).is_none());
        // a fall-through arm -> refused
        let falls = vec![if_stmt(
            local_value(&c),
            vec![print_x()],
            vec![return_one(local_value(&clone))],
        )];
        assert!(arm_tail_ret(&falls, 0, 1, true).is_none());
        // the value must not be written inside the window
        let written = vec![if_stmt(
            local_value(&c),
            vec![assign_local(&clone, number(1.0), false), return_one(local_value(&clone))],
            vec![return_one(local_value(&clone))],
        )];
        assert!(arm_tail_ret(&written, 0, 1, true).is_none());
    }

    #[test]
    fn alias_result_leaves_rewrites_early_result_write() {
        // if c then RESULT = create(); use(RESULT) else RESULT = x end
        let r = local("result");
        let c = local("c");
        let x = local("x");
        let region = vec![if_stmt(
            local_value(&c),
            vec![
                assign_local(&r, RValue::Call(Call::new(global("create"), vec![])), false),
                print_local(&r),
            ],
            vec![assign_local(&r, local_value(&x), false)],
        )];
        let rewritten = alias_result_leaves(&region, &r).expect("then-leaf is the alias shape");
        let Statement::If(f) = &rewritten[0] else {
            panic!()
        };
        let then = f.then_block.lock();
        assert_eq!(then.0.len(), 3);
        let Statement::Assign(decl) = &then.0[0] else {
            panic!()
        };
        assert!(decl.prefix, "the early write becomes a fresh local decl");
        let LValue::Local(t) = &decl.left[0] else {
            panic!()
        };
        assert_ne!(t, &r);
        assert_eq!(count_local_reads(&then.0[1..2], t), 1, "uses are redirected to the temp");
        let Statement::Assign(last) = &then.0[2] else {
            panic!()
        };
        assert!(!last.prefix && matches!(&last.left[0], LValue::Local(l) if l == &r));
        // the else leaf already ends in the result write: untouched
        assert_eq!(f.else_block.lock().0.len(), 1);
        // a leaf already ending in `RESULT = X` everywhere -> nothing to do
        let plain = vec![if_stmt(
            local_value(&c),
            vec![assign_local(&r, number(1.0), false)],
            vec![assign_local(&r, local_value(&x), false)],
        )];
        assert!(alias_result_leaves(&plain, &r).is_none());
        // RESULT read before its write -> refused (left unchanged)
        let read_first = vec![if_stmt(
            local_value(&c),
            vec![print_local(&r), assign_local(&r, number(1.0), false), print_local(&r)],
            vec![assign_local(&r, local_value(&x), false)],
        )];
        assert!(alias_result_leaves(&read_first, &r).is_none());
    }

    #[test]
    fn written_param_binds_through_prefix_copy() {
        // helper(p, v): if p then v = v + 1 end; print(v)   -- `v` is WRITTEN
        let p = local("p");
        let v = local("v");
        let pat = canon(&[
            if_stmt(local_value(&p), vec![assign_local(&v, add_one(&v), false)], vec![]),
            print_local(&v),
        ]);
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let mut locals = FxHashSet::default();
        locals.insert(v.clone());
        let t = Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Void,
            pat_raw_len: 2,
            pat_spine_len: tail_spine_len(&pat),
            pat_nodes: 1,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind,
            pat0_anchor_key: None,
            pat,
            params: [p.clone()].into_iter().collect(),
            locals,
            param_order: vec![p.clone(), v.clone()],
            written_params: vec![v.clone()],
            unread: FxHashSet::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        };
        // site: local L = 7; if q then L = L + 1 end; print(L)
        let q = local("q");
        let l = local("L");
        let cand = canon(&[
            if_stmt(local_value(&q), vec![assign_local(&l, add_one(&l), false)], vec![]),
            print_local(&l),
        ]);
        let prefix = vec![(l.clone(), number(7.0))];
        let u = try_unify_site(&t, &cand, &prefix, None).expect("written param binds to the copy");
        assert_eq!(u.args.len(), 2);
        assert!(rvalue_exact_eq(&u.args[0], &local_value(&q)));
        assert!(rvalue_exact_eq(&u.args[1], &number(7.0)));
        assert!(u.callee_locals.contains(&l), "the copy is a callee temp (must be dead after)");
        // without the copy the written param has no argument -> refused
        assert!(try_unify_site(&t, &cand, &[], None).is_none());
        // a copy that binds no param would be silently deleted -> refused
        let stray = vec![(l.clone(), number(7.0)), (local("other"), number(1.0))];
        assert!(try_unify_site(&t, &cand, &stray, None).is_none());
    }
}
