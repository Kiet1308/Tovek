//! Exact structural proof shared by statement and expression de-inlining.
//!
//! Parameters bind once, callee locals rename injectively, and external values
//! retain their identity. Statement matching additionally checks the target's
//! proven result shape. Argument evaluation safety and rewrite eligibility stay
//! with the callers; this module only proves structural correspondence.

use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Call, Closure, LValue, MethodCall, RValue, RcLocal, Select, Statement, Table, Upvalue};

use super::canonical::{cond_exact_invertible, negate_canon};
use super::{Target, TKind};

#[derive(Default, Clone)]
pub(crate) struct Bindings {
    pub(crate) params: FxHashMap<RcLocal, RValue>,
    pub(super) locals: FxHashMap<RcLocal, RcLocal>,
    pub(super) locals_rev: FxHashMap<RcLocal, RcLocal>,
    /// For Value targets: the single caller local that every `return X` in the
    /// pattern maps to (i.e. the inlined result local `RESULT`).
    pub(super) result: Option<RcLocal>,
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
/// `value_anchor`, ...). The statement matcher builds one via [`super::Target::ctx`].
pub(crate) struct MatchCtx<'a> {
    pub(crate) params: &'a FxHashSet<RcLocal>,
    pub(crate) locals: &'a FxHashSet<RcLocal>,
}

pub(super) fn unify_block(
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

pub(super) fn unify_stmt(t: &Target, p: &Statement, c: &Statement, b: &mut Bindings) -> Result<(), ()> {
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
pub(super) fn is_identity_producing(rv: &RValue) -> bool {
    match rv {
        RValue::Table(_) | RValue::Closure(_) => true,
        RValue::IfExpression(e) => {
            is_identity_producing(&e.then_value) || is_identity_producing(&e.else_value)
        }
        _ => false,
    }
}
