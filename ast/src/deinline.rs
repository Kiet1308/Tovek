//! De-inliner: reverses Luau `-O2` function inlining.
//!
//! The Roblox Luau compiler at `-O2` inlines small local functions: it copies
//! the callee's body into each caller's bytecode at the call site. medal
//! faithfully reproduces that, so the output shows the function inlined
//! everywhere instead of called. This pass detects those inlined regions and
//! rewrites them back into real calls `funcName(args)`. Each rebuilt call
//! carries its `rebuilt` attribute; the formatter prints the site comment and
//! the definition's count from it, so no marker statement enters the tree.
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

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Binary, BinaryOperation, Block, Call, Closure, Function, GenericFor, If,
    LValue, Literal, LocalRw, MethodCall, NumericFor, RValue, RcLocal, Reduce, Repeat, Return,
    Select, SideEffects, Statement, Table, Traverse, Unary, UnaryOperation, Upvalue, While,
};

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
}

impl RejectReason {
    /// The reason's name in `--stats-json`.
    fn name(self) -> &'static str {
        self.counter().trim_start_matches("reject_")
    }

    fn counter(self) -> &'static str {
        match self {
            Self::TargetStillReferenced => "reject_reassigned_binder",
            Self::Variadic => "reject_variadic",
            Self::UnsafeBody => "reject_unsafe_body",
            Self::UnsupportedReturnShape => "reject_return_shape",
            Self::EmptyPattern => "reject_empty_pattern",
            Self::LowAnchorScore => "reject_low_anchors",
            Self::ShapeBudget => "reject_shape_budget",
        }
    }
}

/// Trace a de-inline target rejection of the helper bound to `$binder`. With
/// the `deinline_trace` feature it prints the gate + function name to stderr.
/// Optional JSON profiling records only a static reason counter, and
/// `--stats-json` the reason per helper; neither evaluates the function-name
/// argument.
macro_rules! deinline_reject {
    ($reason:expr, $binder:expr, $name:expr) => {{
        if crate::telemetry::enabled() { crate::telemetry::count($reason.counter(), 1); }
        crate::reconstruction_stats::refuse_helper($binder.stable_id(), $reason.name());
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
    /// `params`); [`absorb_arguments`] takes that declaration into the call,
    /// recovering `ARG` as the argument, or, where SSA coalesced `L` into a
    /// dead caller local, passes that local.
    written_params: Vec<RcLocal>,
    /// Parameters the callee body NEVER reads (F6a). On a non-variadic helper an
    /// unread param cannot be observed, so a call-site region that matches the body
    /// minus that param is still a valid de-inline: `try_unify_site` supplies `nil`
    /// for it (trailing such args are trimmed). Computed once via `collect_reads`
    /// over the body, nested blocks and closure bodies included. Empty for the
    /// overwhelmingly common all-params-read helper, so this changes nothing for
    /// those.
    unread: FxHashSet<RcLocal>,
    /// The parameters the body reads once, before anything observable, in
    /// the order it reads them ([`crate::evaluation_order::LeadingReads`]).
    /// Luau evaluated the arguments in parameter order right before the
    /// inlined body, so arguments that run code may still move back into the
    /// call when they are such reads, in that order
    /// (`tween(TweenInfo.new(...), { ... })`). Read off the pattern the
    /// first time a site needs it ([`Target::leading`]).
    leading: std::cell::OnceCell<crate::evaluation_order::LeadingReads>,
    /// Outer locals the body reads that a closure assigns. The helper
    /// fetches one as an upvalue where it stands; a site holding it in a
    /// register reads it when an operation runs, maybe after a call changed
    /// it (`x + change()`), so such a site is refused
    /// ([`crate::evaluation_order::region_late_read_conflict`]). Read off
    /// the pattern the first time a site needs it ([`Target::free_cells`]).
    free_cells: std::cell::OnceCell<Vec<RcLocal>>,
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
    /// The unwritten parameters a value leaf returns as they are (`return
    /// source`), in parameter order. Where the caller stores the result into
    /// that very argument (`buf = expand(buf, n)`), Luau's copy has nothing
    /// on the leaf's path: it would store a register into itself. Such a
    /// leaf matches an empty block only with its parameter pre-bound to the
    /// result local ([`Bindings::elide`], [`match_assigned_value`]).
    identity_params: Vec<RcLocal>,
    /// This void target is a value helper's body as a call for no result
    /// runs it ([`discard_body`]). Its guards keep the value helper's
    /// polarity ([`unify_stmt`]'s flip).
    discarded: bool,
    /// [`match_assigned_value`] is tried in this fixed-point iteration (see
    /// `deinline`'s assign phase).
    assigns: bool,
    /// For a discard target: the parameter every leaf of the value helper
    /// returns, if one. Its call hands back that argument, so a site keeping
    /// the argument's temp alive declares the result from the call
    /// (`local t = track(task.delay(1, f))`).
    returns_parameter: Option<RcLocal>,
    /// Every call of the helper returns exactly one value (Luau's
    /// `returnsOne`, read off its declaration before any rewrite): its call
    /// may stand where all of a value's results are taken (`return f(x)`).
    single_valued: bool,
    /// The value `V` of a value helper whose whole body is `return V`, when
    /// its copies may be rebuilt wherever the caller evaluates them
    /// ([`match_hosted_value`]).
    hosted: Option<RValue>,
    /// For a discard target: the outer local every leaf of the value helper
    /// returns (`return ignoreList`). Its copy leaves the value there, which
    /// the statement after it may read first ([`host_returned_cell`]).
    returns_cell: Option<RcLocal>,
    /// A specialization variant ([`specialization_variants`]): the truth
    /// parameter its pattern was specialized for, and the constant its
    /// calls pass there.
    inferred: Option<(RcLocal, InferredTruth)>,
    /// The helper's own locals declared once as a function literal that
    /// DUPCLOSURE shares, and only ever called, there and in its own body
    /// (`local function DeepCopy(t) ... DeepCopy(v) ... end; return
    /// DeepCopy(t)`): no code can see whether the helper's object or the
    /// copy's runs, so such a literal may match ([`unify_closure`]). The
    /// site's local is the region's own, dead after it, and unifies with
    /// these reads one for one.
    private_closures: FxHashSet<RcLocal>,
    /// An orphan's target ([`Orphan`]): the function whose body its
    /// kept-aside declaration belongs to (`None`: the chunk). Active in all
    /// of that body; its declaration goes back in once a call is rebuilt.
    orphan: Option<Option<FnPtr>>,
    /// The helper's body as it was before a rewrite in it (dual-version
    /// patterns, [`HelperCache::earlier`]): its copies at sites may still
    /// hold the code a call of another helper now stands for there.
    earlier_body: bool,
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
    /// The identity parameter pre-bound to `result` ([`match_assigned_value`]):
    /// a pattern block that only returns it stands for an empty site block,
    /// on whose path the result keeps the argument it already holds.
    elide: Option<RcLocal>,
    /// `result` is a local the caller already has, stored into by the leaves
    /// (`R = f(args)`), rather than one the site declares.
    assigned: bool,
    /// The site returns the helper's value itself: each `return X` of the
    /// pattern stands for a `return X` of the site ([`match_returned_value`]).
    returning: bool,
    /// `result` is a local the site declared without a value and that only
    /// the leaves store into: a pattern block that only returns `nil`
    /// stands for an empty site block, on whose path the result keeps that
    /// `nil` (nil-leaf elision, [`match_value`]).
    elide_nil: bool,
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
    /// The module's census, when the matcher has one: it tells the outer
    /// locals no code assigns after their declaration ([`unify_closure`]).
    pub(crate) captures: Option<&'a crate::deinline_safety::CaptureSafety>,
}

impl Target {
    fn ctx(&self) -> MatchCtx<'_> {
        MatchCtx {
            params: &self.params,
            locals: &self.locals,
            captures: Some(&self.captures),
        }
    }

    /// [`Target::leading`]: the parameters whose arguments may run code
    /// before the body, read once, before anything observable, in order.
    /// The body's own locals are its registers; an outer local is its
    /// upvalue, fetched where it stands.
    fn leading(&self) -> &crate::evaluation_order::LeadingReads {
        self.leading.get_or_init(|| {
            let facts = crate::evaluation_order::Body {
                registers: &|local| self.params.contains(local) || self.locals.contains(local),
                unchanged: &|value| self.captures.unchanged_by_calls(value),
            };
            let unwritten: Vec<RcLocal> = self.param_order.iter().filter(|p| self.params.contains(*p)).cloned().collect();
            crate::evaluation_order::LeadingReads::new(&self.pat, &unwritten, |p| count_local_reads(&self.pat, p) == 1, &facts)
        })
    }

    /// A specialization variant ([`Target::inferred`]) is tried only once
    /// the other targets are stable, with the assignment phase: a copy for
    /// a non-constant argument (`f(x, p >= 50)`, an `if` around both
    /// specialized bodies) holds a variant's copy in each arm, and the
    /// whole copy must be rebuilt before its arms are.
    fn late(&self) -> bool {
        self.inferred.is_some()
    }

    /// [`Target::free_cells`].
    fn free_cells(&self) -> &[RcLocal] {
        self.free_cells.get_or_init(|| {
            let mut pat_reads: FxHashSet<RcLocal> = FxHashSet::default();
            collect_reads(&self.pat, &mut pat_reads);
            let mut free_cells: Vec<RcLocal> = pat_reads
                .into_iter()
                .filter(|l| !self.params.contains(l) && !self.locals.contains(l) && self.captures.closure_written(l))
                .collect();
            free_cells.sort();
            free_cells
        })
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
    /// Each body's priority key per target
    /// ([`crate::reconstruction_search::priority_keys`]), read once per
    /// iteration however many blocks it has.
    priorities: FxHashMap<Option<FnPtr>, Option<std::rc::Rc<Vec<bool>>>>,
    /// Where a target's own matcher found a site, by block, position and
    /// target ([`Rescan`]).
    found: FxHashSet<(usize, usize, usize)>,
    /// What the previous scan tried, when this one starts on the same tree.
    rescan: Option<Rescan>,
    /// The sites rewritten so far.
    splices: usize,
    /// The orphans' targets active in each function body ([`Orphan`]),
    /// by the body (`None`: the chunk).
    orphan_scopes: FxHashMap<Option<FnPtr>, Vec<usize>>,
}

/// A function only ever called, all of whose calls Luau `-O2` inlined: an
/// anonymous function literal capturing nothing (`local scramble =
/// function(v, key) ... end`). The SSA inliner deletes such a dead
/// declaration as before, but keeps it aside in its function's
/// [`Function::orphans`], where it counts as no use of anything, so every
/// analysis sees the tree it saw. Its copies may rebuild calls of it; only
/// then does the declaration go back, at the top level of that body right
/// before the first statement calling it ([`materialize_orphans`]). Line
/// info must show its code inlined somewhere
/// ([`Function::inlined_by_compiler`]); capturing nothing, it may be
/// declared anywhere in that body, and only ever called, its object is
/// seen by no code.
struct Orphan {
    binder: RcLocal,
    function: Arc<Mutex<Function>>,
    /// The function whose body held the declaration, `None` for the chunk.
    scope: Option<FnPtr>,
    scope_function: Option<Arc<Mutex<Function>>>,
}

/// The orphans kept aside by `body`'s functions, nested ones included,
/// those of the chunk first (`chunk`).
fn collect_orphans(body: &Block, chunk: &[(RcLocal, Closure)], nested: bool) -> Vec<Orphan> {
    fn walk(stmts: &[Statement], out: &mut Vec<Orphan>) {
        for statement in stmts {
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Closure(closure) = value {
                    let function = closure.function.0.lock();
                    for (binder, orphan) in &function.orphans {
                        out.push(Orphan {
                            binder: binder.clone(),
                            function: orphan.function.0.clone(),
                            scope: Some(Arc::as_ptr(&closure.function.0)),
                            scope_function: Some(closure.function.0.clone()),
                        });
                    }
                    walk(&function.body.0, out);
                }
            });
            match statement {
                Statement::If(branch) => {
                    walk(&branch.then_block.lock().0, out);
                    walk(&branch.else_block.lock().0, out);
                }
                Statement::While(node) => walk(&node.block.lock().0, out),
                Statement::Repeat(node) => walk(&node.block.lock().0, out),
                Statement::NumericFor(node) => walk(&node.block.lock().0, out),
                Statement::GenericFor(node) => walk(&node.block.lock().0, out),
                _ => {}
            }
        }
    }
    let mut orphans: Vec<Orphan> = chunk
        .iter()
        .map(|(binder, closure)| Orphan { binder: binder.clone(), function: closure.function.0.clone(), scope: None, scope_function: None })
        .collect();
    if nested {
        walk(&body.0, &mut orphans);
    }
    orphans
}

/// Each orphan a call of which was rebuilt goes back into its body
/// ([`Orphan`]), right before the first top-level statement reading it,
/// and leaves the side table.
fn materialize_orphans(body: &mut Block, chunk: &mut Vec<(RcLocal, Closure)>, orphans: &[Orphan], used: &FxHashSet<RcLocal>) {
    for orphan in orphans.iter().filter(|orphan| used.contains(&orphan.binder)) {
        let mut scope_function = orphan.scope_function.as_ref().map(|function| function.lock());
        let (stmts, table) = match &mut scope_function {
            Some(function) => {
                let function = &mut **function;
                (&mut function.body.0, &mut function.orphans)
            }
            None => (&mut body.0, &mut *chunk),
        };
        let Some(at) = table.iter().position(|(binder, _)| *binder == orphan.binder) else { continue };
        let (binder, closure) = table.remove(at);
        let Some(first) = stmts.iter().position(|statement| count_local_reads(std::slice::from_ref(statement), &binder) > 0) else {
            // Its calls went with a later rewrite: it stays aside.
            table.insert(at, (binder, closure));
            continue;
        };
        let mut declaration = Assign::new(vec![LValue::Local(binder)], vec![RValue::Closure(closure)]);
        declaration.prefix = true;
        stmts.insert(first, declaration.into());
    }
}

/// The scan of an iteration that rewrote nothing, as the next one, which
/// starts on the very same tree with the same targets, sees it: the targets
/// it tried at every position of their scope (`tried`, and every target in
/// the bodies of `everywhere`), and where their own matchers found a site
/// (`found`). Elsewhere such a matcher finds none again, so the assignment
/// phase entered then goes straight to its store matcher, while the block
/// is as the previous scan saw it: no site rewritten in it or below it yet,
/// and no closure in the target's pattern, whose body a rewrite elsewhere
/// may change (`with_closure`).
struct Rescan {
    tried: Vec<bool>,
    with_closure: Vec<bool>,
    everywhere: FxHashSet<Option<FnPtr>>,
    found: FxHashSet<(usize, usize, usize)>,
}

pub fn deinline(body: &mut Block) {
    deinline_orphans_in(body, &mut Vec::new(), false);
}

/// [`deinline`] with the chunk's orphans ([`Orphan`]); nested functions
/// hold theirs.
pub fn deinline_with_orphans(body: &mut Block, chunk_orphans: &mut Vec<(RcLocal, Closure)>) {
    deinline_orphans_in(body, chunk_orphans, true);
}

/// [`deinline_with_orphans`], where `nested` says whether a function of
/// the chunk may hold orphans of its own (else none is looked for).
pub fn deinline_orphans_in(body: &mut Block, chunk_orphans: &mut Vec<(RcLocal, Closure)>, nested: bool) {
    let orphans = if nested || !chunk_orphans.is_empty() { collect_orphans(body, chunk_orphans, nested) } else { Vec::new() };
    // Every rewrite needs a target; the module-wide censuses below are only
    // worth building when some helper passes the per-declaration gates.
    if !any_structural_target(body) && orphans.is_empty() {
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
    for orphan in &orphans {
        if returns_exactly_one(&orphan.function.lock().body.0) {
            single_valued.insert(orphan.binder.clone());
        }
    }
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
    // Values stored into a local the caller has (`match_assigned_value`) are
    // matched only once everything else is stable. Such a store may be the
    // leaf of an enclosing helper inlined around the copy, whose own body
    // holds that leaf as a `return`, which never rebuilds the same way:
    // rebuilding the inner call first would hide the outer copy.
    let mut assign_phase = false;
    let mut entering_assign_phase = false;
    // The targets of an iteration that rewrote nothing, and the census of the
    // tree as it stands while no rewrite followed it: collecting on the same
    // tree gives them again. What that iteration's scan tried goes to the
    // next one ([`Rescan`]).
    let mut unchanged_targets: Option<Vec<Target>> = None;
    let mut rescan: Option<Rescan> = None;
    let mut current_captures: Option<std::rc::Rc<crate::deinline_safety::CaptureSafety>> = None;
    // What collecting found for each helper whose code no rewrite changed
    // since, and the targets of the last collection.
    let mut helper_cache = HelperCache::default();
    let mut last_targets: Option<Vec<Target>> = None;
    for _ in 0..64 {
        dprof::inc(&dprof::ITERATIONS, 1);
        crate::telemetry::count("iterations", 1);
        let targets = {
            let _t = dprof::T::new(&dprof::COLLECT_TARGETS_US);
            let _span = crate::telemetry::Span::new("D_COLLECT_TARGETS");
            let mut targets = match unchanged_targets.take() {
                Some(targets) => {
                    // What the previous scan of this tree tried and found.
                    // It rewrote nothing: a target outside its focus could
                    // not match anew there ([`Target::focused`]), nor on the
                    // same tree now. Every target counts as tried but the
                    // late ones, which wait for the assignment phase.
                    let tried = targets.iter().map(|target| !target.late() || target.assigns).collect();
                    let with_closure = targets.iter().map(|target| block_has_closure(&target.pat)).collect();
                    rescan = previous.as_mut().map(|previous| Rescan {
                        tried,
                        with_closure,
                        everywhere: previous.revisit.clone(),
                        found: std::mem::take(&mut previous.found),
                    });
                    targets
                }
                None => {
                    let captures = initial_captures.take().unwrap_or_else(||
                        std::rc::Rc::new(crate::deinline_safety::CaptureSafety::new(body)));
                    current_captures = Some(captures.clone());
                    if let Some(last) = last_targets.take() {
                        helper_cache.keep_targets(last);
                    }
                    let targets = collect_targets(body, &write_counts, &single_valued, captures, &orphans, &mut helper_cache);
                    crate::telemetry::count("accepted_targets", targets.len() as u64);
                    // The budget counts helpers: a value helper's discard
                    // variant shares its definition.
                    if targets.iter().filter(|target| !target.discarded && target.inferred.is_none() && !target.earlier_body).count() > 256 {
                        crate::telemetry::count("target_budget_exhausted", 1);
                        break;
                    }
                    targets
                }
            };
            for target in &mut targets {
                target.search = search.clone();
                target.assigns = assign_phase;
                if entering_assign_phase {
                    // Only a value helper can match anew, through a store,
                    // or a late target ([`Target::late`]).
                    target.focused = target.kind == TKind::Value || target.late();
                } else if let Some(previous) = &previous {
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
        // f_local -> its targets' indices (a value helper and the variant
        // for its calls without a result sit side by side), so we can
        // recognise each target's declaration statement during the scan and
        // only activate it for code in its scope.
        let mut decl_map: FxHashMap<RcLocal, std::ops::Range<usize>> = FxHashMap::default();
        for (idx, t) in targets.iter().enumerate() {
            decl_map.entry(t.f_local.clone()).and_modify(|range| range.end = idx + 1).or_insert(idx..idx + 1);
        }
        let mut newly = Progress {
            revisit: previous.as_ref().map(|previous| previous.bodies.clone()).unwrap_or_default(),
            rescan: rescan.take(),
            ..Progress::default()
        };
        for (index, target) in targets.iter().enumerate() {
            if let Some(scope) = target.orphan {
                newly.orphan_scopes.entry(scope).or_default().push(index);
            }
        }
        let chunk_orphans_active = newly.orphan_scopes.get(&None).cloned().unwrap_or_default();
        {
            let _span = crate::telemetry::Span::new("D_SCAN");
            deinline_block(
                &mut body.0,
                &targets,
                &decl_map,
                &chunk_orphans_active,
                &[],
                &Enclosing::function(&[]),
                None,
                true,
                true,
                false,
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
        entering_assign_phase = false;
        if newly.binders.is_empty() {
            if assign_phase || !targets.iter().any(|target| target.kind == TKind::Value || target.late()) {
                break;
            }
            assign_phase = true;
            entering_assign_phase = true;
            unchanged_targets = Some(targets);
            previous = Some(newly);
            continue;
        }
        current_captures = None;
        converted.extend(newly.binders.iter().cloned());
        helper_cache.forget_rewritten(&newly.bodies);
        last_targets = Some(targets);
        previous = Some(newly);
        if search.exhausted() { break; }
    }
    if search.exhausted() {
        crate::reconstruction_stats::refuse_site("search_budget_exhausted");
    }
    if !orphans.is_empty() {
        materialize_orphans(body, chunk_orphans, &orphans, &converted);
    }
    if !converted.is_empty() {
        {
            let _t = dprof::T::new(&dprof::COLLAPSE_US);
            let _span = crate::telemetry::Span::new("D_COLLAPSE_RESULTS");
            // The collapse reads the final tree's registers and stable values.
            let captures = current_captures
                .unwrap_or_else(|| std::rc::Rc::new(crate::deinline_safety::CaptureSafety::new(body)));
            let facts = Collapse { single_valued: &single_valued, captures: &captures, function: None };
            collapse_value_results(&mut body.0, &facts, &FxHashSet::default());
        }
    }
    dprof::dump();
}

// ===================================================================
// Readability: collapse a single-use value de-inline
//   local v = f(args)   (a rebuilt call)
//   if v then BODY end  -- v used exactly once, as the whole condition
// into
//   if f(args) then BODY end
// matching the original source. Only when `v` is read exactly once (in the
// immediately-following statement, anywhere incl. closures) so single-evaluation
// and ordering are preserved.
// ===================================================================

/// A statement that `canon_top` drops: an `Empty` placeholder, a runtime no-op
/// that does not occupy a logical position in a candidate window. MUST stay in
/// lock-step with the filter in `canon_top`: the candidate generator decides
/// which raw index is the K-th *effective* statement, and canon decides what
/// the unifier actually sees, so the two must agree on what counts as a no-op.
/// A comment is NOT trivia (it refuses the body via `body_unsafe`).
fn is_match_trivia(s: &Statement) -> bool {
    matches!(s, Statement::Empty(_))
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
/// `local RESULT` declaration: the structurer can leave an `Empty` between the
/// callee-prefix statement(s) and that decl, so the fixed offset
/// `i + prefix_len` would point at it and `result_decl` would bail —
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
/// ceiling. Interposed `Empty`s do NOT consume the budget, so a window whose
/// *effective* length already equals the pattern is never cut short by the raw
/// ceiling (DeinlineReportNew §2 / F2). The
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

/// What the collapse knows where it runs: the helpers whose calls give one
/// value, and, for a store's address, the registers of the function the block
/// belongs to (`None`: the chunk) and the reads no call can change.
struct Collapse<'a> {
    single_valued: &'a FxHashSet<RcLocal>,
    captures: &'a crate::deinline_safety::CaptureSafety,
    function: Option<usize>,
}

impl Collapse<'_> {
    /// An address operand of `t[k] = f(args)` that reads the same whether
    /// evaluated before or after the call: a constant; a register of this
    /// function, which SETTABLE reads when it runs, after the call, as the
    /// store after `local v = f(args)` does; or a read no call can change.
    fn stable_address(&self, address: &RValue) -> bool {
        match address {
            RValue::Literal(_) => true,
            RValue::Local(local) if self.captures.register_of(local, self.function) => true,
            _ => self.captures.unchanged_by_calls(address),
        }
    }
}

/// `live_out`: locals of `stmts` read after them (a `repeat` body's, by its
/// `until` condition).
fn collapse_value_results(stmts: &mut Vec<Statement>, facts: &Collapse, live_out: &FxHashSet<RcLocal>) {
    // recurse into nested blocks and closure bodies first.
    let none = FxHashSet::default();
    for s in stmts.iter_mut() {
        match s {
            Statement::If(f) => {
                collapse_value_results(&mut f.then_block.lock().0, facts, &none);
                collapse_value_results(&mut f.else_block.lock().0, facts, &none);
            }
            Statement::While(w) => collapse_value_results(&mut w.block.lock().0, facts, &none),
            Statement::Repeat(r) => {
                let reads = condition_reads(&r.condition);
                collapse_value_results(&mut r.block.lock().0, facts, &reads);
            }
            Statement::NumericFor(nf) => collapse_value_results(&mut nf.block.lock().0, facts, &none),
            Statement::GenericFor(gf) => collapse_value_results(&mut gf.block.lock().0, facts, &none),
            _ => {}
        }
        visit_stmt_rvalues_mut(s, &mut |rv| {
            collapse_in_closures(rv, facts);
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
    // From the end, so that a value moved into the statement after it lets
    // the one before move in as well (`local a = f(); local b = g(); h(a,
    // b)` is `h(f(), g())`). `kept` holds the statements kept so far, last
    // first, each with the last original statement it covers.
    let mut kept: Vec<(Statement, usize)> = Vec::with_capacity(n);
    for (i, statement) in taken.into_iter().enumerate().rev() {
        if let Some((next, covers)) = kept.last()
            && let Statement::Assign(a) = &statement
            && let Some((v, call)) = value_call_decl(a)
            && count_local_reads(std::slice::from_ref(next), &v) == 1
            && last_read.get(&v).is_none_or(|k| k <= covers)
            && !live_out.contains(&v)
            // `v`'s declaration is about to be removed, so `v` must not be
            // *written* anywhere we keep either — a later `v = ...` (e.g. inside
            // the collapsed `if`) would otherwise be left with no declaration.
            && last_write.get(&v).is_none_or(|&k| k < i + 1)
            && let Some(collapsed) = collapse_use(next, &v, call, facts)
        {
            // The rebuilt call now lives inside `collapsed`, still carrying
            // its `rebuilt` attribute for the formatter's site comment.
            let covers = *covers;
            kept.pop();
            kept.push((collapsed, covers));
        } else {
            kept.push((statement, i));
        }
    }
    *stmts = kept.into_iter().rev().map(|(statement, _)| statement).collect();
}

/// `local v = <rebuilt Call>` -> (v, the call rvalue); also `local v = P
/// or f(args)` ([`match_short_circuit`]), one value whatever `f` returns.
fn value_call_decl(a: &Assign) -> Option<(RcLocal, &RValue)> {
    let rebuilt = |value: &RValue| matches!(value, RValue::Call(c) if c.rebuilt.is_some());
    if a.prefix && !a.parallel && a.left.len() == 1 && a.right.len() == 1 {
        if let LValue::Local(v) = &a.left[0]
            && (rebuilt(&a.right[0])
                || matches!(&a.right[0], RValue::Binary(binary)
                    if binary.operation == BinaryOperation::Or && rebuilt(&binary.right)))
        {
            return Some((v.clone(), &a.right[0]));
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
/// sound for every helper. `stable_address` tells the address operands a store
/// may evaluate before the call (`lvalue_safe_for_collapse`).
fn collapse_use(s: &Statement, v: &RcLocal, call: &RValue, facts: &Collapse) -> Option<Statement> {
    let stable_address = |address: &RValue| facts.stable_address(address);
    let is_v = |rv: &RValue| matches!(rv, RValue::Local(x) if x == v);
    let is_not_v = |rv: &RValue| {
        matches!(rv, RValue::Unary(u)
            if u.operation == UnaryOperation::Not && is_v(&u.value))
    };
    // Only a call proven to return one value may move into a multi-value
    // context; any other value is one.
    let exactly_one = !is_multiple(call) || call_callee_local(call).is_some_and(|l| facts.single_valued.contains(l));
    let specific = match s {
        Statement::If(f) if is_v(&f.condition) || is_not_v(&f.condition) => {
            let cond = if is_v(&f.condition) {
                call.clone()
            } else {
                RValue::Unary(Unary {
                    node_origin: Default::default(),
                    value: Box::new(call.clone()),
                    operation: UnaryOperation::Not,
                })
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
        // (`t[k]`, `t.field`) only when its address reads the same before and
        // after the moved-in call (see `lvalue_safe_for_collapse`), and only as
        // the single target of the store.
        // A SINGLE-LHS `x = v` truncates the call to one value either way (sound for
        // any helper); a MULTI-LHS `a, b = v` is a multi-value context, so refuse it
        // for a multi-value helper (it would bind b/... to values the original
        // single-LHS `local v = f(args)` truncated away).
        Statement::Assign(a)
            if a.right.len() == 1
                && is_v(&a.right[0])
                && !a.compound
                && a.left.iter().all(|left| lvalue_safe_for_collapse(left, &stable_address))
                && (a.left.len() == 1 || (exactly_one && a.left.iter().all(|left| !matches!(left, LValue::Index(_))))) =>
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
    };
    specific.or_else(|| collapse_into_leading_value(s, v, call, exactly_one, facts))
}

/// The value placed where `s` reads `v` first of all it evaluates, after
/// only reads (`assert(v, msg)` -> `assert(isCallable(x), msg)`, a host's
/// leading value as [`visit_leading_values`] finds it): it runs there
/// rather than before `s`, so none of those reads may see a change, a
/// local no call can change or an unchanging import. Where all of a value's
/// results are taken, only one value may move in.
fn collapse_into_leading_value(s: &Statement, v: &RcLocal, value: &RValue, exactly_one: bool, facts: &Collapse) -> Option<Statement> {
    // A local the source declared stays declared.
    if v.preserve_binding() {
        return None;
    }
    let register = |local: &RcLocal| facts.captures.register_of(local, facts.function);
    let unchanged = |read: &Earlier| {
        let Earlier::Value(read) = read;
        facts.captures.stable_at(read, facts.function) || facts.captures.unchanged_by_calls(read)
    };
    let mut host = s.clone();
    let mut placed = false;
    visit_leading_values(&mut host, &register, &mut |slot, evaluated_before, spread| {
        if !matches!(slot, RValue::Local(read) if read == v) {
            return false;
        }
        if (spread == Spread::One || exactly_one) && evaluated_before.iter().all(unchanged) {
            *slot = value.clone();
            placed = true;
        }
        // `v` is read once: this read decides.
        true
    });
    placed.then(|| {
        crate::telemetry::count("collapse_into_leading_value", 1);
        select_prefix_calls(&mut host);
        host
    })
}

/// A collapse-safe assignment target: a bare name binding (`x` / `GLOBAL`) whose
/// only effect is the store of the RHS value, or an INDEXED target (`t[k]`,
/// `t.f`, the Store result mode) whose address reads the same before and after
/// the call. Luau evaluates an address operand that is not a register of the
/// function before the RHS (`compileLValue`), so `t[k] = f()` would read it
/// where `local v = f(); t[k] = v` reads it after the call: exact only for a
/// constant, a register (SETTABLE reads it when it runs, after the call, in
/// both) or a value no call can change.
fn lvalue_safe_for_collapse(l: &LValue, stable_address: &dyn Fn(&RValue) -> bool) -> bool {
    match l {
        LValue::Local(_) | LValue::Global(_) => true,
        LValue::Index(index) => stable_address(&index.left) && stable_address(&index.right),
    }
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

/// The locals a `repeat` condition reads, which its body's locals may be.
fn condition_reads(condition: &RValue) -> FxHashSet<RcLocal> {
    let mut reads = FxHashSet::default();
    collect_reads(&[Statement::Return(Return::new(vec![condition.clone()]))], &mut reads);
    reads
}

/// Where a block sits in its function, for the locals read after it: a
/// site's own locals must be dead once it is a call, also beyond the block
/// (a written parameter stands for a local declared anywhere before the
/// site, [`absorb_arguments`]). Each block holding another lends it the
/// statements around the one holding it; nothing is collected until a site
/// asks ([`Liveness::live_after_block`]).
#[derive(Clone, Copy, Default)]
struct Enclosing<'a> {
    /// The parent block's statements before the one holding this block:
    /// their top-level declarations are in scope here.
    before: &'a [Statement],
    /// The parent block's statements after it: they run after this block.
    after: &'a [Statement],
    /// The loop variables the statement holding this block declares for it,
    /// bound anew on every trip.
    vars: &'a [RcLocal],
    /// The block runs again when it ends (a loop body): a local declared
    /// outside it is read by the next trip (taken as read, as every local
    /// a site binds outside its window is).
    looping: bool,
    /// A `repeat` body's `until` condition, which reads the body's locals.
    until: Option<&'a RValue>,
    /// The parent block's own; `None` for a function's body.
    parent: Option<&'a Enclosing<'a>>,
    /// A function body's parameters.
    params: &'a [RcLocal],
}

impl<'a> Enclosing<'a> {
    /// A function's body.
    fn function(params: &'a [RcLocal]) -> Self {
        Self { params, ..Self::default() }
    }

    /// A block of the statement between `before` and `after` in the block
    /// `parent` describes.
    fn nested(parent: &'a Enclosing<'a>, before: &'a [Statement], after: &'a [Statement]) -> Self {
        Self { before, after, parent: Some(parent), ..Self::default() }
    }

    /// Each local declared outside the block and in scope in it, with the
    /// depth of the block declaring it (0 for this one, 1 for its parent):
    /// `k` for the loop variables of the `k`-th block up, `k + 1` for the
    /// declarations in its `before`, the function body's depth for its
    /// parameters.
    fn declared_outside(&self) -> FxHashMap<RcLocal, usize> {
        let mut declared = FxHashMap::default();
        let mut node = self;
        let mut depth = 0;
        loop {
            for var in node.vars {
                declared.entry(var.clone()).or_insert(depth);
            }
            for statement in node.before {
                if let Statement::Assign(assign) = statement
                    && assign.prefix
                {
                    for left in &assign.left {
                        if let LValue::Local(local) = left {
                            declared.entry(local.clone()).or_insert(depth + 1);
                        }
                    }
                }
            }
            let Some(parent) = node.parent else {
                for param in node.params {
                    declared.entry(param.clone()).or_insert(depth);
                }
                return declared;
            };
            node = parent;
            depth += 1;
        }
    }

    /// Whether `local`, declared by the block `depth` levels up, is read
    /// after this block: by the statements after a block in between, by the
    /// next trip of a loop in between, or by the declaring block's `until`.
    fn read_after(&self, local: &RcLocal, depth: usize) -> bool {
        let mut node = self;
        for _ in 0..depth {
            if node.looping || block_reads_local(node.after, local) {
                return true;
            }
            let Some(parent) = node.parent else { return false };
            node = parent;
        }
        node.until.is_some_and(|until| reads_local(until, local))
    }
}

/// The tail liveness of a block's locals ([`tail_has_live`]): the index of
/// its own statements, built on first use and dropped by the driver after
/// each splice, and where the block sits.
#[derive(Default)]
struct Liveness<'a> {
    index: Option<FxHashMap<RcLocal, usize>>,
    around: Option<&'a Enclosing<'a>>,
    declared_outside: std::cell::OnceCell<FxHashMap<RcLocal, usize>>,
}

impl<'a> Liveness<'a> {
    fn new(around: &'a Enclosing<'a>) -> Self {
        Self { around: Some(around), ..Self::default() }
    }

    /// Whether `local` is read after the block: one the block declares only
    /// by a `repeat` body's `until`, one declared around it wherever its
    /// scope goes on ([`Enclosing::read_after`]). A local of an enclosing
    /// function is captured, which every rule binding a local declared
    /// outside its window refuses first.
    fn live_after_block(&self, local: &RcLocal) -> bool {
        let Some(around) = self.around else { return false };
        let depth = self.declared_outside.get_or_init(|| around.declared_outside()).get(local).copied().unwrap_or(0);
        around.read_after(local, depth)
    }
}

/// Map each local to the greatest top-level index `k` (in `from..stmts.len()`)
/// whose statement reads-or-writes it. The read half mirrors `count_local_reads`
/// and the write half reuses `collect_written`, so membership is identical to the
/// per-statement predicate `any_local_live` tests. Rebuilt from the cursor on the
/// rare accept; because the cursor only advances, occurrences below `from` are
/// never queried (every future query uses `tail_start >= from`), so dropping them
/// is sound.
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
/// The cache is invalidated by the driver whenever it splices, so it is always
/// consistent with the current `stmts`. (Empty `set` ⇒ false, matching
/// `any_local_live`, and without forcing a build.) A local dead in the rest of
/// the block is still live when read after the block
/// ([`Liveness::live_after_block`]).
fn tail_has_live(
    last_occ: &mut Liveness,
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
    let idx = last_occ.index.get_or_insert_with(|| build_last_occ(stmts, from));
    set.iter().any(|v| idx.get(v).is_some_and(|&k| k >= tail_start)) || set.iter().any(|v| last_occ.live_after_block(v))
}

fn collapse_in_closures(rv: &mut RValue, facts: &Collapse) {
    // Find every closure within `rv` and run the collapse inside its body. Descent
    // uses the enum_dispatch `Traverse::rvalues_mut` (exhaustive by construction, so
    // it can never silently drop a new RValue variant — incl. `IfExpression`),
    // mirroring `expr_deinline::write_counts_in_closures`.
    if let RValue::Closure(c) = rv {
        let inner = Collapse { function: Some(Arc::as_ptr(&c.function.0) as usize), ..*facts };
        collapse_value_results(&mut c.function.0.lock().body.0, &inner, &FxHashSet::default());
        return;
    }
    rv.visit_rvalues_mut(&mut |child| {
        collapse_in_closures(child, facts);
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

/// A canonical window: a run of statements shared with others (the block's
/// canonical statements, or one window built on its own).
#[derive(Clone, Default)]
struct Window {
    statements: std::rc::Rc<Vec<Statement>>,
    range: std::ops::Range<usize>,
}

impl Window {
    fn owned(statements: Vec<Statement>) -> Self {
        let range = 0..statements.len();
        Self { statements: std::rc::Rc::new(statements), range }
    }
}

impl std::ops::Deref for Window {
    type Target = [Statement];

    fn deref(&self) -> &[Statement] {
        &self.statements[self.range.clone()]
    }
}

/// The tail-canon of the contiguous candidate windows of the block being
/// scanned. A window `stmts[start..start + w]` is built once per position,
/// however many targets ask for it (`windows`, cleared at each position).
/// Below that, canon maps each top-level statement on its own unless a guard
/// folds what follows it, and only an `if` takes another form as the last
/// statement. So each statement's canonical form is built once for the
/// whole scan (`middle`, the last `if`s in `last`), and a window of real
/// statements ending in anything else is a run of `middle`, shared rather
/// than copied. The window fuel adds up node counts kept as prefix sums
/// (`nodes`). Only the void attempt-1 path and the value regions use it; the
/// non-contiguous `match_value_prefixed` union and the rewritten windows are
/// built where they are needed. A splice updates what it changes
/// ([`CanonCache::spliced`]).
#[derive(Default)]
struct CanonCache {
    windows: FxHashMap<(usize, usize), Window>,
    middle: Option<std::rc::Rc<Vec<Statement>>>,
    last: Vec<Option<Statement>>,
    nodes: Vec<usize>,
    /// Whether the block returns on every path ([`block_always_returns`]),
    /// which every return-mode window, running to its end, needs.
    returns: Option<bool>,
    /// The kinds of the values the statement at this position evaluates
    /// ([`kind_bit`]), which a hosted copy's value must be one of.
    kinds: Option<u32>,
}

impl CanonCache {
    /// A new position: its windows start elsewhere.
    fn clear(&mut self) {
        self.windows.clear();
        self.kinds = None;
    }

    /// [`CanonCache::returns`].
    fn returns(&mut self, stmts: &[Statement]) -> bool {
        *self.returns.get_or_insert_with(|| block_always_returns(stmts))
    }

    /// [`CanonCache::kinds`] of `statement`, the one at this position.
    fn kinds(&mut self, statement: &Statement) -> u32 {
        *self.kinds.get_or_insert_with(|| {
            fn walk(value: &RValue, kinds: &mut u32) {
                *kinds |= kind_bit(value);
                value.visit_rvalues(&mut |child| {
                    walk(child, kinds);
                    true
                });
            }
            let mut kinds = 0;
            visit_stmt_rvalues(statement, &mut |value| {
                walk(value, &mut kinds);
                true
            });
            kinds
        })
    }

    /// `stmts[start..start + added]` replaced `removed` statements at
    /// `start`: the canonical forms of the others stay as they are.
    fn spliced(&mut self, stmts: &[Statement], start: usize, removed: usize, added: usize) {
        self.windows.clear();
        match self.middle.as_mut().map(std::rc::Rc::get_mut) {
            Some(Some(middle)) => {
                middle.splice(
                    start..start + removed,
                    stmts[start..start + added].iter().map(|statement| canon_statement(statement.clone(), false)),
                );
            }
            _ => self.middle = None,
        }
        self.last.clear();
        self.nodes.truncate(start + 1);
        self.returns = None;
        self.kinds = None;
    }

    /// Every statement's canonical form as a middle one.
    fn middle(&mut self, stmts: &[Statement]) -> &std::rc::Rc<Vec<Statement>> {
        self.middle.get_or_insert_with(|| {
            std::rc::Rc::new(stmts.iter().map(|statement| canon_statement(statement.clone(), false)).collect())
        })
    }

    /// The node count of `stmts[start..start + w]`, what [`charge_window`]
    /// charges for building it.
    fn nodes(&mut self, stmts: &[Statement], start: usize, w: usize) -> usize {
        if self.nodes.is_empty() {
            self.nodes.push(0);
        }
        while self.nodes.len() <= start + w {
            let total = self.nodes[self.nodes.len() - 1] + dbg_stmt_node_count(&stmts[self.nodes.len() - 1]);
            self.nodes.push(total);
        }
        self.nodes[start + w] - self.nodes[start]
    }

    /// `canon_recurse(canon_top(&stmts[start..start + w], true), true)`. With
    /// no guard folding the rest of the window (`unguard`), that is each
    /// statement's own canonical form, the last one's in tail position.
    fn canonical(&mut self, stmts: &[Statement], start: usize, w: usize) -> Window {
        let mut real: Vec<usize> = (start..start + w).filter(|&k| !is_match_trivia(&stmts[k])).collect();
        // N1: a trailing void return is dropped.
        if real.last().is_some_and(|&k| matches!(&stmts[k], Statement::Return(r) if r.values.is_empty())) {
            real.pop();
        }
        let (Some(&first), Some((&tail, body))) = (real.first(), real.split_last()) else { return Window::default() };
        let distributed = body.last().is_some_and(|&k| {
            matches!(&stmts[k], Statement::If(f) if distributes_return(f, std::slice::from_ref(&stmts[tail])))
        });
        if distributed || body.iter().any(|&k| is_foldable_guard(&stmts[k])) {
            return Window::owned(canon_recurse(canon_top(&stmts[start..start + w], true), true));
        }
        let middle = self.middle(stmts).clone();
        let contiguous = tail + 1 - first == real.len();
        if contiguous && !matches!(stmts[tail], Statement::If(_)) {
            return Window { statements: middle, range: first..tail + 1 };
        }
        if self.last.is_empty() {
            self.last.resize(stmts.len(), None);
        }
        let mut out = Vec::with_capacity(real.len());
        out.extend(body.iter().map(|&k| middle[k].clone()));
        out.push(self.last[tail].get_or_insert_with(|| canon_statement(stmts[tail].clone(), true)).clone());
        Window::owned(out)
    }
}

/// One statement of a window as `canon_recurse` gives it: its child blocks
/// canonicalized, a select diamond fused, and in tail position a trailing
/// return diamond too.
fn canon_statement(statement: Statement, tail: bool) -> Statement {
    let mut statement = [canon_children_owned(statement, tail)];
    fuse_assign_diamond(&mut statement[0]);
    if tail {
        fuse_return_diamond(&mut statement);
    }
    let [statement] = statement;
    statement
}

/// Tail-canon of the contiguous window `stmts[start..start+w]`, memoized in `cache`.
/// Equal to `canon_recurse(canon_top(&stmts[start..start+w], true), true)`;
/// the only effect is that repeated requests pay the deep copy once.
fn canon_window(
    cache: &mut CanonCache,
    t: &Target,
    stmts: &[Statement],
    start: usize,
    w: usize,
) -> Window {
    if let Some(c) = cache.windows.get(&(start, w)) {
        return c.clone();
    }
    dprof::inc(&dprof::CANON_RECURSE_CALLS, 1);
    crate::telemetry::count("canonicalize_calls", 1);
    let _t = dprof::T::new(&dprof::CANON_RECURSE_US);
    // An exhausted budget refuses the whole position (`try_match_at`); the
    // empty window built in its place matches no pattern.
    let nodes = cache.nodes(stmts, start, w);
    if t.search.exhausted() || !t.search.spend(nodes) {
        return Window::default();
    }
    let c = cache.canonical(stmts, start, w);
    #[cfg(debug_assertions)]
    {
        let whole = canon_recurse(canon_top(&stmts[start..start + w], true), true);
        debug_assert_eq!(format!("{:?}", &*c), format!("{whole:?}"), "the window from canonical statements is its canon");
    }
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
    // N2: drop Empty placeholders (runtime no-ops), symmetrically in pattern
    // and candidate, so they never perturb the length gates.
    let mut s: Vec<Statement> = stmts.iter().filter(|st| !is_match_trivia(st)).cloned().collect();
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
            let distributes = guard.is_none() && !open && distributes_return(f, stmts.as_slice());
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
            } else if distributes {
                let rest: Vec<Statement> = stmts.filter(|s| !is_match_trivia(s)).collect();
                out.extend(graft_all(vec![statement], &rest));
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

/// E1, return distribution: an `if` that returns on some path and falls
/// off its end on two or more others, followed by nothing but one `return`
/// of one value. That `return` belongs at each of those ends, where the
/// copy Luau inlined for a value stores the result (`if a then return true
/// end; if b then ...; if c then return true end end; return false`, whose
/// `return false` closes both the `b` and the `not b` paths). Each path
/// still runs one copy of it, the last thing it runs. An `if` that never
/// returns keeps the `return` after it, as its copies keep the store.
fn distributes_return(f: &If, rest: &[Statement]) -> bool {
    let mut real = rest.iter().filter(|s| !is_match_trivia(s));
    let (Some(Statement::Return(ret)), None) = (real.next(), real.next()) else { return false };
    ret.values.len() == 1 && is_distributing_if(f)
}

/// The `if` half of [`distributes_return`]: two or more open ends
/// ([`open_ends`]) and a `return` on some path.
fn is_distributing_if(f: &If) -> bool {
    let then = f.then_block.lock();
    let els = f.else_block.lock();
    matches!((open_ends(&then.0), open_ends(&els.0)), (Some(a), Some(b)) if a + b >= 2)
        && (block_has_return(&then.0) || block_has_return(&els.0))
}

/// `stmts` with a copy of `rest` at every path that falls off its end
/// (see [`distributes_return`]).
fn graft_all(mut stmts: Vec<Statement>, rest: &[Statement]) -> Vec<Statement> {
    if open_ends(&stmts) == Some(0) {
        return stmts;
    }
    if let Some(at) = stmts.iter().rposition(|s| !is_match_trivia(s))
        && let Statement::If(f) = &stmts[at]
    {
        let then = graft_all(f.then_block.lock().0.clone(), rest);
        let els = graft_all(f.else_block.lock().0.clone(), rest);
        stmts[at] = If::new(f.condition.clone(), Block(then), Block(els)).into();
        return stmts;
    }
    stmts.extend(rest.iter().cloned());
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
    // E1: the last two, an `if` taking a copy of the `return` after it.
    let mut last_two: [Option<&Statement>; 2] = [None, None];
    for (idx, s) in stmts
        .filter(|s| !is_match_trivia(s))
        .take(effective)
        .enumerate()
    {
        if idx + 1 < effective && is_foldable_guard(s) {
            return idx + 1;
        }
        last_two = [last_two[1], Some(s)];
    }
    if let [Some(Statement::If(f)), Some(last)] = last_two
        && distributes_return(f, std::slice::from_ref(last))
    {
        len -= 1;
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
        // Leaf elision: a block ending `return x` against the same block
        // without it, where `x` is what the result local already holds on
        // this path ([`is_elided_leaf`]): the identity parameter pre-bound
        // to it, the helper's own local in its register, or `nil` it was
        // declared with. Nothing in the block stores into the result, which
        // only the leaves of the region write.
        return match pat.split_last() {
            Some((last, rest)) if rest.len() == cand.len() && is_elided_leaf(std::slice::from_ref(last), b) => {
                for (p, c) in rest.iter().zip(cand) {
                    unify_stmt(t, p, c, b)?;
                }
                Ok(())
            }
            _ => Err(()),
        };
    }
    for (p, c) in pat.iter().zip(cand) {
        unify_stmt(t, p, c, b)?;
    }
    Ok(())
}

/// A pattern block that only returns the parameter `b` elides
/// ([`Bindings::elide`]).
fn is_elided_leaf(pat: &[Statement], b: &Bindings) -> bool {
    matches!(pat, [Statement::Return(ret)]
        if match ret.values.as_slice() {
            [RValue::Local(param)] => b.elide.as_ref() == Some(param),
            [RValue::Literal(Literal::Nil)] => b.elide_nil,
            _ => false,
        })
}

fn unify_stmt(t: &Target, p: &Statement, c: &Statement, b: &mut Bindings) -> Result<(), ()> {
    // The expression-level unifier only needs the binding-hole sets; build the
    // shared context once. `unify_block` (statement-level) still needs `t.kind`,
    // so it keeps taking `t`.
    let ctx = t.ctx();
    match (p, c) {
        (Statement::Assign(_) | Statement::Return(_), Statement::Assign(ca)) => unify_assignment(t, &ctx, p, ca, ca.prefix, b),
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
        // A discard target keeps its value helper's guards (`discard_body`).
        (Statement::If(pf), Statement::If(cf)) if t.kind == TKind::Value || t.discarded => {
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
            // A copy returning the helper's value from the caller: the same
            // value, taken the same way.
            ([pattern], [site]) if b.returning => unify_rvalue(&ctx, pattern, site, b),
            _ => Err(()),
        },
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

/// [`unify_stmt`] of a pattern assignment or value `return` against the
/// assignment `ca`, read as a declaration when `prefix`, whatever its own
/// flag says: a site declaration can be unified as the store it stands for
/// without a copy (`match_declared_value`).
fn unify_assignment(t: &Target, ctx: &MatchCtx, p: &Statement, ca: &Assign, prefix: bool, b: &mut Bindings) -> Result<(), ()> {
    match p {
        Statement::Assign(pa) => {
            // `prefix` distinguishes a `local x = ...` declaration from a plain
            // `x = ...` reassignment — they are NOT interchangeable. Matching a
            // declaration against a reassignment (or vice versa) would erase a
            // write to a caller-visible local. An inlined copy preserves the
            // callee's `local`, so genuine matches keep equal prefixes.
            // `t[k] += v` evaluates `t` and `k` once, `t[k] = t[k] + v` twice.
            if pa.left.len() != ca.left.len()
                || pa.right.len() != ca.right.len()
                || pa.parallel != ca.parallel
                || pa.prefix != prefix
                || pa.compound != ca.compound
            {
                return Err(());
            }
            for (pl, cl) in pa.left.iter().zip(&ca.left) {
                unify_lvalue(ctx, pl, cl, b)?;
            }
            // A local function of the helper called and nothing else: a
            // shared closure there is no object any code sees.
            if let ([LValue::Local(binder)], [RValue::Closure(pattern)], [RValue::Closure(site)]) =
                (pa.left.as_slice(), pa.right.as_slice(), ca.right.as_slice())
                && t.private_closures.contains(binder)
            {
                return unify_closure(ctx, pattern, site, true, b);
            }
            for (pr, cr) in pa.right.iter().zip(&ca.right) {
                unify_rvalue(ctx, pr, cr, b)?;
            }
            Ok(())
        }
        // Value target: the callee's `return X` was lowered to `RESULT = X` in
        // the inlined copy. Bind the single result local and unify the value.
        // The result-write leaf is always a PLAIN reassignment (`RESULT = X`):
        // `RESULT` is the init-less decl pinned by `result_decl` (prefix=true), and
        // LocalDeclarer's single-declaration invariant (local_declarations.rs) gives
        // each local exactly ONE prefix=true decl, so every in-region write to it is
        // prefix=false / parallel=false. A `local RESULT = X` redeclaration or a
        // parallel phi-copy here would change scope when spliced, so refuse it —
        // mirrors the sibling assignment arm's prefix/parallel equality and
        // `result_decl`'s own `prefix && !parallel` gate (F10a hardening).
        // A compound store (`r += x`) also reads `r`: never a leaf's store.
        Statement::Return(pr)
            if t.kind == TKind::Value
                && pr.values.len() == 1
                && ca.left.len() == 1
                && ca.right.len() == 1
                && !prefix
                && !ca.parallel
                && !ca.compound =>
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
            unify_returned_value(ctx, &pr.values[0], &ca.right[0], b)
        }
        _ => Err(()),
    }
}

/// The value a helper returns against the one its inlined copy stores into
/// the result local, which takes one value: a call leaf (P7-A) is the call
/// the store adjusts to one result, and a call the helper truncates
/// (`return (DeepCopy(t))`) the call such a store makes (`r = DeepCopy(v)`).
/// A caller taking more than one value of the site checks it apart
/// ([`hosted_spread`]).
fn unify_returned_value(ctx: &MatchCtx, pattern: &RValue, site: &RValue, b: &mut Bindings) -> Result<(), ()> {
    match (pattern, site) {
        (RValue::Call(x), RValue::Select(Select::Call(y))) | (RValue::Select(Select::Call(x)), RValue::Call(y)) => {
            unify_call(ctx, x, y, b)
        }
        (RValue::MethodCall(x), RValue::Select(Select::MethodCall(y)))
        | (RValue::Select(Select::MethodCall(x)), RValue::MethodCall(y)) => unify_method(ctx, x, y, b),
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
        (RValue::Closure(a), RValue::Closure(d)) => unify_closure(ctx, a, d, false, b),
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
    // Both are bound to a local only ever called ([`Target::private_closures`]):
    // their identity is seen by no code.
    private: bool,
    bindings: &mut Bindings,
) -> Result<(), ()> {
    let same_function = Arc::ptr_eq(&pattern.function.0, &candidate.function.0);
    // A closure DUPCLOSURE loads is one object per constant of the loading
    // prototype (`Function::closure_constant`): the helper's own once the
    // call is rebuilt, the caller's for its inlined copy. Trading one for
    // the other is visible to `==` and to table keys, so neither side may
    // be such a closure; a NEWCLOSURE is a fresh object either way.
    let (pattern_proto, pattern_shared) = {
        let function = pattern.function.0.lock();
        (function.bytecode_proto_id, function.closure_constant.is_some())
    };
    let (candidate_proto, candidate_shared) = if same_function {
        (pattern_proto, pattern_shared)
    } else {
        let function = candidate.function.0.lock();
        (function.bytecode_proto_id, function.closure_constant.is_some())
    };
    if (pattern_shared || candidate_shared) && !private {
        return Err(());
    }
    if !same_function && (pattern_proto.is_none() || pattern_proto != candidate_proto) {
        return Err(());
    }

    if pattern.upvalues.len() != candidate.upvalues.len() {
        return Err(());
    }
    for (pattern_upvalue, candidate_upvalue) in pattern.upvalues.iter().zip(&candidate.upvalues) {
        let (pattern_local, candidate_local) = match (pattern_upvalue, candidate_upvalue) {
            (Upvalue::Copy(pattern), Upvalue::Copy(candidate))
            | (Upvalue::Ref(pattern), Upvalue::Ref(candidate)) => (pattern, candidate),
            // The helper's closure reaches an outer local through the
            // helper's own upvalue (`LCT_UPVAL`, a shared cell), its inlined
            // copy straight from the caller's register (`LCT_VAL`). The
            // helper captured that local when it was created, the copy when
            // it runs: the same value where nothing assigns the local after
            // its declaration.
            (Upvalue::Ref(pattern), Upvalue::Copy(candidate))
                if pattern == candidate
                    && !ctx.params.contains(pattern)
                    && !ctx.locals.contains(pattern)
                    && ctx.captures.is_some_and(|captures| captures.never_reassigned(pattern)) =>
            {
                (pattern, candidate)
            }
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
    decl_map: &FxHashMap<RcLocal, std::ops::Range<usize>>,
    targets: &[Target],
) -> Option<std::ops::Range<usize>> {
    if let Statement::Assign(a) = s {
        if a.prefix
            && a.left.len() == 1
            && a.right.len() == 1
            && let LValue::Local(l) = &a.left[0]
            && let RValue::Closure(c) = &a.right[0]
            && let Some(range) = decl_map.get(l)
            && Arc::as_ptr(&c.function.0) == targets[range.start].func_ptr
        {
            return Some(range.clone());
        }
    }
    None
}

fn deinline_block(
    stmts: &mut Vec<Statement>,
    targets: &[Target],
    decl_map: &FxHashMap<RcLocal, std::ops::Range<usize>>,
    outer_active: &[usize],
    outer_continuation: &[&[Statement]],
    // Where this block sits, for the locals read after it.
    around: &Enclosing<'_>,
    current_func: Option<FnPtr>,
    is_func_tail: bool,
    is_func_body_top: bool,
    // Falling off the end of this block continues the loop around it (a
    // `while` or `for` body, or the last arm of one): `continue` there is a
    // jump to the end of the block.
    loop_tail: bool,
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
    // Rewrites below this block and in it change what its windows hold.
    let splices_before = newly.splices;
    let mut child_tails = void_return_tails(stmts);
    if is_func_tail && let Some(last) = child_tails.last_mut() {
        *last = true;
    }
    let last_statement = stmts.iter().rposition(|statement| !is_match_trivia(statement));
    {
        // An `if`'s continuation is the statements after it, which the
        // recursion into the statements before them leaves as they are.
        let continuations = targets.iter().any(|target| target.cps_loop_return);
        let mut active: Vec<usize> = outer_active.to_vec();
        for j in 0..stmts.len() {
            let (head, rest) = stmts.split_at_mut(j + 1);
            let (before, current) = head.split_at_mut(j);
            let s = &mut current[0];
            // The blocks of `s` run before `rest`, after `before` declared
            // its locals.
            let (before, rest): (&[Statement], &[Statement]) = (before, rest);
            let nested = Enclosing::nested(around, before, rest);
            let continuation = if continuations && matches!(s, Statement::If(_)) {
                continuation_segments(rest, outer_continuation)
            } else {
                Vec::new()
            };
            let child_tail = child_tails[j];
            let arm_loop_tail = loop_tail && Some(j) == last_statement;
            match s {
                Statement::If(f) => {
                    for arm in [&f.then_block, &f.else_block] {
                        deinline_block(
                            &mut arm.lock().0,
                            targets,
                            decl_map,
                            &active,
                            &continuation,
                            &nested,
                            current_func,
                            child_tail,
                            false,
                            arm_loop_tail,
                            newly,
                        );
                    }
                }
                Statement::While(w) => deinline_block(
                    &mut w.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &Enclosing { looping: true, ..nested },
                    current_func,
                    false,
                    false,
                    true,
                    newly,
                ),
                // `continue` in a `repeat` body runs its `until` condition,
                // which may read locals the body declares.
                Statement::Repeat(r) => deinline_block(
                    &mut r.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &Enclosing { looping: true, until: Some(&r.condition), ..nested },
                    current_func,
                    false,
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
                    &Enclosing { looping: true, vars: std::slice::from_ref(&nf.counter), ..nested },
                    current_func,
                    false,
                    false,
                    true,
                    newly,
                ),
                Statement::GenericFor(gf) => deinline_block(
                    &mut gf.block.lock().0,
                    targets,
                    decl_map,
                    &active,
                    &[],
                    &Enclosing { looping: true, vars: &gf.res_locals, ..nested },
                    current_func,
                    false,
                    false,
                    true,
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
            if let Some(declared) = target_decl_index(s, decl_map, targets) {
                active.extend(declared);
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
    let keys = newly
        .priorities
        .entry(current_func)
        .or_insert_with(|| {
            let helpers: Vec<usize> = targets.iter().map(|target| target.func_ptr as usize).collect();
            crate::reconstruction_search::priority_keys(current_func.map(|p| p as usize), &helpers).map(std::rc::Rc::new)
        })
        .clone();
    // `reconstruction_search::prioritize` of the focused targets: a stable
    // partition by the body's keys.
    let prioritize = |active: &[usize]| {
        let (mut focused, rivals): (Vec<usize>, Vec<usize>) =
            active.iter().partition(|&&i| revisit || targets[i].focused);
        if let Some(keys) = &keys {
            let (first, rest): (Vec<usize>, Vec<usize>) = focused.iter().partition(|&&i| keys[i]);
            focused = first;
            focused.extend(rest);
        }
        (focused, rivals)
    };
    let (mut ordered, mut rivals) = prioritize(&active);
    let mut i = 0;
    let mut anchor = 0;
    // Tail-liveness index, replacing the per-window O(N) `any_local_live` rescan
    // with an O(|set|) lookup. Built lazily on the first query (so target-free /
    // never-matching blocks pay nothing) and reused across positions; the driver
    // invalidates it after each splice, after which the next query rebuilds it.
    let mut last_occ = Liveness::new(around);
    // The canonical windows of this block ([`CanonCache`]): the canon of a
    // contiguous tail-window `stmts[start..start+w]` depends ONLY on the block,
    // not on which target requested it. Single-threaded (the serial tail), so
    // `Rc` is fine; the whole `deinline` pass never runs inside the parallel
    // region.
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
            newly,
            splices_before,
            current_func,
            is_func_tail,
            is_func_body_top,
            outer_continuation,
            loop_tail,
            &mut last_occ,
            &mut canon_cache,
        ) {
            let mut call = Call::new(RValue::Local(hit.f_local.clone()), hit.args)
                .reconstructed(crate::call_origins::Kind::StatementDeinline);
            call.one_result = hit.single_valued;
            let stmt = match hit.host {
                Some(mut host) => {
                    if let Some((placeholder, wrap)) = &hit.placeholder {
                        let value = if *wrap { RValue::Select(Select::Call(call)) } else { RValue::Call(call) };
                        fill_placeholder(&mut host, placeholder, value);
                    }
                    select_prefix_calls(&mut host);
                    host
                }
                None if hit.returns_value => {
                    let mut values = hit.returned_before;
                    values.push(RValue::Call(call));
                    Statement::Return(Return::new(values))
                }
                None if hit.results.is_empty() => Statement::Call(call),
                None => Statement::Assign(Assign {
                    node_origin: Default::default(),
                    left: hit.results.into_iter().map(LValue::Local).collect(),
                    right: vec![RValue::Call(call)],
                    prefix: !hit.assign,
                    parallel: false,
                    compound: false,
                }),
            };
            // The call's `rebuilt` attribute is its marker: the formatter
            // prints the site comment wherever the call ends up.
            let mut replacement = vec![stmt];
            if let Some(ret) = hit.tail_ret {
                replacement.push(Statement::Return(Return { node_origin: Default::default(), values: vec![ret] }));
            }
            let advance = replacement.len();
            // The absorbed argument temps and copies right before `i` go too.
            let start = i - hit.absorbed;
            let removed = i + hit.consume - start;
            stmts.splice(start..start + removed, replacement);
            if !hit.mode.is_empty() {
                crate::telemetry::count(hit.mode, 1);
            }
            newly.binders.insert(hit.f_local);
            newly.splices += 1;
            newly.bodies.insert(current_func);
            i = start + advance;
            // The block changed; drop the cached index so the next query rebuilds
            // it against the spliced `stmts`.
            last_occ.index = None;
            canon_cache.spliced(stmts, start, removed, advance);
            anchor = i;
        } else {
            // A target declared inside a matched window went with it and is never
            // activated; one reached here unmatched is in scope from here on.
            if let Some(declared) = target_decl_index(&stmts[i], decl_map, targets) {
                active.extend(declared);
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
    decl_map: &FxHashMap<RcLocal, std::ops::Range<usize>>,
    active: &[usize],
    newly: &mut Progress,
) {
    match rv {
        RValue::Closure(c) => {
            let fp = Arc::as_ptr(&c.function.0);
            // The orphans this body kept aside are in scope in all of it.
            let with_orphans: Vec<usize>;
            let active = match newly.orphan_scopes.get(&Some(fp)) {
                Some(orphans) => {
                    with_orphans = active.iter().chain(orphans).copied().collect();
                    &with_orphans
                }
                None => active,
            };
            let mut function = c.function.0.lock();
            let function = &mut *function;
            deinline_block(
                &mut function.body.0,
                targets,
                decl_map,
                active,
                &[],
                &Enclosing::function(&function.parameters),
                Some(fp),
                true,
                true,
                false,
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

/// A matched width, what its unification found, and (Gap B arm-return form)
/// the tail return value to re-emit after the call.
struct Site {
    width: usize,
    unified: Unified,
    tail_ret: Option<RValue>,
}

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
    let found = || Some(Site { width: w, unified: u.clone(), tail_ret: tail_ret.cloned() });
    match site {
        None => *site = found(),
        // Only inferred matches were seen, so only they made it ambiguous.
        Some(prev) if prev.unified.inferred.is_some() && u.inferred.is_none() => {
            *site = found();
            *ambiguous = false;
        }
        Some(prev) if prev.unified.inferred.is_none() && u.inferred.is_some() => {}
        Some(prev) => {
            let same_ret = match (prev.tail_ret.as_ref(), tail_ret) {
                (None, None) => true,
                (Some(a), Some(b)) => rvalue_exact_eq(a, b),
                _ => false,
            };
            if !args_vec_eq(&prev.unified.args, &u.args) || !same_ret || prev.unified.returned != u.returned {
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
    /// The results are locals the caller already has, stored into by the
    /// call (`r = f(args)`), not declared by it.
    assign: bool,
    /// Statements right before `i` the call takes in: argument temps and
    /// written-parameter copies ([`absorb_arguments`]).
    absorbed: usize,
    /// [`Unified::written`] and [`Unified::first_moved`], for
    /// [`absorb_arguments`].
    written: Vec<usize>,
    first_moved: Option<usize>,
    /// Matched by a discard target ([`Target::discarded`]).
    discarded: bool,
    /// The call's value is the caller's return value: `return f(args)`
    /// ([`match_returned_value`]).
    returns_value: bool,
    /// The values the caller's `return` gives before the call's
    /// (`return p, f(args)`).
    returned_before: Vec<RValue>,
    /// Where `host` holds the call: the read of this local, and whether the
    /// call keeps one result there (`(f(args))`). The call is built once the
    /// arguments are known: the temps right before the statement may still
    /// go into it ([`match_hosted_value`], [`absorb_arguments`]).
    placeholder: Option<(RcLocal, bool)>,
    /// The site shape a new result mode found, for telemetry (`""` for the
    /// others).
    mode: &'static str,
    /// [`Target::single_valued`] of the helper called.
    single_valued: bool,
    /// Where the last statement covered stays with the call in it
    /// (`host`), the nodes of its code the helper's body stands for, the
    /// arguments not counted; `None` where every statement covered goes
    /// ([`Hit::extent`]).
    partial: Option<usize>,
}

impl Hit {
    /// A call of `t` replacing `consume` statements, declaring `results`,
    /// with the arguments unification found.
    fn call(t: &Target, consume: usize, u: Unified, results: Vec<RcLocal>) -> Hit {
        Hit {
            f_local: t.f_local.clone(),
            consume,
            args: u.args,
            results,
            tail_ret: None,
            host: None,
            inferred: u.inferred,
            assign: false,
            absorbed: 0,
            written: u.written,
            first_moved: u.first_moved,
            discarded: false,
            returns_value: false,
            returned_before: Vec::new(),
            placeholder: None,
            mode: "",
            single_valued: t.single_valued,
            partial: None,
        }
    }

    /// The statements the call replaces, the absorbed ones included.
    fn covered(&self) -> usize {
        self.absorbed + self.consume
    }

    /// How much of the site the call stands for: the statements it covers,
    /// then, where the last of them stays with the call in it, how much of
    /// that statement (a whole one counting as more than any part).
    fn extent(&self) -> (usize, usize) {
        (self.covered(), self.partial.unwrap_or(usize::MAX))
    }
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
    // Receives the helpers of a site refused because two of them match
    // (`contested`) and where a target's own matcher found one (`found`).
    progress: &mut Progress,
    // `progress.splices` before this block's subtree was scanned.
    splices_before: usize,
    current_func: Option<FnPtr>,
    is_func_tail: bool,
    is_func_body_top: bool,
    outer_continuation: &[&[Statement]],
    loop_tail: bool,
    last_occ: &mut Liveness,
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
    let anchor_stmt = stmts.get(anchor);
    let anchor_disc = anchor_stmt.map(std::mem::discriminant);
    // Second prefilter dimension: the fixed-name anchor of that first statement
    // (method / global-call name). Computed once per position; compared to each
    // candidate target's `pat0_anchor_key`. Same sound domain as `pat0_kind`.
    let anchor_key = anchor_stmt.and_then(stmt_anchor_key);
    let anchor_is_if = matches!(anchor_stmt, Some(Statement::If(_)));
    let block = stmts.as_ptr() as usize;
    let found_at = &mut progress.found;
    let rescan = progress.rescan.as_ref().filter(|_| progress.splices == splices_before);
    let tried_everywhere = rescan.is_some_and(|rescan| rescan.everywhere.contains(&current_func));
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
        let seen_without_site = rescan.is_some_and(|rescan| {
            // A late target was not tried before the assignment phase.
            (rescan.tried[ti] || (tried_everywhere && !t.late()))
                && !rescan.with_closure[ti]
                && !rescan.found.contains(&(block, i, ti))
        });
        // A late target waits for the assignment phase.
        if t.late() && !t.assigns {
            return Ok(None);
        }
        let mut hit = if seen_without_site { None } else { match (t.kind, t.value_anchor) {
            (TKind::Void, _) => match_void(
                stmts,
                i,
                t,
                is_func_tail,
                is_func_body_top,
                outer_continuation,
                loop_tail,
                last_occ,
                canon_cache,
                current_func,
            ),
            (TKind::Value, _) if t.loop_exit_at.is_some() => {
                match_value_loop(stmts, i, t, is_func_body_top, last_occ, current_func)
            }
            (TKind::Value, ValueAnchor::AtResultDecl) => {
                match_value(stmts, i, t, is_func_body_top, last_occ, canon_cache, current_func)
                    .or_else(|| match_returned_value(stmts, i, t, is_func_body_top, last_occ, canon_cache, current_func))
                    .or_else(|| match_hosted_value(stmts, i, anchor, t, canon_cache, current_func))
            }
            (TKind::Value, ValueAnchor::AtPrefix) => {
                match_value_prefixed(stmts, i, t, current_func, is_func_body_top, last_occ)
                    .or_else(|| match_returned_value(stmts, i, t, is_func_body_top, last_occ, canon_cache, current_func))
                    .or_else(|| {
                        (!narrow_prefix(&t.pat))
                            .then(|| match_value(stmts, i, t, is_func_body_top, last_occ, canon_cache, current_func))
                            .flatten()
                    })
            }
        } };
        if hit.is_some() {
            found_at.insert((block, i, ti));
        }
        // A value stored into a local the caller already has.
        if hit.is_none() && t.assigns && t.kind == TKind::Value && assign_head_may_match(t, anchor_stmt, anchor_key) {
            hit = match_assigned_value(stmts, i, t, is_func_body_top, last_occ, canon_cache, current_func);
        }
        let hit = hit.filter(|hit| !hit.inferred.as_ref().is_some_and(|param| continues_pruned_branch(t, param, stmts, i + hit.consume)));
        let hit = hit.map(|hit| host_returned_cell(stmts, i, hit, t, current_func).unwrap_or_else(|hit| hit));
        Ok(hit.and_then(|hit| absorb_arguments(stmts, i, t, Hit { discarded: t.discarded, ..hit }, last_occ, is_func_body_top)))
    };
    // Where several helpers match, the one standing for the most code wins
    // ([`Hit::extent`]): each rebuild is exact, and a smaller one would leave
    // the rest pasted (`cancel()` matches only the first statement of
    // `purchase(nil)`; `flag(x)` only one operand of the `key(p, data)` a
    // whole value is a copy of). Two of the same extent are ambiguous:
    // refuse.
    fn offer(found: &mut Option<Hit>, tied: &mut Vec<RcLocal>, hit: Hit) {
        match found {
            // A value helper and its discard variant over the same
            // statements are one call either way, not rivals: the value
            // form, which keeps a result the site reads, wins. Of two
            // versions of one body ([`HelperCache::earlier`]), the one
            // standing for more.
            Some(best) if best.covered() == hit.covered() && best.f_local == hit.f_local => {
                if (best.discarded && !hit.discarded) || (best.discarded == hit.discarded && hit.extent() > best.extent()) {
                    *found = Some(hit);
                }
            }
            Some(best) if best.extent() > hit.extent() => {}
            Some(best) if best.extent() == hit.extent() => tied.push(hit.f_local),
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
        progress.contested.extend(tied);
        progress.contested.extend(found.map(|best| best.f_local));
        return refused("ambiguous");
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
    // Falling off the end of `stmts` continues the enclosing loop.
    loop_tail: bool,
    last_occ: &mut Liveness,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let kc = t.pat.len();
    // The window starts with the body; the argument temps and the copies of
    // written parameters before it are taken in afterwards
    // (`absorb_arguments`).
    let start = i;
    if head_refused(t, &stmts[start..]) {
        return None;
    }
    // F2: an effective-count ceiling (trivia don't consume the budget) so a nested
    // candidate region carrying 2+ interposed `Empty`s is still reachable. The
    // ceiling is the pattern's tail-spine length (guard-form expansion of every
    // tail `if`, see `tail_spine_len`) plus one for a site-only trailing `return`.
    let max_w = raw_width_for_effective(stmts, start, t.pat_spine_len + 1);
    let mut site: Option<Site> = None;
    let mut ambiguous = false;
    // A constant argument can remove a branch of the body (Tier B), so a
    // specializable helper's copy may be shorter than its body.
    let min_w = if t.specializable { 1 } else { kc };
    // The canon length and return scan of each width, one statement at a time.
    let mut grown = WindowGrowth::new(stmts, start, min_w);
    for w in min_w..=max_w {
        dprof::inc(&dprof::WIDTH_ITERS, 1);
        crate::telemetry::count("width_candidates", 1);
        let raw = &stmts[start..start + w];
        let (top_len, has_return) = grown.extend_to(stmts, start + w);
        // Never replace a function's ENTIRE top-level body with a single call:
        // the ambiguous thin-wrapper case (`B(x)=A(x)`).
        if is_func_body_top && i == 0 && start + w == stmts.len() {
            continue;
        }
        if w < kc || top_len < kc {
            let shorter = may_specialize(t)
                && top_len < kc
                && !(has_return && !(is_func_tail && start + w == stmts.len()));
            if shorter {
                let plain = canon_window(canon_cache, t, stmts, start, w);
                if charge_unify(t, &plain)
                    && let Some(u) = try_unify_specialized_site(t, &plain, current_func)
                    && !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals)
                {
                    record_site(&mut site, &mut ambiguous, w, &u, None);
                }
            }
            continue;
        }
        // Attempt 1 — plain canon, with tail-safety for consuming a caller return.
        // The canon length rejects most widths before any window is built.
        if top_len == kc {
            let plain_blocked = has_return && !(is_func_tail && start + w == stmts.len());
            if !plain_blocked && plain_kinds_may_match(t, raw) {
                let plain = canon_window(canon_cache, t, stmts, start, w);
                if let Some(u) = try_unify_site_any(t, &plain, current_func) {
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
                if let Some(u) = try_unify_cps_site(t, raw, &plain, &continuation, current_func) {
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
            if let Some(u) = try_unify_site_any(t, &folded, current_func) {
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
                if let Some(u) = try_unify_site_any(t, &folded, current_func) {
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
                if let Some(u) = try_unify_site_any(t, &folded, current_func) {
                    if !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals) {
                        record_site(&mut site, &mut ambiguous, w, &u, Some(&ret));
                    }
                }
            }
        }
        // Attempt 5 — the window ends a loop body: the helper's `return`, a
        // jump to the end of its copy, is the loop's `continue` there.
        if loop_tail
            && stmts[start + w..].iter().all(is_match_trivia)
            && continues_as_returns_shape(raw)
            && let Some(returning) = continues_as_returns(raw)
            && canon_top_len(&returning, true) == kc
            && charge_window(t, &returning)
        {
            let folded = canon_recurse(canon_top(&returning, true), true);
            if let Some(u) = try_unify_site_any(t, &folded, current_func)
                && !tail_has_live(last_occ, stmts, i, start + w, &u.callee_locals)
            {
                record_site(&mut site, &mut ambiguous, w, &u, None);
            }
        }
    }
    if ambiguous {
        return None;
    }
    let site = site?;
    let results = site.unified.returned.clone();
    Some(Hit { tail_ret: site.tail_ret, ..Hit::call(t, (start - i) + site.width, site.unified, results) })
}

/// Whether [`continues_as_returns`] gives a body, read without copying one:
/// most loop bodies have no `continue`.
fn continues_as_returns_shape(stmts: &[Statement]) -> bool {
    fn walk(stmts: &[Statement], found: &mut bool) -> bool {
        stmts.iter().all(|statement| match statement {
            Statement::Continue(_) => {
                *found = true;
                true
            }
            Statement::Break(_) | Statement::Return(_) | Statement::Goto(_) | Statement::Label(_) => false,
            Statement::If(branch) => walk(&branch.then_block.lock().0, found) && walk(&branch.else_block.lock().0, found),
            other => !statement_has_return(other),
        })
    }
    let mut found = false;
    let shaped = walk(stmts, &mut found) && found;
    debug_assert_eq!(shaped, continues_as_returns(stmts).is_some());
    shaped
}

/// [`canon_top_len`] (tail position) and [`block_has_return`] of the window
/// `stmts[start..end]` as `end` grows, one statement at a time instead of
/// rescanning every width. The canon length is the count of real
/// statements, less a trailing void return (N1), or up to the first
/// foldable guard with an effective statement after it (N3).
struct WindowGrowth {
    start: usize,
    end: usize,
    real: usize,
    last_void: bool,
    first_guard: Option<usize>,
    has_return: bool,
    /// The last real statement, and the one before it (E1,
    /// [`distributes_return`]).
    last: Option<usize>,
    before_last: Option<usize>,
}

impl WindowGrowth {
    fn new(stmts: &[Statement], start: usize, width: usize) -> Self {
        let mut grown = Self {
            start,
            end: start,
            real: 0,
            last_void: false,
            first_guard: None,
            has_return: false,
            last: None,
            before_last: None,
        };
        // A block shorter than the narrowest width has no window to grow.
        grown.extend_to(stmts, (start + width.saturating_sub(1)).min(stmts.len()));
        grown
    }

    /// The canon length and whether a return is in the window ending at `end`.
    fn extend_to(&mut self, stmts: &[Statement], end: usize) -> (usize, bool) {
        for (k, statement) in stmts[self.end..end].iter().enumerate() {
            self.has_return |= statement_has_return(statement);
            if is_match_trivia(statement) {
                continue;
            }
            if self.first_guard.is_none() && is_foldable_guard(statement) {
                self.first_guard = Some(self.real);
            }
            self.last_void = matches!(statement, Statement::Return(r) if r.values.is_empty());
            self.real += 1;
            self.before_last = self.last;
            self.last = Some(self.end + k);
        }
        self.end = self.end.max(end);
        let effective = self.real - usize::from(self.last_void);
        let distributed = !self.last_void
            && matches!((self.before_last, self.last), (Some(before), Some(last))
                if matches!(&stmts[before], Statement::If(f) if distributes_return(f, std::slice::from_ref(&stmts[last]))));
        let len = match self.first_guard {
            Some(guard) if guard + 1 < effective => guard + 1,
            _ if distributed => effective - 1,
            _ => effective,
        };
        debug_assert_eq!(len, canon_top_len(&stmts[self.start..end], true));
        debug_assert_eq!(self.has_return, block_has_return(&stmts[self.start..end]));
        (len, self.has_return)
    }
}

/// Whether `stmts` create a closure, at any depth.
fn block_has_closure(stmts: &[Statement]) -> bool {
    stmts.iter().any(|statement| {
        let mut found = false;
        statement.traverse_rvalues_ref(&mut |value| found |= matches!(value, RValue::Closure(_)));
        found
            || match statement {
                Statement::If(branch) => {
                    block_has_closure(&branch.then_block.lock().0) || block_has_closure(&branch.else_block.lock().0)
                }
                Statement::While(node) => block_has_closure(&node.block.lock().0),
                Statement::Repeat(node) => block_has_closure(&node.block.lock().0),
                Statement::NumericFor(node) => block_has_closure(&node.block.lock().0),
                Statement::GenericFor(node) => block_has_closure(&node.block.lock().0),
                _ => false,
            }
    })
}

/// `stmts`, the end of a loop body, with each `continue` of that loop a
/// void `return`: what it was in the helper whose copy ends the body. `None`
/// when it has none, or leaves another way (`break`, `return`).
fn continues_as_returns(stmts: &[Statement]) -> Option<Vec<Statement>> {
    fn rewrite(stmts: &[Statement], found: &mut bool) -> Option<Vec<Statement>> {
        stmts
            .iter()
            .map(|statement| match statement {
                Statement::Continue(_) => {
                    *found = true;
                    Some(Statement::Return(Return::default()))
                }
                Statement::Break(_) | Statement::Return(_) | Statement::Goto(_) | Statement::Label(_) => None,
                Statement::If(branch) => Some(
                    If::new(
                        branch.condition.clone(),
                        Block(rewrite(&branch.then_block.lock().0, found)?),
                        Block(rewrite(&branch.else_block.lock().0, found)?),
                    )
                    .into(),
                ),
                // A nested loop's own `continue` and `break` stay its own.
                other if statement_has_return(other) => None,
                other => Some(other.clone()),
            })
            .collect()
    }
    let mut found = false;
    let rewritten = rewrite(stmts, &mut found)?;
    found.then_some(rewritten)
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
    last_occ: &mut Liveness,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let Some(r) = result_decl(&stmts[i]) else {
        return match_declared_value(stmts, i, i, t, is_func_body_top, last_occ, current_func, &SitePrefix::new(&[]));
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
            Window::default()
        };
        // Plain form first; then the result-alias form (`alias_result_leaves`):
        // a leaf that writes RESULT early and keeps using it (`RESULT = E;
        // S(RESULT)…`) is rewritten to `local T = E; S(T)…; RESULT = T` — the
        // shape the callee's `local L = E; S(L)…; return L` unifies against.
        let attempts: [(Window, Option<Vec<Statement>>); 2] = [
            (cwin, None),
            match alias_result_leaves(region, &r) {
                Some(rw) if !block_has_return(&rw) && canon_top_len(&rw, true) == kc && charge_window(t, &rw) => (
                    Window::owned(canon_recurse(canon_top(&rw, true), true)),
                    Some(rw),
                ),
                _ => (Window::default(), None),
            },
        ];
        for (idx, (cw, rewritten)) in attempts.iter().enumerate() {
            if (idx == 0 && cw.is_empty()) || (idx == 1 && rewritten.is_none()) {
                continue;
            }
            let region_eff: &[Statement] = rewritten.as_deref().unwrap_or(region);
            if let Some(u) = try_unify_declared_result(t, cw, current_func) {
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
    let Some(site) = site else {
        return match_short_circuit(stmts, i, &r, t, is_func_body_top, last_occ, current_func);
    };
    Some(Hit::call(t, 1 + site.width, site.unified, vec![r]))
}

/// `local R = P or f(args)` for a helper whose first guard returns `true`
/// (`f == nil or isCallable(f)`). Luau compiles the comparison `P` as a
/// jump to the store of `true` the copy's first guard takes too, so the
/// site reads `local R; if P or A then R = true else REST end`. With `P`
/// provably boolean (a comparison, `not`, a boolean literal, or `and` /
/// `or` of those), `R` gets `P` exactly where it is `true`, and what is left
/// once `P` is split off is the copy: `if A then R = true else REST end`.
/// `P` runs first and the copy only where it is false, as in `P or
/// f(args)`; the arguments stay inside the copy, nothing is absorbed.
fn match_short_circuit(
    stmts: &[Statement],
    i: usize,
    r: &RcLocal,
    t: &Target,
    is_func_body_top: bool,
    last_occ: &mut Liveness,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let at = nth_effective_index(stmts, i + 1, 0)?;
    let Statement::If(branch) = &stmts[at] else { return None };
    let RValue::Binary(or) = &branch.condition else { return None };
    if or.operation != BinaryOperation::Or || !crate::binary::is_boolean(&or.left) || reads_local(&or.left, r) {
        return None;
    }
    let stores_true = {
        let then = branch.then_block.lock();
        let mut real = then.0.iter().filter(|s| !is_match_trivia(s));
        matches!((real.next(), real.next()), (Some(Statement::Assign(store)), None)
            if is_plain_local_write(store, r) && !store.compound
                && matches!(store.right[0], RValue::Literal(Literal::Boolean(true))))
    };
    if !stores_true || (is_func_body_top && i == 0 && at + 1 == stmts.len()) {
        return None;
    }
    let region = vec![Statement::If(If::new(
        or.right.as_ref().clone(),
        Block(branch.then_block.lock().0.clone()),
        Block(branch.else_block.lock().0.clone()),
    ))];
    if canon_top_len(&region, true) != t.pat.len() || block_has_return(&region) || !charge_window(t, &region) {
        return None;
    }
    let u = try_unify_site_any(t, &canon_recurse(canon_top(&region, true), true), current_func)?;
    let complete = u.result.as_ref() == Some(r)
        && !u.callee_locals.contains(r)
        && !block_reads_local(&stmts[at..=at], r)
        && !tail_has_live(last_occ, stmts, i, at + 1, &u.callee_locals);
    if !complete {
        return None;
    }
    let value = Binary::new(or.left.as_ref().clone(), hosted_call(t, u.args.clone(), false), BinaryOperation::Or);
    let mut declaration = Assign::new(vec![LValue::Local(r.clone())], vec![value.into()]);
    declaration.prefix = true;
    let partial = Some(region.iter().map(dbg_stmt_node_count).sum::<usize>().saturating_sub(u.args.iter().map(value_nodes).sum()));
    Some(Hit { host: Some(declaration.into()), partial, mode: "site_short_circuit", ..Hit::call(t, at + 1 - i, u, Vec::new()) })
}

/// Return mode: the copy of a single-valued helper returns its value from
/// the caller (`return isCallable(object.andThen)`), the store of each leaf
/// cloned into the caller's `return`. The window is the rest of the block,
/// returning one value on every path; each `return X` of the pattern
/// unifies with one of the site. Rebuilt as `return f(args)`: the helper
/// gives exactly one value on every path (Luau's `returnsOne`, the only
/// helpers Luau inlines there), as each `return X` of the copy did.
fn match_returned_value(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    is_func_body_top: bool,
    last_occ: &mut Liveness,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    if !t.single_valued || t.loop_exit_at.is_some() || !t.returns.is_empty() || t.falls_off {
        return None;
    }
    let end = stmts.len();
    let kc = t.pat.len();
    if !canon_cache.returns(stmts) || !return_window_may_fit(&stmts[i..end], kc) {
        return None;
    }
    // A whole function body is never one call (the thin-wrapper case):
    // only where its `return` gives more values than the copy's.
    let whole_body = is_func_body_top && i == 0;
    let mut attempt = |window: &[Statement], before: Vec<RValue>| -> Option<Hit> {
        if whole_body && before.is_empty() {
            return None;
        }
        if !block_always_returns(window) || leading_statement_refused(t, window) || leading_condition_refused(t, window) {
            return None;
        }
        if block_has_void_return(window) || has_depth_zero_loop_control(window, 0) || canon_top_len(window, true) != kc {
            return None;
        }
        if !charge_window(t, window) {
            return None;
        }
        let canonical = canon_recurse(canon_top(window, true), true);
        if !charge_unify(t, &canonical) {
            return None;
        }
        let u = try_unify_seeded(t, &canonical, current_func, Bindings { returning: true, ..Bindings::default() })?;
        if u.result.is_some() || tail_has_live(last_occ, stmts, i, end, &u.callee_locals) {
            return None;
        }
        // Those values ran before the copy and now run before the call:
        // each a constant, or a local neither the copy nor a closure writes.
        if !before.is_empty() {
            let mut written = FxHashSet::default();
            collect_written(window, &mut written);
            let stable = |value: &RValue| match value {
                RValue::Literal(_) => true,
                RValue::Local(local) => {
                    !written.contains(local) && !u.callee_locals.contains(local) && !t.captures.closure_written(local)
                }
                _ => false,
            };
            if !before.iter().all(stable) {
                return refused("returned_before_unstable");
            }
        }
        let mode = if before.is_empty() { "site_returned" } else { "site_returned_tuple" };
        Some(Hit { returns_value: true, returned_before: before, mode, ..Hit::call(t, end - i, u, Vec::new()) })
    };
    if let Some(hit) = attempt(&stmts[i..end], Vec::new()) {
        return Some(hit);
    }
    // The caller's `return` may give values before the copy's: the same
    // ones on every path (`return p, f(x)`, the copy's leaves each `return
    // p, A`), split off.
    let (stripped, before) = returned_before(&stmts[i..end])?;
    attempt(&stripped, before)
}

/// `stmts`, a window returning on every path, with each `return v1, ...,
/// vn, x` cut to `return x`, and `v1, ..., vn`: one to several values, the
/// same ones (constants and locals) in every `return`. `None` where the
/// returns give one value, differ before the last one, or a `return` sits
/// in a loop.
fn returned_before(stmts: &[Statement]) -> Option<(Vec<Statement>, Vec<RValue>)> {
    fn cut(stmts: &[Statement], before: &mut Option<Vec<RValue>>) -> Option<Vec<Statement>> {
        let mut out = Vec::with_capacity(stmts.len());
        for statement in stmts {
            match statement {
                Statement::Return(ret) => {
                    let (last, values) = ret.values.split_last()?;
                    if values.is_empty() || !values.iter().all(|v| matches!(v, RValue::Local(_) | RValue::Literal(_))) {
                        return None;
                    }
                    match before {
                        Some(seen) if seen.len() != values.len() || !seen.iter().zip(values).all(|(a, b)| rvalue_exact_eq(a, b)) => return None,
                        Some(_) => {}
                        None => *before = Some(values.to_vec()),
                    }
                    out.push(Statement::Return(Return::new(vec![last.clone()])));
                }
                Statement::If(branch) => out.push(
                    If::new(
                        branch.condition.clone(),
                        Block(cut(&branch.then_block.lock().0, before)?),
                        Block(cut(&branch.else_block.lock().0, before)?),
                    )
                    .into(),
                ),
                other if statement_has_return(other) => return None,
                other => out.push(other.clone()),
            }
        }
        Some(out)
    }
    // Cheapest reject first: the first `return` gives one value.
    fn first_arity(stmts: &[Statement]) -> Option<usize> {
        stmts.iter().find_map(|statement| match statement {
            Statement::Return(ret) => Some(ret.values.len()),
            Statement::If(branch) => first_arity(&branch.then_block.lock().0).or_else(|| first_arity(&branch.else_block.lock().0)),
            _ => None,
        })
    }
    if first_arity(stmts)? < 2 {
        return None;
    }
    let mut before = None;
    let stripped = cut(stmts, &mut before)?;
    Some((stripped, before?))
}

/// Whether no window opening with `stmts` can match a value pattern opening
/// with an `if`, read off the first statement's condition alone: canon keeps
/// an `if` heading a window, its condition negated where a guard folds what
/// follows, and the unification compares the conditions first, from no
/// bindings, directly or negated ([`unify_stmt`]'s flip).
fn leading_condition_refused(t: &Target, stmts: &[Statement]) -> bool {
    let (Some(Statement::If(pattern)), Some(Statement::If(head))) = (t.pat.first(), stmts.iter().find(|s| !is_match_trivia(s))) else {
        return false;
    };
    let negated = negate_canon(head.condition.clone());
    let twice = negate_canon(negated.clone());
    [&head.condition, &negated, &twice]
        .into_iter()
        .all(|condition| unify_rvalue(&t.ctx(), &pattern.condition, condition, &mut Bindings::default()).is_err())
}

/// Whether the window `stmts`, running to the end of its block, may
/// canonicalize to `kc` statements, read off its first `kc + 2` statements:
/// past those, only a guard among the first `kc` folds it that short
/// ([`canon_top_len`]; E1 takes one statement off the end at most).
fn return_window_may_fit(stmts: &[Statement], kc: usize) -> bool {
    let mut seen = 0;
    for statement in stmts.iter().filter(|s| !is_match_trivia(s)) {
        if seen < kc && is_foldable_guard(statement) {
            return true;
        }
        seen += 1;
        if seen >= kc + 2 {
            return false;
        }
    }
    true
}

/// Whether `stmts` hold a `return` of no value, nested blocks included.
fn block_has_void_return(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Return(r) => r.values.is_empty(),
        Statement::If(f) => block_has_void_return(&f.then_block.lock().0) || block_has_void_return(&f.else_block.lock().0),
        Statement::While(w) => block_has_void_return(&w.block.lock().0),
        Statement::Repeat(r) => block_has_void_return(&r.block.lock().0),
        Statement::NumericFor(nf) => block_has_void_return(&nf.block.lock().0),
        Statement::GenericFor(gf) => block_has_void_return(&gf.block.lock().0),
        _ => false,
    })
}

/// A value helper's result stored into a local the caller already has
/// (`buf = expand(buf, offset + 1, state)`): no declaration marks the
/// window, which ends every path with a leaf's store `R = X` (its terminal
/// writes). On a path returning the very argument the caller passes as `R`,
/// Luau's copy stores nothing (a register into itself); such a leaf matches
/// an empty block once that identity parameter is pre-bound to `R`
/// ([`Bindings::elide`]), tried for each one after the plain form. Rebuilt
/// as `R = f(args)`; [`absorb_arguments`] declares `R` instead where it is
/// the argument's own temp (`local t = f(E)`).
fn match_assigned_value(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    is_func_body_top: bool,
    last_occ: &mut Liveness,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    // Every window here opens with the statement at `i`; the bindings fixed
    // in advance only narrow what unifies with it.
    if t.loop_exit_at.is_some() || !t.returns.is_empty() || leading_statement_refused(t, &stmts[i..]) {
        return None;
    }
    let kc = t.pat.len();
    let max_w = raw_width_for_effective(stmts, i, t.pat_raw_len + 1);
    let mut site: Option<Site> = None;
    let mut ambiguous = false;
    let mut grown = WindowGrowth::new(stmts, i, kc);
    for w in kc..=max_w {
        let (top_len, has_return) = grown.extend_to(stmts, i + w);
        if is_func_body_top && i == 0 && i + w == stmts.len() {
            continue;
        }
        let region = &stmts[i..i + w];
        if top_len != kc || has_return {
            continue;
        }
        let Some(result) = terminal_store(region) else { continue };
        if !plain_kinds_may_match(t, region) {
            continue;
        }
        let cwin = canon_window(canon_cache, t, stmts, i, w);
        for elided in std::iter::once(None).chain(t.identity_params.iter().map(Some)) {
            if cwin.is_empty() || !charge_unify(t, &cwin) {
                break;
            }
            let mut seed = Bindings { result: Some(result.clone()), assigned: true, ..Bindings::default() };
            if let Some(param) = elided {
                seed.params.insert(param.clone(), RValue::Local(result.clone()));
                seed.elide = Some(param.clone());
            }
            if let Some(u) = try_unify_seeded(t, &cwin, current_func, seed)
                && !u.callee_locals.contains(&result)
                && !tail_has_live(last_occ, stmts, i, i + w, &u.callee_locals)
            {
                record_site(&mut site, &mut ambiguous, w, &u, None);
            }
        }
    }
    if ambiguous {
        return None;
    }
    let site = site?;
    let result = site.unified.result.clone()?;
    Some(Hit { assign: true, ..Hit::call(t, site.width, site.unified, vec![result]) })
}

/// The local the leaf stores of a region write: its last statement `R = X`,
/// or the first such store at the end of an arm of the `if` ending it.
fn terminal_store(stmts: &[Statement]) -> Option<RcLocal> {
    match stmts.iter().rev().find(|statement| !is_match_trivia(statement))? {
        Statement::Assign(assign)
            if !assign.prefix && !assign.parallel && !assign.compound && assign.left.len() == 1 && assign.right.len() == 1 =>
        {
            assign.left[0].as_local().cloned()
        }
        Statement::If(branch) => {
            terminal_store(&branch.then_block.lock().0).or_else(|| terminal_store(&branch.else_block.lock().0))
        }
        _ => None,
    }
}

/// The cheap first-statement gate of [`match_assigned_value`], whose window
/// has no declaration to look for: it opens with a statement of `pat[0]`'s
/// kind, or an `if` canon may fuse into an assignment or a return (N4, N5),
/// or, for a pattern opening with its value `return X`, the store `R = X`;
/// with the same fixed name where both have one (`head_key`, the head's
/// `stmt_anchor_key`).
fn assign_head_may_match(t: &Target, head: Option<&Statement>, head_key: Option<u64>) -> bool {
    let Some(head) = head else { return false };
    let kinds = match (&t.pat[0], head) {
        (Statement::Return(_), Statement::Assign(_) | Statement::If(_)) | (Statement::Assign(_), Statement::If(_)) => true,
        (pattern, site) => std::mem::discriminant(pattern) == std::mem::discriminant(site),
    };
    kinds && !matches!((head_key, t.pat0_anchor_key), (Some(site), Some(pattern)) if site != pattern)
}

/// The arguments Luau evaluated into registers before the inlined body, as
/// declarations right before the window `stmts[i..i + hit.consume]`: a temp
/// for an argument the body reads more than once (`local t = E`, bound as
/// that argument), or the fresh local of a parameter the body writes
/// (`local L = E`, bound as that parameter). They are taken into the call,
/// `E` moving into its argument, while they come in parameter order, before
/// any argument the body evaluates where it reads it (`Unified::first_moved`),
/// and are read nowhere else. A written parameter left without one stands
/// for the caller local SSA coalesced it into: dead after the window (the
/// callee-local check) and captured by no closure, as private as the
/// parameter was. A taken temp that is the result local, alive after the
/// window, becomes the call's declaration (`local t = f(E)`): an assigned
/// result, or the argument a discard target's call hands back. `None`
/// refuses the site.
fn absorb_arguments(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    mut hit: Hit,
    last_occ: &mut Liveness,
    is_func_body_top: bool,
) -> Option<Hit> {
    let end = i + hit.consume;
    let result: Option<RcLocal> = if hit.assign {
        hit.results.first().cloned()
    } else if hit.results.is_empty() && hit.host.is_none() {
        t.returns_parameter
            .as_ref()
            .and_then(|param| t.param_order.iter().position(|p| p == param))
            .and_then(|at| hit.args.get(at))
            .and_then(RValue::as_local)
            .cloned()
    } else {
        None
    };
    let reads = |value: &RValue, local: &RcLocal| value.any_local_read(&mut |read| read == local);
    let mut taken: Vec<(usize, RcLocal, RValue)> = Vec::new();
    let mut declared = None;
    let mut start = i;
    // A host statement holds the call itself; its arguments stay as found,
    // unless it holds the call first of all it evaluates
    // (`Hit::placeholder`).
    let into_host = hit.placeholder.is_some();
    let mut below = if hit.host.is_some() && !into_host { 0 } else { hit.first_moved.unwrap_or(usize::MAX) };
    let host_reads = |local: &RcLocal| hit.host.as_ref().is_some_and(|host| count_local_reads(std::slice::from_ref(host), local) > 0);
    while let Some(k) = (0..start).rev().find(|&k| !is_match_trivia(&stmts[k])) {
        let Statement::Assign(declaration) = &stmts[k] else { break };
        let ([LValue::Local(local)], [init]) = (declaration.left.as_slice(), declaration.right.as_slice()) else { break };
        if !declaration.prefix || declaration.parallel || declaration.compound || matches!(init, RValue::Closure(_)) {
            break;
        }
        let Some(at) = hit.args.iter().position(|arg| matches!(arg, RValue::Local(read) if read == local)) else { break };
        if declared_by_source(local, init, t.param_order.get(at)) {
            break;
        }
        let read_elsewhere = hit.args.iter().enumerate().any(|(other, arg)| other != at && reads(arg, local))
            || taken.iter().any(|(_, _, later)| reads(later, local))
            || host_reads(local);
        // Never the whole body of a function in one call.
        if at >= below || read_elsewhere || (is_func_body_top && k == 0 && end == stmts.len()) {
            break;
        }
        let alive = tail_has_live(last_occ, stmts, i, end, &FxHashSet::from_iter([local.clone()]));
        if alive {
            if result.as_ref() != Some(local) {
                break;
            }
            declared = Some(local.clone());
        }
        taken.push((at, local.clone(), init.clone()));
        below = at;
        start = k;
    }
    // A store's base Luau evaluated before the stored value, into a temp
    // right before the arguments' (`local m = A; local t = E; m.k = f(t)`
    // is `A.k = f(E)`): read nowhere but there, with a constant key. Luau
    // evaluates a base that is not one of the function's locals before the
    // value, where the temp stood.
    if into_host
        && let Some(k) = (0..start).rev().find(|&k| !is_match_trivia(&stmts[k]))
        && let Statement::Assign(declaration) = &stmts[k]
        && let ([LValue::Local(base)], [address]) = (declaration.left.as_slice(), declaration.right.as_slice())
        && declaration.prefix
        && !declaration.parallel
        && !declaration.compound
        && !matches!(address, RValue::Local(_) | RValue::Closure(_))
        && !declared_by_source(base, address, None)
        && !(is_func_body_top && k == 0 && end == stmts.len())
        && let Some(Statement::Assign(store)) = &hit.host
        && let [LValue::Index(index)] = store.left.as_slice()
        && matches!((index.left.as_ref(), index.right.as_ref()), (RValue::Local(read), RValue::Literal(_)) if read == base)
        && count_local_reads(std::slice::from_ref(hit.host.as_ref().unwrap()), base) == 1
        && !hit.args.iter().any(|arg| reads(arg, base))
        && !taken.iter().any(|(_, _, init)| reads(init, base))
        && !tail_has_live(last_occ, stmts, i, end, &FxHashSet::from_iter([base.clone()]))
    {
        let address = crate::untruncated(address.clone());
        if let Some(Statement::Assign(store)) = &mut hit.host
            && let [LValue::Index(index)] = store.left.as_mut_slice()
        {
            *index.left = address;
        }
        start = k;
    }
    // An assigned result whose temp went with the rest and is read nowhere
    // after: the call is for its effects only.
    let result_taken = result.as_ref().is_some_and(|result| taken.iter().any(|(_, local, _)| local == result));
    let taken_at: Vec<usize> = taken.iter().map(|(at, ..)| *at).collect();
    for (at, _, init) in taken {
        hit.args[at] = crate::untruncated(init);
    }
    for &at in hit.written.iter().filter(|at| !taken_at.contains(at)) {
        let Some(RValue::Local(local)) = hit.args.get(at) else { return refused("written_parameter_unbound") };
        if !t.captures.uncaptured(local) || hit.args.iter().enumerate().any(|(other, arg)| other != at && reads(arg, local)) {
            return refused("written_parameter_unbound");
        }
    }
    match declared {
        Some(local) => {
            hit.results = vec![local];
            hit.assign = false;
        }
        None if hit.assign && result_taken => {
            hit.results.clear();
            hit.assign = false;
        }
        None => {}
    }
    hit.absorbed = i - start;
    Some(hit)
}

/// A declaration `local L = init` the source wrote, which stays declared
/// (D4): a service or module handle, which the SSA inliner keeps declared
/// as well, or a local with a debug name other than the helper parameter it
/// is the argument of (Luau names the register of an argument it evaluates
/// for an inlined call after the parameter), or an inferred conditional
/// result.
fn declared_by_source(local: &RcLocal, init: &RValue, param: Option<&RcLocal>) -> bool {
    if crate::inline_temps::is_service_or_require_handle(init) {
        return true;
    }
    if !local.preserve_binding() {
        return false;
    }
    let parameter = param.and_then(|param| param.0.lock().source_name().map(str::to_owned));
    let local = local.0.lock();
    local.4.conditional_result || local.2.iter().any(|binding| parameter.as_deref() != Some(binding.name.as_str()))
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
    last_occ: &mut Liveness,
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
    let u = try_unify_site_any(t, &canon_recurse(canon_top(&window, true), true), current_func)?;
    let mut dead = |set: &FxHashSet<RcLocal>| !tail_has_live(last_occ, stmts, i, end, set);
    let complete = u.result.as_ref() == Some(&result)
        && !u.callee_locals.contains(&result)
        && !u.callee_locals.contains(&flag)
        && !block_reads_local(&window, &result)
        && dead(&u.callee_locals)
        && dead(&FxHashSet::from_iter([flag]));
    complete.then(|| Hit::call(t, end - i, u, vec![result]))
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
    last_occ: &mut Liveness,
    current_func: Option<FnPtr>,
    // `stmts[i..d]`, shared by the plain forms.
    site: &SitePrefix,
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
    // The declaration counts as the store it stands for: canon reads no flag
    // of an assignment.
    if canon_top_len_of(stmts[i..=d].iter(), true) != t.pat.len() || block_has_return(prefix) {
        return None;
    }
    // A lone store is its own canon: unify its first statement, the store,
    // before copying it (`head_refused`; an earlier one was the caller's).
    let lone = i == d;
    if lone && !may_specialize(t) && unify_assignment(t, &t.ctx(), &t.pat[0], decl, false, &mut Bindings::default()).is_err() {
        return None;
    }
    let u = site.unify_store(t, Statement::Assign(Assign { prefix: false, ..decl.clone() }), current_func)?;
    let complete = u.result.as_ref() == Some(r)
        && !u.callee_locals.contains(r)
        && !block_reads_local(prefix, r)
        && !tail_has_live(last_occ, stmts, i, d + 1, &u.callee_locals);
    complete.then(|| Hit::call(t, d + 1 - i, u, vec![r.clone()]))
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
    last_occ: &mut Liveness,
    // The site local the prefix alone binds the returned local to, where
    // [`prefix_may_unify`] tells: `Some(None)` when it binds none.
    hint: Option<&Option<RcLocal>>,
    // `stmts[i..d]`, shared by the plain forms.
    site: &SitePrefix,
) -> Option<Hit> {
    let prefix = &stmts[i..d];
    if t.falls_off || prefix.is_empty() || block_has_return(prefix) {
        return None;
    }
    let Some(Statement::Return(ret)) = t.pat.last() else { return None };
    if !matches!(ret.values.as_slice(), [RValue::Local(_)]) {
        return None;
    }
    let declared = prefix.iter().filter_map(|statement| match statement {
        Statement::Assign(assign) if assign.prefix => Some(assign.left.iter()),
        _ => None,
    }).flatten().filter_map(|left| left.as_local());
    // What may write a candidate after the prefix defines it: the closures
    // the prefix makes, and the statements after it, where the tail index
    // shows the candidate at all. Each read once a candidate needs it.
    let mut closure_written: Option<FxHashSet<RcLocal>> = None;
    let mut written_after: Option<FxHashSet<RcLocal>> = None;
    for local in declared {
        if hint.is_some_and(|hint| hint.as_ref() != Some(local)) {
            continue;
        }
        let in_closures = closure_written.get_or_insert_with(|| {
            let mut written = FxHashSet::default();
            closure_writes(prefix, &mut written);
            written
        });
        if in_closures.contains(local) {
            continue;
        }
        let read_later = tail_has_live(last_occ, stmts, i, d, &FxHashSet::from_iter([local.clone()]));
        if read_later
            && written_after
                .get_or_insert_with(|| {
                    let mut written = FxHashSet::default();
                    collect_written(&stmts[d..], &mut written);
                    written
                })
                .contains(local)
        {
            continue;
        }
        let result = RcLocal::default();
        let store: Statement = Assign::new(vec![result.clone().into()], vec![RValue::Local(local.clone())]).into();
        if canon_top_len_of(prefix.iter().chain(std::iter::once(&store)), true) != t.pat.len() {
            continue;
        }
        let Some(u) = site.unify_store(t, store, current_func) else {
            continue;
        };
        let mut others = u.callee_locals.clone();
        others.remove(local);
        if u.result.as_ref() != Some(&result) || tail_has_live(last_occ, stmts, i, d, &others) {
            continue;
        }
        let results = if read_later { vec![local.clone()] } else { Vec::new() };
        return Some(Hit::call(t, d - i, u, results));
    }
    None
}

/// Own-local result (Luau's move elision): a value helper some leaves of
/// which return its own prefix local `L` (`local v = E; if v then return v
/// end; ...; return x`). Luau computes `L` in the result's register, so such
/// a leaf has nothing to store, and the site is the prefix declaring the
/// result itself, then the value branch storing into it on the other paths:
/// `local R = E'; if not R then ... R = x' end`. Unified with `L` bound to
/// `R`, each `return L` leaf standing for its block without it
/// ([`Bindings::elide`]); rebuilt as `local R = f(args)`. No closure of the
/// window may capture `R`, which lives on after it as the helper's `L`
/// does not.
fn match_own_local_value(
    stmts: &[Statement],
    i: usize,
    d: usize,
    t: &Target,
    current_func: Option<FnPtr>,
    is_func_body_top: bool,
    last_occ: &mut Liveness,
) -> Option<Hit> {
    if narrow_prefix_only() || t.falls_off || !t.returns.is_empty() || t.prefix_len == 0 {
        return None;
    }
    let Some(Statement::If(_)) = t.pat.last() else { return None };
    let mut leaves = Vec::new();
    value_leaves(&t.pat, &mut leaves);
    // The prefix's own local a leaf returns, and where it is declared.
    let (k, own) = t.pat[..t.prefix_len].iter().enumerate().find_map(|(k, statement)| match statement {
        Statement::Assign(assign) if assign.prefix && assign.left.len() == 1 => match &assign.left[0] {
            LValue::Local(local) if leaves.iter().any(|leaf| matches!(leaf, RValue::Local(l) if l == local)) => Some((k, local.clone())),
            _ => None,
        },
        _ => None,
    })?;
    let at = nth_effective_index(stmts, i, k)?;
    let Statement::Assign(declaration) = &stmts[at] else { return None };
    let (true, [LValue::Local(r)]) = (declaration.prefix, declaration.left.as_slice()) else { return None };
    let prefix = &stmts[i..d];
    if at >= d || block_has_return(prefix) {
        return None;
    }
    let kc = t.pat.len();
    let max_w = raw_width_for_effective(stmts, d, t.pat_raw_len + 1);
    let mut site: Option<Site> = None;
    let mut ambiguous = false;
    for w in kc.saturating_sub(t.prefix_len).max(1)..=max_w {
        if is_func_body_top && i == 0 && d + w == stmts.len() {
            continue;
        }
        let region = &stmts[d..d + w];
        if canon_top_len_of(prefix.iter().chain(region), true) != kc || block_has_return(region) {
            continue;
        }
        let mut union: Vec<Statement> = Vec::with_capacity(prefix.len() + w);
        union.extend_from_slice(prefix);
        union.extend_from_slice(region);
        if !charge_window(t, &union) {
            break;
        }
        let cwin = canon_recurse(canon_top(&union, true), true);
        let seed = Bindings { result: Some(r.clone()), elide: Some(own.clone()), ..Bindings::default() };
        let Some(mut u) = try_unify_seeded(t, &cwin, current_func, seed) else { continue };
        // `R` is the helper's `L`, the call's result now, alive after it.
        if u.result.as_ref() != Some(r) || !u.callee_locals.remove(r) || closures_capture_any(&stmts[i..d + w], std::slice::from_ref(r)) {
            continue;
        }
        if !tail_has_live(last_occ, stmts, i, d + w, &u.callee_locals) {
            record_site(&mut site, &mut ambiguous, w, &u, None);
        }
    }
    if ambiguous {
        return None;
    }
    let site = site?;
    Some(Hit { mode: "site_own_local", ..Hit::call(t, (d - i) + site.width, site.unified, vec![r.clone()]) })
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
    last_occ: &mut Liveness,
    // `stmts[i..d]`, shared by the plain forms.
    site: &SitePrefix,
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
    let nil_store: Statement = Assign::new(vec![result.clone().into()], vec![RValue::Literal(Literal::Nil)]).into();
    if canon_top_len_of(prefix.iter().chain(std::iter::once(&nil_store)), true) != t.pat.len() {
        return None;
    }
    let mut host = stmts[d].clone();
    let mut found = None;
    let mut replaced = 0;
    // The call runs the helper's statements after every value `S` evaluated
    // before its place, which the prefix ran before: none of those may see a
    // difference. A local the prefix writes, a cell a call in it may write,
    // and a global or field that code may change once the prefix runs any
    // (`dispatch = new; dispatch(f())` is not `dispatch(helper())`). A method
    // lookup is no effect, as everywhere in evaluation order.
    // Each read once a value needs it.
    let prefix_writes = std::cell::OnceCell::new();
    let prefix_runs_code = std::cell::OnceCell::new();
    let writes = || {
        prefix_writes.get_or_init(|| {
            let mut written = FxHashSet::default();
            collect_written(prefix, &mut written);
            written
        })
    };
    let runs_code = || *prefix_runs_code.get_or_init(|| may_run_code(prefix));
    let function = current_func.map(|function| function as usize);
    let register = |local: &RcLocal| t.captures.register_of(local, function);
    let changed_by_prefix = |read: &Earlier| match read {
        Earlier::Value(RValue::Literal(_)) => false,
        Earlier::Value(value @ RValue::Local(local)) => {
            writes().contains(local) || (runs_code() && !t.captures.stable_at(value, function))
        }
        Earlier::Value(value) => runs_code() && !t.captures.constant_import(value),
    };
    visit_leading_values(&mut host, &register, &mut |value, evaluated_before, spread| {
        if value_kind(value) != root || evaluated_before.iter().any(&changed_by_prefix) {
            return false;
        }
        // A local of the function's registers is read when the operation
        // using it runs, after its other operands (`cache + hook()`): where
        // one of them may change it, the call, run first, gives another value.
        if let RValue::Local(local) = value
            && register(local)
            && crate::evaluation_order::region_late_read_conflict(&stmts[d..=d], local, &t.captures.may_change(local))
        {
            return false;
        }
        // Where every result is taken, the call must give as many as the
        // value it replaces: a helper returning `(find(...))` is one value,
        // the inlined `find(...)` in `local a, b = ...` two.
        if spread.takes_all(value) != (spread != Spread::One && is_multiple(pattern_value)) {
            return false;
        }
        let store = Assign::new(vec![result.clone().into()], vec![value.clone()]).into();
        let Some(u) = site.unify_store(t, store, current_func) else {
            return false;
        };
        if u.result.as_ref() != Some(&result) || u.callee_locals.contains(&result) {
            return false;
        }
        // A site keeping one result where all are taken keeps it so: the
        // copy gave one value, and a helper not proven single-valued
        // (`return table.clone(t)` on another path) may give more.
        let wrap = spread != Spread::One
            && !t.single_valued
            && matches!(value, RValue::Select(Select::Call(_) | Select::MethodCall(_)));
        replaced = value_nodes(value).saturating_sub(u.args.iter().map(value_nodes).sum());
        *value = hosted_call(t, u.args.clone(), wrap);
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
    Some(Hit { host: Some(host), partial: Some(replaced), ..Hit::call(t, d + 1 - i, u, Vec::new()) })
}

/// [`Target::hosted`]: the `V` of a `return V` pattern, when it pins its
/// copies down: not a parameter handed back as it is (any value would
/// match it), with no parameter the body writes (a copy writes a local of
/// its own), and no smaller than the expression helpers' floor
/// (`expr_deinline::NODE_COUNT_FLOOR`), a function literal counted with
/// its body ([`value_nodes`]).
fn hosted_pattern(t: &Target) -> Option<RValue> {
    if t.kind != TKind::Value || t.loop_exit_at.is_some() || !t.returns.is_empty() || !t.written_params.is_empty() {
        return None;
    }
    let [Statement::Return(ret)] = t.pat.as_slice() else { return None };
    let [value] = ret.values.as_slice() else { return None };
    if matches!(value, RValue::Local(local) if t.params.contains(local))
        || matches!(value, RValue::VarArg(_) | RValue::Select(Select::VarArg(_)))
    {
        return None;
    }
    (value_nodes(value) >= crate::expr_deinline::NODE_COUNT_FLOOR).then(|| value.clone())
}

/// The nodes of `value`, a function literal's body included: the size of
/// the code a call stands for.
fn value_nodes(value: &RValue) -> usize {
    match value {
        RValue::Closure(closure) => 1 + closure.function.0.lock().body.0.iter().map(dbg_stmt_node_count).sum::<usize>(),
        _ => {
            let mut nodes = 1;
            value.visit_rvalues(&mut |child| {
                nodes += value_nodes(child);
                true
            });
            nodes
        }
    }
}

/// Expression mode: the copy of a value helper whose whole body is `return
/// V` ([`Target::hosted`]) is `V` itself, evaluated in place in the
/// statement at `anchor` (`state.Lifetime = jit(state.Lifetime)`,
/// `print(toCF(keys[3]).Position)`, `self.a = bindSelf(self, self.a)`,
/// `onCancel(finalize(reject))`). Nothing moves but the arguments, which
/// the call evaluates before the body as Luau did (`finish_unified`'s
/// hoist proof). A copy Lua evaluates first in its statement
/// ([`visit_leading_values`]), after nothing a moved temp could change,
/// may also take in the temps Luau made for its arguments and a store's
/// address right before the statement ([`absorb_arguments`]); one anywhere
/// else keeps them. A store of the whole value into a local the caller has
/// waits for the assignment phase, as [`match_assigned_value`] does. The
/// call must save what the expression helpers' floor asks
/// (`expr_deinline::NET_SAVING_FLOOR`).
fn match_hosted_value(
    stmts: &[Statement],
    i: usize,
    anchor: usize,
    t: &Target,
    canon_cache: &mut CanonCache,
    current_func: Option<FnPtr>,
) -> Option<Hit> {
    let pattern = t.hosted.as_ref()?;
    let statement = stmts.get(anchor)?;
    if canon_cache.kinds(statement) & kind_bit(pattern) == 0 {
        return None;
    }
    let kind = value_kind(pattern);
    // No copy to build where no value of the statement has the pattern's
    // shape (the walk does not enter function bodies, as the visits below
    // do not).
    if !holds_shape(statement, &|value| value_kind(value) == kind && unify_returned_value(&t.ctx(), pattern, value, &mut Bindings::default()).is_ok()) {
        return None;
    }
    let function = current_func.map(|function| function as usize);
    let register = |local: &RcLocal| t.captures.register_of(local, function);
    let unchanged = |value: &Earlier| {
        let Earlier::Value(value) = value;
        t.captures.stable_at(value, function) || t.captures.unchanged_by_calls(value)
    };
    // The whole value stored into a local the caller has (`r = V`).
    let whole_store = |host: &Statement| match host {
        Statement::Assign(assign)
            if !assign.prefix && assign.right.len() == 1 && assign.left.iter().all(|left| matches!(left, LValue::Local(_))) =>
        {
            Some(&assign.right[0] as *const RValue)
        }
        _ => None,
    };
    let placeholder = RcLocal::default();
    let try_value = |value: &RValue, spread: Spread, whole: Option<*const RValue>| -> Option<(Unified, bool, usize)> {
        if value_kind(value) != kind || (!t.assigns && whole == Some(value as *const RValue)) {
            return None;
        }
        if !charge_unify(t, &[]) || unify_returned_value(&t.ctx(), pattern, value, &mut Bindings::default()).is_err() {
            return None;
        }
        let wrap = hosted_spread(t, pattern, value, spread)?;
        let result = RcLocal::default();
        let window = [Statement::Assign(Assign::new(vec![LValue::Local(result.clone())], vec![value.clone()]))];
        let u = try_unify_site(t, &window, current_func)?;
        let argument_nodes: usize = u.args.iter().map(value_nodes).sum();
        let nodes = value_nodes(value);
        if u.result.as_ref() != Some(&result)
            || !u.callee_locals.is_empty()
            || nodes < 1 + argument_nodes + crate::expr_deinline::NET_SAVING_FLOOR
        {
            return None;
        }
        Some((u, wrap, nodes - argument_nodes))
    };
    // First the copy Lua evaluates first, which may take in the temps
    // before the statement.
    let mut found: Option<(Unified, bool, usize)> = None;
    let mut host = statement.clone();
    let whole = whole_store(&host);
    let (mut leading, mut absorbs) = (false, false);
    visit_leading_values(&mut host, &register, &mut |value, evaluated_before, spread| {
        let Some(matched) = try_value(value, spread, whole) else { return false };
        leading = true;
        absorbs = evaluated_before.iter().all(unchanged);
        found = Some(matched);
        *value = RValue::Local(placeholder.clone());
        true
    });
    if found.is_none() {
        host = statement.clone();
        let whole = whole_store(&host);
        visit_value_slots(&mut host, &mut |value, spread| {
            let Some((u, wrap, nodes)) = try_value(value, spread, whole) else { return false };
            *value = hosted_call(t, u.args.clone(), wrap);
            found = Some((u, wrap, nodes));
            true
        });
    }
    let (u, wrap, nodes) = found?;
    // Where a temp moved in could change what the statement read before
    // the copy, the call takes the arguments as they stand.
    if leading && !absorbs {
        fill_placeholder(&mut host, &placeholder, hosted_call(t, u.args.clone(), wrap));
    }
    let placeholder = (leading && absorbs).then_some((placeholder, wrap));
    let mode = if leading { "site_hosted_leading" } else { "site_hosted" };
    Some(Hit { host: Some(host), placeholder, mode, partial: Some(nodes), ..Hit::call(t, anchor + 1 - i, u, Vec::new()) })
}

/// The uniform-cell return: a helper every leaf of which returns the outer
/// local `cell` (`return ignoreList`), matched as the body its call runs
/// for no result (`hit`, a discard target's), and the statement right
/// after the copy reading `cell` first of all it evaluates, after only
/// values no call can change (`FindPartOnRayWithIgnoreList(ray,
/// ignoreList)`): that read gets the value the call returns, the one the
/// copy left in `cell`. Rebuilt as that statement with the call there
/// (`...(ray, getIgnoreList())`), the copy's body run after those values;
/// before one that may see a change, the copy stays the call statement it
/// is (`refill(a, 3); print(tag, list[1])`). A
/// register Luau reads only when an operation runs, after a value that may
/// change it, would see another value: refused. Where all of the results
/// are taken, only a `single_valued` helper's call stands for the read.
fn host_returned_cell(stmts: &[Statement], i: usize, mut hit: Hit, t: &Target, current_func: Option<FnPtr>) -> Result<Hit, Hit> {
    let Some(cell) = t.returns_cell.as_ref() else { return Err(hit) };
    // A plain call statement, the copy ending where the window does.
    if hit.host.is_some() || hit.tail_ret.is_some() || !hit.results.is_empty() || hit.assign {
        return Err(hit);
    }
    let Some(at) = nth_effective_index(stmts, i + hit.consume, 0) else { return Err(hit) };
    let function = current_func.map(|function| function as usize);
    if t.captures.register_of(cell, function)
        && crate::evaluation_order::region_late_read_conflict(&stmts[at..=at], cell, &t.captures.may_change(cell))
    {
        return Err(hit);
    }
    let register = |local: &RcLocal| t.captures.register_of(local, function);
    let unchanged = |value: &Earlier| {
        let Earlier::Value(value) = value;
        t.captures.stable_at(value, function) || t.captures.unchanged_by_calls(value)
    };
    let placeholder = RcLocal::default();
    let mut host = stmts[at].clone();
    let mut taken: Option<bool> = None;
    visit_leading_values(&mut host, &register, &mut |value, evaluated_before, spread| {
        if !matches!(value, RValue::Local(read) if read == cell) {
            return false;
        }
        if spread == Spread::One || t.single_valued {
            taken = Some(evaluated_before.iter().all(unchanged));
            *value = RValue::Local(placeholder.clone());
        }
        // The first read of the cell decides: a later one ran after it.
        true
    });
    // The copy's body moves past everything the statement evaluates before
    // the read: none of it may see a change the body makes. Otherwise the
    // copy is the plain call statement, before the statement as it stands.
    if taken != Some(true) {
        return Err(hit);
    }
    hit.placeholder = Some((placeholder, false));
    hit.consume = at + 1 - i;
    hit.host = Some(host);
    hit.partial = Some(1);
    hit.mode = "site_returned_cell";
    Ok(hit)
}

/// Whether some value `statement` evaluates (its own, function bodies
/// aside) passes `shaped`: a cheap gate before any copy is built.
fn holds_shape(statement: &Statement, shaped: &dyn Fn(&RValue) -> bool) -> bool {
    fn walk(value: &RValue, shaped: &dyn Fn(&RValue) -> bool) -> bool {
        shaped(value) || !value.visit_rvalues(&mut |child| !walk(child, shaped))
    }
    match statement {
        Statement::Assign(_)
        | Statement::Call(_)
        | Statement::MethodCall(_)
        | Statement::Return(_)
        | Statement::If(_)
        | Statement::While(_)
        | Statement::NumericFor(_)
        | Statement::GenericFor(_) => !visit_stmt_rvalues(statement, &mut |value| !walk(value, shaped)),
        _ => false,
    }
}

/// Whether a call of `t` can stand where its copy `site` stood, taking
/// `spread` of its results, and whether it then needs `(...)` to keep one.
/// Luau inlines a call taking all of its results only when the helper is
/// `single_valued`: a copy there is a copy only then. A helper returning a
/// call gives all of that call's results, so it never is: a copy of it
/// takes one result (`(V)`, or an operand) or as many as a store needs.
/// One returning another value gives one.
fn hosted_spread(t: &Target, pattern: &RValue, site: &RValue, spread: Spread) -> Option<bool> {
    match (is_multiple(pattern), site) {
        (true, RValue::Call(_) | RValue::MethodCall(_)) => (spread != Spread::Values).then_some(false),
        (true, RValue::Select(Select::Call(_) | Select::MethodCall(_))) => Some(spread != Spread::One),
        (true, _) => None,
        // A call the helper truncates stands for a site call only where one
        // of its values is taken.
        (false, RValue::Call(_) | RValue::MethodCall(_)) if matches!(pattern, RValue::Select(_)) => {
            (spread == Spread::One).then_some(false)
        }
        (false, _) => (spread == Spread::One || t.single_valued).then_some(false),
    }
}

/// The rebuilt call of a hosted copy, `(f(args))` where it must keep one
/// result.
fn hosted_call(t: &Target, args: Vec<RValue>, wrap: bool) -> RValue {
    let mut call = Call::new(RValue::Local(t.f_local.clone()), args).reconstructed(crate::call_origins::Kind::StatementDeinline);
    call.one_result = t.single_valued;
    if wrap { RValue::Select(Select::Call(call)) } else { RValue::Call(call) }
}

/// A rebuilt call where a prefix stands (`f().x`, `f():m()`, `f()()`) as
/// the one-result select the lifter writes there, which prints without
/// parentheses: a call takes one value there either way.
fn select_prefix_calls(statement: &mut Statement) {
    fn selected(prefix: &mut RValue) {
        if matches!(prefix, RValue::Call(call) if call.rebuilt.is_some()) {
            let RValue::Call(call) = std::mem::replace(prefix, RValue::Literal(Literal::Nil)) else { unreachable!() };
            *prefix = RValue::Select(Select::Call(call));
        }
    }
    fn walk(value: &mut RValue) {
        match value {
            RValue::Index(index) => selected(&mut index.left),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => selected(&mut call.value),
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => selected(&mut call.value),
            _ => {}
        }
        value.visit_rvalues_mut(&mut |child| {
            walk(child);
            true
        });
    }
    match statement {
        Statement::Call(call) => selected(&mut call.value),
        Statement::MethodCall(call) => selected(&mut call.value),
        Statement::Assign(assign) => {
            for left in &mut assign.left {
                if let LValue::Index(index) = left {
                    selected(&mut index.left);
                }
            }
        }
        _ => {}
    }
    visit_stmt_rvalues_mut(statement, &mut |value| {
        walk(value);
        true
    });
}

/// Puts `value` in place of the read of `placeholder` in `statement`.
fn fill_placeholder(statement: &mut Statement, placeholder: &RcLocal, value: RValue) {
    fn fill(slot: &mut RValue, placeholder: &RcLocal, value: &mut Option<RValue>) -> bool {
        if matches!(slot, RValue::Local(local) if local == placeholder) {
            *slot = value.take().expect("one placeholder");
            return true;
        }
        let mut done = false;
        slot.visit_rvalues_mut(&mut |child| {
            done = fill(child, placeholder, value);
            !done
        });
        done
    }
    let mut value = Some(value);
    visit_stmt_rvalues_mut(statement, &mut |slot| !fill(slot, placeholder, &mut value));
    debug_assert!(value.is_none(), "the placeholder was filled");
}

/// Every value Lua evaluates in `statement`, outermost first, offered to
/// `visit` with how many results its place takes, until `visit` takes one.
/// Function bodies are not entered: they run where they are called, and
/// are scanned as functions of their own.
fn visit_value_slots(statement: &mut Statement, visit: &mut impl FnMut(&mut RValue, Spread) -> bool) -> bool {
    fn value(slot: &mut RValue, spread: Spread, visit: &mut impl FnMut(&mut RValue, Spread) -> bool) -> bool {
        if visit(slot, spread) {
            return true;
        }
        match slot {
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                value(&mut call.value, Spread::One, visit) || list(&mut call.arguments, Spread::Values, visit)
            }
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                value(&mut call.value, Spread::One, visit) || list(&mut call.arguments, Spread::Values, visit)
            }
            RValue::Table(table) => {
                let last = table.0.len();
                table.0.iter_mut().enumerate().any(|(at, (key, item))| {
                    let spread = if key.is_none() && at + 1 == last { Spread::Values } else { Spread::One };
                    key.as_mut().is_some_and(|key| value(key, Spread::One, visit)) || value(item, spread, visit)
                })
            }
            RValue::Closure(_) => false,
            other => {
                let mut taken = false;
                other.visit_rvalues_mut(&mut |child| {
                    taken = value(child, Spread::One, visit);
                    !taken
                });
                taken
            }
        }
    }
    fn list(values: &mut [RValue], last: Spread, visit: &mut impl FnMut(&mut RValue, Spread) -> bool) -> bool {
        let count = values.len();
        values.iter_mut().enumerate().any(|(at, slot)| value(slot, if at + 1 == count { last } else { Spread::One }, visit))
    }
    match statement {
        Statement::Assign(assign) if !assign.parallel => {
            let last = if assign.left.len() > assign.right.len() { Spread::Store } else { Spread::One };
            assign.left.iter_mut().any(|left| match left {
                LValue::Index(index) => value(&mut index.left, Spread::One, visit) || value(&mut index.right, Spread::One, visit),
                _ => false,
            }) || list(&mut assign.right, last, visit)
        }
        Statement::Call(call) => value(&mut call.value, Spread::One, visit) || list(&mut call.arguments, Spread::Values, visit),
        Statement::MethodCall(call) => {
            value(&mut call.value, Spread::One, visit) || list(&mut call.arguments, Spread::Values, visit)
        }
        Statement::Return(ret) => list(&mut ret.values, Spread::Values, visit),
        Statement::If(branch) => value(&mut branch.condition, Spread::One, visit),
        Statement::While(node) => value(&mut node.condition, Spread::One, visit),
        Statement::NumericFor(node) => {
            value(&mut node.initial, Spread::One, visit)
                || value(&mut node.limit, Spread::One, visit)
                || value(&mut node.step, Spread::One, visit)
        }
        Statement::GenericFor(node) => list(&mut node.right, Spread::Store, visit),
        _ => false,
    }
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

/// One bit per kind of value, equal for values of an equal [`value_kind`].
fn kind_bit(value: &RValue) -> u32 {
    1 << match value {
        RValue::Call(_) | RValue::Select(Select::Call(_)) => 0,
        RValue::MethodCall(_) | RValue::Select(Select::MethodCall(_)) => 1,
        RValue::Binary(_) => 2,
        RValue::Unary(_) => 3,
        RValue::Index(_) => 4,
        RValue::Table(_) => 5,
        RValue::Closure(_) => 6,
        RValue::Local(_) => 7,
        RValue::Literal(_) => 8,
        RValue::Global(_) => 9,
        RValue::IfExpression(_) => 10,
        _ => 11,
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
    last_occ: &mut Liveness,
) -> Option<Hit> {
    // Every window here opens with the callee prefix, an `if` of which keeps
    // its condition through canon (the prefix returns nothing).
    if head_refused(t, &stmts[i..])
        || (!may_specialize(t) && (leading_condition_refused(t, &stmts[i..]) || leading_prefix_refused(t, &stmts[i..])))
    {
        return None;
    }
    let p = t.prefix_len; // effective callee-prefix statement count (>= 1)
    // P1: the interposed init-less `local RESULT` decl is the p-th EFFECTIVE
    // statement at/after i — `i + p` (the old fixed offset) would land on an
    // `Empty` the structurer left between the prefix and the
    // decl, making `result_decl` bail and silently killing chained AtPrefix
    // reconstruction. Count only non-trivia statements instead.
    let d = nth_effective_index(stmts, i, p)?;
    let Some(r) = result_decl(&stmts[d]) else {
        // The next three forms unify the pattern against the site's prefix
        // and one plain store: a prefix that does not unify alone (from no
        // bindings, as they start) refuses them all, read once.
        let Some(site) = prefix_may_unify(t, &stmts[i..d]) else {
            return match_own_local_value(stmts, i, d, t, current_func, is_func_body_top, last_occ);
        };
        // The site local the prefix binds the returned local to, if known.
        let returned_hint = site.bindings.as_ref().and_then(|bindings| {
            let Some(Statement::Return(ret)) = t.pat.last() else { return None };
            let [RValue::Local(local)] = ret.values.as_slice() else { return None };
            Some(bindings.locals.get(local).cloned())
        });
        return match_declared_value(stmts, i, d, t, is_func_body_top, last_occ, current_func, &site)
            .or_else(|| match_embedded_value(stmts, i, d, t, current_func, is_func_body_top, last_occ, &site))
            .or_else(|| match_returned_local(stmts, i, d, t, current_func, last_occ, returned_hint.as_ref(), &site))
            .or_else(|| match_own_local_value(stmts, i, d, t, current_func, is_func_body_top, last_occ));
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
        if let Some(u) = try_unify_declared_result(t, &cwin, current_func) {
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
    let site = site?;
    // Absolute span from i: prefix + any interposed trivia + the RESULT decl
    // (at d) + the w-statement region. `(d - i)` counts the prefix and trivia
    // so the splice removes the interposed trivia along with the window.
    Some(Hit::call(t, (d - i) + 1 + site.width, site.unified, vec![r]))
}

/// Whether the site's prefix `prefix` (the statements before the value of
/// an `AtPrefix` target) may unify with the pattern's, and the bindings it
/// does so with: a necessary condition of [`match_declared_value`],
/// [`match_embedded_value`] and [`match_returned_local`], which unify
/// `prefix` and one plain store after it from no bindings, the prefix
/// first. No bindings known (every form tried) for a target whose
/// specializations unify instead ([`may_specialize`]), and for a narrow
/// prefix, as before; `None` refuses the three forms.
fn prefix_may_unify<'a>(t: &Target, prefix: &'a [Statement]) -> Option<SitePrefix<'a>> {
    let site = SitePrefix::new(prefix);
    if narrow_prefix(&t.pat) || may_specialize(t) || block_has_return(prefix) {
        return Some(site);
    }
    if !charge_window(t, prefix) {
        return None;
    }
    let mut bindings = Bindings::default();
    let unified = {
        let window = site.window();
        unify_block(t, &t.pat[..t.prefix_len], &window[..window.len() - 1], &mut bindings).is_ok()
    };
    unified.then(|| SitePrefix { bindings: Some(bindings), ..site })
}

/// The prefix of a plain-form site: the statements before the one plain
/// store whose window [`match_declared_value`], [`match_embedded_value`]
/// and [`match_returned_local`] unify, each with stores of its own. Canon's
/// tail rules all read returns, so a return-free prefix canonicalizes alike
/// alone and before a plain store, which is its own canon: the canonical
/// prefix is built once, on first use, with a slot after it that each store
/// takes in turn. Where the pattern's prefix unified with it alone
/// ([`prefix_may_unify`]), it holds those bindings, and a window then
/// unifies exactly when its store unifies with the pattern's last statement
/// from them.
struct SitePrefix<'a> {
    statements: &'a [Statement],
    /// The canonical prefix, then the store slot, once built.
    window: std::cell::RefCell<Option<Vec<Statement>>>,
    /// The nodes of `statements`: a window is charged to the search budget
    /// as the statements it is made of ([`charge_window`]).
    nodes: std::cell::Cell<Option<usize>>,
    bindings: Option<Bindings>,
}

impl<'a> SitePrefix<'a> {
    fn new(statements: &'a [Statement]) -> Self {
        Self { statements, window: Default::default(), nodes: Default::default(), bindings: None }
    }

    /// The canonical prefix and the store slot.
    fn window(&self) -> std::cell::RefMut<'_, Vec<Statement>> {
        std::cell::RefMut::map(self.window.borrow_mut(), |window| {
            window.get_or_insert_with(|| {
                let mut window = canon_recurse(canon_top(self.statements, true), true);
                window.push(Statement::Empty(crate::Empty {}));
                window
            })
        })
    }

    /// [`try_unify_site_any`] of the window this return-free prefix and
    /// `store` make, or from the prefix's bindings where known.
    fn unify_store(&self, t: &Target, store: Statement, current_func: Option<FnPtr>) -> Option<Unified> {
        if let Some(bindings) = &self.bindings {
            if !charge_unify(t, &[]) {
                return None;
            }
            let mut b = bindings.clone();
            unify_stmt(t, t.pat.last()?, &store, &mut b).ok()?;
            let mut window = self.window();
            *window.last_mut()? = store;
            return finish_unified(t, &window, b, current_func);
        }
        let nodes = self.nodes.get().unwrap_or_else(|| {
            let nodes = self.statements.iter().map(dbg_stmt_node_count).sum();
            self.nodes.set(Some(nodes));
            nodes
        });
        if t.search.exhausted() || !t.search.spend(nodes + dbg_stmt_node_count(&store)) {
            return None;
        }
        let mut window = self.window();
        *window.last_mut()? = store;
        try_unify_site_any(t, &window, current_func)
    }
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

#[derive(Clone)]
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
    /// The indices of the written parameters, whose arguments are, for now,
    /// the caller locals standing for them ([`absorb_arguments`]).
    written: Vec<usize>,
    /// The first parameter whose argument may run code and is evaluated
    /// where the body reads it ([`Target::leading`]): every argument the site
    /// evaluated before the body must come before it.
    first_moved: Option<usize>,
}

fn try_unify_site(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>) -> Option<Unified> {
    try_unify_seeded(t, cwin, current_func, Bindings::default())
}

/// [`try_unify_site`] from bindings fixed in advance (the result local, an
/// elided identity parameter: [`match_assigned_value`]).
fn try_unify_seeded(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>, mut b: Bindings) -> Option<Unified> {
    dprof::inc(&dprof::UNIFY_CALLS, 1);
    crate::telemetry::count("unify_calls", 1);
    let _t = dprof::T::new(&dprof::UNIFY_US);
    if unify_block(t, &t.pat, cwin, &mut b).is_err() {
        return None;
    }
    finish_unified(t, cwin, b, current_func)
}

/// Refuse a site whose region unified with a helper, counting why
/// (`--stats-json`).
fn refused<T>(reason: &'static str) -> Option<T> {
    crate::reconstruction_stats::refuse_site(reason);
    None
}

fn finish_unified(
    t: &Target,
    cwin: &[Statement],
    mut b: Bindings,
    // The function the site is in: its registers read alike across calls.
    current_func: Option<FnPtr>,
) -> Option<Unified> {
    // A specialization variant passes its constant, where nothing the
    // specialization kept (a function literal's capture) read the argument
    // the site passed instead.
    if let Some((param, truth)) = &t.inferred {
        if b.params.contains_key(param) {
            return refused("constant_parameter_still_read");
        }
        b.params.insert(param.clone(), RValue::Literal(truth.literal()));
    }
    let mut args = Vec::with_capacity(t.param_order.len());
    let mut written = Vec::new();
    for (idx, p) in t.param_order.iter().enumerate() {
        if t.written_params.contains(p) {
            // Written param: matched as a callee local, the site's own `L`.
            // Its argument is `L` until `absorb_arguments` finds the copy
            // `local L = ARG` that initialised it, or proves `L` a caller
            // local the parameter could stand for. Never `nil`-supplied.
            let Some(local) = b.locals.get(p) else {
                return refused("written_parameter_unbound");
            };
            written.push(idx);
            args.push(RValue::Local(local.clone()));
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
            None => return refused("parameter_unbound"),
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
    if t.free_cells().iter().any(|local| {
        t.captures.register_of(local, current_func.map(|function| function as usize))
            && crate::evaluation_order::region_late_read_conflict(cwin, local, &t.captures.may_change(local))
    }) {
        return refused("late_read_conflict");
    }
    let mut region_writes: FxHashSet<RcLocal> = FxHashSet::default();
    collect_written(cwin, &mut region_writes);
    // An assigned result is written only by the leaves, the last effect on
    // their paths, so every read of it in the region, an argument's
    // included, sees the value it had at the call; unless a closure writes
    // it meanwhile (the terminal-write exception).
    if b.assigned && let Some(result) = &b.result {
        if !result_writes_are_terminal(cwin, result) {
            return refused("result_written_before_its_leaf");
        }
        let read = block_reads_local(cwin, result) || args.iter().any(|a| reads_local(a, result));
        if read && t.captures.closure_written(result) {
            return refused("result_written_by_closure");
        }
        // A closure of the region holding the result's cell would see the
        // leaf's store, where the helper's closure holds its own parameter.
        if args.iter().any(|a| reads_local(a, result)) && closures_capture_any(cwin, std::slice::from_ref(result)) {
            return refused("result_captured_in_region");
        }
        region_writes.remove(result);
    }
    // Other arguments may run code when the body reads their parameters
    // first, in parameter order (`Target::leading`): they ran right before
    // the inlined body, after every argument the site evaluated before it
    // (`absorb_arguments`).
    let mut unstable: Vec<(&RcLocal, bool)> = Vec::new();
    let mut first_moved = None;
    for (idx, a) in args.iter().enumerate() {
        // A written parameter's local is the region's to write.
        if written.contains(&idx) {
            continue;
        }
        if !t.captures.stable_at(a, current_func.map(|function| function as usize)) {
            unstable.push((&t.param_order[idx], matches!(a, RValue::Local(_))));
            first_moved.get_or_insert(idx);
        }
        for r in a.values_read() {
            if region_writes.contains(r) {
                return refused("argument_reads_region_write");
            }
        }
    }
    if !t.leading().admits(unstable) {
        return refused("unstable_argument");
    }
    // Each returned local is declared by the pattern, so the site's matching
    // declaration bound it. The caller's local outlives the region.
    let Some(returned) = t.returns.iter().map(|l| b.locals.get(l).cloned()).collect::<Option<Vec<_>>>() else {
        return refused("returned_local_unbound");
    };
    let mut callee_locals: FxHashSet<RcLocal> = b.locals.into_values().collect();
    for l in &returned {
        callee_locals.remove(l);
    }
    let mut inferred = None;
    if let Some((param, _)) = &t.inferred {
        // A `nil` stays written: Luau's inliner weighs a constant argument,
        // not one left out, and the copy has another shape only for one.
        if !specialization_is_honest(t, cwin, &args) {
            return refused("specialization_no_larger_than_call");
        }
        inferred = Some(param.clone());
    }
    Some(Unified {
        // Every target is non-variadic, so a trailing call's extra results
        // fill no parameter the body reads: `helper(f())` needs no `(f())`,
        // whether the site or a written parameter's copy adjusted it.
        args: args.into_iter().map(crate::untruncated).collect(),
        result: b.result,
        callee_locals,
        returned,
        inferred,
        written,
        first_moved,
    })
}

/// Whether every write of the result `r` in the canonical region `stmts` is
/// a leaf store `r = X`: the region's last statement, or the last of an arm
/// of the `if` ending it, recursively. Nothing runs after such a store on
/// its path, and nothing else may write `r` (a closure of the region
/// included).
fn result_writes_are_terminal(stmts: &[Statement], r: &RcLocal) -> bool {
    let last = stmts.iter().rposition(|s| !is_match_trivia(s));
    stmts.iter().enumerate().all(|(k, statement)| {
        let mut closure_writes = FxHashSet::default();
        match statement {
            Statement::Assign(assign) if Some(k) == last && is_plain_local_write(assign, r) => {
                collect_written_in_closures(&assign.right[0], &mut closure_writes);
                !closure_writes.contains(r)
            }
            Statement::If(branch) if Some(k) == last => {
                collect_written_in_closures(&branch.condition, &mut closure_writes);
                !closure_writes.contains(r)
                    && result_writes_are_terminal(&branch.then_block.lock().0, r)
                    && result_writes_are_terminal(&branch.else_block.lock().0, r)
            }
            other => !block_writes_local(std::slice::from_ref(other), r),
        }
    })
}

/// Exact Tier-A match first; when a parameter controls a branch, fall back to a
/// verified partial evaluation.  The fallback never trusts the partial match:
/// it only uses it to seed arguments, specializes a deep copy of the recovered
/// definition, then requires a full structural unification against the site.
fn try_unify_site_any(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>) -> Option<Unified> {
    if !charge_unify(t, cwin) {
        return None;
    }
    try_unify_site(t, cwin, current_func).or_else(|| try_unify_specialized_site(t, cwin, current_func))
}

/// [`try_unify_site_any`] of a region storing into a result its site
/// declared without a value: a `return nil` leaf of the pattern may stand
/// for an empty block there ([`Bindings::elide_nil`]).
fn try_unify_declared_result(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>) -> Option<Unified> {
    if !charge_unify(t, cwin) {
        return None;
    }
    try_unify_seeded(t, cwin, current_func, Bindings { elide_nil: true, ..Bindings::default() })
        .or_else(|| try_unify_specialized_site(t, cwin, current_func))
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
    finish_unified(t, cwin, bindings, current_func)
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

/// Whether no window opening with `stmts` can match `t`, read off its first
/// statement alone, before any window is built. Canon keeps a leading
/// assignment or call as it stands, and so do the rewrites of a window's
/// returns and `continue`s, so the exact unification of every window here
/// starts by unifying that statement with the pattern's first, from no
/// bindings (`try_unify_site`). Where that fails, only a specialized body
/// (`may_specialize`) or a continuation proof (`cps_loop_return`) could
/// still match.
fn head_refused(t: &Target, stmts: &[Statement]) -> bool {
    !t.cps_loop_return && !may_specialize(t) && leading_statement_refused(t, stmts)
}

/// Whether the first statement of `stmts`, an assignment or a call, which
/// heads every window opening there, fails to unify with the pattern's
/// first statement from no bindings, and so from any bindings
/// (`head_refused`).
fn leading_statement_refused(t: &Target, stmts: &[Statement]) -> bool {
    match stmts.iter().find(|statement| !is_match_trivia(statement)) {
        Some(head @ (Statement::Assign(_) | Statement::Call(_) | Statement::MethodCall(_))) => {
            unify_stmt(t, &t.pat[0], head, &mut Bindings::default()).is_err()
        }
        _ => false,
    }
}

/// Whether the plain statements opening `stmts` (assignments with values,
/// calls) fail to unify, one for one and from no bindings, with the
/// statements opening `t`'s prefix. Canon keeps such statements as they are
/// and in place (it only drops trivia), so every window of a prefixed
/// matcher, which unifies its prefix first and from no stricter bindings
/// than these, is refused with them. Stops at the first other statement.
fn leading_prefix_refused(t: &Target, stmts: &[Statement]) -> bool {
    let mut b = Bindings::default();
    let site = stmts.iter().filter(|statement| !is_match_trivia(statement));
    for (pattern, statement) in t.pat[..t.prefix_len].iter().zip(site) {
        let plain = match statement {
            Statement::Assign(assign) => !assign.right.is_empty(),
            Statement::Call(_) | Statement::MethodCall(_) => true,
            _ => false,
        };
        if !plain {
            return false;
        }
        if unify_stmt(t, pattern, statement, &mut b).is_err() {
            return true;
        }
    }
    false
}

/// Whether [`try_unify_specialized_site`] can match anything: a window
/// shorter than the pattern is worth building only then.
fn may_specialize(t: &Target) -> bool {
    (t.specializable && !t.params.is_empty()) || (1..=MAX_TRUTH_PARAMS).contains(&t.truth_params.len())
}

fn try_unify_specialized_site(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>) -> Option<Unified> {
    try_seeded_specialization(t, cwin, current_func).or_else(|| try_inferred_constant(t, cwin, current_func))
}

fn try_seeded_specialization(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>) -> Option<Unified> {
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
    let unified = finish_unified(t, cwin, verified, current_func)?;
    if !specialization_is_honest(t, cwin, &unified.args) {
        return refused("specialization_no_larger_than_call");
    }
    Some(unified)
}

/// Honesty refusal for a specialized match (Tier B, [`try_seeded_specialization`],
/// [`try_inferred_constant`]): the rebuilt call claims the helper ran with
/// constants its copy folded away. Exact either way, but worth claiming only
/// where the copy is larger than the call standing for it, an import path
/// (`table.clone`) counting as the one load it is. A copy no larger keeps its
/// code: `table.clone(t)` never becomes `Copy(t, false)`, not even inside
/// the copy of another helper (`DeepCopy`'s `table.clone(tbl)`).
fn specialization_is_honest(t: &Target, cwin: &[Statement], args: &[RValue]) -> bool {
    fn nodes(value: &RValue) -> usize {
        if is_import_path(value) {
            return 1;
        }
        if let RValue::Closure(closure) = value {
            return 1 + closure.function.0.lock().body.0.iter().map(statement_nodes).sum::<usize>();
        }
        let mut count = 1;
        value.visit_rvalues(&mut |child| {
            count += nodes(child);
            true
        });
        count
    }
    fn statement_nodes(statement: &Statement) -> usize {
        let mut count = 1;
        visit_stmt_rvalues(statement, &mut |value| {
            count += nodes(value);
            true
        });
        let blocks: Vec<&Arc<Mutex<Block>>> = match statement {
            Statement::If(branch) => vec![&branch.then_block, &branch.else_block],
            Statement::While(node) => vec![&node.block],
            Statement::Repeat(node) => vec![&node.block],
            Statement::NumericFor(node) => vec![&node.block],
            Statement::GenericFor(node) => vec![&node.block],
            _ => Vec::new(),
        };
        count + blocks.into_iter().map(|block| block.lock().0.iter().map(statement_nodes).sum::<usize>()).sum::<usize>()
    }
    let window: usize = cwin.iter().map(statement_nodes).sum();
    // `f(args)` as a statement, or the value of the store a value window
    // ends with: the statement, the call, its callee and its arguments.
    let call = 2 + usize::from(t.kind == TKind::Value) + args.iter().map(nodes).sum::<usize>();
    window > call
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
fn try_inferred_constant(t: &Target, cwin: &[Statement], current_func: Option<FnPtr>) -> Option<Unified> {
    if t.truth_params.is_empty() || t.truth_params.len() > MAX_TRUTH_PARAMS {
        return None;
    }
    for (index, param) in t.truth_params.iter().enumerate() {
        let optional = t.optional_params.contains(param);
        let truthy = try_truth(t, cwin, current_func, index, InferredTruth::True);
        let preferred = if optional { InferredTruth::Nil } else { InferredTruth::False };
        let other = if optional { InferredTruth::False } else { InferredTruth::Nil };
        let falsy = try_truth(t, cwin, current_func, index, preferred)
            .or_else(|| try_truth(t, cwin, current_func, index, other));
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
    // A read the specialization kept (a function literal's capture) holds
    // the argument the site passed, which the constant is not.
    if bindings.params.contains_key(param) {
        return refused("constant_parameter_still_read");
    }
    bindings.params.insert(param.clone(), RValue::Literal(truth.literal()));
    let mut unified = finish_unified(t, cwin, bindings, current_func)?;
    unified.inferred = Some(param.clone());
    // A `nil` supplied last is an argument left out.
    if truth == InferredTruth::Nil
        && matches!(unified.args.last(), Some(RValue::Literal(Literal::Nil)))
        && t.param_order.get(unified.args.len() - 1) == Some(param)
    {
        unified.args.pop();
    }
    if !specialization_is_honest(t, cwin, &unified.args) {
        return refused("specialization_no_larger_than_call");
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
            // A function literal holds the value it captures.
            RValue::Closure(closure) => {
                for upvalue in &closure.upvalues {
                    let (Upvalue::Copy(local) | Upvalue::Ref(local)) = upvalue;
                    if params.contains(local) {
                        found.valued.insert(local.clone());
                    }
                }
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
fn any_structural_target(body: &Block) -> bool {
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
    if let Some((lowered, returned)) = straight_tuple_return(body, parameters) {
        return (std::borrow::Cow::Owned(lowered), returned);
    }
    (std::borrow::Cow::Borrowed(body), Vec::new())
}

/// A helper ending with one `return e1, e2, ...` of two or more values, not
/// all its own locals ([`local_tuple_return`] takes those):
/// `local unit = cross.Unit; return unit, v:Cross(unit).Unit`. Luau inlines
/// `local a, b = f(x)` as the body with the values evaluated into the
/// caller's locals in order (`compileExprListTemp`): a value that is a local
/// of the body's top level shares its result's register, another one is
/// declared into a fresh result right there. Returns the body with those
/// declarations in place of the `return`, and the results in order. A local
/// result must be returned once and captured by no closure; a value before
/// the last is cut to one, and the last must be one.
fn straight_tuple_return(body: &[Statement], parameters: &[RcLocal]) -> Option<(Vec<Statement>, Vec<RcLocal>)> {
    let (Statement::Return(ret), rest) = body.split_last()? else { return None };
    let (last, before) = ret.values.split_last()?;
    // The last value gives one result: not a call, unless cut to one or a
    // rebuilt call of a helper returning one (`return look,
    // horizontalUnit(v)` once that copy is a call again).
    let one = is_scalar_return_value(last)
        || matches!(last, RValue::Select(Select::Call(_) | Select::MethodCall(_)))
        || matches!(last, RValue::Call(call) if call.one_result);
    if before.is_empty() || block_has_return(rest) || !before.iter().all(is_truncatable_return_value) || !one {
        return None;
    }
    let declared = |local: &RcLocal| {
        !parameters.contains(local)
            && rest.iter().any(|statement| {
                matches!(statement, Statement::Assign(a)
                    if a.prefix && a.left.iter().any(|l| matches!(l, LValue::Local(x) if x == local)))
            })
    };
    let mut lowered = rest.to_vec();
    let mut results: Vec<RcLocal> = Vec::with_capacity(ret.values.len());
    for value in &ret.values {
        // A shared result is never stored into, so later values may read it.
        let own = match value {
            RValue::Local(local)
                if declared(local) && !results.contains(local) && !closures_capture_any(rest, std::slice::from_ref(local)) =>
            {
                Some(local.clone())
            }
            _ => None,
        };
        match own {
            Some(local) => results.push(local),
            None => {
                let result = RcLocal::default();
                let mut declaration = Assign::new(vec![LValue::Local(result.clone())], vec![value.clone()]);
                declaration.prefix = true;
                lowered.push(declaration.into());
                results.push(result);
            }
        }
    }
    // All of them the body's own locals: `local_tuple_return`'s shape.
    (lowered.len() > rest.len()).then_some((lowered, results))
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
    single_valued: &FxHashSet<RcLocal>,
    captures: std::rc::Rc<crate::deinline_safety::CaptureSafety>,
    orphans: &[Orphan],
    cache: &mut HelperCache,
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
            deinline_reject!(RejectReason::TargetStillReferenced, f_local, "<binder>");
            continue;
        }
        helper_targets(&f_local, &func, single_valued, &captures, None, &mut targets, cache);
    }
    // Orphans: functions only ever called where Luau inlined them, their
    // dead declarations kept aside ([`Orphan`]).
    for orphan in orphans {
        crate::telemetry::count("candidate_binders", 1);
        helper_targets(&orphan.binder, &orphan.function, single_valued, &captures, Some(orphan.scope), &mut targets, cache);
    }
    targets
}

/// The targets of the helper `func` bound to `f_local`, if it passes the
/// per-helper gates: its pattern, its discard variant and its
/// specialization variants. `orphan` is the scope of an orphan's kept-aside
/// declaration ([`Orphan`]), `None` for a declaration in the tree. What an
/// earlier collection found for the helper stands while its code is as it
/// was ([`HelperCache`]); only the gate reading the module's capture facts,
/// which any rewrite may change, is read anew.
fn helper_targets(
    f_local: &RcLocal,
    func: &Arc<Mutex<Function>>,
    single_valued: &FxHashSet<RcLocal>,
    captures: &std::rc::Rc<crate::deinline_safety::CaptureSafety>,
    orphan: Option<Option<FnPtr>>,
    targets: &mut Vec<Target>,
    cache: &mut HelperCache,
) {
    let g = func.lock();
    let key = (Arc::as_ptr(func), f_local.stable_id());
    let mut found = cache.targets.remove(&key);
    let entry = match cache.helpers.entry(key) {
        std::collections::hash_map::Entry::Occupied(entry)
            if found.is_some() || !matches!(entry.get().analysis, HelperAnalysis::Accepted) => entry.into_mut(),
        entry => {
            let mut functions = vec![Arc::as_ptr(func)];
            functions_within(&g.body.0, &mut functions);
            let analysis = analyze_helper(f_local, func, &g, single_valued, captures);
            let (analysis, fresh) = match analysis {
                Ok(fresh) => (HelperAnalysis::Accepted, Some(fresh)),
                Err(refusal) => (refusal, None),
            };
            // An earlier version alike to the current one adds nothing, and
            // one of a helper refused now stays out: its body became too
            // little a copy to tell from the helpers it calls (a wrapper
            // whose earlier body is another helper's).
            if let Some(earlier) = cache.earlier.get(&key)
                && fresh.as_ref().is_none_or(|fresh| {
                    earlier.len() == fresh.len()
                        && earlier.iter().zip(fresh).all(|(a, b)| crate::factor_common_tails::block_alpha_eq(&a.pat, &b.pat))
                })
            {
                cache.earlier.remove(&key);
            }
            found = fresh;
            let helper = CachedHelper { functions, analysis };
            match entry {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.insert(helper);
                    entry.into_mut()
                }
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(helper),
            }
        }
    };
    match entry.analysis {
        HelperAnalysis::Refused(reason) => deinline_reject!(reason, f_local, g.name.as_deref().unwrap_or("<anon>")),
        // Its code runs a call frame deeper inside the helper, where reading
        // frames gives another answer.
        _ if captures.reads_frames(&g.body.0) => {
            deinline_reject!(RejectReason::UnsafeBody, f_local, g.name.as_deref().unwrap_or("<anon>"))
        }
        HelperAnalysis::RefusedLater(reason) => deinline_reject!(reason, f_local, g.name.as_deref().unwrap_or("<anon>")),
        HelperAnalysis::Accepted => {
            // Each candidate binder is accepted or refused once; its targets
            // (variants, an earlier body) are counted in `accepted_targets`.
            crate::telemetry::count("accepted_helpers", 1);
            crate::call_origins::register_callee(f_local.stable_id(), g.bytecode_proto_id);
            crate::reconstruction_stats::accept_helper(f_local.stable_id());
            let earlier = cache.earlier.remove(&key).unwrap_or_default();
            for mut target in found.unwrap_or_default().into_iter().chain(earlier) {
                // What it read of the earlier capture facts is read anew.
                target.captures = captures.clone();
                target.leading = Default::default();
                target.free_cells = Default::default();
                target.orphan = orphan;
                targets.push(target);
            }
        }
    }
}

/// What collecting found for each helper, by its function and binder, kept
/// while the helper's code is as it was: a rewrite in its body, or in a
/// function within it, drops it ([`HelperCache::forget_rewritten`]). The
/// analysis reads nothing else that changes: the write-once census and the
/// single-valued helpers are read once, and the capture facts only by the
/// frame gate and lazily by the targets, both read anew. The targets of the
/// last collection come back for reuse rather than as copies.
#[derive(Default)]
struct HelperCache {
    helpers: FxHashMap<(FnPtr, u64), CachedHelper>,
    targets: FxHashMap<(FnPtr, u64), Vec<Target>>,
    /// Dual-version patterns: the targets a helper had before the first
    /// rewrite in its body, matched beside its current ones. A rewrite
    /// replaced an inlined copy of another helper by a call there; a copy
    /// of this helper at a site may still hold that code, where the call
    /// did not rebuild (another context, an ambiguous match). Both versions
    /// describe the same function exactly: the rewrite was proven.
    earlier: FxHashMap<(FnPtr, u64), Vec<Target>>,
}

struct CachedHelper {
    /// The helper's function and those within its body.
    functions: Vec<FnPtr>,
    analysis: HelperAnalysis,
}

#[derive(Clone, Copy)]
enum HelperAnalysis {
    /// Refused before the frame gate.
    Refused(RejectReason),
    /// Refused after it.
    RefusedLater(RejectReason),
    Accepted,
}

impl HelperCache {
    /// Forgets each helper whose code a rewrite in `bodies` changed.
    fn forget_rewritten(&mut self, bodies: &FxHashSet<Option<FnPtr>>) {
        if bodies.iter().all(Option::is_none) {
            // Only the chunk's own statements changed, in no helper.
            return;
        }
        self.helpers.retain(|_, helper| !helper.functions.iter().any(|function| bodies.contains(&Some(*function))));
    }

    /// Takes back the targets of the collection before: for the helpers
    /// still known, and as their earlier version for those rewritten first
    /// since (where it differs from what they are now).
    fn keep_targets(&mut self, targets: Vec<Target>) {
        self.targets.clear();
        let mut rewritten: FxHashMap<(FnPtr, u64), Vec<Target>> = FxHashMap::default();
        for mut target in targets {
            let key = (target.func_ptr, target.f_local.stable_id());
            if target.earlier_body {
                self.earlier.entry(key).or_default().push(target);
            } else if self.helpers.contains_key(&key) {
                self.targets.entry(key).or_default().push(target);
            } else if !self.earlier.contains_key(&key) {
                target.earlier_body = true;
                rewritten.entry(key).or_default().push(target);
            }
        }
        // Only a value helper whose whole body is `return V` keeps one: its
        // copies in one statement rebuild one per round ([`match_hosted_value`]),
        // so the later ones meet the rewritten body. Other helpers' copies
        // rebuilt in the round that rewrote the body, inner copies and all,
        // throughout the corpus: keeping theirs cost scans and changed no
        // file.
        for (key, targets) in rewritten {
            if targets.first().is_some_and(|target| target.hosted.is_some()) && !self.earlier.contains_key(&key) {
                self.earlier.insert(key, targets);
            }
        }
    }
}

/// The functions whose bodies `stmts` holds, at any depth.
fn functions_within(stmts: &[Statement], out: &mut Vec<FnPtr>) {
    for statement in stmts {
        statement.traverse_rvalues_ref(&mut |value| {
            if let RValue::Closure(closure) = value {
                out.push(Arc::as_ptr(&closure.function.0));
                functions_within(&closure.function.0.lock().body.0, out);
            }
        });
        match statement {
            Statement::If(branch) => {
                functions_within(&branch.then_block.lock().0, out);
                functions_within(&branch.else_block.lock().0, out);
            }
            Statement::While(node) => functions_within(&node.block.lock().0, out),
            Statement::Repeat(node) => functions_within(&node.block.lock().0, out),
            Statement::NumericFor(node) => functions_within(&node.block.lock().0, out),
            Statement::GenericFor(node) => functions_within(&node.block.lock().0, out),
            _ => {}
        }
    }
}

/// [`helper_targets`]' analysis of the helper `g` (`func`, bound to
/// `f_local`): its targets, or why it is refused. It reads the capture
/// facts for no gate.
fn analyze_helper(
    f_local: &RcLocal,
    func: &Arc<Mutex<Function>>,
    g: &Function,
    single_valued: &FxHashSet<RcLocal>,
    captures: &std::rc::Rc<crate::deinline_safety::CaptureSafety>,
) -> Result<Vec<Target>, HelperAnalysis> {
    let f_local = f_local.clone();
    // P5-A: drop the `g.name.is_none()` gate. `g.name` is only the bytecode
    // debugname — never consumed by emission (the call/marker use `f_local`,
    // line ~1377) nor the formatter; only this gate and a debug-trace string
    // read it. Refusing a name-less closure therefore dropped the `name == 0`
    // subset of the IDENTICAL `local f = function…end` shape for no soundness
    // reason. (Variadic stays refused — see P5-B: `...`→multi-arg arity is
    // unprovable from the inlined body, so it is left for `body_unsafe`-style
    // refusal here.) Every soundness gate downstream is unchanged.
    if g.is_variadic {
        return Err(HelperAnalysis::Refused(RejectReason::Variadic));
    }
    // The frame gate is [`helper_targets`]'.
    if body_unsafe(&g.body.0) {
        return Err(HelperAnalysis::Refused(RejectReason::UnsafeBody));
    }
    let (body, returns) = pattern_body(&g.body.0, &g.parameters);
    let (kind, falls_off) = match classify_returns(&body) {
        Some(classified) => classified,
        None => {
            // multi-return / mixed / bare-vararg leaf / non-terminal value return
            return Err(HelperAnalysis::RefusedLater(RejectReason::UnsupportedReturnShape));
        }
    };
    let shape = crate::deinline_safety::CaptureSafety::new(&g.body);
    if !shape.complete() || shape.nodes() > 2048 {
        return Err(HelperAnalysis::RefusedLater(RejectReason::ShapeBudget));
    }
    let pat = if falls_off { canon(&returning_nil(&body)) } else { canon(&body) };
    if pat.is_empty() {
        return Err(HelperAnalysis::RefusedLater(RejectReason::EmptyPattern));
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
                return Err(HelperAnalysis::RefusedLater(RejectReason::UnsupportedReturnShape));
            }
        }
        // value: every leaf must be a single value-return (the result),
        // or a return from inside a loop (`loop_return_split`).
        TKind::Value => {
            if !value_leaf_shape(&pat) && loop_exit_at.is_none() {
                return Err(HelperAnalysis::RefusedLater(RejectReason::UnsupportedReturnShape));
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
        return Err(HelperAnalysis::RefusedLater(RejectReason::LowAnchorScore));
    }
    // Written params (see `Target::written_params`) are matched as callee
    // locals: the site materialises them as `local L = ARG` copies, or
    // coalesced them into a dead caller local (`absorb_arguments`).
    let mut body_written: FxHashSet<RcLocal> = FxHashSet::default();
    collect_written(&g.body.0, &mut body_written);
    let written_params: Vec<RcLocal> = g
        .parameters
        .iter()
        .filter(|p| body_written.contains(*p))
        .cloned()
        .collect();
    let params: FxHashSet<RcLocal> = g
        .parameters
        .iter()
        .filter(|p| !body_written.contains(*p))
        .cloned()
        .collect();
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
    let (value_anchor, prefix_len) = value_anchor_of(kind, &pat, loop_exit_at);
    // The parameters a value leaf hands back as they are, and the one
    // every leaf does, if any.
    let mut leaves = Vec::new();
    if kind == TKind::Value && loop_exit_at.is_none() {
        value_leaves(&pat, &mut leaves);
    }
    let returned_param = |leaf: &RValue| match leaf {
        RValue::Local(local) if params.contains(local) => Some(local.clone()),
        _ => None,
    };
    let identity_params: Vec<RcLocal> =
        g.parameters.iter().filter(|p| leaves.iter().filter_map(returned_param).any(|l| l == **p)).cloned().collect();
    let returns_parameter = match identity_params.as_slice() {
        [param] if leaves.iter().all(|leaf| returned_param(leaf).as_ref() == Some(param)) => Some(param.clone()),
        _ => None,
    };
    // Called for no result, a value helper runs its discard body.
    let discard = (kind == TKind::Value && loop_exit_at.is_none() && returns.is_empty())
        .then(|| discard_body(&body))
        .flatten()
        .map(|raw| (canon(&raw), raw))
        .filter(|(pattern, _)| {
            !pattern.is_empty() && !block_has_return(pattern) && anchor_score(pattern, &g.parameters) >= 2
        });
    let common = TargetCommon {
        f_local: &f_local,
        func_ptr: Arc::as_ptr(&func),
        parameters: &g.parameters,
        params: &params,
        written_params: &written_params,
        unread: &unread,
        captures: &captures,
    };
    let mut target = common.target(kind, pat, &body);
    target.value_anchor = value_anchor;
    target.prefix_len = prefix_len;
    target.falls_off = falls_off;
    target.cps_loop_return = cps_loop_return;
    target.loop_exit_at = loop_exit_at;
    target.returns = returns;
    target.identity_params = identity_params;
    target.single_valued = single_valued.contains(&f_local);
    target.hosted = hosted_pattern(&target);
    // The outer local every leaf returns, if one.
    let returns_cell = match leaves.split_first() {
        Some((RValue::Local(cell), rest))
            if !g.parameters.contains(cell)
                && !target.locals.contains(cell)
                && rest.iter().all(|leaf| matches!(leaf, RValue::Local(other) if other == cell)) =>
        {
            Some(cell.clone())
        }
        _ => None,
    };
    let discard_target = discard.map(|(pattern, raw)| Target {
        discarded: true,
        returns_parameter,
        returns_cell,
        single_valued: target.single_valued,
        ..common.target(TKind::Void, pattern, &raw)
    });
    let variants = specialization_variants(&target, &common, &body);
    let mut found = vec![target];
    found.extend(discard_target);
    found.extend(variants);
    Ok(found)
}

/// Where a value pattern's result sits at its sites ([`ValueAnchor`]), and
/// the number of statements before its value branch.
fn value_anchor_of(kind: TKind, pat: &[Statement], loop_exit_at: Option<usize>) -> (ValueAnchor, usize) {
    if kind == TKind::Value && pat.len() >= 2 && loop_exit_at.is_none() {
        let k = pat.len() - 1;
        if !narrow_prefix_only() || narrow_prefix(pat) {
            return (ValueAnchor::AtPrefix, k);
        }
    }
    (ValueAnchor::AtResultDecl, 0)
}

/// Whether the prefix of the value pattern `pat` is what the value-site
/// locator took before it had no prefix limit: one to four statements, none
/// a branch. Such a target never tries the result-first form
/// ([`match_value`]) after the prefixed ones; a wider one still does.
fn narrow_prefix(pat: &[Statement]) -> bool {
    const MAX_PREFIX: usize = 4;
    let k = pat.len().saturating_sub(1);
    (1..=MAX_PREFIX).contains(&k)
        && pat[..k].iter().all(|s| matches!(s, Statement::Assign(_) | Statement::Call(_) | Statement::MethodCall(_)))
}

/// `MEDAL_DEINLINE_LEGACY_VALUE` (diagnostic): the value-site locator of
/// before, a prefix of at most four statements without a branch, and no
/// own-local result ([`match_own_local_value`]).
fn narrow_prefix_only() -> bool {
    crate::env_flag!("MEDAL_DEINLINE_LEGACY_VALUE")
}

/// Specialization variants of `t`: a copy Luau made for a constant argument
/// that it folded away may have another shape than the body (`Copy(v,
/// true)` drops the `if not deep` around the rest: `local function DeepCopy
/// ... end; r = DeepCopy(v)`). For each truth parameter ([`Target::truth_params`])
/// and each constant giving another statement count, the specialized body
/// is matched as a target of its own, through every site shape its kind
/// has, the constant passed for the parameter ([`Target::inferred`]). A void
/// copy shorter than its body is the base target's own Tier B already
/// ([`match_void`]); only a longer one gets a variant. Where both constants
/// give one shape, the copy says nothing of the argument: no variant. A
/// variant stands only where the copy is larger than its call
/// ([`specialization_is_honest`]). Built once per helper.
fn specialization_variants(t: &Target, common: &TargetCommon, raw: &[Statement]) -> Vec<Target> {
    if t.truth_params.is_empty()
        || t.truth_params.len() > MAX_TRUTH_PARAMS
        || t.loop_exit_at.is_some()
        || t.cps_loop_return
        || !t.returns.is_empty()
    {
        return Vec::new();
    }
    let mut variants = Vec::new();
    for param in &t.truth_params {
        let falsy = if t.optional_params.contains(param) { InferredTruth::Nil } else { InferredTruth::False };
        let mut shapes: Vec<(InferredTruth, Vec<Statement>)> = Vec::new();
        for truth in [InferredTruth::True, falsy] {
            // `canon` deep-copies every block-bearing statement, avoiding
            // mutation of the recovered function body's shared Arcs.
            let mut specialized = canon(&t.pat);
            specialize_block(&mut specialized, &FxHashMap::from_iter([(param.clone(), RValue::Literal(truth.literal()))]));
            let specialized = canon(&specialized);
            let changed = match t.kind {
                TKind::Value => specialized.len() != t.pat.len() && value_leaf_shape(&specialized),
                TKind::Void => specialized.len() > t.pat.len() && !block_has_return(&specialized),
            };
            if changed && anchor_score(&specialized, &t.param_order) >= 2 {
                shapes.push((truth, specialized));
            }
        }
        if let [(_, a), (_, b)] = shapes.as_slice()
            && crate::factor_common_tails::block_alpha_eq(a, b)
        {
            continue;
        }
        for (truth, pattern) in shapes {
            let (value_anchor, prefix_len) = value_anchor_of(t.kind, &pattern, None);
            let mut variant = common.target(t.kind, pattern, raw);
            variant.value_anchor = value_anchor;
            variant.prefix_len = prefix_len;
            variant.falls_off = t.falls_off;
            variant.single_valued = t.single_valued;
            variant.specializable = false;
            variant.truth_params = Vec::new();
            variant.optional_params = Vec::new();
            variant.inferred = Some((param.clone(), truth));
            variants.push(variant);
        }
    }
    variants
}

/// The facts a helper's targets share, whatever pattern each matches.
struct TargetCommon<'a> {
    f_local: &'a RcLocal,
    func_ptr: FnPtr,
    parameters: &'a [RcLocal],
    params: &'a FxHashSet<RcLocal>,
    written_params: &'a [RcLocal],
    unread: &'a FxHashSet<RcLocal>,
    captures: &'a std::rc::Rc<crate::deinline_safety::CaptureSafety>,
}

impl TargetCommon<'_> {
    /// A target matching `pat`, the canonical form of `raw`, with the facts
    /// that pattern decides; the value-shape fields are left at their
    /// defaults for the caller to set.
    fn target(&self, kind: TKind, pat: Vec<Statement>, raw: &[Statement]) -> Target {
        let params = self.params;
        let specializable = branch_conditions_read_any(&pat, params);
        let (truth_params, optional_params) = truth_tested_params(&pat, params, self.parameters);
        let mut locals: FxHashSet<RcLocal> = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        for p in params {
            locals.remove(p);
        }
        locals.extend(self.written_params.iter().cloned());
        // The window starts at the body: the copies of written parameters
        // before it are taken in afterwards (`absorb_arguments`).
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        let pat_nodes = pat.iter().map(dbg_stmt_node_count).sum();
        // Read now, while no function body is locked by a scan: a pattern's
        // literals may be the very ones a scan holds.
        let private_closures = private_closures(&pat);
        Target {
            f_local: self.f_local.clone(),
            func_ptr: self.func_ptr,
            kind,
            pat_raw_len: raw.len(),
            pat_spine_len: tail_spine_len(raw),
            pat_nodes,
            focused: true,
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: params.clone(),
            locals,
            param_order: self.parameters.to_vec(),
            written_params: self.written_params.to_vec(),
            unread: self.unread.clone(),
            leading: Default::default(),
            free_cells: Default::default(),
            specializable,
            truth_params,
            optional_params,
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures,
            orphan: None,
            earlier_body: false,
            captures: self.captures.clone(),
            search: Default::default(),
        }
    }
}

/// [`Target::private_closures`] of `pattern`: a local assigned one shared
/// function literal (`local function f`, or `local f; f = function` where
/// the literal reads `f`) and nothing else, read only as a callee and by
/// the one capture of its own literal.
fn private_closures(pattern: &[Statement]) -> FxHashSet<RcLocal> {
    // Each local assigned a shared literal, with how many times; and each
    // init-less declaration.
    fn walk(stmts: &[Statement], literals: &mut FxHashMap<RcLocal, usize>, declared: &mut FxHashMap<RcLocal, usize>) {
        for statement in stmts {
            if let Statement::Assign(assign) = statement
                && let [LValue::Local(local)] = assign.left.as_slice()
            {
                match assign.right.as_slice() {
                    [RValue::Closure(closure)] if closure.function.0.lock().closure_constant.is_some() => {
                        *literals.entry(local.clone()).or_default() += 1;
                    }
                    [] if assign.prefix => *declared.entry(local.clone()).or_default() += 1,
                    _ => {}
                }
            }
            match statement {
                Statement::If(branch) => {
                    walk(&branch.then_block.lock().0, literals, declared);
                    walk(&branch.else_block.lock().0, literals, declared);
                }
                Statement::While(node) => walk(&node.block.lock().0, literals, declared),
                Statement::Repeat(node) => walk(&node.block.lock().0, literals, declared),
                Statement::NumericFor(node) => walk(&node.block.lock().0, literals, declared),
                Statement::GenericFor(node) => walk(&node.block.lock().0, literals, declared),
                _ => {}
            }
        }
    }
    let mut found = FxHashSet::default();
    let (mut literals, mut declared) = (FxHashMap::default(), FxHashMap::default());
    walk(pattern, &mut literals, &mut declared);
    if literals.is_empty() {
        return found;
    }
    let mut writes = FxHashMap::default();
    crate::expr_deinline::collect_write_counts(pattern, &mut writes);
    for (binder, assigned) in literals {
        let declarations = declared.get(&binder).copied().unwrap_or(0);
        if assigned != 1 || declarations > 1 || writes.get(&binder).copied() != Some(1 + declarations) {
            continue;
        }
        // Every read a callee, but the one capture its own literal makes.
        let (mut calls, mut captures) = (0, 0);
        count_callee_reads(pattern, &binder, &mut calls, &mut captures);
        if captures <= 1 && count_local_reads(pattern, &binder) == calls + captures {
            found.insert(binder);
        }
    }
    found
}

/// The reads of `local` in `stmts` as the callee of a call, and its
/// captures by function literals, at any depth, function bodies included.
fn count_callee_reads(stmts: &[Statement], local: &RcLocal, calls: &mut usize, captures: &mut usize) {
    fn value(value: &RValue, local: &RcLocal, calls: &mut usize, captures: &mut usize) {
        match value {
            RValue::Call(call) | RValue::Select(Select::Call(call))
                if matches!(call.value.as_ref(), RValue::Local(callee) if callee == local) =>
            {
                *calls += 1;
            }
            RValue::Closure(closure) => {
                *captures += closure.upvalues.iter().filter(|upvalue| matches!(upvalue, Upvalue::Copy(l) | Upvalue::Ref(l) if l == local)).count();
                count_callee_reads(&closure.function.0.lock().body.0, local, calls, captures);
            }
            _ => {}
        }
        value_children(value, local, calls, captures);
    }
    fn value_children(parent: &RValue, local: &RcLocal, calls: &mut usize, captures: &mut usize) {
        if matches!(parent, RValue::Closure(_)) {
            return;
        }
        parent.visit_rvalues(&mut |child| {
            value(child, local, calls, captures);
            true
        });
    }
    for statement in stmts {
        if let Statement::Call(call) = statement
            && matches!(call.value.as_ref(), RValue::Local(callee) if callee == local)
        {
            *calls += 1;
        }
        visit_stmt_rvalues(statement, &mut |v| {
            value(v, local, calls, captures);
            true
        });
        match statement {
            Statement::If(branch) => {
                count_callee_reads(&branch.then_block.lock().0, local, calls, captures);
                count_callee_reads(&branch.else_block.lock().0, local, calls, captures);
            }
            Statement::While(node) => count_callee_reads(&node.block.lock().0, local, calls, captures),
            Statement::Repeat(node) => count_callee_reads(&node.block.lock().0, local, calls, captures),
            Statement::NumericFor(node) => count_callee_reads(&node.block.lock().0, local, calls, captures),
            Statement::GenericFor(node) => count_callee_reads(&node.block.lock().0, local, calls, captures),
            _ => {}
        }
    }
}

/// The values a value pattern's leaves return: its last statement's
/// `return X`, or the leaves of the arms of the `if` ending it.
fn value_leaves(stmts: &[Statement], out: &mut Vec<RValue>) {
    match stmts.iter().rev().find(|statement| !is_match_trivia(statement)) {
        Some(Statement::Return(ret)) => out.extend(ret.values.first().cloned()),
        Some(Statement::If(branch)) => {
            value_leaves(&branch.then_block.lock().0, out);
            value_leaves(&branch.else_block.lock().0, out);
        }
        _ => {}
    }
}

/// The body Luau runs for an inlined call whose value is unused: each
/// `return X` evaluates `X` only for its effects (`compileExprSide`), which
/// is nothing for a local, a global, a constant or a function literal, and
/// the call itself for a call, then leaves the copy. `None` for any other
/// leaf, whose evaluation the copy keeps in a form no statement here spells.
fn discard_body(stmts: &[Statement]) -> Option<Vec<Statement>> {
    let mut out = Vec::with_capacity(stmts.len() + 1);
    for statement in stmts {
        match statement {
            Statement::Return(ret) => {
                match ret.values.as_slice() {
                    [] | [RValue::Local(_) | RValue::Global(_) | RValue::Literal(_) | RValue::Closure(_)] => {}
                    [RValue::Call(call) | RValue::Select(Select::Call(call))] => out.push(Statement::Call(call.clone())),
                    [RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call))] => {
                        out.push(Statement::MethodCall(call.clone()))
                    }
                    _ => return None,
                }
                out.push(Statement::Return(Return::default()));
            }
            Statement::If(branch) => {
                let then_block = discard_body(&branch.then_block.lock().0)?;
                let else_block = discard_body(&branch.else_block.lock().0)?;
                out.push(If::new(branch.condition.clone(), Block(then_block), Block(else_block)).into());
            }
            // A `return` from inside a loop leaves it too: not this shape.
            other if statement_has_return(other) => return None,
            other => out.push(other.clone()),
        }
    }
    Some(out)
}

/// Decide the return shape: `Void` (no value returns), `Value` (single scalar
/// value on every path), or refuse (`None`) for multi-return, mixed void/value,
/// a non-scalar value (call/method/vararg/select — arity unprovable), or a body
/// whose value returns are not all terminal leaves after canonicalization.
/// With the flag `Target::falls_off`: a value function that may also return
/// nothing is matched as [`returning_nil`] of its body.
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
            Statement::Comment(_)
            | Statement::Goto(_)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Break, Closure, Comment, Empty, ForOrigin, ForPrepKind, Function, Global, Index, Local,
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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

        assert!(try_unify_site(&target, &cand, None).is_none());
    }

    #[test]
    fn written_argument_copies_keep_order_dependencies_without_truncation() {
        let p = local("p"); let q = local("q"); let a = local("a"); let b = local("b");
        let mut target = void_target(vec![print_x()], [p.clone(), q.clone()].into_iter().collect());
        target.param_order = vec![p.clone(), q.clone()];
        target.written_params = target.param_order.clone();
        let bindings = Bindings { locals: [(p, a.clone()), (q, b.clone())].into_iter().collect(), ..Default::default() };
        let unified = finish_unified(&target, &[], bindings, None).unwrap();
        let first: RValue = Call::new(global("first"), vec![]).into();
        let last: RValue = Call::new(global("last"), vec![]).into();
        let copy = |local: &RcLocal, init: RValue| assign_local(local, init, true);
        let absorbed = |stmts: Vec<Statement>| {
            let at = stmts.len() - 1;
            absorb_arguments(&stmts, at, &target, Hit::call(&target, 1, unified.clone(), Vec::new()), &mut Liveness::default(), false)
        };
        // In parameter order, both copies go into the call. A non-variadic
        // helper drops a trailing call's extra results itself.
        let hit = absorbed(vec![copy(&a, first.clone()), copy(&b, last.clone()), print_x()]).unwrap();
        assert_eq!(hit.absorbed, 2);
        assert!(hit.args.iter().all(|v| matches!(v, RValue::Call(_))));
        // Out of order, only the nearer one does; the other stays a caller
        // local handed to its parameter, which keeps the evaluation order.
        let hit = absorbed(vec![copy(&b, last.clone()), copy(&a, first.clone()), print_x()]).unwrap();
        assert_eq!(hit.absorbed, 1);
        assert!(matches!(&hit.args[1], RValue::Local(local) if *local == b));
        // A copy initialised from another: the call would read the first
        // parameter's local as the second argument. Refused.
        assert!(absorbed(vec![copy(&a, first), copy(&b, a.clone().into()), print_x()]).is_none());
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
        // The value moves into the one statement reading it.
        assert!(output.contains("print(findItem(list, key))"), "{output}");
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
        // The value moves into the one statement reading it.
        assert!(output.contains("print(findItem(list, key))"), "{output}");

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
        assert!(output.contains("print(classify(list))"), "{output}");

        // for i = 1, #list do if a(i) then return i end end (nothing after)
        let helper_body = vec![numeric(&i, vec![guard(test("a", &i), vec![return_one(local_value(&i))], vec![])])];
        let site = vec![
            declare_r(),
            assign_local(&ok, boolean(true), true),
            numeric(&n, vec![guard(test("a", &n), leave(local_value(&n)), vec![])]),
            use_r(),
        ];
        let output = run(helper_body, site);
        // Falling off its end, the helper may return nothing: its call keeps
        // the local where all of a value's results are taken.
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

        // local function emit(enabled) if enabled then work("a", "b") end end:
        // `false` leaves nothing, which no statement may stand for.
        let (emit, enabled) = (local("emit"), local("enabled"));
        let work_both = || Statement::Call(global_call("work", vec![string("a"), string("b")]));
        let work = |name: &str| Statement::Call(global_call("work", vec![string(name)]));
        let output = run(vec![
            declare(&emit, vec![enabled.clone()], vec![Statement::If(If::new(local_value(&enabled), Block(vec![work_both()]), Block::default()))]),
            print_x(),
            work_both(),
        ]);
        assert!(output.contains("emit(true)") && !output.contains("emit(false)"), "{output}");
        // A copy no larger than the call claiming it (`work("a")` for
        // `emit(true)`) keeps its code.
        let output = run(vec![
            declare(&emit, vec![enabled.clone()], vec![Statement::If(If::new(local_value(&enabled), Block(vec![work("a")]), Block::default()))]),
            print_x(),
            work("a"),
        ]);
        assert!(!output.contains("emit(true)"), "{output}");

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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: true,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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

        let unified = try_unify_site_any(&target, &candidate, None)
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
            try_unify_site_any(&target, &wrong, None).is_none(),
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: true,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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
            try_unify_specialized_site(&target, &candidate, None).is_none(),
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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
            try_unify_site(&target, &candidate, None).is_none(),
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: true,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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

        let unified = try_unify_cps_site(&target, &window, &candidate, &continuation, None)
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
        let structured = try_unify_cps_site(&target, &structured_window, &structured_candidate, &continuation, None)
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
            try_unify_cps_site(&target, &window, &candidate, &wrong_continuation, None).is_none(),
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
            try_unify_site(&t, &cand, None).is_none(),
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
            match_void(&cand, 0, &t, false, true, &[], false, &mut Liveness::default(), &mut canon_cache, None).is_none(),
            "replacing a function's entire body with one call must be refused"
        );
        // Not the whole body (is_func_body_top = false) -> matches.
        assert!(
            match_void(&cand, 0, &t, false, false, &[], false, &mut Liveness::default(), &mut canon_cache, None).is_some(),
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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
            match_value_prefixed(&cand, 0, &t, None, false, &mut Liveness::default()).is_none(),
            "an in-place accumulator with a param-LHS must not de-inline"
        );
    }

    /// F2: a candidate region carrying TWO interposed `Empty`s must still match.
    /// The old raw ceiling `pat_raw_len + 1` capped the window at 4 raw
    /// statements — one short of the 5 needed (3 calls + 2 trivia) — silently
    /// missing the reconstruction; the effective-count ceiling (trivia don't
    /// consume the budget) reaches it.
    #[test]
    fn void_region_with_two_interposed_trivia_matches_f2() {
        let mk = || Statement::Empty(Empty {});
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
        let hit = match_void(&cand, 0, &t, false, false, &[], false, &mut Liveness::default(), &mut canon_cache, None)
            .expect("two interposed trivia must not exceed the effective window ceiling");
        assert_eq!(
            hit.consume, 5,
            "window spans the 3 calls + 2 interior trivia"
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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
        let u = try_unify_site(&t, &cand, None).expect("unused trailing param must not block de-inline");
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
        let u = try_unify_site(&t, &cand, None).expect("interior unused param must not block de-inline");
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
            try_unify_site(&t, &cand, None).is_none(),
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
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

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default())
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

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default())
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
            match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default()).is_none(),
            "a prefix=true result-write leaf must not unify as the result lane (F10a)"
        );
    }

    /// P1 regression: an `Empty` between the callee-prefix statement and the
    /// interposed `local RESULT` decl must NOT break the AtPrefix match. The old
    /// `d = i + p` offset pointed at it (`result_decl` -> None -> bail), silently
    /// killing chained reconstruction; `nth_effective_index` skips the trivia
    /// and still finds the decl, and the `consume` span removes it along with
    /// the window.
    #[test]
    fn value_prefix_trivia_between_prefix_and_result_decl_still_matches() {
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
            Statement::Empty(Empty {}), // left by the structurer
            init_less_decl(&v),
            if_stmt(
                bin(local_value(&k2), BinaryOperation::Equal, number(1.0)),
                vec![assign_local(&v, string("a"), false)],
                vec![assign_local(&v, string("b"), false)],
            ),
            print_x(), // trailing stmt: window isn't whole-body; doesn't read k2
        ];

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default())
            .expect("interposed trivia must not break the AtPrefix match");
        // span = prefix(0) + trivia(1) + decl(2) + region-if(3): removes 4 stmts,
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

        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default())
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
        let hit = match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default())
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
            match_value_prefixed(&candidate, 0, &t, None, false, &mut Liveness::default()).is_none(),
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
            captures: None,
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
            captures: None,
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

    /// [`collapse_use`] in the chunk, with `single_valued`: every local of it
    /// a register that reads the same before and after a call (`stable`), or
    /// nothing known (an exhausted census).
    fn collapse_in_chunk(s: &Statement, v: &RcLocal, call: &RValue, single_valued: &FxHashSet<RcLocal>, stable: bool) -> Option<Statement> {
        let captures = if stable {
            crate::deinline_safety::CaptureSafety::default()
        } else {
            crate::deinline_safety::CaptureSafety::exhausted_for_tests()
        };
        collapse_use(s, v, call, &Collapse { single_valued, captures: &captures, function: None })
    }

    /// F2: an indexed-LHS value collapse is refused when it would reorder the
    /// target prefix relative to the moved-in call; a bare-local LHS still
    /// collapses, and so does an address that reads the same either way.
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
        assert!(collapse_in_chunk(&indexed, &v, &call, &empty, false).is_none());
        // Store mode: an address that reads the same before and after the call
        // (a register base, a constant key) takes the call in.
        match collapse_in_chunk(&indexed, &v, &call, &empty, true).expect("a stable address collapses") {
            Statement::Assign(a) => assert!(matches!(a.right[0], RValue::Call(_)) && matches!(a.left[0], LValue::Index(_))),
            _ => panic!("expected an Assign"),
        }
        // Never into one of several targets: their stores have an order of their own.
        let two = Statement::Assign(Assign {
            node_origin: Default::default(),
            left: vec![LValue::Index(Index::new(local_value(&t), string("field"))), LValue::Local(local("w"))],
            right: vec![local_value(&v)],
            prefix: false,
            parallel: false, compound: false,
        });
        let proven: FxHashSet<RcLocal> = [local("f")].into_iter().collect();
        assert!(collapse_in_chunk(&two, &v, &call, &proven, true).is_none());

        let x = local("x");
        let local_lhs = assign_local(&x, local_value(&v), false);
        match collapse_in_chunk(&local_lhs, &v, &call, &empty, false).expect("local LHS must collapse") {
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
        assert!(collapse_in_chunk(&ret, &v, &call, &unknown, false).is_none(), "return v must NOT collapse an unproven helper");
        assert!(collapse_in_chunk(&ret, &v, &call, &proven, false).is_some(), "return v DOES collapse a single-value helper");

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
        assert!(collapse_in_chunk(&multi_lhs, &v, &call, &unknown, false).is_none(), "multi-LHS a,b = v must NOT collapse an unproven helper");

        // SINGLE-LHS `x = v` and `if v` truncate to one value for any helper.
        let x = local("x");
        let single_lhs = assign_local(&x, local_value(&v), false);
        assert!(collapse_in_chunk(&single_lhs, &v, &call, &unknown, false).is_some(), "single-LHS x = v collapses any helper (truncates)");
        let if_v = if_stmt(local_value(&v), vec![print_x()], vec![]);
        assert!(collapse_in_chunk(&if_v, &v, &call, &unknown, false).is_some(), "if v collapses any helper (single-value condition)");
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

    /// The de-inliner puts no marker statement in the tree (a rebuilt call
    /// carries its attribute), so any comment in a body refuses it, while
    /// `canon_top` drops `Empty` trivia so a pattern stays length-aligned
    /// with its candidates.
    #[test]
    fn comments_refuse_a_body_and_canon_drops_empty_trivia() {
        let padded = vec![print_x(), Statement::Empty(Empty {})];
        assert!(!body_unsafe(&padded));

        let real_comment = vec![
            print_x(),
            Statement::Comment(Comment::new(" a real source comment".to_string())),
        ];
        assert!(body_unsafe(&real_comment));

        assert_eq!(canon_top(&padded, true).len(), 1, "Empty dropped by canon_top");
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
    /// over EVERY sequence of length 0..=4 from a canon-relevant alphabet (Empty
    /// trivia / source comment; plain / void-return / value-return
    /// statements; foldable + several non-foldable guard shapes; an `if` a value
    /// return is distributed into; a 2-value return), for both tail values — ~57k
    /// cases. Computes the real length via `canon_top` directly
    /// (independent of the in-function debug_assert), pinning the non-allocating length
    /// mirror to `canon_top` even for release builds where the debug_assert is gone.
    #[test]
    fn canon_top_len_mirrors_canon_top_exhaustively() {
        let make = |sym: u8| -> Statement {
            match sym {
                0 => Statement::Empty(Empty {}),
                1 => Statement::Comment(Comment::trailing(" note".to_string())),     // NOT trivia
                2 => Statement::Comment(Comment::new(" source".to_string())),        // NOT trivia
                3 => print_x(),                                                      // plain stmt
                4 => void_return(),                                                  // void return
                5 => return_one(number(1.0)),                                        // value return
                6 => if_stmt(global("c"), vec![void_return()], vec![]), // foldable void guard
                7 => if_stmt(global("c"), vec![return_one(number(2.0))], vec![]), // foldable value guard
                8 => if_stmt(global("c"), vec![void_return()], vec![print_x()]),  // else nonempty
                9 => if_stmt(global("c"), vec![print_x(), void_return()], vec![]), // then len 2
                10 => if_stmt(global("c"), vec![print_x()], vec![]),              // then non-return
                // two open ends and a return: takes a following value return (E1)
                11 => if_stmt(global("c"), vec![if_stmt(global("d"), vec![return_one(number(3.0))], vec![])], vec![]),
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
        const ALPHA: u8 = 13;
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
            leading: std::cell::OnceCell::from(crate::evaluation_order::LeadingReads::default()),
            free_cells: std::cell::OnceCell::from(Vec::new()),
            specializable: false,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            identity_params: Vec::new(),
            discarded: false,
            assigns: false,
            returns_parameter: None,
            single_valued: false,
            hosted: None,
            returns_cell: None,
            inferred: None,
            private_closures: Default::default(),
            orphan: None,
            earlier_body: false,
            captures: Default::default(),
            search: Default::default(),
        };
        // site: local L = 7; if q then L = L + 1 end; print(L)
        let q = local("q");
        let l = local("L");
        let body = vec![
            if_stmt(local_value(&q), vec![assign_local(&l, add_one(&l), false)], vec![]),
            print_local(&l),
        ];
        let u = try_unify_site(&t, &canon(&body), None).expect("the written param binds to the site's local");
        assert_eq!(u.args.len(), 2);
        assert!(rvalue_exact_eq(&u.args[0], &local_value(&q)));
        assert!(rvalue_exact_eq(&u.args[1], &local_value(&l)));
        assert!(u.callee_locals.contains(&l), "the copy is a callee temp (must be dead after)");
        let site = |before: Statement| [vec![before], body.clone()].concat();
        let absorbed = |stmts: &[Statement], t: &Target| {
            absorb_arguments(stmts, 1, t, Hit::call(t, 2, u.clone(), Vec::new()), &mut Liveness::default(), false)
        };
        // The copy right before the window goes into the call.
        let hit = absorbed(&site(assign_local(&l, number(7.0), true)), &t).expect("the copy is the argument");
        assert_eq!(hit.absorbed, 1);
        assert!(rvalue_exact_eq(&hit.args[1], &number(7.0)));
        // Without one, `L` is the caller local SSA coalesced the copy into:
        // dead after the window (the callee-local check) and captured by
        // nothing, the parameter's writes are its own. A declaration binding
        // no parameter stays where it is.
        let stray = site(assign_local(&local("other"), number(1.0), true));
        let hit = absorbed(&stray, &t).expect("an uncaptured dead local stands for the parameter");
        assert_eq!(hit.absorbed, 0);
        assert!(rvalue_exact_eq(&hit.args[1], &local_value(&l)));
        // A closure reading `L` could see the parameter's writes: refused.
        let reader = Statement::Call(Call::new(
            RValue::Closure(Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(Function { body: Block(vec![print_local(&l)]), ..Function::default() }))),
                upvalues: vec![Upvalue::Ref(l.clone())],
            }),
            vec![],
        ));
        let captured = Target { captures: std::rc::Rc::new(crate::deinline_safety::CaptureSafety::new(&Block(vec![reader]))), ..t };
        assert!(absorbed(&stray, &captured).is_none());
    }

    /// A target built as `collect_targets` builds one, from a raw body.
    fn helper_target(kind: TKind, body: &[Statement], parameters: &[RcLocal]) -> Target {
        let params: FxHashSet<RcLocal> = parameters.iter().cloned().collect();
        let common = TargetCommon {
            f_local: &local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            parameters,
            params: &params,
            written_params: &[],
            unread: &FxHashSet::default(),
            captures: &Default::default(),
        };
        let mut target = common.target(kind, canon(body), body);
        let mut leaves = Vec::new();
        value_leaves(&target.pat, &mut leaves);
        target.identity_params =
            parameters.iter().filter(|p| leaves.iter().any(|leaf| matches!(leaf, RValue::Local(l) if l == *p))).cloned().collect();
        target
    }

    fn method_call(object: RValue, name: &str, arguments: Vec<RValue>) -> RValue {
        RValue::MethodCall(MethodCall { node_origin: Default::default(), value: Box::new(object), method: name.into(), arguments })
    }

    #[test]
    fn a_discard_body_keeps_only_what_luau_evaluates_for_an_unused_result() {
        let (c, x, log) = (local("c"), local("x"), local("log"));
        let insert = Statement::Call(Call::new(global("insert"), vec![local_value(&log), local_value(&x)]));
        let leaf = |value: RValue| Return::new(vec![value]).into();
        // A local, a global, a constant: nothing; a call: the call.
        let body = vec![
            if_stmt(local_value(&c), vec![leaf(local_value(&x))], vec![]),
            insert.clone(),
            leaf(call1(global("tostring"), local_value(&x))),
        ];
        let discarded = canon(&discard_body(&body).expect("locals and calls have a discard body"));
        let expected = canon(&[
            if_stmt(local_value(&c), vec![void_return()], vec![]),
            insert.clone(),
            Statement::Call(Call::new(global("tostring"), vec![local_value(&x)])),
        ]);
        // Compound statements compare their blocks by identity: compare the trees.
        assert_eq!(format!("{discarded:?}"), format!("{expected:?}"));
        for value in [global("VERSION"), number(1.0), RValue::Literal(Literal::Nil)] {
            assert!(discard_body(&[insert.clone(), leaf(value)]).is_some());
        }
        // An index is still evaluated, for its `__index`: no discard body.
        assert!(discard_body(&[insert.clone(), leaf(field(local_value(&x), "count"))]).is_none());
        // Nor for a `return` from inside a loop, which leaves the loop too.
        let looped = Statement::While(While::new(local_value(&c), Block(vec![leaf(local_value(&x))])));
        assert!(discard_body(&[looped, leaf(local_value(&x))]).is_none());
    }

    #[test]
    fn an_identity_leaf_is_elided_only_through_its_pre_bound_parameter() {
        // better(current, candidate): if candidate.score > current.score then
        // return candidate end; return current
        let (current, candidate) = (local("current"), local("candidate"));
        let score = |l: &RcLocal| field(local_value(l), "score");
        let body = vec![
            if_stmt(bin(score(&candidate), BinaryOperation::GreaterThan, score(&current)), vec![return_one(local_value(&candidate))], vec![]),
            return_one(local_value(&current)),
        ];
        let t = helper_target(TKind::Value, &body, &[current.clone(), candidate.clone()]);
        assert_eq!(t.identity_params, vec![current.clone(), candidate.clone()]);
        // top = better(top, item): `if item.score > top.score then top = item end`.
        let (top, item, other) = (local("top"), local("item"), local("other"));
        let site = canon(&[if_stmt(
            bin(score(&item), BinaryOperation::GreaterThan, score(&top)),
            vec![assign_local(&top, local_value(&item), false)],
            vec![],
        )]);
        let seed = |result: &RcLocal, elide: Option<&RcLocal>| {
            let mut b = Bindings { result: Some(result.clone()), assigned: true, ..Bindings::default() };
            if let Some(param) = elide {
                b.params.insert(param.clone(), local_value(result));
                b.elide = Some(param.clone());
            }
            b
        };
        let u = try_unify_seeded(&t, &site, None, seed(&top, Some(&current))).expect("current stands for top");
        assert!(rvalue_exact_eq(&u.args[0], &local_value(&top)) && rvalue_exact_eq(&u.args[1], &local_value(&item)));
        // No elision without the pre-binding, nor with another parameter or
        // result: an empty block proves nothing on its own.
        assert!(try_unify_seeded(&t, &site, None, seed(&top, None)).is_none());
        assert!(try_unify_seeded(&t, &site, None, seed(&top, Some(&candidate))).is_none());
        assert!(try_unify_seeded(&t, &site, None, seed(&other, Some(&current))).is_none());
    }

    #[test]
    fn an_assigned_result_is_written_only_by_its_leaves() {
        let (r, c, x) = (local("r"), local("c"), local("x"));
        let store = |value: RValue| assign_local(&r, value, false);
        assert!(result_writes_are_terminal(&[if_stmt(local_value(&c), vec![store(number(1.0))], vec![])], &r));
        assert!(result_writes_are_terminal(&[print_local(&x), store(local_value(&x))], &r));
        // A store something reads after, or one before the end of the region.
        assert!(!result_writes_are_terminal(&[store(number(1.0)), print_local(&r)], &r));
        assert!(!result_writes_are_terminal(&[if_stmt(local_value(&c), vec![store(number(1.0)), print_local(&r)], vec![])], &r));
        assert!(!result_writes_are_terminal(&[store(number(1.0)), if_stmt(local_value(&c), vec![store(number(2.0))], vec![])], &r));
    }

    #[test]
    fn an_argument_temp_that_is_the_result_becomes_its_declaration() {
        let (source, t, e, user) = (local("source"), local("t"), local("e"), local("user"));
        let target = helper_target(TKind::Value, &[return_one(method_call(local_value(&source), "Clone", vec![]))], &[source.clone()]);
        let unified = Unified {
            args: vec![local_value(&t)],
            result: Some(t.clone()),
            callee_locals: FxHashSet::default(),
            returned: Vec::new(),
            inferred: None,
            written: Vec::new(),
            first_moved: None,
        };
        let hit = || Hit { assign: true, ..Hit::call(&target, 1, unified.clone(), vec![t.clone()]) };
        let init = call1(global("make"), local_value(&e));
        let window = assign_local(&t, method_call(local_value(&t), "Clone", vec![]), false);
        // `local t = make(e); t = t:Clone(); use(t)` is `local t = f(make(e))`.
        let read_after = vec![assign_local(&t, init.clone(), true), window.clone(), print_local(&t)];
        let declared = absorb_arguments(&read_after, 1, &target, hit(), &mut Liveness::default(), false).unwrap();
        assert!(!declared.assign && declared.results == vec![t.clone()] && declared.absorbed == 1);
        assert!(rvalue_exact_eq(&declared.args[0], &init));
        // Read nowhere after: the call is for its effects only.
        let dead = vec![assign_local(&t, init.clone(), true), window.clone(), print_local(&user)];
        let effects = absorb_arguments(&dead, 1, &target, hit(), &mut Liveness::default(), false).unwrap();
        assert!(!effects.assign && effects.results.is_empty() && effects.absorbed == 1);
        // Not right before the window: the result stays assigned.
        let apart = vec![assign_local(&t, init, true), print_local(&user), window, print_local(&t)];
        let assigned = absorb_arguments(&apart, 2, &target, hit(), &mut Liveness::default(), false).unwrap();
        assert!(assigned.assign && assigned.absorbed == 0 && rvalue_exact_eq(&assigned.args[0], &local_value(&t)));
    }

    #[test]
    fn a_handle_or_a_named_local_the_source_declared_stays_declared() {
        let named = |name: &str, debug: Option<&str>| {
            let local = local(name);
            if let Some(debug) = debug {
                local.0.lock().add_source_binding(crate::SourceBinding {
                    origin: crate::BindingOrigin::DebugLocal { prototype: 0, register: 0, start_pc: 0, end_pc: 9 },
                    name: debug.into(),
                });
            }
            local
        };
        let s = named("s", Some("s"));
        let target = helper_target(TKind::Void, &[print_local(&s)], &[s.clone()]);
        let absorbed = |h: &RcLocal, init: RValue| {
            let unified = Unified {
                args: vec![local_value(h)],
                result: None,
                callee_locals: FxHashSet::default(),
                returned: Vec::new(),
                inferred: None,
                written: Vec::new(),
                first_moved: None,
            };
            let site = vec![assign_local(h, init, true), print_local(h)];
            absorb_arguments(&site, 1, &target, Hit::call(&target, 1, unified, Vec::new()), &mut Liveness::default(), false)
                .unwrap()
                .absorbed
        };
        let service = method_call(global("game"), "GetService", vec![string("Lighting")]);
        let module = call1(global("require"), global("script"));
        let made = call1(global("make"), number(1.0));
        // A service or module handle, as the SSA inliner keeps it (D4).
        assert_eq!(absorbed(&named("h", None), service), 0);
        assert_eq!(absorbed(&named("h", None), module), 0);
        // A local with a debug name of its own.
        assert_eq!(absorbed(&named("h", Some("Lighting")), made.clone()), 0);
        // The register Luau named after the parameter, or a temp with no
        // name: the argument.
        assert_eq!(absorbed(&named("h", Some("s")), made.clone()), 1);
        assert_eq!(absorbed(&named("h", None), made), 1);
    }

    #[test]
    fn continue_ending_a_loop_body_is_a_copys_return() {
        let (c, x) = (local("c"), local("x"));
        let continued = vec![if_stmt(local_value(&c), vec![Statement::Continue(crate::Continue {})], vec![]), print_local(&x)];
        let returning = continues_as_returns(&continued).expect("a continue to rewrite");
        let expected = vec![if_stmt(local_value(&c), vec![void_return()], vec![]), print_local(&x)];
        assert_eq!(format!("{returning:?}"), format!("{expected:?}"));
        // Nothing to rewrite, or another exit: not this shape.
        assert!(continues_as_returns(&[print_local(&x)]).is_none());
        let broken = vec![if_stmt(local_value(&c), vec![Statement::Break(Break {})], vec![Statement::Continue(crate::Continue {})])];
        assert!(continues_as_returns(&broken).is_none());
        // A nested loop's own `continue` stays its own.
        let inner = Statement::While(While::new(local_value(&c), Block(vec![Statement::Continue(crate::Continue {})])));
        assert!(continues_as_returns(&[inner.clone()]).is_none());
        let mixed = continues_as_returns(&[inner.clone(), if_stmt(local_value(&c), vec![Statement::Continue(crate::Continue {})], vec![])]).unwrap();
        assert_eq!(format!("{:?}", mixed[0]), format!("{inner:?}"));
    }

    /// `local name = function(parameters) body end`.
    fn helper_with(name: &RcLocal, parameters: &[RcLocal], body: Vec<Statement>) -> Statement {
        let declaration = helper_decl(name, body);
        if let Statement::Assign(assign) = &declaration
            && let RValue::Closure(closure) = &assign.right[0]
        {
            closure.function.lock().parameters = parameters.to_vec();
        }
        declaration
    }

    /// `if type(v) == "function" then return true end; if type(v) ==
    /// "table" then local m = getmetatable(v); if m and m.call then return
    /// true end end; return false`: `isCallable`'s shape.
    fn is_callable_body(v: &RcLocal, m: &RcLocal) -> Vec<Statement> {
        let is_type = |name: &str| bin(call1(global("type"), local_value(v)), BinaryOperation::Equal, string(name));
        vec![
            if_stmt(is_type("function"), vec![return_one(boolean(true))], vec![]),
            if_stmt(
                is_type("table"),
                vec![
                    assign_local(m, call1(global("getmetatable"), local_value(v)), true),
                    if_stmt(
                        bin(local_value(m), BinaryOperation::And, field(local_value(m), "call")),
                        vec![return_one(boolean(true))],
                        vec![],
                    ),
                ],
                vec![],
            ),
            return_one(boolean(false)),
        ]
    }

    /// The copy Luau inlines of [`is_callable_body`] for `local r =
    /// isCallable(x)`, its first test `first`: `if first then r = true
    /// elseif type(x) == "table" then local m = getmetatable(x); r = m and
    /// m.call and true or false else r = false end`.
    fn is_callable_copy(first: RValue, x: &RcLocal, m: &RcLocal, r: &RcLocal) -> Statement {
        let mut condition = bin(local_value(m), BinaryOperation::And, field(local_value(m), "call"));
        let select = crate::select_value(&mut condition, boolean(true), boolean(false)).unwrap();
        let is_table = bin(call1(global("type"), local_value(x)), BinaryOperation::Equal, string("table"));
        if_stmt(
            first,
            vec![assign_local(r, boolean(true), false)],
            vec![if_stmt(
                is_table,
                vec![assign_local(m, call1(global("getmetatable"), local_value(x)), true), assign_local(r, select, false)],
                vec![assign_local(r, boolean(false), false)],
            )],
        )
    }

    #[test]
    fn a_return_after_an_if_with_two_open_ends_closes_both() {
        let (v, m) = (local("value"), local("metatable"));
        let pattern = canon(&is_callable_body(&v, &m));
        // One `if`, every path ending in a value `return`: a value leaf
        // shape, which the copy's stores unify with.
        assert_eq!(pattern.len(), 1);
        assert!(value_leaf_shape(&pattern), "{pattern:?}");
        assert_eq!(canon_top_len(&is_callable_body(&v, &m), true), 1);
        // An `if` that never returns keeps the `return` after it: its copy
        // keeps the store after it too.
        let plain = vec![if_stmt(global("c"), vec![print_x()], vec![print_x()]), return_one(number(1.0))];
        assert_eq!(canon(&plain).len(), 2);
        assert_eq!(canon_top_len(&plain, true), 2);
    }

    #[test]
    fn a_comparison_before_the_copy_splits_off_as_or() {
        let (helper, v, m) = (local("isCallable"), local("value"), local("metatable"));
        let (x, r, mm) = (local("x"), local("r"), local("mm"));
        let run = |first: RValue| {
            let mut block = Block(vec![
                helper_with(&helper, std::slice::from_ref(&v), is_callable_body(&v, &m)),
                init_less_decl(&r),
                is_callable_copy(first, &x, &mm, &r),
                Statement::Call(global_call("print", vec![local_value(&r)])),
            ]);
            deinline(&mut block);
            block.to_string()
        };
        let is_function = || bin(call1(global("type"), local_value(&x)), BinaryOperation::Equal, string("function"));
        let is_nil = bin(local_value(&x), BinaryOperation::Equal, RValue::Literal(Literal::Nil));
        let output = run(bin(is_nil, BinaryOperation::Or, is_function()));
        assert!(output.contains("print(x == nil or isCallable(x))"), "{output}");
        // A field's value is no boolean: `t.flag or isCallable(x)` would be
        // that value where the copy stores `true`.
        let output = run(bin(field(global("t"), "flag"), BinaryOperation::Or, is_function()));
        assert!(!output.contains("isCallable(x)"), "{output}");
    }

    #[test]
    fn a_copy_returning_from_its_caller_rebuilds_as_a_return() {
        let (helper, v, m) = (local("isCallable"), local("value"), local("metatable"));
        let (x, mm) = (local("x"), local("mm"));
        let caller = local("check");
        let mut site = vec![Statement::Call(global_call("print", vec![string("start")]))];
        site.extend(is_callable_body(&x, &mm));
        let mut block = Block(vec![
            helper_with(&helper, std::slice::from_ref(&v), is_callable_body(&v, &m)),
            helper_with(&caller, std::slice::from_ref(&x), site),
        ]);
        deinline(&mut block);
        let output = block.to_string();
        assert!(output.contains("return isCallable(x)"), "{output}");
    }

    #[test]
    fn a_copy_evaluated_in_place_rebuilds_there() {
        let (helper, player) = (local("getCharacter"), local("player"));
        let character = || bin(local_value(&player), BinaryOperation::And, field(local_value(&player), "Character"));
        let mut block = Block(vec![
            assign_local(&player, global("LocalPlayer"), true),
            helper_with(&helper, &[], vec![return_one(character())]),
            Statement::Call(global_call("print", vec![string("x"), character()])),
        ]);
        deinline(&mut block);
        let output = block.to_string();
        // A last argument takes every result: the helper gives exactly one.
        assert!(output.contains("print(\"x\", getCharacter())"), "{output}");
    }

    #[test]
    fn a_call_taking_all_results_is_no_copy_of_a_helper_returning_a_call() {
        let (helper, s, x) = (local("shout"), local("s"), local("x"));
        let shouted = |of: &RcLocal| RValue::Call(global_call("rep", vec![field(local_value(of), "name"), number(2.0), string("!")]));
        let run = |arguments: Vec<RValue>| {
            let mut block = Block(vec![
                assign_local(&x, global("input"), true),
                helper_with(&helper, std::slice::from_ref(&s), vec![return_one(shouted(&s))]),
                Statement::Call(global_call("print", arguments)),
            ]);
            deinline(&mut block);
            block.to_string()
        };
        // An operand takes one result: Luau inlines the call there.
        let output = run(vec![shouted(&x), number(1.0)]);
        assert!(output.contains("print(shout(x), 1)"), "{output}");
        // A last argument takes all of them: Luau never inlines a helper
        // returning a call there, so that is no copy.
        let output = run(vec![number(1.0), shouted(&x)]);
        assert!(output.contains("print(1, rep(x.name, 2, \"!\"))"), "{output}");
    }

    fn helper_with_params(name: &RcLocal, parameters: Vec<RcLocal>, body: Vec<Statement>) -> Statement {
        let declaration = helper_decl(name, body);
        if let Statement::Assign(assign) = &declaration
            && let RValue::Closure(closure) = &assign.right[0]
        {
            closure.function.lock().parameters = parameters;
        }
        declaration
    }

    fn closure_of(proto: usize, shared: bool, upvalues: Vec<Upvalue>, body: Vec<Statement>) -> RValue {
        RValue::Closure(Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function {
                bytecode_proto_id: Some(proto),
                closure_constant: shared.then_some(0),
                body: Block(body),
                ..Function::default()
            }))),
            upvalues,
        })
    }

    #[test]
    fn a_copy_for_a_constant_with_another_shape_matches_its_specialization() {
        // local function f(t, deep) if not deep then return clone(t) end
        //     local out = copyAll(t); out.deep = true; return out end
        let (f, t, deep, out, v, out2) = (local("f"), local("t"), local("deep"), local("out"), local("v"), local("out2"));
        let body = vec![
            Statement::If(If::new(not_rv(local_value(&deep)), Block(vec![return_one(call1(global("clone"), local_value(&t)))]), Block::default())),
            assign_local(&out, call1(global("copyAll"), local_value(&t)), true),
            Statement::Assign(Assign::new(vec![LValue::Index(crate::Index::new(local_value(&out), string("deep")))], vec![boolean(true)])),
            return_one(local_value(&out)),
        ];
        let mut block = Block(vec![
            helper_with_params(&f, vec![t.clone(), deep.clone()], body),
            assign_local(&v, global("input"), true),
            assign_local(&out2, call1(global("copyAll"), local_value(&v)), true),
            Statement::Assign(Assign::new(vec![LValue::Index(crate::Index::new(local_value(&out2), string("deep")))], vec![boolean(true)])),
            Statement::Call(global_call("print", vec![local_value(&out2)])),
        ]);
        deinline(&mut block);
        let output = block.to_string();
        assert!(output.contains("print(f(v, true))"), "{output}");
    }

    #[test]
    fn a_copy_holding_both_specializations_in_its_arms_is_rebuilt_whole() {
        // local function paint(b, on) if on then b.Auto = true; b.Alpha = 0
        //     else b.Auto = false; b.Alpha = 0.5 end end
        let (paint, b, on, button, n) = (local("paint"), local("b"), local("on"), local("button"), local("n"));
        let arm = |object: &RcLocal, auto: bool, alpha: f64| {
            vec![
                Statement::Assign(Assign::new(vec![LValue::Index(crate::Index::new(local_value(object), string("Auto")))], vec![boolean(auto)])),
                Statement::Assign(Assign::new(vec![LValue::Index(crate::Index::new(local_value(object), string("Alpha")))], vec![number(alpha)])),
            ]
        };
        let body = vec![Statement::If(If::new(local_value(&on), Block(arm(&b, true, 0.0)), Block(arm(&b, false, 0.5))))];
        let condition = bin(local_value(&n), BinaryOperation::GreaterThanOrEqual, number(50.0));
        let mut block = Block(vec![
            helper_with_params(&paint, vec![b.clone(), on.clone()], body),
            assign_local(&button, global("input"), true),
            assign_local(&n, global("count"), true),
            Statement::If(If::new(condition, Block(arm(&button, true, 0.0)), Block(arm(&button, false, 0.5)))),
        ]);
        deinline(&mut block);
        let output = block.to_string();
        assert!(output.contains("paint(button, n >= 50)") && !output.contains("paint(button, true)"), "{output}");
    }

    #[test]
    fn a_constant_is_never_inferred_for_a_parameter_a_literal_still_captures() {
        // local function play(cb, m) if not m then return end; run(function() cb(m) end) end
        // copied under the caller's own `if child then`: only the literal is
        // left, capturing `child`, which `true` is not.
        let (play, cb, m, act, child) = (local("play"), local("cb"), local("m"), local("act"), local("child"));
        let literal = |callback: &RcLocal, model: &RcLocal| {
            closure_of(7, false, vec![Upvalue::Copy(callback.clone()), Upvalue::Copy(model.clone())],
                vec![Statement::Call(Call::new(local_value(callback), vec![local_value(model)]))])
        };
        let body = vec![
            Statement::If(If::new(not_rv(local_value(&m)), Block(vec![void_return()]), Block::default())),
            Statement::Call(global_call("run", vec![literal(&cb, &m)])),
        ];
        let mut block = Block(vec![
            helper_with_params(&play, vec![cb.clone(), m.clone()], body),
            assign_local(&act, global("act"), true),
            assign_local(&child, global("input"), true),
            Statement::If(If::new(local_value(&child), Block(vec![Statement::Call(global_call("run", vec![literal(&act, &child)]))]), Block::default())),
        ]);
        deinline(&mut block);
        let output = block.to_string();
        assert!(!output.contains("play(act, true)"), "{output}");
    }

    #[test]
    fn a_shared_literal_only_ever_called_is_private() {
        // local function deepCopy(t) ... deepCopy(v) ... end; return deepCopy(t)
        let (binder, t, v) = (local("deepCopy"), local("t"), local("v"));
        let body = vec![Statement::Call(Call::new(local_value(&binder), vec![local_value(&v)]))];
        let declared = |escapes: bool| {
            let mut pattern = vec![
                assign_local(&binder, closure_of(3, true, vec![Upvalue::Ref(binder.clone())], body.clone()), true),
                return_one(RValue::Select(crate::Select::Call(Call::new(local_value(&binder), vec![local_value(&t)])))),
            ];
            if escapes {
                pattern.insert(1, Statement::Call(global_call("store", vec![local_value(&binder)])));
            }
            pattern
        };
        assert!(private_closures(&declared(false)).contains(&binder));
        // Passed on, its identity can be seen.
        assert!(!private_closures(&declared(true)).contains(&binder));
    }

    #[test]
    fn a_straight_tuple_declares_its_values_into_results() {
        // local unit = norm(x); return unit, scale(unit)
        let (unit, x) = (local("unit"), local("x"));
        let body = vec![
            assign_local(&unit, call1(global("norm"), local_value(&x)), true),
            Statement::Return(Return::new(vec![local_value(&unit), call1(global("scale"), local_value(&unit))])),
        ];
        // `scale(unit)` takes all its results last: no fixed arity.
        assert!(straight_tuple_return(&body, std::slice::from_ref(&x)).is_none());
        let mut body = body;
        let Statement::Return(ret) = &mut body[1] else { unreachable!() };
        ret.values[1] = field(local_value(&unit), "k");
        let (lowered, results) = straight_tuple_return(&body, std::slice::from_ref(&x)).expect("one value each");
        assert_eq!(results[0], unit);
        assert_eq!(lowered.len(), 2);
        assert!(matches!(&lowered[1], Statement::Assign(a) if a.prefix && a.left[0].as_local() == Some(&results[1])));
        // All the body's own locals: `local_tuple_return`'s shape, not this.
        let other = local("other");
        let own = vec![
            assign_local(&unit, number(1.0), true),
            assign_local(&other, number(2.0), true),
            Statement::Return(Return::new(vec![local_value(&unit), local_value(&other)])),
        ];
        assert!(straight_tuple_return(&own, &[]).is_none());
    }

    #[test]
    fn the_same_leading_return_values_split_off_every_return() {
        let (p, c, a, b) = (local("p"), local("c"), local("a"), local("b"));
        let returning = |first: &RcLocal, last: &RcLocal| Statement::Return(Return::new(vec![local_value(first), local_value(last)]));
        let window = vec![
            Statement::If(If::new(local_value(&c), Block(vec![returning(&p, &a)]), Block::default())),
            returning(&p, &b),
        ];
        let (stripped, before) = returned_before(&window).expect("same leading value");
        assert!(matches!(before.as_slice(), [RValue::Local(l)] if *l == p));
        assert!(matches!(&stripped[1], Statement::Return(r) if r.values.len() == 1));
        let mixed = vec![
            Statement::If(If::new(local_value(&c), Block(vec![returning(&p, &a)]), Block::default())),
            returning(&b, &b),
        ];
        assert!(returned_before(&mixed).is_none());
        assert!(returned_before(&[return_one(local_value(&a))]).is_none());
    }

    #[test]
    fn an_orphan_goes_back_only_where_its_call_is_rebuilt() {
        // local scramble = function(v, key) if key then v = clamp(v, "hi") end; log(v) end
        // kept aside; `run` holds a copy, another body a different shape.
        let (scramble, v, key, x, k) = (local("scramble"), local("v"), local("key"), local("x"), local("k"));
        let body = |value: &RcLocal, flag: &RcLocal| {
            vec![
                Statement::If(If::new(local_value(flag), Block(vec![assign_local(value, Call::new(global("clamp"), vec![local_value(value), string("hi")]).into(), false)]), Block::default())),
                Statement::Call(global_call("log", vec![local_value(value)])),
            ]
        };
        let orphan = |used: bool| {
            let closure = Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(Function {
                    bytecode_proto_id: Some(9),
                    parameters: vec![v.clone(), key.clone()],
                    body: Block(body(&v, &key)),
                    ..Function::default()
                }))),
                upvalues: Vec::new(),
            };
            let mut site = vec![assign_local(&x, global("input"), true), assign_local(&k, global("flag"), true)];
            if used {
                site.extend(body(&x, &k));
            } else {
                site.push(Statement::Call(global_call("log", vec![local_value(&x)])));
            }
            (Block(site), vec![(scramble.clone(), closure)])
        };
        let (mut block, mut aside) = orphan(true);
        deinline_with_orphans(&mut block, &mut aside);
        let output = block.to_string();
        assert!(aside.is_empty() && output.contains("scramble(x, flag)"), "{output}");
        let declared = block.0.iter().position(|s| matches!(s, Statement::Assign(a) if a.prefix && a.left[0].as_local() == Some(&scramble))).expect("declared again");
        let called = block.0.iter().position(|s| count_local_reads(std::slice::from_ref(s), &scramble) > 0).unwrap();
        assert_eq!(declared + 1, called);
        // No copy: the tree stays as it was, the orphan aside.
        let (mut block, mut aside) = orphan(false);
        let before = block.to_string();
        deinline_with_orphans(&mut block, &mut aside);
        assert_eq!(block.to_string(), before);
        assert_eq!(aside.len(), 1);
    }

    #[test]
    fn a_nil_leaf_stands_for_an_empty_arm_of_a_result_declared_without_a_value() {
        // local function f(c) if not c then return nil end; return wrap(c, "x") end
        let (f, c, x, r) = (local("f"), local("c"), local("x"), local("r"));
        let body = vec![
            Statement::If(If::new(not_rv(local_value(&c)), Block(vec![return_one(RValue::Literal(Literal::Nil))]), Block::default())),
            return_one(Call::new(global("wrap"), vec![local_value(&c), string("x")]).into()),
        ];
        let site = |arm: Vec<Statement>| {
            let mut block = Block(vec![
                helper_with_params(&f, vec![c.clone()], body.clone()),
                assign_local(&x, global("input"), true),
                init_less_decl(&r),
                Statement::If(If::new(
                    local_value(&x),
                    Block(vec![assign_local(&r, Call::new(global("wrap"), vec![local_value(&x), string("x")]).into(), false)]),
                    Block(arm),
                )),
                Statement::Call(global_call("print", vec![local_value(&r)])),
            ]);
            deinline(&mut block);
            block.to_string()
        };
        let output = site(Vec::new());
        assert!(output.contains("local r = f(input)"), "{output}");
        // An arm storing something else is no copy.
        let output = site(vec![assign_local(&r, number(1.0), false)]);
        assert!(!output.contains("f(input)"), "{output}");
    }

    #[test]
    fn a_helper_local_returned_on_some_paths_is_the_result_itself() {
        // local function f(p) local v = lookup(p, "a"); [keep(function() return v end)]
        //     if v then return v end; return fallback(p, "b") end
        let (f, p, v, x, r) = (local("f"), local("p"), local("v"), local("x"), local("r"));
        let lookup = |of: &RcLocal| RValue::Call(Call::new(global("lookup"), vec![local_value(of), string("a")]));
        let fallback = |of: &RcLocal| RValue::Call(Call::new(global("fallback"), vec![local_value(of), string("b")]));
        let keep = |of: &RcLocal| Statement::Call(global_call("keep", vec![closure_of(5, false, vec![Upvalue::Ref(of.clone())], Vec::new())]));
        let run = |captured: bool| {
            let mut body = vec![
                assign_local(&v, lookup(&p), true),
                Statement::If(If::new(local_value(&v), Block(vec![return_one(local_value(&v))]), Block::default())),
                return_one(fallback(&p)),
            ];
            let mut site = vec![
                assign_local(&x, global("input"), true),
                assign_local(&r, lookup(&x), true),
                Statement::If(If::new(not_rv(local_value(&r)), Block(vec![assign_local(&r, fallback(&x), false)]), Block::default())),
                Statement::Call(global_call("print", vec![local_value(&r)])),
            ];
            if captured {
                // The copy's closure holds `r`, which lives on after it.
                body.insert(1, keep(&v));
                site.insert(2, keep(&r));
            }
            site.insert(0, helper_with_params(&f, vec![p.clone()], body));
            let mut block = Block(site);
            deinline(&mut block);
            block.to_string()
        };
        let output = run(false);
        assert!(output.contains("f(input)"), "{output}");
        let output = run(true);
        assert!(!output.contains("f(input)"), "{output}");
    }

    #[test]
    fn a_call_the_helper_truncates_matches_a_site_call_taking_one_value() {
        let (helper, s, x) = (local("first"), local("s"), local("x"));
        let produce = |of: &RcLocal| Call::new(global("produce"), vec![field(local_value(of), "name"), string("!")]);
        let run = |arguments: Vec<RValue>| {
            let mut block = Block(vec![
                assign_local(&x, global("input"), true),
                helper_with(&helper, std::slice::from_ref(&s), vec![return_one(RValue::Select(crate::Select::Call(produce(&s))))]),
                Statement::Call(global_call("print", arguments)),
            ]);
            deinline(&mut block);
            block.to_string()
        };
        // An operand takes one value of the site's call, as `(produce(s))`.
        let output = run(vec![RValue::Call(produce(&x)), number(1.0)]);
        assert!(output.contains("print(first(x), 1)"), "{output}");
        // A last argument takes all of them, which the helper cuts to one.
        let output = run(vec![number(1.0), RValue::Call(produce(&x))]);
        assert!(output.contains("print(1, produce(x.name, \"!\"))"), "{output}");
    }

    #[test]
    fn an_upvalue_capture_matches_a_copy_of_a_local_never_reassigned() {
        let (once, rebound) = (local("once"), local("rebound"));
        let census = |reassigned: bool| {
            let mut block = vec![assign_local(&once, number(1.0), true), assign_local(&rebound, number(1.0), true)];
            if reassigned {
                block.push(assign_local(&rebound, number(2.0), false));
            }
            crate::deinline_safety::CaptureSafety::new(&Block(block))
        };
        let function = ByAddress(Arc::new(Mutex::new(Function { bytecode_proto_id: Some(3), ..Function::default() })));
        let closure = |upvalue: Upvalue| Closure { node_origin: Default::default(), function: function.clone(), upvalues: vec![upvalue] };
        let (params, locals) = (FxHashSet::default(), FxHashSet::default());
        let unify = |captures: &crate::deinline_safety::CaptureSafety, pattern: Upvalue, site: Upvalue| {
            let ctx = MatchCtx { params: &params, locals: &locals, captures: Some(captures) };
            unify_closure(&ctx, &closure(pattern), &closure(site), false, &mut Bindings::default()).is_ok()
        };
        // The helper's closure reaches it through its own upvalue, the copy
        // straight from the caller's local.
        assert!(unify(&census(false), Upvalue::Ref(once.clone()), Upvalue::Copy(once.clone())));
        assert!(unify(&census(true), Upvalue::Ref(rebound.clone()), Upvalue::Ref(rebound.clone())));
        assert!(!unify(&census(true), Upvalue::Ref(rebound.clone()), Upvalue::Copy(rebound.clone())));
        // Nor the other way round, nor between different locals.
        assert!(!unify(&census(false), Upvalue::Copy(once.clone()), Upvalue::Ref(once.clone())));
        assert!(!unify(&census(false), Upvalue::Ref(once.clone()), Upvalue::Copy(rebound.clone())));
    }
}
