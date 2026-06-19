//! Normalizes boolean / condition shapes into idiomatic, source-like Luau
//! (proposal §10 and §2.4). Three independent, value-exact rewrites applied
//! bottom-up over every expression in the module (a post-order rewrite walk per
//! statement, plus a separate descent into closure bodies and nested statement
//! blocks):
//!
//! ## Part A — collapse reconstructed `IfExpression` ternaries
//!
//! ```text
//! if C then false else true   ->  not C            (always)
//! if C then true  else false  ->  C                (only if C is boolean)
//! if C then V     else false  ->  C and V          (only if C is boolean)
//! if C then true  else V      ->  C or V           (only if C is boolean)
//! ```
//!
//! The `if C then V else false` case is the §10 flagship: `return if
//! typeof(lock) == "table" then lock.IsBlocked == true else false` becomes
//! `return typeof(lock) == "table" and lock.IsBlocked == true`.
//!
//! ## Part B — value-safe `not (...)` normalization
//!
//! ```text
//! not (a == b)   ->  a ~= b                         (always — comparators are boolean)
//! not (a ~= b)   ->  a == b
//! not (not X)    ->  X                              (only if X is boolean)
//! not (a and b)  ->  not a or not b                 (De Morgan, gated for readability)
//! not (a or b)   ->  not a and not b
//! ```
//!
//! This realizes the other §10 flagship: `if not (typeof(p) == "table" and
//! #p ~= 0) then` becomes `if typeof(p) ~= "table" or #p == 0 then`.
//!
//! ## Part C — left-reassociate same-operator `and`/`or` spines (§2.4)
//!
//! ```text
//! a or (b or (c or d))   ->  ((a or b) or c) or d   (printed `a or b or c or d`)
//! ```
//!
//! Luau parses `a or b or c` left-associatively, but the decompiler builds the
//! spine right-leaning, so the formatter's right-group rule emits redundant
//! parens (`a or (b or c)`). Flattening the maximal same-operator spine and
//! rebuilding it left-leaning drops those parens. `and`/`or` are fully
//! associative in Lua including short-circuit and side-effect order, so this is
//! value-exact; operators are never mixed (those parens are required).
//!
//! ## Why this is correct (and why it does NOT use `flatten_guards::negate`)
//!
//! Every rewrite preserves the concrete *value*, not merely truthiness, so it is
//! safe in value context as well as condition context:
//!
//! * The `and`/`or` collapses are gated on `C` being provably boolean
//!   ([`crate::binary::is_boolean`]). With `C ∈ {true, false}`, `if C then V else
//!   false` and `C and V` agree on both branches (the falsy branch yields the
//!   literal `false` either way); a non-boolean `C` could make `C and V` surface
//!   `C`'s own falsy value (e.g. `nil`) where the ternary yields `false`, so we
//!   refuse it. `if C then false else true -> not C` needs no gate because `not`
//!   already booleanizes.
//! * De Morgan `not (a and b) ≡ not a or not b` is an exact Lua identity (both
//!   sides are always boolean). We build the negated operands with literal `not`
//!   wrapping via the recursive [`normalize_not`] — NOT via
//!   `flatten_guards::negate`, which strips `not (not X) -> X` *unconditionally*
//!   and would de-booleanize a non-boolean `X` in value context (`not (not 0)` is
//!   `true`, but `0` is not). The double-`not` strip here is gated on
//!   `is_boolean(X)` for exactly that reason.
//! * Relational operators (`< <= > >=`) are NEVER flipped: `a >= b` differs from
//!   `not (a < b)` when an operand is NaN. A `not (a < b)` is kept verbatim. This
//!   is also why the pass must NOT call `reduce`/`reduce_condition` (which *do*
//!   flip them) and must run before `recover_guard_continue`.
//!
//! Rewrites are positional (operands keep their order; `and`/`or` short-circuit
//! exactly like the ternary), so no side-effect / evaluation-order analysis is
//! needed. Each Part A collapse removes an `IfExpression`; Part B strips one
//! boolean-operator level per recursion — so a single post-order pass converges
//! with no fixpoint loop.

use crate::{
    binary::is_boolean, Binary, BinaryOperation, Block, IfExpression, Literal, RValue, Statement,
    Traverse, Unary, UnaryOperation,
};

/// See module docs. Walks `block`, every nested block, and every closure body.
pub fn normalize_conditions(block: &mut Block) {
    for statement in &mut block.0 {
        normalize_in_statement(statement);
    }
}

fn normalize_in_statement(statement: &mut Statement) {
    // Closures embedded in this statement's expressions are independent scopes;
    // `post_traverse_rvalues` stops at the `Closure` node (empty `Traverse`
    // impl), so descend into their bodies explicitly.
    normalize_closures_in_statement(statement);

    // Nested statement blocks are not reached by `post_traverse_rvalues` either.
    match statement {
        Statement::If(r#if) => {
            normalize_conditions(&mut r#if.then_block.lock());
            normalize_conditions(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => normalize_conditions(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => normalize_conditions(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => normalize_conditions(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => normalize_conditions(&mut generic_for.block.lock()),
        _ => {}
    }

    // Rewrite every RValue in this statement's own expressions — including the
    // if/while/repeat header condition (which is an rvalue of the statement) and
    // any IfExpression/`not` nested arbitrarily deep. Post-order guarantees
    // children are normalized before their parent, so cascades resolve in one
    // pass.
    statement.post_traverse_rvalues(&mut |rvalue: &mut RValue| -> Option<()> {
        normalize_node(rvalue);
        None
    });
}

fn normalize_closures_in_statement(statement: &mut Statement) {
    let mut functions = Vec::new();
    statement.post_traverse_rvalues(&mut |rvalue| -> Option<()> {
        if let RValue::Closure(closure) = rvalue {
            functions.push(closure.function.clone());
        }
        None
    });
    for function in functions {
        normalize_conditions(&mut function.lock().body);
    }
}

/// Apply the single-node rewrites. Children are already normalized (post-order).
fn normalize_node(rvalue: &mut RValue) {
    match rvalue {
        RValue::IfExpression(_) => {
            let RValue::IfExpression(if_expr) =
                std::mem::replace(rvalue, RValue::Literal(Literal::Nil))
            else {
                unreachable!()
            };
            *rvalue = collapse_if_expression(if_expr);
        }
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => {
            let RValue::Unary(unary) = std::mem::replace(rvalue, RValue::Literal(Literal::Nil))
            else {
                unreachable!()
            };
            *rvalue = normalize_not(*unary.value);
        }
        // Part C (§2.4) — left-reassociate a same-operator `and`/`or` spine.
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::And | BinaryOperation::Or
            ) =>
        {
            let operation = binary.operation;
            let RValue::Binary(binary) =
                std::mem::replace(rvalue, RValue::Literal(Literal::Nil))
            else {
                unreachable!()
            };
            *rvalue = reassociate_left(binary, operation);
        }
        _ => {}
    }
}

/// Part C (§2.4). Flatten the maximal contiguous same-operator (`and`/`or`)
/// spine rooted at `binary` into its operands in left-to-right evaluation order,
/// then rebuild it LEFT-leaning: `((o0 OP o1) OP o2) OP o3 ...`.
///
/// Luau parses `a or b or c` left-associatively as `(a or b) or c`, but the
/// decompiler reconstructs the spine right-leaning (`a or (b or c)`), which the
/// formatter then prints with redundant parens (`a or (b or c)`) because its
/// right-group rule parenthesizes a same-precedence right child. Rebuilding the
/// spine left-leaning matches the parse and drops those parens.
///
/// SOUNDNESS: `and`/`or` are fully associative in Lua *including* short-circuit
/// and side-effect order — `(a or b) or c` and `a or (b or c)` both evaluate
/// `a`, then `b`, then `c`, yielding the identical concrete value (the first
/// truthy operand for `or` / first falsy for `and`, else the last). This is a
/// pure structural move: it never crosses an `and`/`or` boundary (those parens
/// are semantically required), never compares (no NaN concern), and does not
/// call `reduce`/`reduce_condition`. A single rotation is deliberately NOT used
/// — it leaves a residual nested pair for spines of depth >= 3; flatten-then-
/// fold-left handles arbitrary depth.
fn reassociate_left(binary: Binary, operation: BinaryOperation) -> RValue {
    let mut operands = Vec::new();
    collect_spine(RValue::Binary(binary), operation, &mut operands);

    let mut iter = operands.into_iter();
    // The spine always has at least two operands (it started as a `Binary`).
    let first = iter.next().expect("spine has at least one operand");
    iter.fold(first, |acc, next| Binary::new(acc, next, operation).into())
}

/// Append the operands of the maximal same-operator spine of `rvalue` to
/// `operands`, in left-to-right order. Operands that are themselves a `Binary`
/// with the SAME `operation` are recursed into (flattened); any other shape —
/// including an `and`/`or` of the *other* operator — is pushed as a single leaf.
/// Children were already normalized by the post-order walk, so this neither
/// re-normalizes them nor needs a fixpoint.
fn collect_spine(rvalue: RValue, operation: BinaryOperation, operands: &mut Vec<RValue>) {
    match rvalue {
        RValue::Binary(binary) if binary.operation == operation => {
            collect_spine(*binary.left, operation, operands);
            collect_spine(*binary.right, operation, operands);
        }
        other => operands.push(other),
    }
}

/// Part A. Collapse an owned `IfExpression`; returns the original (rebuilt) when
/// no rule applies — e.g. `else nil`, or a non-boolean condition.
fn collapse_if_expression(if_expr: IfExpression) -> RValue {
    let IfExpression {
        condition,
        then_value,
        else_value,
    } = if_expr;
    let condition = *condition;
    let then_value = *then_value;
    let else_value = *else_value;

    let then_true = is_bool_lit(&then_value, true);
    let then_false = is_bool_lit(&then_value, false);
    let else_true = is_bool_lit(&else_value, true);
    let else_false = is_bool_lit(&else_value, false);

    // `if C then false else true` -> `not C`  (unconditional; `not` booleanizes).
    if then_false && else_true {
        return normalize_not(condition);
    }
    // `if C then true else false` -> `C`  (only when C is already boolean;
    // checked before the `else false` rule so we emit `C`, not `C and true`).
    if then_true && else_false && is_boolean(&condition) {
        return condition;
    }
    // `if C then V else false` -> `C and V`  (only when C is boolean).
    if else_false && is_boolean(&condition) {
        return Binary::new(condition, then_value, BinaryOperation::And).into();
    }
    // `if C then true else V` -> `C or V`  (only when C is boolean).
    if then_true && is_boolean(&condition) {
        return Binary::new(condition, else_value, BinaryOperation::Or).into();
    }

    // No rule fired: faithfully restore the ternary (covers `else nil`, a
    // non-boolean condition, the unhandled `then nil`/`then false else V`
    // shapes, etc.).
    IfExpression::new(condition, then_value, else_value).into()
}

/// Part B. Returns the value-exact, simplified form of `not inner`. `inner` is
/// already normalized.
fn normalize_not(inner: RValue) -> RValue {
    match inner {
        // not (a == b) -> a ~= b ; not (a ~= b) -> a == b. Always exact:
        // `==`/`~=` are total booleans and exact complements (even for NaN).
        RValue::Binary(binary) if binary.operation == BinaryOperation::Equal => {
            Binary::new(*binary.left, *binary.right, BinaryOperation::NotEqual).into()
        }
        RValue::Binary(binary) if binary.operation == BinaryOperation::NotEqual => {
            Binary::new(*binary.left, *binary.right, BinaryOperation::Equal).into()
        }
        // not true -> false, not false -> true. Exact, and avoids leaving a
        // `not true` behind when De Morgan negates a boolean-literal operand.
        RValue::Literal(Literal::Boolean(b)) => RValue::Literal(Literal::Boolean(!b)),
        // not (not X) -> X, only when X is provably boolean (else `X` would drop
        // the booleanization the double `not` performs).
        RValue::Unary(unary)
            if unary.operation == UnaryOperation::Not && is_boolean(&unary.value) =>
        {
            *unary.value
        }
        // De Morgan, gated on net simplification (readability only — the identity
        // is exact regardless): `not (a and b)` -> `not a or not b`, etc. The
        // operand negations recurse through `normalize_not`, so a `not X` operand
        // is kept value-safe (NOT stripped via `negate`).
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::And | BinaryOperation::Or
            ) && (negation_simplifies(&binary.left) || negation_simplifies(&binary.right)) =>
        {
            let flipped = if binary.operation == BinaryOperation::And {
                BinaryOperation::Or
            } else {
                BinaryOperation::And
            };
            let left = normalize_not(*binary.left);
            let right = normalize_not(*binary.right);
            Binary::new(left, right, flipped).into()
        }
        // Relational comparators, plain locals/fields/calls, non-boolean `not`,
        // literals: keep an explicit `not (...)`. Relational is deliberately
        // never flipped (NaN-unsafe).
        other => Unary::new(other, UnaryOperation::Not).into(),
    }
}

/// Readability gate for De Morgan: would negating `x` collapse it (flip a `==`,
/// strip a boolean double-`not`) rather than just wrap it in a fresh `not`? This
/// keeps `not (p and p.Parent)` (plain guard) untouched while expanding
/// `not (a == 1 and b == 2)`.
fn negation_simplifies(x: &RValue) -> bool {
    match x {
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => is_boolean(&unary.value),
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::Equal | BinaryOperation::NotEqual
            ) =>
        {
            true
        }
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::And | BinaryOperation::Or
            ) =>
        {
            negation_simplifies(&binary.left) || negation_simplifies(&binary.right)
        }
        _ => false,
    }
}

fn is_bool_lit(value: &RValue, expected: bool) -> bool {
    matches!(value, RValue::Literal(Literal::Boolean(b)) if *b == expected)
}

#[cfg(test)]
mod tests {
    use super::normalize_conditions;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Global, IfExpression, Index, LValue, Literal,
        Local, RValue, RcLocal, Return, Statement, Unary, UnaryOperation,
    };

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn lv(local: &RcLocal) -> RValue {
        RValue::Local(local.clone())
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global(name.as_bytes().to_vec()))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn boolean(value: bool) -> RValue {
        RValue::Literal(Literal::Boolean(value))
    }

    fn nil() -> RValue {
        RValue::Literal(Literal::Nil)
    }

    fn field(receiver: &RcLocal, key: &str) -> RValue {
        RValue::Index(Index::new(lv(receiver), string(key)))
    }

    fn eq(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::Equal).into()
    }

    fn ne(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::NotEqual).into()
    }

    fn lt(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::LessThan).into()
    }

    fn and(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::And).into()
    }

    fn or(a: RValue, b: RValue) -> RValue {
        Binary::new(a, b, BinaryOperation::Or).into()
    }

    fn not(a: RValue) -> RValue {
        Unary::new(a, UnaryOperation::Not).into()
    }

    fn if_expr(condition: RValue, then_value: RValue, else_value: RValue) -> RValue {
        RValue::IfExpression(IfExpression::new(condition, then_value, else_value))
    }

    fn ret(value: RValue) -> Statement {
        Return::new(vec![value]).into()
    }

    fn assign_field(receiver: &RcLocal, key: &str, value: RValue) -> Statement {
        Assign::new(
            vec![LValue::Index(Index::new(lv(receiver), string(key)))],
            vec![value],
        )
        .into()
    }

    fn render(statement: Statement) -> String {
        let mut block = Block(vec![statement]);
        normalize_conditions(&mut block);
        block.to_string()
    }

    // ---- Part A: IfExpression collapse ----

    #[test]
    fn comparator_then_value_else_false_becomes_and() {
        // return if typeof(p) == "number" then p >= 1 else false
        let p = local("p");
        let cond = eq(
            Call::new(global("typeof"), vec![lv(&p)]).into(),
            string("number"),
        );
        let then = Binary::new(lv(&p), number(1.0), BinaryOperation::GreaterThanOrEqual).into();
        assert_eq!(
            render(ret(if_expr(cond, then, boolean(false)))),
            "return typeof(p) == \"number\" and p >= 1"
        );
    }

    #[test]
    fn and_chain_of_comparators_else_false_becomes_and() {
        // return if (typeof(p) == "number" and p >= 1) then p <= 6 else false
        let p = local("p");
        let cond = and(
            eq(
                Call::new(global("typeof"), vec![lv(&p)]).into(),
                string("number"),
            ),
            Binary::new(lv(&p), number(1.0), BinaryOperation::GreaterThanOrEqual).into(),
        );
        let then = Binary::new(lv(&p), number(6.0), BinaryOperation::LessThanOrEqual).into();
        assert_eq!(
            render(ret(if_expr(cond, then, boolean(false)))),
            "return typeof(p) == \"number\" and p >= 1 and p <= 6"
        );
    }

    #[test]
    fn then_true_else_false_becomes_condition() {
        // return if a == b then true else false  ->  return a == b
        let a = local("a");
        let b = local("b");
        assert_eq!(
            render(ret(if_expr(eq(lv(&a), lv(&b)), boolean(true), boolean(false)))),
            "return a == b"
        );
    }

    #[test]
    fn then_false_else_true_becomes_not_even_for_nonboolean() {
        // return if p then false else true  ->  return not p   (always safe)
        let p = local("p");
        assert_eq!(
            render(ret(if_expr(lv(&p), boolean(false), boolean(true)))),
            "return not p"
        );
    }

    #[test]
    fn then_false_else_true_flips_equality() {
        // return if a == b then false else true  ->  return a ~= b
        let a = local("a");
        let b = local("b");
        assert_eq!(
            render(ret(if_expr(eq(lv(&a), lv(&b)), boolean(false), boolean(true)))),
            "return a ~= b"
        );
    }

    #[test]
    fn comparator_then_true_else_value_becomes_or() {
        // return if x ~= nil then true else v  ->  return x ~= nil or v
        let x = local("x");
        let v = local("v");
        assert_eq!(
            render(ret(if_expr(ne(lv(&x), nil()), boolean(true), lv(&v)))),
            "return x ~= nil or v"
        );
    }

    #[test]
    fn refuses_nonboolean_condition_else_false() {
        // Mirrors BlackMarket.luau:1035. C = p._marketFrame (field, not boolean).
        let p = local("p");
        let market_frame_visible =
            RValue::Index(Index::new(field(&p, "_marketFrame"), string("Visible")));
        let then = or(market_frame_visible, boolean(false));
        assert_eq!(
            render(ret(if_expr(field(&p, "_marketFrame"), then, boolean(false)))),
            "return if p._marketFrame then p._marketFrame.Visible or false else false"
        );
    }

    #[test]
    fn refuses_plain_local_condition_else_false() {
        let p = local("p");
        let v = local("v3");
        assert_eq!(
            render(ret(if_expr(lv(&p), lv(&v), boolean(false)))),
            "return if p then v3 else false"
        );
    }

    #[test]
    fn refuses_else_nil_value_context() {
        // false vs nil are distinct values; never collapse in value context.
        let a = local("a");
        let b = local("b");
        let v = local("v");
        assert_eq!(
            render(ret(if_expr(eq(lv(&a), lv(&b)), lv(&v), nil()))),
            "return if a == b then v else nil"
        );
    }

    #[test]
    fn refuses_call_condition_else_false() {
        // C = isReady() — a call result is not provably boolean.
        let v = local("v");
        let cond = Call::new(global("isReady"), vec![]).into();
        assert_eq!(
            render(ret(if_expr(cond, lv(&v), boolean(false)))),
            "return if isReady() then v else false"
        );
    }

    // ---- Part B: not-normalization / De Morgan ----

    #[test]
    fn demorgan_not_equality_or_flips_to_and_notequal() {
        // not (p5 == 2 or p5 == 3)  ->  p5 ~= 2 and p5 ~= 3
        let p5 = local("p5");
        assert_eq!(
            render(ret(not(or(
                eq(lv(&p5), number(2.0)),
                eq(lv(&p5), number(3.0))
            )))),
            "return p5 ~= 2 and p5 ~= 3"
        );
    }

    #[test]
    fn demorgan_not_notequal_and_flips_to_equal_or() {
        // not (a ~= b and c ~= d)  ->  a == b or c == d
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        assert_eq!(
            render(ret(not(and(ne(lv(&a), lv(&b)), ne(lv(&c), lv(&d)))))),
            "return a == b or c == d"
        );
    }

    #[test]
    fn demorgan_flagship_typeof_table_length() {
        // not (typeof(p) == "table" and #p ~= 0)  ->  typeof(p) ~= "table" or #p == 0
        let p = local("p");
        let typeof_table = eq(
            Call::new(global("typeof"), vec![lv(&p)]).into(),
            string("table"),
        );
        let len_ne_zero = ne(
            Unary::new(lv(&p), UnaryOperation::Length).into(),
            number(0.0),
        );
        assert_eq!(
            render(ret(not(and(typeof_table, len_ne_zero)))),
            "return typeof(p) ~= \"table\" or #p == 0"
        );
    }

    #[test]
    fn demorgan_folds_negated_boolean_literal_operand() {
        // not (a == b and true)  ->  a ~= b or false  (no `not true` left behind)
        let a = local("a");
        let b = local("b");
        assert_eq!(
            render(ret(not(and(eq(lv(&a), lv(&b)), boolean(true))))),
            "return a ~= b or false"
        );
    }

    #[test]
    fn not_equality_flips() {
        // not (a == b)  ->  a ~= b
        let a = local("a");
        let b = local("b");
        assert_eq!(render(ret(not(eq(lv(&a), lv(&b))))), "return a ~= b");
    }

    #[test]
    fn refuses_relational_flip_nan_safety() {
        // not (a < b) must stay `not (a < b)`, never `a >= b` (NaN).
        let a = local("a");
        let b = local("b");
        assert_eq!(render(ret(not(lt(lv(&a), lv(&b))))), "return not (a < b)");
    }

    #[test]
    fn refuses_relational_inside_disjunction() {
        // not (stock >= 1 or stock == -1) — relational present, gate sees the
        // `== -1` and would expand, but the `>=` operand must not be flipped.
        // Expansion yields `not (stock >= 1) and stock ~= -1` (relational kept
        // under an explicit `not`).
        let stock = local("stock");
        let ge = Binary::new(lv(&stock), number(1.0), BinaryOperation::GreaterThanOrEqual).into();
        let eq_neg = eq(lv(&stock), number(-1.0));
        assert_eq!(
            render(ret(not(or(ge, eq_neg)))),
            "return not (stock >= 1) and stock ~= -1"
        );
    }

    #[test]
    fn leaves_plain_field_guard_unchanged() {
        // not (p and p.Parent) — no comparator operands, gate refuses to expand.
        let p = local("p");
        assert_eq!(
            render(ret(not(and(lv(&p), field(&p, "Parent"))))),
            "return not (p and p.Parent)"
        );
    }

    #[test]
    fn double_not_strip_only_when_boolean() {
        // not (not (a == b))  ->  a == b   (inner is boolean -> strip is safe)
        let a = local("a");
        let b = local("b");
        assert_eq!(render(ret(not(not(eq(lv(&a), lv(&b)))))), "return a == b");
    }

    #[test]
    fn double_not_kept_when_nonboolean() {
        // not (not p) with p non-boolean must NOT strip to `p` (value-unsafe:
        // `not (not 0)` is true, not 0). The formatter renders the kept double
        // negation as `not not p` (no parens needed — equal unary precedence).
        let p = local("p");
        assert_eq!(render(ret(not(not(lv(&p))))), "return not not p");
    }

    // ---- traversal / interaction ----

    #[test]
    fn collapses_inside_assignment_field() {
        // v.IsVisible = if a == b then v2 else false
        let v = local("v");
        let a = local("a");
        let b = local("b");
        let v2 = local("v2");
        assert_eq!(
            render(assign_field(
                &v,
                "IsVisible",
                if_expr(eq(lv(&a), lv(&b)), lv(&v2), boolean(false))
            )),
            "v.IsVisible = a == b and v2"
        );
    }

    #[test]
    fn collapses_if_statement_condition() {
        // if (if c == d then false else true) then return end
        // condition collapses to `not (c == d)` -> `c ~= d`.
        let c = local("c");
        let d = local("d");
        let stmt: Statement = crate::If::new(
            if_expr(eq(lv(&c), lv(&d)), boolean(false), boolean(true)),
            Block(vec![Return::new(vec![]).into()]),
            Block(vec![]),
        )
        .into();
        assert_eq!(render(stmt), "if c ~= d then\n\treturn\nend");
    }

    #[test]
    fn idempotent_on_second_run() {
        let p = local("p");
        let input = ret(not(or(eq(lv(&p), number(2.0)), eq(lv(&p), number(3.0)))));
        let mut block = Block(vec![input]);
        normalize_conditions(&mut block);
        let once = block.to_string();
        normalize_conditions(&mut block);
        assert_eq!(once, block.to_string());
        assert_eq!(once, "return p ~= 2 and p ~= 3");
    }

    #[test]
    fn nested_if_expression_in_arm() {
        // if a == b then (if c == d then true else false) else false
        //   inner collapses to `c == d`, then outer to `a == b and c == d`.
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        let inner = if_expr(eq(lv(&c), lv(&d)), boolean(true), boolean(false));
        let outer = if_expr(eq(lv(&a), lv(&b)), inner, boolean(false));
        assert_eq!(render(ret(outer)), "return a == b and c == d");
    }

    #[test]
    fn refuses_then_nil() {
        // `then nil` is not a handled shape -> left verbatim.
        let a = local("a");
        let b = local("b");
        let v = local("v");
        assert_eq!(
            render(ret(if_expr(eq(lv(&a), lv(&b)), nil(), lv(&v)))),
            "return if a == b then nil else v"
        );
    }

    #[test]
    fn refuses_then_false_else_value() {
        // `then false else <non-true>` is not a handled shape -> left verbatim.
        let a = local("a");
        let b = local("b");
        let v = local("v");
        assert_eq!(
            render(ret(if_expr(eq(lv(&a), lv(&b)), boolean(false), lv(&v)))),
            "return if a == b then false else v"
        );
    }

    #[test]
    fn normalizes_inside_while_condition() {
        // while not (a == b) do ... -> while a ~= b do ...
        let a = local("a");
        let b = local("b");
        let stmt: Statement = crate::While::new(
            not(eq(lv(&a), lv(&b))),
            Block(vec![Return::new(vec![]).into()]),
        )
        .into();
        assert!(
            render(stmt).starts_with("while a ~= b do"),
            "while condition not normalized"
        );
    }

    #[test]
    fn normalizes_inside_for_loop_body() {
        // for x in pairs do return if a == b then true else false end
        //   -> body return collapses to `return a == b`.
        let a = local("a");
        let b = local("b");
        let body = vec![ret(if_expr(eq(lv(&a), lv(&b)), boolean(true), boolean(false)))];
        let stmt: Statement =
            crate::GenericFor::new(vec![local("x")], vec![global("pairs")], Block(body)).into();
        assert!(
            render(stmt).contains("return a == b"),
            "for-loop body not normalized"
        );
    }

    #[test]
    fn normalizes_inside_closure_body() {
        use crate::{Closure, Function};
        use by_address::ByAddress;
        use parking_lot::Mutex;
        use triomphe::Arc;

        // A closure embedded as a call argument: its body must be normalized.
        let a = local("a");
        let b = local("b");
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![ret(if_expr(
                eq(lv(&a), lv(&b)),
                boolean(false),
                boolean(true),
            ))]),
            ..Function::default()
        }));
        let closure = RValue::Closure(Closure {
            function: ByAddress(function),
            upvalues: vec![],
        });
        let mut block = Block(vec![Call::new(global("print"), vec![closure]).into()]);
        normalize_conditions(&mut block);
        assert!(
            block.to_string().contains("return a ~= b"),
            "closure body not normalized: {}",
            block
        );
    }

    // ---- Part C: left-reassociate same-operator and/or spines (§2.4) ----

    #[test]
    fn reassociates_or_spine_depth3_drops_all_parens() {
        // Decompiler builds the right-leaning spine `a or (b or (c or d))`; it must
        // print without any parens after left-reassociation.
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        let spine = or(lv(&a), or(lv(&b), or(lv(&c), lv(&d))));
        assert_eq!(render(ret(spine)), "return a or b or c or d");
    }

    #[test]
    fn reassociates_and_spine_depth3_drops_all_parens() {
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        let spine = and(lv(&a), and(lv(&b), and(lv(&c), lv(&d))));
        assert_eq!(render(ret(spine)), "return a and b and c and d");
    }

    #[test]
    fn left_leaning_or_spine_is_unchanged_and_unparenthesized() {
        // `(a or b) or c` is already left-leaning; it stays left-leaning and prints
        // without parens (idempotence over the already-correct shape).
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let spine = or(or(lv(&a), lv(&b)), lv(&c));
        assert_eq!(render(ret(spine)), "return a or b or c");
    }

    #[test]
    fn mixed_and_or_keeps_required_parens() {
        // `a and (b or c)` mixes operators — the inner parens are semantically
        // required and must be preserved (never merged across `and`/`or`).
        let a = local("a");
        let b = local("b");
        let c = local("c");
        assert_eq!(
            render(ret(and(lv(&a), or(lv(&b), lv(&c))))),
            "return a and (b or c)"
        );
    }

    #[test]
    fn or_of_and_groups_does_not_flatten_across_operators() {
        // `(a and b) or (c and d)`: the top spine is `or`, whose operands are two
        // `and` groups. Flattening must stop at each `and` (different operator).
        // Lower-precedence `or` over higher-precedence `and` children needs no
        // parens, so this prints flat.
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        assert_eq!(
            render(ret(or(and(lv(&a), lv(&b)), and(lv(&c), lv(&d))))),
            "return a and b or c and d"
        );
    }

    #[test]
    fn and_spine_with_nested_or_operand_keeps_or_parens() {
        // `a and (b and (c or d))`: the `and` spine flattens to `[a, b, (c or d)]`
        // and rebuilds left-leaning; the `(c or d)` operand keeps its required
        // parens because `or` binds looser than the enclosing `and`.
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        let spine = and(lv(&a), and(lv(&b), or(lv(&c), lv(&d))));
        assert_eq!(render(ret(spine)), "return a and b and (c or d)");
    }

    #[test]
    fn corpus_typeof_number_huge_drops_inner_parens() {
        // return typeof(p) == "number" and (p == p and p > -math.huge) and p < math.huge
        // The middle operand is a same-operator `and` group built right-leaning;
        // flattening the whole `and` spine drops the inner parens.
        let p = local("p");
        let math = local("math");
        let typeof_number = eq(
            Call::new(global("typeof"), vec![lv(&p)]).into(),
            string("number"),
        );
        let p_eq_p = eq(lv(&p), lv(&p));
        let neg_huge: RValue =
            Unary::new(field(&math, "huge"), UnaryOperation::Negate).into();
        let p_gt_neg_huge =
            Binary::new(lv(&p), neg_huge, BinaryOperation::GreaterThan).into();
        let inner = and(p_eq_p, p_gt_neg_huge);
        let p_lt_huge =
            Binary::new(lv(&p), field(&math, "huge"), BinaryOperation::LessThan).into();
        // Right-leaning as the decompiler builds it: A and (B and C).
        let spine = and(typeof_number, and(inner, p_lt_huge));
        assert_eq!(
            render(ret(spine)),
            "return typeof(p) == \"number\" and p == p and p > -math.huge and p < math.huge"
        );
    }

    #[test]
    fn reassociation_is_idempotent() {
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        let input = ret(or(lv(&a), or(lv(&b), or(lv(&c), lv(&d)))));
        let mut block = Block(vec![input]);
        normalize_conditions(&mut block);
        let once = block.to_string();
        normalize_conditions(&mut block);
        assert_eq!(once, block.to_string());
        assert_eq!(once, "return a or b or c or d");
    }

    #[test]
    fn reassociation_composes_with_demorgan() {
        // not (a == b or (c == d or e == f))  -> a ~= b and c ~= d and e ~= f
        // De Morgan distributes the `not` over the (right-leaning) `or` spine,
        // producing a right-leaning `and` spine of negated comparators, which is
        // then left-reassociated to print without parens.
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let d = local("d");
        let e = local("e");
        let f = local("f");
        let spine = or(eq(lv(&a), lv(&b)), or(eq(lv(&c), lv(&d)), eq(lv(&e), lv(&f))));
        assert_eq!(
            render(ret(not(spine))),
            "return a ~= b and c ~= d and e ~= f"
        );
    }
}
