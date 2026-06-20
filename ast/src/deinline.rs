//! De-inliner: reverses Luau `-O2` function inlining.
//!
//! The Roblox Luau compiler at `-O2` inlines small local functions: it copies
//! the callee's body into each caller's bytecode at the call site. medal
//! faithfully reproduces that, so the output shows the function inlined
//! everywhere instead of called. This pass detects those inlined regions and
//! rewrites them back into real calls `funcName(args)`, each marked
//! `INLINED / UNHOOKABLE`, and marks the recovered definition too.
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

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    Assign, Binary, BinaryOperation, Block, Call, Comment, Function, GenericFor, If, LValue,
    Literal, LocalRw, MethodCall, NumericFor, RValue, RcLocal, Repeat, Return, Select, SideEffects,
    Statement, Table, Traverse, Unary, UnaryOperation, While,
};

const DEF_MARKER: &str = " [-O2 INLINED, UNHOOKABLE] reconstructed definition;";
// Trailing (same-line) marker appended to a reconstructed call: `f(args) -- ...`.
// No leading `^` caret (it no longer points up at a separate line above).
const CALL_MARKER: &str = "inlined by Luau -O2 (UNHOOKABLE)";

type FnPtr = *const Mutex<Function>;

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
}

/// Trace a de-inline target rejection. With the `deinline_trace` feature it prints
/// the gate + function name to stderr; without it the body is absent, so the
/// reason/name arguments are never evaluated (no lock, no alloc) and release output
/// stays byte-identical. Kept to the COLD `collect_targets` path only — the hot
/// per-site matchers stay allocation-free, exactly as the report recommends.
macro_rules! deinline_reject {
    ($reason:expr, $name:expr) => {{
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
    pat_raw_len: usize,  // raw body length (window ceiling)
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

/// The only target context the *expression*-level unifier (`unify_rvalue` and the
/// `unify_*` helpers it calls) actually reads: the binding-hole sets. `params` are
/// bind-once holes (each binds to one caller argument expression, and every later
/// occurrence must match it); `locals` are the callee's own declared locals,
/// matched as an injective renaming. Everything else (globals, literals, operators,
/// upvalues by `RcLocal` identity, ...) must match exactly.
///
/// Factoring this out of `Target` lets the §7 expression de-inliner
/// (`crate::expr_deinline`) reuse the exact same battle-tested structural unifier
/// (NaN-safe `lit_eq`, closure-arg refusal, param consistency, local injectivity)
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

pub fn deinline(body: &mut Block) {
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
    crate::expr_deinline::collect_write_counts(&body.0, &mut write_counts);
    loop {
        let targets = collect_targets(body, &write_counts);
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
        let mut newly: FxHashSet<RcLocal> = FxHashSet::default();
        deinline_block(
            &mut body.0,
            &targets,
            &decl_map,
            &[],
            None,
            true,
            true,
            &mut newly,
        );
        if newly.is_empty() {
            break;
        }
        let before = converted.len();
        converted.extend(newly);
        if converted.len() == before {
            break;
        }
    }
    if !converted.is_empty() {
        // Binders of CONVERTED helpers that can return more than one value (a P7-A
        // call-return leaf). `collapse_value_results` must not spread these into a
        // multi-value context (see `collapse_use` / `body_has_call_return`).
        let mut multivalue: FxHashSet<RcLocal> = FxHashSet::default();
        each_closure_decl(&body.0, &mut |l, fa| {
            if converted.contains(l) && body_has_call_return(&fa.lock().body.0) {
                multivalue.insert(l.clone());
            }
        });
        collapse_value_results(&mut body.0, &multivalue);
        insert_def_markers(&mut body.0, &converted);
    }
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

const COLLAPSE_MARKER: &str = " [-O2 INLINED, UNHOOKABLE] reconstructed call";

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
    matches!(s, Statement::Empty(_))
        || matches!(s, Statement::Comment(c) if is_internal_marker(c))
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

fn collapse_value_results(stmts: &mut Vec<Statement>, multivalue: &FxHashSet<RcLocal>) {
    // recurse into nested blocks and closure bodies first.
    for s in stmts.iter_mut() {
        match s {
            Statement::If(f) => {
                collapse_value_results(&mut f.then_block.lock().0, multivalue);
                collapse_value_results(&mut f.else_block.lock().0, multivalue);
            }
            Statement::While(w) => collapse_value_results(&mut w.block.lock().0, multivalue),
            Statement::Repeat(r) => collapse_value_results(&mut r.block.lock().0, multivalue),
            Statement::NumericFor(nf) => collapse_value_results(&mut nf.block.lock().0, multivalue),
            Statement::GenericFor(gf) => collapse_value_results(&mut gf.block.lock().0, multivalue),
            _ => {}
        }
        for rv in stmt_rvalues_mut(s) {
            collapse_in_closures(rv, multivalue);
        }
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
            // `v`'s declaration is about to be removed, so `v` must not be
            // *written* anywhere we keep either — a later `v = ...` (e.g. inside
            // the collapsed `if`) would otherwise be left with no declaration.
            && last_write.get(&v).is_none_or(|&k| k < i + 2)
            && let Some(collapsed) = collapse_use(&taken[i + 2], &v, call, multivalue)
        {
            // The reconstructed call now lives inside `collapsed`. For a
            // single-line `return f(args)` / `x = f(args)` the marker reads best
            // appended to that line; for the multi-line `if f(args) then … end`
            // shape it stays a leading header above the block.
            if matches!(collapsed, Statement::If(_)) {
                out.push(Statement::Comment(Comment::new(COLLAPSE_MARKER.to_string())));
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
/// `multivalue` holds the binders of helpers that can return MORE than one value
/// (a P7-A call-return helper). For such a call, the MULTI-VALUE-context arms are
/// refused — `return v` -> `return f(args)` (a tail call spreads all values) and a
/// MULTI-LHS `a, b = v` -> `a, b = f(args)` would expose values the original
/// single-LHS `local v = f(args)` had truncated away. Single-value contexts (an
/// `if` condition, a SINGLE-LHS assign) truncate to one value either way and stay
/// sound for every helper.
fn collapse_use(
    s: &Statement,
    v: &RcLocal,
    call: &RValue,
    multivalue: &FxHashSet<RcLocal>,
) -> Option<Statement> {
    let is_v = |rv: &RValue| matches!(rv, RValue::Local(x) if x == v);
    let is_not_v = |rv: &RValue| {
        matches!(rv, RValue::Unary(u)
            if u.operation == UnaryOperation::Not && is_v(&u.value))
    };
    // Does the moved-in call target a multi-value helper? Then it must not be
    // spread into a multi-value context.
    let call_is_multivalue = call_callee_local(call).is_some_and(|l| multivalue.contains(l));
    match s {
        Statement::If(f) => {
            let cond = if is_v(&f.condition) {
                call.clone()
            } else if is_not_v(&f.condition) {
                RValue::Unary(Unary {
                    value: Box::new(call.clone()),
                    operation: UnaryOperation::Not,
                })
            } else {
                return None;
            };
            Some(Statement::If(If {
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
            if r.values.len() == 1 && is_v(&r.values[0]) && !call_is_multivalue =>
        {
            Some(Statement::Return(Return {
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
                && (a.left.len() == 1 || !call_is_multivalue) =>
        {
            Some(Statement::Assign(Assign {
                left: a.left.clone(),
                right: vec![call.clone()],
                prefix: a.prefix,
                parallel: a.parallel,
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
fn body_has_call_return(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| match s {
        Statement::Return(r) => r.values.len() == 1 && !is_scalar_return_value(&r.values[0]),
        Statement::If(f) => {
            body_has_call_return(&f.then_block.lock().0)
                || body_has_call_return(&f.else_block.lock().0)
        }
        Statement::While(w) => body_has_call_return(&w.block.lock().0),
        Statement::Repeat(r) => body_has_call_return(&r.block.lock().0),
        Statement::NumericFor(nf) => body_has_call_return(&nf.block.lock().0),
        Statement::GenericFor(gf) => body_has_call_return(&gf.block.lock().0),
        _ => false,
    })
}

fn count_local_reads(stmts: &[Statement], v: &RcLocal) -> usize {
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
            for rv in stmt_rvalues(s) {
                n += rvalue_closure_reads(rv, v);
            }
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
fn collect_reads(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for s in stmts {
        for r in s.values_read() {
            out.insert(r.clone());
        }
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
        for rv in stmt_rvalues(s) {
            collect_reads_in_closures(rv, out);
        }
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

fn collapse_in_closures(rv: &mut RValue, multivalue: &FxHashSet<RcLocal>) {
    // Find every closure within `rv` and run the collapse inside its body. Descent
    // uses the enum_dispatch `Traverse::rvalues_mut` (exhaustive by construction, so
    // it can never silently drop a new RValue variant — incl. `IfExpression`),
    // mirroring `expr_deinline::write_counts_in_closures`.
    if let RValue::Closure(c) = rv {
        collapse_value_results(&mut c.function.0.lock().body.0, multivalue);
        return;
    }
    for child in rv.rvalues_mut() {
        collapse_in_closures(child, multivalue);
    }
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
                left: b.left,
                right: b.right,
                operation,
            })
        }
        other => RValue::Unary(Unary {
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
    canon_recurse(canon_top(stmts, tail), tail)
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

/// The recursive half of canon: canonicalise each statement's child blocks.
/// A 1:1 statement map, so it never changes the count produced by `canon_top`.
/// Tail-position only propagates to the LAST statement's branches (and never
/// into loop bodies, where a `return` exits across iterations + the loop).
fn canon_recurse(s: Vec<Statement>, tail: bool) -> Vec<Statement> {
    let n = s.len();
    s.iter()
        .enumerate()
        .map(|(j, st)| canon_children(st, tail && j + 1 == n))
        .collect()
}

fn canon_children(s: &Statement, tail: bool) -> Statement {
    match s {
        Statement::If(f) => Statement::If(If::new(
            f.condition.clone(),
            Block(canon_tail(&f.then_block.lock().0, tail)),
            Block(canon_tail(&f.else_block.lock().0, tail)),
        )),
        Statement::While(w) => Statement::While(While::new(
            w.condition.clone(),
            Block(canon_tail(&w.block.lock().0, false)),
        )),
        Statement::Repeat(r) => Statement::Repeat(Repeat::new(
            r.condition.clone(),
            Block(canon_tail(&r.block.lock().0, false)),
        )),
        Statement::NumericFor(nf) => Statement::NumericFor(NumericFor {
            initial: nf.initial.clone(),
            limit: nf.limit.clone(),
            step: nf.step.clone(),
            counter: nf.counter.clone(),
            block: Arc::new(Mutex::new(Block(canon_tail(&nf.block.lock().0, false)))),
        }),
        Statement::GenericFor(gf) => Statement::GenericFor(GenericFor::new(
            gf.res_locals.clone(),
            gf.right.clone(),
            Block(canon_tail(&gf.block.lock().0, false)),
        )),
        other => other.clone(),
    }
}

fn unguard(mut stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        if let Statement::If(f) = &stmts[i] {
            // A "guard" is `if cond then return [X] end` (empty else, then-block is
            // a single return). The early-return splits the body: when `cond` holds
            // we exit (returning X); otherwise we run the rest. Re-nest it into the
            // positive form the structurer produces for inlined copies:
            //   void  return:  `if not cond then REST end`        (return is a no-op
            //                                                       tail fall-through)
            //   value return:  `if not cond then REST else return X end`
            //                                                       (X is the result)
            let guard: Option<Option<RValue>> = {
                let then = f.then_block.lock();
                let els = f.else_block.lock();
                if els.0.is_empty() && then.0.len() == 1 {
                    match &then.0[0] {
                        Statement::Return(r) if r.values.is_empty() => Some(None),
                        Statement::Return(r) if r.values.len() == 1 => {
                            Some(Some(r.values[0].clone()))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            };
            if let Some(ret_val) = guard {
                if i + 1 < stmts.len() {
                    let cond = f.condition.clone();
                    let suffix: Vec<Statement> = stmts.split_off(i + 1);
                    let folded = unguard(suffix);
                    let else_block = match ret_val {
                        None => Block::default(),
                        Some(x) => Block(vec![Statement::Return(Return { values: vec![x] })]),
                    };
                    out.push(Statement::If(If::new(
                        negate_canon(cond),
                        Block(folded),
                        else_block,
                    )));
                    return out;
                }
            }
        }
        out.push(stmts[i].clone());
        i += 1;
    }
    out
}

/// The foldable-guard shape `unguard` collapses: `if cond then return [X] end`
/// (empty else, then-block a single 0-or-1-value return). Factored out so
/// `canon_top_len` computes the post-unguard length using the EXACT same predicate
/// `unguard` folds on (no drift); `canon_top_len`'s debug_assert cross-checks the
/// whole length against the real `canon_top`.
fn is_foldable_guard(s: &Statement) -> bool {
    if let Statement::If(f) = s {
        let then = f.then_block.lock();
        let els = f.else_block.lock();
        els.0.is_empty()
            && then.0.len() == 1
            && matches!(&then.0[0], Statement::Return(r) if r.values.len() <= 1)
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
    let total = stmts.iter().filter(|s| !is_match_trivia(s)).count();
    let n = if !tail || total == 0 {
        total
    } else {
        // N1: drop a trailing void return (the LAST non-trivia statement).
        let last_void = stmts
            .iter()
            .rev()
            .find(|s| !is_match_trivia(s))
            .is_some_and(|s| matches!(s, Statement::Return(r) if r.values.is_empty()));
        let effective = total - usize::from(last_void);
        // N3: unguard folds at the first foldable guard that has a following
        // (within-`effective`) statement -> top-level length is its index + 1.
        let mut len = effective;
        for (idx, s) in stmts
            .iter()
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
    };
    debug_assert_eq!(
        n,
        canon_top(stmts, tail).len(),
        "canon_top_len must mirror canon_top length exactly"
    );
    n
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
            if pa.left.len() != ca.left.len()
                || pa.right.len() != ca.right.len()
                || pa.parallel != ca.parallel
                || pa.prefix != ca.prefix
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
        (Statement::Return(pr), Statement::Return(cr)) => {
            if pr.values.is_empty() && cr.values.is_empty() {
                Ok(())
            } else {
                Err(())
            }
        }
        // Value target: the callee's `return X` was lowered to `RESULT = X` in
        // the inlined copy. Bind the single result local and unify the value.
        (Statement::Return(pr), Statement::Assign(ca))
            if t.kind == TKind::Value
                && pr.values.len() == 1
                && ca.left.len() == 1
                && ca.right.len() == 1 =>
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
            unify_rvalue(&ctx, &pr.values[0], &ca.right[0], b)
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

fn unify_lvalue(ctx: &MatchCtx, p: &LValue, c: &LValue, b: &mut Bindings) -> Result<(), ()> {
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
/// literals via NaN-bit-exact `lit_eq`, operators, method/field names, upvalues by
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
            if lit_eq(a, d) {
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
        (RValue::VarArg(_), RValue::VarArg(_)) => Ok(()),
        (RValue::Select(a), RValue::Select(d)) => unify_select(ctx, a, d, b),
        _ => Err(()),
    }
}

fn unify_call(ctx: &MatchCtx, a: &Call, d: &Call, b: &mut Bindings) -> Result<(), ()> {
    if a.arguments.len() != d.arguments.len() {
        return Err(());
    }
    unify_rvalue(ctx, &a.value, &d.value, b)?;
    for (x, y) in a.arguments.iter().zip(&d.arguments) {
        unify_rvalue(ctx, x, y, b)?;
    }
    Ok(())
}

fn unify_method(ctx: &MatchCtx, a: &MethodCall, d: &MethodCall, b: &mut Bindings) -> Result<(), ()> {
    if a.method != d.method || a.arguments.len() != d.arguments.len() {
        return Err(());
    }
    unify_rvalue(ctx, &a.value, &d.value, b)?;
    for (x, y) in a.arguments.iter().zip(&d.arguments) {
        unify_rvalue(ctx, x, y, b)?;
    }
    Ok(())
}

fn unify_table(ctx: &MatchCtx, a: &Table, d: &Table, b: &mut Bindings) -> Result<(), ()> {
    if a.0.len() != d.0.len() {
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

fn unify_local(ctx: &MatchCtx, pl: &RcLocal, cl: &RcLocal, b: &mut Bindings) -> Result<(), ()> {
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
    if pl == cl {
        Ok(())
    } else {
        Err(())
    }
}

pub(crate) fn lit_eq(a: &Literal, b: &Literal) -> bool {
    match (a, b) {
        (Literal::Number(x), Literal::Number(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

/// Bit-exact structural equality for the correctness gates. The derived
/// `PartialEq` bottoms out at `f64::eq`, so it wrongly equates `+0.0` with `-0.0`
/// and refuses `NaN == NaN`; this descends the whole tree comparing every
/// `Literal::Number` via `lit_eq`'s `to_bits()`. Closures compare by `Function`
/// pointer identity only — two `{}`/closures are distinct values and must never
/// be treated as equal by structure. Use this anywhere a gate's soundness depends
/// on two reconstructed expressions being the SAME value (return-folding,
/// repeated-argument consistency, recorded-site agreement).
pub(crate) fn rvalue_exact_eq(a: &RValue, b: &RValue) -> bool {
    match (a, b) {
        (RValue::Local(x), RValue::Local(y)) => x == y,
        (RValue::Global(x), RValue::Global(y)) => x == y,
        (RValue::Literal(x), RValue::Literal(y)) => lit_eq(x, y),
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
        (RValue::Closure(x), RValue::Closure(y)) => {
            Arc::as_ptr(&x.function.0) == Arc::as_ptr(&y.function.0)
        }
        // Select wraps Call/MethodCall/VarArg — recurse so an inner `±0.0` arg does
        // not leak back to the derived `f64` equality of the wrapped call.
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
    current_func: Option<FnPtr>,
    is_func_tail: bool,
    is_func_body_top: bool,
    newly: &mut FxHashSet<RcLocal>,
) {
    // 1. recurse into nested statement-blocks and into closure bodies first.
    //    A child block/closure only sees targets whose declaration lexically
    //    precedes it — `active` grows as we pass each declaration in THIS block.
    let n = stmts.len();
    {
        let mut active: Vec<usize> = outer_active.to_vec();
        for (j, s) in stmts.iter_mut().enumerate() {
            let child_tail = is_func_tail && j == n - 1;
            match s {
                Statement::If(f) => {
                    deinline_block(
                        &mut f.then_block.lock().0,
                        targets,
                        decl_map,
                        &active,
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
            for rv in stmt_rvalues_mut(s) {
                recurse_into_closures(rv, targets, decl_map, &active, newly);
            }
            if let Some(idx) = target_decl_index(s, decl_map, targets) {
                active.push(idx);
            }
        }
    }

    // 2. scan this block left to right, activating each target after its decl.
    let mut active: Vec<usize> = outer_active.to_vec();
    let mut i = 0;
    // Tail-liveness index, replacing the per-window O(N) `any_local_live` rescan
    // with an O(|set|) lookup. Built lazily on the first query (so target-free /
    // never-matching blocks pay nothing) and reused across positions; the driver
    // invalidates it after each splice, after which the next query rebuilds it.
    let mut last_occ: Option<FxHashMap<RcLocal, usize>> = None;
    while i < stmts.len() {
        if let Some(hit) = try_match_at(
            stmts,
            i,
            targets,
            &active,
            current_func,
            is_func_tail,
            is_func_body_top,
            &mut last_occ,
        ) {
            let call = Call::new(RValue::Local(hit.f_local.clone()), hit.args);
            let stmt = match &hit.result {
                None => Statement::Call(call),
                Some(r) => Statement::Assign(Assign {
                    left: vec![LValue::Local(r.clone())],
                    right: vec![RValue::Call(call)],
                    prefix: true,
                    parallel: false,
                }),
            };
            let marker = Statement::Comment(Comment::trailing(CALL_MARKER.to_string()));
            stmts.splice(i..i + hit.consume, [stmt, marker]);
            newly.insert(hit.f_local);
            i += 2;
            // The block changed; drop the cached index so the next query rebuilds
            // it against the spliced `stmts`.
            last_occ = None;
        } else {
            // a target's own declaration is never inside a matched window (a body
            // with a closure is refused), so activating here is safe.
            if let Some(idx) = target_decl_index(&stmts[i], decl_map, targets) {
                active.push(idx);
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
    newly: &mut FxHashSet<RcLocal>,
) {
    match rv {
        RValue::Closure(c) => {
            let fp = Arc::as_ptr(&c.function.0);
            deinline_block(
                &mut c.function.0.lock().body.0,
                targets,
                decl_map,
                active,
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

fn record_site(
    site: &mut Option<(usize, Vec<RValue>)>,
    ambiguous: &mut bool,
    w: usize,
    args: &[RValue],
) {
    match site {
        None => *site = Some((w, args.to_vec())),
        Some((_, prev)) => {
            if !args_vec_eq(prev, args) {
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
    if !block_has_return(raw) || !all_returns_are(raw, &ret) {
        return None;
    }
    // RET must not read a local the window writes (early-path vs tail-path value).
    let mut written: FxHashSet<RcLocal> = FxHashSet::default();
    collect_written(raw, &mut written);
    for r in ret.values_read() {
        if written.contains(r) {
            return None;
        }
    }
    Some(ret)
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
            Statement::NumericFor(nf) => Statement::NumericFor(NumericFor {
                initial: nf.initial.clone(),
                limit: nf.limit.clone(),
                step: nf.step.clone(),
                counter: nf.counter.clone(),
                block: Arc::new(Mutex::new(Block(rewrite_return_to_void(
                    &nf.block.lock().0,
                    ret,
                )))),
            }),
            Statement::GenericFor(gf) => Statement::GenericFor(GenericFor::new(
                gf.res_locals.clone(),
                gf.right.clone(),
                Block(rewrite_return_to_void(&gf.block.lock().0, ret)),
            )),
            other => other.clone(),
        })
        .collect()
}

struct Hit {
    f_local: RcLocal,
    consume: usize, // statements to replace, starting at i
    args: Vec<RValue>,
    result: Option<RcLocal>, // Some -> emit `local result = f(args)`; None -> `f(args)`
}

fn try_match_at(
    stmts: &[Statement],
    i: usize,
    targets: &[Target],
    active: &[usize],
    current_func: Option<FnPtr>,
    is_func_tail: bool,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
) -> Option<Hit> {
    if active.is_empty() {
        return None; // no targets in scope here — nothing to match (skip the scan)
    }
    let mut found: Option<Hit> = None;
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
    let anchor_stmt = stmts[i..].iter().find(|s| !matches!(s, Statement::Empty(_)));
    let anchor_disc = anchor_stmt.map(std::mem::discriminant);
    // Second prefilter dimension: the fixed-name anchor of that first statement
    // (method / global-call name). Computed once per position; compared to each
    // candidate target's `pat0_anchor_key`. Same sound domain as `pat0_kind`.
    let anchor_key = anchor_stmt.and_then(stmt_anchor_key);
    // only targets whose local function is in scope here (declared earlier, in a
    // visible block) are candidates — emitting a call to an out-of-scope local
    // would be invalid.
    for &ti in active {
        let t = &targets[ti];
        if current_func == Some(t.func_ptr) {
            continue; // never match a function against its own definition body
        }
        if t.pat.is_empty() {
            continue;
        }
        // O(1) variant prefilter: the first non-`Empty` statement at `i` must share
        // `pat[0]`'s variant. Applies to Void targets and to `AtPrefix` Value
        // targets (whose `pat[0]` is the leading callee-prefix `Assign`, which
        // `canon` preserves). NOT to `AtResultDecl` Value targets — their `pat[0]`
        // may be a leaf `return X` unified against an `Assign`, so the variant
        // check would be unsound there.
        let use_disc = t.kind == TKind::Void
            || (t.kind == TKind::Value && t.value_anchor == ValueAnchor::AtPrefix);
        if use_disc && anchor_disc != Some(t.pat0_kind) {
            continue; // first-statement variant cannot match this pattern
        }
        // Name prefilter (same targets as the variant check): when BOTH the position
        // and the pattern have a fixed-name head anchor and they differ, the exact
        // unify of `pat[0]` would fail — skip without entering the window scan.
        if use_disc
            && let (Some(ak), Some(tk)) = (anchor_key, t.pat0_anchor_key)
            && ak != tk
        {
            continue;
        }
        let hit = match (t.kind, t.value_anchor) {
            (TKind::Void, _) => match_void(stmts, i, t, is_func_tail, is_func_body_top, last_occ),
            (TKind::Value, ValueAnchor::AtResultDecl) => {
                match_value(stmts, i, t, is_func_body_top, last_occ)
            }
            (TKind::Value, ValueAnchor::AtPrefix) => {
                match_value_prefixed(stmts, i, t, is_func_body_top, last_occ)
            }
        };
        if let Some(h) = hit {
            if found.is_some() {
                return None; // two different functions match here: refuse
            }
            found = Some(h);
        }
    }
    found
}

fn match_void(
    stmts: &[Statement],
    i: usize,
    t: &Target,
    is_func_tail: bool,
    is_func_body_top: bool,
    last_occ: &mut Option<FxHashMap<RcLocal, usize>>,
) -> Option<Hit> {
    let kc = t.pat.len();
    let max_w = std::cmp::min(stmts.len() - i, t.pat_raw_len + 1);
    let mut site: Option<(usize, Vec<RValue>)> = None;
    let mut ambiguous = false;
    for w in kc..=max_w {
        let raw = &stmts[i..i + w];
        // Never replace a function's ENTIRE top-level body with a single call:
        // the ambiguous thin-wrapper case (`B(x)=A(x)`).
        if is_func_body_top && i == 0 && i + w == stmts.len() {
            continue;
        }
        // Attempt 1 — plain canon, with tail-safety for consuming a caller return.
        // Check the cheap top-level canon length (and the blocked gate) before
        // paying for the deep nested-block rebuild (`canon_recurse`).
        let plain_blocked = block_has_return(raw) && !(is_func_tail && i + w == stmts.len());
        if !plain_blocked {
            // Cheap non-allocating length pre-check before the canon allocations.
            if canon_top_len(raw, true) == kc {
                let plain = canon_recurse(canon_top(raw, true), true);
                if let Some(u) = try_unify_site(t, &plain) {
                    // every callee-temp must be dead after the consumed window, else
                    // a later use would reference a now-removed declaration.
                    if !tail_has_live(last_occ, stmts, i, i + w, &u.callee_locals) {
                        record_site(&mut site, &mut ambiguous, w, &u.args);
                    }
                }
            }
        }
        // Attempt 2 (Gap B) — void callee inlined at a value-returning caller's
        // tail, its void early-returns lowered to the caller's tail `return RET`.
        if let Some(ret) = value_tail_ret(stmts, i, w, is_func_tail) {
            let rewritten = rewrite_return_to_void(raw, &ret);
            if canon_top_len(&rewritten, true) == kc {
                let folded = canon_recurse(canon_top(&rewritten, true), true);
                if let Some(u) = try_unify_site(t, &folded) {
                    if !tail_has_live(last_occ, stmts, i, i + w, &u.callee_locals) {
                        record_site(&mut site, &mut ambiguous, w, &u.args);
                    }
                }
            }
        }
    }
    if ambiguous {
        return None;
    }
    let (w, args) = site?;
    Some(Hit {
        f_local: t.f_local.clone(),
        consume: w,
        args,
        result: None,
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
) -> Option<Hit> {
    let r = result_decl(&stmts[i])?;
    let kc = t.pat.len();
    let body_start = i + 1;
    let avail = stmts.len() - body_start;
    let max_w = std::cmp::min(avail, t.pat_raw_len + 1);
    let mut site: Option<(usize, Vec<RValue>)> = None;
    let mut ambiguous = false;
    for w in kc..=max_w {
        // whole-body gate: the decl + region must not be the function's entire body.
        if is_func_body_top && i == 0 && body_start + w == stmts.len() {
            continue;
        }
        let region = &stmts[body_start..body_start + w];
        // the region assigns RESULT; it must not itself contain returns.
        if block_has_return(region) {
            continue;
        }
        // Reject by cheap (non-allocating) top-level canon length before the deep rebuild.
        if canon_top_len(region, true) != kc {
            continue;
        }
        let cwin = canon_recurse(canon_top(region, true), true);
        if let Some(u) = try_unify_site(t, &cwin) {
            // RESULT must be exactly the declared local and only written (never
            // read) inside the region, so the region is its full computation.
            // A later reassignment of RESULT is FINE: the replacement re-declares
            // it as `local RESULT = f(args)`, so subsequent writes stay valid —
            // we must NOT reject on that. Every OTHER callee-temp, though, must be
            // dead after the region (its declaration is being removed).
            if u.result.as_ref() == Some(&r)
                && !block_reads_local(region, &r)
                && !tail_has_live(last_occ, stmts, i, body_start + w, &u.callee_locals)
            {
                record_site(&mut site, &mut ambiguous, w, &u.args);
            }
        }
    }
    if ambiguous {
        return None;
    }
    let (w, args) = site?;
    Some(Hit {
        f_local: t.f_local.clone(),
        consume: 1 + w,
        args,
        result: Some(r),
    })
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
    let r = result_decl(&stmts[d])?;
    let kc = t.pat.len();
    let region_start = d + 1;
    let avail = stmts.len() - region_start;
    let max_w = std::cmp::min(avail, t.pat_raw_len + 1);
    // The prefix (the callee's leading statement) is loop-invariant — neither it
    // nor its return-freeness depends on `w` — so resolve it once up front. A
    // return in the prefix means a different shape (the value branch must be the
    // last, sole returning statement), so bail before the per-width scan.
    let prefix = &stmts[i..d];
    if block_has_return(prefix) {
        return None;
    }
    let mut site: Option<(usize, Vec<RValue>)> = None;
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
        // the value region must not contain a return: it assigns RESULT on every
        // path; a return would be a different shape.
        if block_has_return(region) {
            continue;
        }
        // UNION window with the RESULT decl spliced out.
        let mut union: Vec<Statement> = Vec::with_capacity(p + w);
        union.extend_from_slice(prefix);
        union.extend_from_slice(region);
        // cheap (non-allocating) top-level canon length reject BEFORE the deep rebuild.
        if canon_top_len(&union, true) != kc {
            continue;
        }
        let cwin = canon_recurse(canon_top(&union, true), true);
        if let Some(u) = try_unify_site(t, &cwin) {
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
                record_site(&mut site, &mut ambiguous, w, &u.args);
            }
        }
    }
    if ambiguous {
        return None;
    }
    let (w, args) = site?;
    Some(Hit {
        f_local: t.f_local.clone(),
        // Absolute span from i: prefix + any interposed trivia + the RESULT decl
        // (at d) + the w-statement region. `(d - i)` counts the prefix and trivia
        // so the splice removes the interposed marker along with the window.
        consume: (d - i) + 1 + w,
        args,
        result: Some(r),
    })
}

struct Unified {
    args: Vec<RValue>,
    result: Option<RcLocal>,
    /// The caller-side locals the callee's own (non-result) locals mapped onto —
    /// the region's internal temps. They cease to exist once the region becomes a
    /// call, so the site is only valid if none is live afterwards.
    callee_locals: FxHashSet<RcLocal>,
}

fn try_unify_site(t: &Target, cwin: &[Statement]) -> Option<Unified> {
    let mut b = Bindings::default();
    if unify_block(t, &t.pat, cwin, &mut b).is_err() {
        return None;
    }
    let mut args = Vec::with_capacity(t.param_order.len());
    for p in &t.param_order {
        match b.params.get(p) {
            Some(e) => args.push(e.clone()),
            None => return None, // a parameter never bound (unread) — refuse
        }
    }
    // Argument hoist-safety. Turning the region back into `f(args)` evaluates
    // every argument at the call site (before the body), in parameter order.
    // That is sound only when each argument can be moved to the front without
    // changing observable behaviour:
    //   * no side effects — a side-effecting arg (Call/MethodCall, and per this
    //     crate's `SideEffects`, also Global/Index reads) would be reordered
    //     relative to the body's own effects; and
    //   * value stability — it must read no local the region writes, so its value
    //     can't change between the front and its in-body use point.
    // A genuine inlined arg with side effects survives in the copy as a `local`
    // temp (the per-function inliner won't hoist it past an effect), which binds
    // here as a side-effect-free `Local` and is accepted. Everything else: REFUSE.
    //
    // NOTE (DeInlineReview §1 / DeinlineReport §2 — verified unreachable, P2):
    // this `collect_written` oracle sees only SYNTACTIC writes and writes inside
    // closure LITERALS — NOT a caller local mutated indirectly by a call to a
    // by-name function whose body writes a captured upvalue. For that to corrupt a
    // reconstruction, a bound argument `a` would have to read a local `x` that some
    // call IN the region mutates between the call-site (front) and `x`'s in-body use
    // — making `f(x)` snapshot a stale value. This is unreachable on genuine -O2
    // output, for THREE independent reasons:
    //   1. The arg side-effect gate just below refuses any non-trivial arg, so `a`
    //      can only be a plain `Local`/literal/operator tree, never a call result.
    //   2. This pass runs BEFORE `inline_temps` (luau-lifter `lib.rs`: deinline at
    //      ~line 204, inline_single_use_temps at ~213). At deinline time no
    //      single-use temp has been forwarded yet, so a value that would be unstable
    //      across an effect still sits in its own distinct snapshot local
    //      (`local tmp = x` before the effect) and `a` binds to that STABLE `tmp`,
    //      not to `x`. (When `inline_temps` later runs, `can_move_between`
    //      ALSO refuses to forward a captured-local read across a side-effecting
    //      statement — `reads_captured_local && has_side_effects` — so the unstable
    //      shape never materialises afterwards either.)
    //   3. An unknown/global/method callee cannot mutate a caller LOCAL unless a
    //      closure capturing that local by ref has already escaped to it; such a
    //      capture makes the local `has_side_effects`-tainted upstream and keeps it
    //      out of the plain-`Local` arg position by (1).
    // A precise interprocedural effect summary was considered (P2) but would change
    // ZERO corpus output (the hole is empty); and the cheap sound guard (refuse any
    // arg reading a "written-in-some-closure" local) was measured to refuse
    // de-inlines across 70+ corpus files — in React/UI Luau nearly every local
    // lives inside a closure — so it stays DEFERRED rather than pay that
    // readability cost for a precursor that cannot occur. A regression tripwire
    // (`captured_mutation_hole_shape_is_refused`) pins the boundary.
    let mut region_writes: FxHashSet<RcLocal> = FxHashSet::default();
    collect_written(cwin, &mut region_writes);
    for a in &args {
        if a.has_side_effects() {
            return None;
        }
        for r in a.values_read() {
            if region_writes.contains(r) {
                return None;
            }
        }
    }
    Some(Unified {
        args,
        result: b.result,
        callee_locals: b.locals.values().cloned().collect(),
    })
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

// ===================================================================
// Target collection + per-function gates
// ===================================================================

fn collect_targets(body: &Block, write_counts: &FxHashMap<RcLocal, usize>) -> Vec<Target> {
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
            deinline_reject!(RejectReason::Variadic, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        if body_unsafe(&g.body.0) {
            deinline_reject!(RejectReason::UnsafeBody, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        let kind = match classify_returns(&g.body.0) {
            Some(k) => k,
            None => {
                // multi-return / mixed / bare-vararg leaf / non-terminal value return
                deinline_reject!(
                    RejectReason::UnsupportedReturnShape,
                    g.name.as_deref().unwrap_or("<anon>")
                );
                continue;
            }
        };
        let pat = canon(&g.body.0);
        if pat.is_empty() {
            deinline_reject!(RejectReason::EmptyPattern, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        match kind {
            // void: canon must have removed/folded all returns.
            TKind::Void => {
                if block_has_return(&pat) {
                    deinline_reject!(
                        RejectReason::UnsupportedReturnShape,
                        g.name.as_deref().unwrap_or("<anon>")
                    );
                    continue;
                }
            }
            // value: every leaf must be a single value-return (the result).
            TKind::Value => {
                if !value_leaf_shape(&pat) {
                    deinline_reject!(
                        RejectReason::UnsupportedReturnShape,
                        g.name.as_deref().unwrap_or("<anon>")
                    );
                    continue;
                }
            }
        }
        if std::env::var("DEINLINE_ANCHOR_TRACE").is_ok() {
            let a = anchors_in_block(&pat);
            let nc: usize = pat.iter().map(crate::deinline::dbg_stmt_node_count).sum();
            let nm = g.name.as_deref().unwrap_or("<none>");
            eprintln!(
                "ANCHORTRACE\tanchors={}\tstmts={}\tnodes={}\tkind={:?}\tname={}",
                a,
                pat.len(),
                nc,
                match kind { TKind::Void => "Void", TKind::Value => "Value" },
                nm
            );
        }
        if anchors_in_block(&pat) < 2 {
            deinline_reject!(RejectReason::LowAnchorScore, g.name.as_deref().unwrap_or("<anon>"));
            continue;
        }
        let params: FxHashSet<RcLocal> = g.parameters.iter().cloned().collect();
        let mut locals: FxHashSet<RcLocal> = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        for p in &params {
            locals.remove(p);
        }
        let func_ptr = Arc::as_ptr(&func);
        let pat_raw_len = g.body.0.len();
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
        let (value_anchor, prefix_len) = if kind == TKind::Value && pat.len() >= 2 {
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
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        drop(g);
        targets.push(Target {
            f_local,
            func_ptr,
            kind,
            pat,
            pat_raw_len,
            value_anchor,
            prefix_len,
            pat0_kind,
            pat0_anchor_key,
            params,
            locals,
            param_order,
        });
    }
    targets
}

/// Decide the return shape: `Void` (no value returns), `Value` (single scalar
/// value on every path), or refuse (`None`) for multi-return, mixed void/value,
/// a non-scalar value (call/method/vararg/select — arity unprovable), or a body
/// whose value returns are not all terminal leaves after canonicalization.
fn classify_returns(body: &[Statement]) -> Option<TKind> {
    let mut has_void = false;
    let mut has_value = false;
    if returns_bad(body, &mut has_void, &mut has_value) {
        return None;
    }
    if has_value {
        if has_void || !value_leaf_shape(&canon(body)) {
            return None;
        }
        Some(TKind::Value)
    } else {
        Some(TKind::Void)
    }
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
        for rv in stmt_rvalues(s) {
            each_closure_in_rvalue(rv, f);
        }
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
/// comments, upvalue-close, lifter-internal for-nodes, or a nested closure.
/// Return shape is handled separately by `classify_returns`.
pub(crate) fn body_unsafe(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| {
        // A nested closure ANYWHERE in this statement's expressions (call/method
        // arguments, returns, conditions, table values, ...) is unsafe — not just
        // a direct assign RHS. Identity-matching closures is unsound, so such a
        // body must never become a de-inline target.
        if stmt_rvalues(s).iter().any(|rv| rvalue_has_closure(rv)) {
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

fn rvalue_has_closure(rv: &RValue) -> bool {
    match rv {
        RValue::Closure(_) => true,
        RValue::Index(i) => rvalue_has_closure(&i.left) || rvalue_has_closure(&i.right),
        RValue::Unary(u) => rvalue_has_closure(&u.value),
        RValue::Binary(b) => rvalue_has_closure(&b.left) || rvalue_has_closure(&b.right),
        RValue::Call(c) => {
            rvalue_has_closure(&c.value) || c.arguments.iter().any(rvalue_has_closure)
        }
        RValue::MethodCall(m) => {
            rvalue_has_closure(&m.value) || m.arguments.iter().any(rvalue_has_closure)
        }
        RValue::Table(t) => {
            t.0.iter()
                .any(|(k, v)| k.as_ref().is_some_and(rvalue_has_closure) || rvalue_has_closure(v))
        }
        RValue::Select(Select::Call(c)) => {
            rvalue_has_closure(&c.value) || c.arguments.iter().any(rvalue_has_closure)
        }
        RValue::Select(Select::MethodCall(m)) => {
            rvalue_has_closure(&m.value) || m.arguments.iter().any(rvalue_has_closure)
        }
        RValue::IfExpression(e) => {
            rvalue_has_closure(&e.condition)
                || rvalue_has_closure(&e.then_value)
                || rvalue_has_closure(&e.else_value)
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

fn collect_declared_locals(stmts: &[Statement], out: &mut FxHashSet<RcLocal>) {
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
        for rv in stmt_rvalues(s) {
            collect_written_in_closures(rv, out);
        }
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
    for rv in crate::deinline::stmt_rvalues(s) {
        n += dbg_rvalue_node_count(rv);
    }
    match s {
        Statement::If(f) => {
            n += f.then_block.lock().0.iter().map(dbg_stmt_node_count).sum::<usize>();
            n += f.else_block.lock().0.iter().map(dbg_stmt_node_count).sum::<usize>();
        }
        Statement::While(w) => n += w.block.lock().0.iter().map(dbg_stmt_node_count).sum::<usize>(),
        Statement::Repeat(r) => n += r.block.lock().0.iter().map(dbg_stmt_node_count).sum::<usize>(),
        Statement::NumericFor(nf) => n += nf.block.lock().0.iter().map(dbg_stmt_node_count).sum::<usize>(),
        Statement::GenericFor(gf) => n += gf.block.lock().0.iter().map(dbg_stmt_node_count).sum::<usize>(),
        _ => {}
    }
    n
}

fn dbg_rvalue_node_count(rv: &RValue) -> usize {
    1 + rv.rvalues().iter().map(|c| dbg_rvalue_node_count(c)).sum::<usize>()
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
    for s in stmts.iter_mut() {
        match s {
            Statement::If(f) => {
                insert_def_markers(&mut f.then_block.lock().0, converted);
                insert_def_markers(&mut f.else_block.lock().0, converted);
            }
            Statement::While(w) => insert_def_markers(&mut w.block.lock().0, converted),
            Statement::Repeat(r) => insert_def_markers(&mut r.block.lock().0, converted),
            Statement::NumericFor(nf) => insert_def_markers(&mut nf.block.lock().0, converted),
            Statement::GenericFor(gf) => insert_def_markers(&mut gf.block.lock().0, converted),
            _ => {}
        }
        // recover definitions inside ANY closure body (call arguments, table
        // values, ...), matching where `deinline_block` recovers the calls.
        for rv in stmt_rvalues_mut(s) {
            markers_in_closures(rv, converted);
        }
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
                // pass already ran), do not emit a second one. Both passes share
                // `DEF_MARKER`, so a single equality check suffices.
                && !matches!(out.last(), Some(Statement::Comment(c)) if c.text == DEF_MARKER)
            {
                out.push(Statement::Comment(Comment::new(DEF_MARKER.to_string())));
            }
        }
        out.push(s);
    }
    *stmts = out;
}

fn markers_in_closures(rv: &mut RValue, converted: &FxHashSet<RcLocal>) {
    match rv {
        RValue::Closure(c) => insert_def_markers(&mut c.function.0.lock().body.0, converted),
        RValue::Call(c) => {
            markers_in_closures(c.value.as_mut(), converted);
            for a in &mut c.arguments {
                markers_in_closures(a, converted);
            }
        }
        RValue::MethodCall(m) => {
            markers_in_closures(m.value.as_mut(), converted);
            for a in &mut m.arguments {
                markers_in_closures(a, converted);
            }
        }
        RValue::Index(ix) => {
            markers_in_closures(ix.left.as_mut(), converted);
            markers_in_closures(ix.right.as_mut(), converted);
        }
        RValue::Unary(u) => markers_in_closures(u.value.as_mut(), converted),
        RValue::Binary(b) => {
            markers_in_closures(b.left.as_mut(), converted);
            markers_in_closures(b.right.as_mut(), converted);
        }
        RValue::Table(t) => {
            for (k, v) in &mut t.0 {
                if let Some(k) = k {
                    markers_in_closures(k, converted);
                }
                markers_in_closures(v, converted);
            }
        }
        RValue::Select(Select::Call(c)) => {
            markers_in_closures(c.value.as_mut(), converted);
            for a in &mut c.arguments {
                markers_in_closures(a, converted);
            }
        }
        RValue::Select(Select::MethodCall(m)) => {
            markers_in_closures(m.value.as_mut(), converted);
            for a in &mut m.arguments {
                markers_in_closures(a, converted);
            }
        }
        RValue::IfExpression(e) => {
            markers_in_closures(e.condition.as_mut(), converted);
            markers_in_closures(e.then_value.as_mut(), converted);
            markers_in_closures(e.else_value.as_mut(), converted);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Break, Closure, Empty, Function, Global, Index, Local};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use rustc_hash::FxHashSet;

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
            left: vec![LValue::Local(local.clone())],
            right: vec![value],
            prefix,
            parallel: false,
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
            value_anchor: ValueAnchor::AtResultDecl,
            prefix_len: 0,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: FxHashSet::default(),
            locals,
            param_order: Vec::new(),
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

        assert!(try_unify_site(&target, &cand).is_none());
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
        assert!(classify_returns(&body).is_none());
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
        assert!(matches!(classify_returns(&body), Some(TKind::Value)));
    }

    /// P7-A: a call/method-call leaf (`return g(x)`) is an admissible Value leaf —
    /// the single-LHS `RESULT = g(x)` inlined site truncates it to one value, so
    /// the candidate shape proves the arity. (Refused pre-P7-A.)
    #[test]
    fn call_leaf_is_an_admissible_value_leaf_p7a() {
        let c = local("c");
        let body = vec![
            Statement::If(If::new(
                local_value(&c),
                Block(vec![return_one(RValue::Call(Call::new(
                    global("g"),
                    vec![local_value(&c)],
                )))]),
                Block(vec![return_one(RValue::Literal(Literal::Nil))]),
            )),
        ];
        let pat = canon(&body);
        assert!(value_leaf_shape(&pat), "call leaf must be admissible");
        assert!(matches!(classify_returns(&body), Some(TKind::Value)));
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
            try_unify_site(&t, &cand).is_none(),
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

        // is_func_body_top = true AND the window is the whole body -> refused.
        assert!(
            match_void(&cand, 0, &t, false, true, &mut None).is_none(),
            "replacing a function's entire body with one call must be refused"
        );
        // Not the whole body (is_func_body_top = false) -> matches.
        assert!(
            match_void(&cand, 0, &t, false, false, &mut None).is_some(),
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
            value_anchor: ValueAnchor::AtPrefix,
            prefix_len: 1,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params,
            locals: FxHashSet::default(),
            param_order: vec![p.clone()],
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
            match_value_prefixed(&cand, 0, &t, false, &mut None).is_none(),
            "an in-place accumulator with a param-LHS must not de-inline"
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
            left: vec![LValue::Local(l.clone())],
            right: vec![],
            prefix: true,
            parallel: false,
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
        assert!(matches!(pat[0], Statement::Assign(_)), "prefix must be an Assign");
        let mut locals = FxHashSet::default();
        collect_declared_locals(&pat, &mut locals);
        let pat0_kind = std::mem::discriminant(&pat[0]);
        let pat0_anchor_key = stmt_anchor_key(&pat[0]);
        Target {
            f_local: local("f"),
            func_ptr: std::ptr::null::<Mutex<Function>>(),
            kind: TKind::Value,
            pat_raw_len: body.len(),
            value_anchor: ValueAnchor::AtPrefix,
            prefix_len: 1,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: FxHashSet::default(),
            locals,
            param_order: Vec::new(),
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
            value_anchor: ValueAnchor::AtPrefix,
            prefix_len: k,
            pat0_kind,
            pat0_anchor_key,
            pat,
            params: FxHashSet::default(),
            locals,
            param_order: Vec::new(),
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
                    bin(local_value(&place_id), BinaryOperation::GreaterThan, number(0.0)),
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

        let hit = match_value_prefixed(&candidate, 0, &t, false, &mut None)
            .expect("isAfkEnabled prefix + guard-polarity flip should match");
        assert_eq!(hit.consume, 3, "consume prefix + decl + value branch");
        assert_eq!(hit.result, Some(v2));
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

        let hit = match_value_prefixed(&candidate, 0, &t, false, &mut None)
            .expect("if/else value prefix should match without a flip");
        assert_eq!(hit.consume, 3);
        assert_eq!(hit.result, Some(v));
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

        let hit = match_value_prefixed(&candidate, 0, &t, false, &mut None)
            .expect("interposed marker must not break the AtPrefix match");
        // span = prefix(0) + marker(1) + decl(2) + region-if(3): removes 4 stmts,
        // leaving the trailing print.
        assert_eq!(hit.consume, 4);
        assert_eq!(hit.result, Some(v));
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
                bin(local_value(&a), BinaryOperation::GreaterThan, local_value(&b)),
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
                bin(local_value(&a2), BinaryOperation::GreaterThan, local_value(&b2)),
                vec![assign_local(&v, local_value(&a2), false)],
                vec![assign_local(&v, local_value(&b2), false)],
            ),
            print_x(), // trailing: window isn't whole-body
        ];

        let hit = match_value_prefixed(&candidate, 0, &t, false, &mut None)
            .expect("K==2 value prefix should match");
        // span = prefix a2(0) + prefix b2(1) + RESULT decl(2) + value-if(3).
        assert_eq!(hit.consume, 4);
        assert_eq!(hit.result, Some(v));
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
        let hit = match_value_prefixed(&candidate, 0, &t, false, &mut None)
            .expect("relational guard condition IS polarity-flipped under P9");
        assert_eq!(hit.consume, 3); // prefix k2(0) + RESULT decl(1) + value-if(2)
        assert_eq!(hit.result, Some(v));
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
                    bin(local_value(&place_id), BinaryOperation::GreaterThan, number(0.0)),
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
            match_value_prefixed(&candidate, 0, &t, false, &mut None).is_none(),
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
        assert!(unify_rvalue(&ctx, &local_value(&p), &RValue::Table(Table::default()), &mut b).is_ok());
        assert!(
            unify_rvalue(&ctx, &local_value(&p), &RValue::Table(Table::default()), &mut b).is_err(),
            "two distinct `{{}}` arguments must not be shared across param occurrences"
        );

        let x = local("x");
        let mut b2 = Bindings::default();
        assert!(unify_rvalue(&ctx, &local_value(&p), &local_value(&x), &mut b2).is_ok());
        assert!(unify_rvalue(&ctx, &local_value(&p), &local_value(&x), &mut b2).is_ok());
    }

    /// F3: `rvalue_has_closure` descends into `IfExpression` arms, so `body_unsafe`
    /// rejects a body whose returned value hides a closure in a branch
    /// (identity-matching a closure is unsound). The same body without the closure
    /// is safe.
    #[test]
    fn body_unsafe_sees_closure_inside_if_expression() {
        let closure = RValue::Closure(Closure {
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

    /// F2: an indexed-LHS value collapse is refused (it would reorder the target
    /// prefix relative to the moved-in call); a bare-local LHS still collapses.
    #[test]
    fn collapse_refuses_indexed_lhs_keeps_local_lhs() {
        let v = local("v");
        let t = local("t");
        let call = call1(global("f"), number(1.0));

        let indexed = Statement::Assign(Assign {
            left: vec![LValue::Index(Index::new(local_value(&t), string("field")))],
            right: vec![local_value(&v)],
            prefix: false,
            parallel: false,
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

    /// P7-A regression: a multi-value helper (one whose binder is in `multivalue`)
    /// must NOT be collapsed into a multi-value context. `local v = helper(args);
    /// return v` keeps its form (a bare `return helper(args)` would propagate ALL of
    /// the helper's values, where `local v =` truncated to one); same for a MULTI-LHS
    /// `a, b = v`. Single-value contexts (`if v`, single-LHS `x = v`) still collapse.
    #[test]
    fn multivalue_helper_not_spread_into_multivalue_context_p7a() {
        let helper = local("helper");
        let v = local("v");
        // call to the multi-value helper `helper(1)`.
        let call = call1(local_value(&helper), number(1.0));
        let mut multivalue = FxHashSet::default();
        multivalue.insert(helper.clone());
        let empty = FxHashSet::default();

        // `return v` — multi-value context: refused for a multi-value helper,
        // allowed (collapsed) for a scalar one.
        let ret = Statement::Return(Return::new(vec![local_value(&v)]));
        assert!(
            collapse_use(&ret, &v, &call, &multivalue).is_none(),
            "return v must NOT collapse a multi-value helper"
        );
        assert!(
            collapse_use(&ret, &v, &call, &empty).is_some(),
            "return v DOES collapse a scalar helper"
        );

        // MULTI-LHS `a, b = v` — multi-value context: refused for a multi-value helper.
        let a = local("a");
        let b = local("b");
        let multi_lhs = Statement::Assign(Assign {
            left: vec![LValue::Local(a.clone()), LValue::Local(b.clone())],
            right: vec![local_value(&v)],
            prefix: false,
            parallel: false,
        });
        assert!(
            collapse_use(&multi_lhs, &v, &call, &multivalue).is_none(),
            "multi-LHS a,b = v must NOT collapse a multi-value helper"
        );

        // SINGLE-LHS `x = v` and `if v` stay sound (truncate to one value) even for
        // a multi-value helper.
        let x = local("x");
        let single_lhs = assign_local(&x, local_value(&v), false);
        assert!(
            collapse_use(&single_lhs, &v, &call, &multivalue).is_some(),
            "single-LHS x = v collapses even a multi-value helper (truncates)"
        );
        let if_v = if_stmt(local_value(&v), vec![print_x()], vec![]);
        assert!(
            collapse_use(&if_v, &v, &call, &multivalue).is_some(),
            "if v collapses even a multi-value helper (single-value condition)"
        );
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

        assert_eq!(canon_top(&marked, true).len(), 1, "marker dropped by canon_top");
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
                3 => print_x(),                                                       // plain stmt
                4 => void_return(),                                                   // void return
                5 => return_one(number(1.0)),                                         // value return
                6 => if_stmt(global("c"), vec![void_return()], vec![]),               // foldable void guard
                7 => if_stmt(global("c"), vec![return_one(number(2.0))], vec![]),     // foldable value guard
                8 => if_stmt(global("c"), vec![void_return()], vec![print_x()]),      // else nonempty
                9 => if_stmt(global("c"), vec![print_x(), void_return()], vec![]),    // then len 2
                10 => if_stmt(global("c"), vec![print_x()], vec![]),                  // then non-return
                _ => if_stmt(
                    global("c"),
                    vec![Statement::Return(Return::new(vec![number(1.0), number(2.0)]))],
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
}
