//! Luau's constant folding, for copies of helpers whose arguments the
//! compiler folded (plan E2).
//!
//! Luau `-O2` inlines a call with a constant argument by binding the
//! parameter to the constant and folding what the body computes from it:
//! `frames(13)` for `local function frames(n) return n / 60 end` leaves the
//! single constant `0.21666666666666667`, `fade(part, 0.5)` leaves `1 - k`
//! as `0.5`. To rebuild such a call the de-inliner solves for the argument
//! and then *proves* it: [`evaluate`] computes what the helper's code gives
//! for that argument, exactly as the compiler folds constants
//! (`Compiler/src/ConstantFolding.cpp` of the pinned luau-compile, release
//! 736), and the rebuilt call stands only where that value is the site's
//! constant bit for bit.
//!
//! The rules are the compiler's, minus everything that could differ when the
//! rebuilt call runs:
//! - numbers: `+ - * /` (IEEE double), `//` as `floor(a / b)`, `%` as
//!   `a - floor(a / b) * b` (only where `floor(a / b) * b` is exact, so a
//!   fused multiply-add in the VM gives the same), unary minus; `^` is
//!   refused (`pow` may differ between the compiler's and the VM's libm);
//! - strings: `..` of two strings up to 4096 bytes in all, `#` of a string;
//! - `==`/`~=` as `constantsEqual` (`0 == -0`, NaN equal to nothing), `<`,
//!   `<=`, `>`, `>=` on numbers only, `not`, `and`/`or` decided by a
//!   constant left side, `if`-expressions decided by a constant condition;
//! - calls only of pure local helpers ([`PureHelpers`]), which the compiler
//!   inlines and folds the same way. A builtin call is never folded: the
//!   compiler folds some (`math.floor(1.5)`), but the rebuilt call would
//!   call the library at run time, which a script may have replaced.
//!
//! Integers (`LBC_CONSTANT_INTEGER`), vectors, tables, globals and any other
//! expression are unknown, and so is a NaN: its payload is the hardware's.

use rustc_hash::FxHashMap;

use crate::{BinaryOperation, Literal, RValue, RcLocal, Select, Statement, Traverse, UnaryOperation};

/// Luau's `kConstantFoldStringLimit`: the longest string `..` folds to.
pub(crate) const STRING_LIMIT: usize = 4096;

/// How deep calls of pure helpers inside one another are followed.
const CALL_DEPTH: usize = 16;

/// A constant the compiler folds: `Constant::Type_Nil`, `Type_Boolean`,
/// `Type_Number` (never NaN here) and `Type_String`.
#[derive(Clone, Debug)]
pub(crate) enum Value {
    Nil,
    Boolean(bool),
    Number(f64),
    String(Vec<u8>),
}

impl Value {
    /// The constant `literal` is, if the evaluator knows its kind.
    pub(crate) fn of(literal: &Literal) -> Option<Value> {
        match literal {
            Literal::Nil => Some(Value::Nil),
            Literal::Boolean(value) => Some(Value::Boolean(*value)),
            Literal::Number(value) => Value::number(*value),
            Literal::String(value) => Some(Value::String(value.clone())),
            Literal::Integer(_) | Literal::Vector(..) | Literal::VectorD(..) => None,
        }
    }

    /// A number, unless it is NaN, whose payload no rule here pins.
    fn number(value: f64) -> Option<Value> {
        (!value.is_nan()).then_some(Value::Number(value))
    }

    /// The literal writing this constant.
    pub(crate) fn literal(&self) -> Literal {
        match self {
            Value::Nil => Literal::Nil,
            Value::Boolean(value) => Literal::Boolean(*value),
            Value::Number(value) => Literal::Number(*value),
            Value::String(value) => Literal::String(value.clone()),
        }
    }

    /// `Constant::isTruthful`.
    fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Boolean(false))
    }

    /// The same constant bit for bit: `-0` is not `0`.
    pub(crate) fn same(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a.to_bits() == b.to_bits(),
            (Value::String(a), Value::String(b)) => a == b,
            _ => false,
        }
    }
}

/// Luau's `constantsEqual`: what `==` folds to.
fn constants_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Nil, Value::Nil) => true,
        (Value::Boolean(a), Value::Boolean(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        _ => false,
    }
}

/// A local function whose whole body is `return V`, `V` computed from its
/// parameters by folding rules only: a call of it with constant arguments
/// is the constant [`evaluate`] gives, here and when the call runs.
#[derive(Clone, Debug)]
pub(crate) struct PureHelper {
    pub(crate) params: Vec<RcLocal>,
    pub(crate) value: RValue,
    /// Its bytecode prototype.
    pub(crate) proto: Option<usize>,
}

/// The pure helpers of a module, by binder (each assigned only by its
/// declaration).
#[derive(Default, Debug)]
pub(crate) struct PureHelpers(FxHashMap<RcLocal, PureHelper>);

impl PureHelpers {
    /// The pure helpers among `declarations` (`local f = function ... end`
    /// with the binder's write count in `write_counts`): a non-variadic
    /// function with parameters whose body is one `return V`, `V`
    /// [`foldable`] over its parameters and the pure helpers it calls.
    pub(crate) fn of(
        declarations: &[(RcLocal, triomphe::Arc<parking_lot::Mutex<crate::Function>>)],
        write_counts: &FxHashMap<RcLocal, usize>,
    ) -> PureHelpers {
        let mut candidates: Vec<(RcLocal, PureHelper)> = Vec::new();
        for (binder, function) in declarations {
            if write_counts.get(binder).copied() != Some(1) {
                continue;
            }
            let function = function.lock();
            if function.is_variadic || function.parameters.is_empty() {
                continue;
            }
            let mut statements = function.body.0.iter().filter(|statement| !matches!(statement, Statement::Empty(_)));
            let (Some(Statement::Return(ret)), None) = (statements.next(), statements.next()) else { continue };
            let [value] = ret.values.as_slice() else { continue };
            candidates.push((
                binder.clone(),
                PureHelper { params: function.parameters.clone(), value: value.clone(), proto: function.bytecode_proto_id },
            ));
        }
        // A helper calling another one is pure once that one is.
        let mut pure = PureHelpers::default();
        loop {
            let before = pure.0.len();
            for (binder, helper) in &candidates {
                if !pure.0.contains_key(binder) && foldable(&helper.value, &|local| helper.params.contains(local), &pure) {
                    pure.0.insert(binder.clone(), helper.clone());
                }
            }
            if pure.0.len() == before {
                return pure;
            }
        }
    }

    pub(crate) fn get(&self, binder: &RcLocal) -> Option<&PureHelper> {
        self.0.get(binder)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&RcLocal, &PureHelper)> {
        self.0.iter()
    }
}

/// Whether `value` is made only of what [`evaluate`] folds: literals it
/// knows, locals `is_param` admits, operators but `^`, `if`-expressions and
/// calls of pure helpers with as many arguments as parameters.
pub(crate) fn foldable(value: &RValue, is_param: &dyn Fn(&RcLocal) -> bool, helpers: &PureHelpers) -> bool {
    match value {
        RValue::Literal(literal) => Value::of(literal).is_some(),
        RValue::Local(local) => is_param(local),
        RValue::Unary(unary) => foldable(&unary.value, is_param, helpers),
        RValue::Binary(binary) => {
            binary.operation != BinaryOperation::Pow
                && foldable(&binary.left, is_param, helpers)
                && foldable(&binary.right, is_param, helpers)
        }
        RValue::IfExpression(select) => {
            foldable(&select.condition, is_param, helpers)
                && foldable(&select.then_value, is_param, helpers)
                && foldable(&select.else_value, is_param, helpers)
        }
        RValue::Call(call) | RValue::Select(Select::Call(call)) => {
            matches!(call.value.as_ref(), RValue::Local(callee)
                if helpers.get(callee).is_some_and(|helper| helper.params.len() == call.arguments.len()))
                && call.arguments.iter().all(|argument| foldable(argument, is_param, helpers))
        }
        _ => false,
    }
}

/// The constant the compiler folds `value` to, the locals read through
/// `env`; `None` where it folds to no constant or the rules here refuse.
pub(crate) fn evaluate(value: &RValue, env: &dyn Fn(&RcLocal) -> Option<Value>, helpers: &PureHelpers) -> Option<Value> {
    evaluate_at(value, env, helpers, 0)
}

fn evaluate_at(value: &RValue, env: &dyn Fn(&RcLocal) -> Option<Value>, helpers: &PureHelpers, depth: usize) -> Option<Value> {
    match value {
        RValue::Literal(literal) => Value::of(literal),
        RValue::Local(local) => env(local),
        RValue::Unary(unary) => fold_unary(unary.operation, &evaluate_at(&unary.value, env, helpers, depth)?),
        RValue::Binary(binary) => {
            let left = evaluate_at(&binary.left, env, helpers, depth)?;
            match binary.operation {
                // `la.isTruthful() ? ra : la`, whatever the right side is.
                BinaryOperation::And if !left.truthy() => Some(left),
                BinaryOperation::Or if left.truthy() => Some(left),
                BinaryOperation::And | BinaryOperation::Or => evaluate_at(&binary.right, env, helpers, depth),
                operation => fold_binary(operation, &left, &evaluate_at(&binary.right, env, helpers, depth)?),
            }
        }
        RValue::IfExpression(select) => {
            if evaluate_at(&select.condition, env, helpers, depth)?.truthy() {
                evaluate_at(&select.then_value, env, helpers, depth)
            } else {
                evaluate_at(&select.else_value, env, helpers, depth)
            }
        }
        RValue::Call(call) | RValue::Select(Select::Call(call)) => {
            let RValue::Local(callee) = call.value.as_ref() else { return None };
            let helper = helpers.get(callee)?;
            if depth >= CALL_DEPTH || helper.params.len() != call.arguments.len() {
                return None;
            }
            let arguments = call
                .arguments
                .iter()
                .map(|argument| evaluate_at(argument, env, helpers, depth))
                .collect::<Option<Vec<Value>>>()?;
            let inner = |local: &RcLocal| helper.params.iter().position(|param| param == local).map(|at| arguments[at].clone());
            evaluate_at(&helper.value, &inner, helpers, depth + 1)
        }
        _ => None,
    }
}

/// `foldUnary`.
fn fold_unary(operation: UnaryOperation, value: &Value) -> Option<Value> {
    match (operation, value) {
        (UnaryOperation::Not, value) => Some(Value::Boolean(!value.truthy())),
        (UnaryOperation::Negate, Value::Number(value)) => Value::number(-value),
        (UnaryOperation::Length, Value::String(value)) => Value::number(value.len() as f64),
        _ => None,
    }
}

/// `foldBinary`, `and`/`or` aside ([`evaluate_at`]).
fn fold_binary(operation: BinaryOperation, left: &Value, right: &Value) -> Option<Value> {
    use BinaryOperation::*;
    match (operation, left, right) {
        (Equal, left, right) => Some(Value::Boolean(constants_equal(left, right))),
        (NotEqual, left, right) => Some(Value::Boolean(!constants_equal(left, right))),
        (Concat, Value::String(a), Value::String(b)) if a.len() + b.len() <= STRING_LIMIT => {
            Some(Value::String([a.as_slice(), b.as_slice()].concat()))
        }
        (operation, Value::Number(a), Value::Number(b)) => {
            let (a, b) = (*a, *b);
            match operation {
                Add => Value::number(a + b),
                Sub => Value::number(a - b),
                Mul => Value::number(a * b),
                Div => Value::number(a / b),
                IDiv => Value::number((a / b).floor()),
                Mod => {
                    // `a - floor(a / b) * b`: the product must be exact, or
                    // a VM contracting it into a fused multiply-add would
                    // round once where the compiler rounded twice.
                    let quotient = (a / b).floor();
                    let product = quotient * b;
                    (quotient.mul_add(b, -product) == 0.0).then_some(())?;
                    Value::number(a - product)
                }
                LessThan => Some(Value::Boolean(a < b)),
                LessThanOrEqual => Some(Value::Boolean(a <= b)),
                GreaterThan => Some(Value::Boolean(a > b)),
                GreaterThanOrEqual => Some(Value::Boolean(a >= b)),
                // `pow` is the C library's, which may differ.
                Pow | Concat | Equal | NotEqual | And | Or => None,
            }
        }
        _ => None,
    }
}

/// How many times `value` reads `local` (function bodies aside).
pub(crate) fn reads(value: &RValue, local: &RcLocal) -> usize {
    match value {
        RValue::Local(read) => usize::from(read == local),
        RValue::Closure(_) => 0,
        _ => {
            let mut count = 0;
            value.visit_rvalues(&mut |child| {
                count += reads(child, local);
                true
            });
            count
        }
    }
}

/// The value of `unknown` that makes `value` fold to `goal` bit for bit,
/// the other locals read through `env`: `None` where none is found.
///
/// `unknown` must be read once. Inverting the operations on the way to it
/// (`n / 60 = v` gives `n = v * 60`, through the calls of pure helpers too)
/// proposes a value; around it, the shortest decimals first (integers
/// among them), and never more than 17 significant digits, the first one
/// [`evaluate`] proves is the argument. An operation with no inverse
/// (`//`, `%`, comparisons) proposes nothing.
pub(crate) fn solve(
    value: &RValue,
    goal: &Value,
    unknown: &RcLocal,
    env: &dyn Fn(&RcLocal) -> Option<Value>,
    helpers: &PureHelpers,
) -> Option<Value> {
    if reads(value, unknown) != 1 {
        return None;
    }
    let proposed = invert(value, goal.clone(), unknown, env, helpers, 0)?;
    let proves = |candidate: &Value| {
        let with = |local: &RcLocal| if local == unknown { Some(candidate.clone()) } else { env(local) };
        evaluate(value, &with, helpers).is_some_and(|folded| folded.same(goal))
    };
    match proposed {
        Value::Number(seed) => shortest_numbers(seed).map(Value::Number).find(proves),
        other => proves(&other).then_some(other),
    }
}

/// The value `unknown` must have for `value` to be `goal`, by exact
/// inversion of each operation on the path to its one read.
fn invert(
    value: &RValue,
    goal: Value,
    unknown: &RcLocal,
    env: &dyn Fn(&RcLocal) -> Option<Value>,
    helpers: &PureHelpers,
    depth: usize,
) -> Option<Value> {
    use BinaryOperation::*;
    match value {
        RValue::Local(local) if local == unknown => Some(goal),
        RValue::Unary(unary) if unary.operation == UnaryOperation::Negate => {
            let Value::Number(goal) = goal else { return None };
            invert(&unary.value, Value::Number(-goal), unknown, env, helpers, depth)
        }
        RValue::Binary(binary) => {
            let in_left = reads(&binary.left, unknown) > 0;
            let (known, inner) = if in_left { (&binary.right, &binary.left) } else { (&binary.left, &binary.right) };
            let known = evaluate(known, env, helpers)?;
            let proposed = match (binary.operation, &known, &goal) {
                (Add, Value::Number(k), Value::Number(g)) => Value::Number(g - k),
                (Sub, Value::Number(k), Value::Number(g)) => Value::Number(if in_left { g + k } else { k - g }),
                (Mul, Value::Number(k), Value::Number(g)) if *k != 0.0 => Value::Number(g / k),
                (Div, Value::Number(k), Value::Number(g)) => Value::Number(if in_left { g * k } else { k / g }),
                (Concat, Value::String(k), Value::String(g)) => {
                    let rest = if in_left { g.strip_suffix(k.as_slice()) } else { g.strip_prefix(k.as_slice()) };
                    Value::String(rest?.to_vec())
                }
                _ => return None,
            };
            invert(inner, proposed, unknown, env, helpers, depth)
        }
        RValue::Call(call) | RValue::Select(Select::Call(call)) => {
            let RValue::Local(callee) = call.value.as_ref() else { return None };
            let helper = helpers.get(callee)?;
            if depth >= CALL_DEPTH || helper.params.len() != call.arguments.len() {
                return None;
            }
            let at = call.arguments.iter().position(|argument| reads(argument, unknown) > 0)?;
            let mut arguments = Vec::with_capacity(call.arguments.len());
            for (index, argument) in call.arguments.iter().enumerate() {
                arguments.push(if index == at { None } else { Some(evaluate(argument, env, helpers)?) });
            }
            let inner = |local: &RcLocal| {
                helper.params.iter().position(|param| param == local).and_then(|index| arguments[index].clone())
            };
            let parameter = invert(&helper.value, goal, &helper.params[at], &inner, helpers, depth + 1)?;
            invert(&call.arguments[at], parameter, unknown, env, helpers, depth)
        }
        _ => None,
    }
}

/// Numbers around `seed`, the shortest decimals first: for 1 to 17
/// significant digits, `seed` and its four closest neighbors on each side
/// rounded to that many. Each comes once.
fn shortest_numbers(seed: f64) -> impl Iterator<Item = f64> {
    let mut seeds = vec![seed];
    if seed.is_finite() {
        let (mut up, mut down) = (seed, seed);
        for _ in 0..4 {
            up = step(up, true);
            down = step(down, false);
            seeds.push(up);
            seeds.push(down);
        }
    }
    let mut seen: Vec<u64> = Vec::new();
    (1..=17usize)
        .flat_map(move |digits| seeds.clone().into_iter().map(move |seed| round_digits(seed, digits)))
        .filter(move |candidate| {
            let fresh = !candidate.is_nan() && !seen.contains(&candidate.to_bits());
            if fresh {
                seen.push(candidate.to_bits());
            }
            fresh
        })
}

/// The next double after `value` up or down.
fn step(value: f64, up: bool) -> f64 {
    if value == 0.0 {
        let smallest = f64::from_bits(1);
        return if up { smallest } else { -smallest };
    }
    let bits = value.to_bits();
    f64::from_bits(if (value > 0.0) == up { bits + 1 } else { bits - 1 })
}

/// `value` rounded to `digits` significant digits.
fn round_digits(value: f64, digits: usize) -> f64 {
    if !value.is_finite() {
        return value;
    }
    format!("{:.*e}", digits - 1, value).parse().unwrap_or(f64::NAN)
}

/// Equations a match recorded: each pattern value must fold to the site's
/// constant. The parameters in `known` are bound to constants; each other
/// parameter an equation reads is solved where it is the one unknown of an
/// equation ([`solve`]), then every equation is proven with all of them.
/// The solved parameters, or `None`.
pub(crate) fn solve_equations(
    equations: &[(RValue, Literal)],
    known: &FxHashMap<RcLocal, Value>,
    unknowns: &[RcLocal],
    helpers: &PureHelpers,
) -> Option<FxHashMap<RcLocal, Value>> {
    let goals = equations.iter().map(|(_, literal)| Value::of(literal)).collect::<Option<Vec<Value>>>()?;
    let mut values = known.clone();
    loop {
        let mut progress = false;
        for ((pattern, _), goal) in equations.iter().zip(&goals) {
            let mut open = unknowns.iter().filter(|param| !values.contains_key(*param) && reads(pattern, param) > 0);
            let (Some(param), None) = (open.next(), open.next()) else { continue };
            let env = |local: &RcLocal| values.get(local).cloned();
            if let Some(solution) = solve(pattern, goal, param, &env, helpers) {
                values.insert(param.clone(), solution);
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    let env = |local: &RcLocal| values.get(local).cloned();
    let proven = equations
        .iter()
        .zip(&goals)
        .all(|((pattern, _), goal)| evaluate(pattern, &env, helpers).is_some_and(|folded| folded.same(goal)));
    proven.then(|| values.into_iter().filter(|(param, _)| !known.contains_key(param)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Binary, Call, IfExpression, Unary};

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }
    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }
    fn boolean(value: bool) -> RValue {
        RValue::Literal(Literal::Boolean(value))
    }
    fn nil() -> RValue {
        RValue::Literal(Literal::Nil)
    }
    fn binary(left: RValue, operation: BinaryOperation, right: RValue) -> RValue {
        RValue::Binary(Binary { node_origin: Default::default(), left: Box::new(left), right: Box::new(right), operation })
    }
    fn unary(operation: UnaryOperation, value: RValue) -> RValue {
        RValue::Unary(Unary { node_origin: Default::default(), value: Box::new(value), operation })
    }
    fn select(condition: RValue, then_value: RValue, else_value: RValue) -> RValue {
        RValue::IfExpression(IfExpression {
            node_origin: Default::default(),
            condition: Box::new(condition),
            then_value: Box::new(then_value),
            else_value: Box::new(else_value),
        })
    }
    fn fold(value: &RValue) -> Option<Value> {
        evaluate(value, &|_| None, &PureHelpers::default())
    }
    fn assert_number(value: &RValue, bits: f64) {
        match fold(value) {
            Some(Value::Number(folded)) => assert_eq!(folded.to_bits(), bits.to_bits(), "{value}: {folded} vs {bits}"),
            other => panic!("{value}: {other:?}"),
        }
    }
    fn assert_value(value: &RValue, expected: Value) {
        let folded = fold(value).unwrap_or_else(|| panic!("{value} folds to nothing"));
        assert!(folded.same(&expected), "{value}: {folded:?} vs {expected:?}");
    }

    use BinaryOperation::*;

    /// Every expected value is what `luau-compile --text -O2` (release 736,
    /// `D:/Medal/luau-tools-src/build`) loads for `print(<expression>)`:
    /// the `LOADK`/`LOADN` constant, printed with 17 significant digits,
    /// which round-trips.
    #[test]
    fn numbers_fold_as_luau_compile_folds_them() {
        assert_number(&binary(number(13.0), Div, number(60.0)), 0.21666666666666667);
        assert_number(&binary(number(143.0), Div, number(60.0)), 2.3833333333333333);
        assert_number(&binary(number(0.35), Mul, number(60.0)), 21.0);
        assert_number(&binary(number(21.0), Div, number(60.0)), 0.34999999999999998);
        assert_number(&binary(number(6.0), Div, number(60.0)), 0.10000000000000001);
        assert_number(&binary(number(7.0), IDiv, number(2.0)), 3.0);
        assert_number(&binary(unary(UnaryOperation::Negate, number(7.0)), IDiv, number(2.0)), -4.0);
        assert_number(&binary(number(7.0), IDiv, number(0.0)), f64::INFINITY);
        assert_number(&binary(unary(UnaryOperation::Negate, number(0.0)), IDiv, number(1.0)), -0.0);
        assert_number(&binary(number(7.0), Mod, unary(UnaryOperation::Negate, number(3.0))), -2.0);
        assert_number(&binary(unary(UnaryOperation::Negate, number(7.0)), Mod, number(3.0)), 2.0);
        assert_number(&binary(number(5.5), Mod, number(2.0)), 1.5);
        assert_number(&binary(number(5.3), Mod, number(1.0)), 0.29999999999999982);
        assert_number(&binary(unary(UnaryOperation::Negate, number(5.3)), Mod, number(1.0)), 0.70000000000000018);
        assert_number(&unary(UnaryOperation::Negate, number(0.0)), -0.0);
        assert_number(&binary(number(0.0), Mul, unary(UnaryOperation::Negate, number(1.0))), -0.0);
        assert_number(&binary(number(1.0), Div, number(0.0)), f64::INFINITY);
        assert_number(&binary(unary(UnaryOperation::Negate, number(1.0)), Div, number(0.0)), f64::NEG_INFINITY);
        assert_number(&unary(UnaryOperation::Negate, binary(number(2.0), Sub, number(2.0))), -0.0);
        assert_number(&binary(number(1.0), Sub, number(0.5)), 0.5);
        assert_number(&binary(number(0.1), Add, number(0.2)), 0.30000000000000004);
        assert_number(&binary(number(1e308), Mul, number(10.0)), f64::INFINITY);
        assert_number(&binary(binary(number(1.0), Sub, number(0.5)), Mul, number(10.0)), 5.0);
        assert_number(&unary(UnaryOperation::Negate, unary(UnaryOperation::Negate, number(0.0))), 0.0);
        assert_number(&binary(unary(UnaryOperation::Negate, number(2.0)), Add, number(2.0)), 0.0);
    }

    /// What the rules here refuse although the compiler folds it: `^`
    /// (`2 ^ 0.5` loads 1.4142135623730951 from the compiler's `pow`), a
    /// NaN (`0 / 0`, `0 % 0`, `1 % 0` load `nan` with the hardware's
    /// payload), and a `%` whose `floor(a / b) * b` is inexact (`0.1 %
    /// 0.01` loads 0, a fused multiply-add gives -3.5e-18). `1e300 % 3`
    /// also loads 0, but there the product is exact: it folds.
    #[test]
    fn pow_nan_and_inexact_remainders_are_refused() {
        assert!(fold(&binary(number(2.0), Pow, number(0.5))).is_none());
        assert!(fold(&binary(number(2.0), Pow, number(10.0))).is_none());
        assert!(fold(&binary(number(0.0), Div, number(0.0))).is_none());
        assert!(fold(&binary(number(0.0), Mod, number(0.0))).is_none());
        assert!(fold(&binary(number(1.0), Mod, number(0.0))).is_none());
        assert_number(&binary(number(1e300), Mod, number(3.0)), 0.0);
        assert!(fold(&binary(number(0.1), Mod, number(0.01))).is_none());
        assert!(fold(&number(f64::NAN)).is_none());
        assert!(fold(&RValue::Literal(Literal::Integer(3))).is_none());
        assert!(fold(&RValue::Literal(Literal::Vector(1.0, 2.0, 3.0))).is_none());
    }

    /// `"ab" .. "cd"` loads 'abcd'; 2048 + 2048 bytes fold, 2048 + 2049 do
    /// not (`kConstantFoldStringLimit`), not even onto an empty string;
    /// `"x" .. 1` and `1 .. 2` stay `CONCAT` (only strings fold); `#"hello"`
    /// loads 5, `#""` 0; `-"2"` stays `MINUS`.
    #[test]
    fn strings_fold_within_the_compilers_limit() {
        assert_value(&binary(string("ab"), Concat, string("cd")), Value::String(b"abcd".to_vec()));
        assert_value(&binary(string(""), Concat, string("x")), Value::String(b"x".to_vec()));
        let a = "a".repeat(2048);
        let b = "b".repeat(2048);
        let c = "c".repeat(2049);
        assert_value(&binary(string(&a), Concat, string(&b)), Value::String(format!("{a}{b}").into_bytes()));
        assert!(fold(&binary(string(&a), Concat, string(&c))).is_none());
        assert!(fold(&binary(string(""), Concat, string(&format!("{a}{c}")))).is_none());
        assert!(fold(&binary(string("x"), Concat, number(1.0))).is_none());
        assert!(fold(&binary(number(1.0), Concat, number(2.0))).is_none());
        assert_number(&unary(UnaryOperation::Length, string("hello")), 5.0);
        assert_number(&unary(UnaryOperation::Length, string("")), 0.0);
        assert!(fold(&unary(UnaryOperation::Negate, string("2"))).is_none());
    }

    /// `0 == -0` loads true, `(0/0) == (0/0)` false and `~=` true (Luau's
    /// `constantsEqual`); `nil == false`, `true == 1`, `"1" == 1` false;
    /// `1 < 2`, `1 <= 1`, `2 >= 2` true, `1 > 2` false, `(0/0) < 1` false;
    /// `"a" < "b"` stays a jump (numbers only).
    #[test]
    fn comparisons_fold_as_constants_equal_and_on_numbers_only() {
        let nan = || binary(number(0.0), Div, number(0.0));
        let negative_zero = unary(UnaryOperation::Negate, number(0.0));
        assert_value(&binary(number(0.0), Equal, negative_zero), Value::Boolean(true));
        // The NaN itself is refused; equality with it is still decided by
        // the rule (here on the evaluator's NaN-free values: see below).
        assert!(fold(&binary(nan(), Equal, nan())).is_none());
        assert_value(&binary(nil(), Equal, boolean(false)), Value::Boolean(false));
        assert_value(&binary(boolean(true), Equal, number(1.0)), Value::Boolean(false));
        assert_value(&binary(string("1"), Equal, number(1.0)), Value::Boolean(false));
        assert_value(&binary(string("a"), Equal, string("a")), Value::Boolean(true));
        assert_value(&binary(string("a"), NotEqual, string("b")), Value::Boolean(true));
        assert_value(&binary(number(1.0), LessThan, number(2.0)), Value::Boolean(true));
        assert_value(&binary(number(2.0), LessThan, number(1.0)), Value::Boolean(false));
        assert_value(&binary(number(1.0), LessThanOrEqual, number(1.0)), Value::Boolean(true));
        assert_value(&binary(number(1.0), GreaterThan, number(2.0)), Value::Boolean(false));
        assert_value(&binary(number(2.0), GreaterThanOrEqual, number(2.0)), Value::Boolean(true));
        assert!(fold(&binary(string("a"), LessThan, string("b"))).is_none());
        // NaN equals nothing, itself included.
        assert!(!constants_equal(&Value::Number(f64::NAN), &Value::Number(f64::NAN)));
        assert!(constants_equal(&Value::Number(0.0), &Value::Number(-0.0)));
    }

    /// `nil and 5` loads nil, `false and u` false, `false or "x"` 'x',
    /// `true or u` true, `3 and 4` 4, `3 or u` 3; `true and u` and `nil or u`
    /// fold to nothing (the result is `u`). `not nil` true, `not 0` and
    /// `not "x"` false. `if true then 1 else 2` loads 1, `if nil then 1 else
    /// 2` 2, `if 0 then "a" else u` 'a'.
    #[test]
    fn logic_folds_on_a_constant_left_side() {
        let unknown = || RValue::Local(RcLocal::default());
        assert_value(&binary(nil(), And, number(5.0)), Value::Nil);
        assert_value(&binary(boolean(false), And, unknown()), Value::Boolean(false));
        assert_value(&binary(boolean(false), Or, string("x")), Value::String(b"x".to_vec()));
        assert_value(&binary(boolean(true), Or, unknown()), Value::Boolean(true));
        assert_number(&binary(number(3.0), And, number(4.0)), 4.0);
        assert_number(&binary(number(3.0), Or, unknown()), 3.0);
        assert!(fold(&binary(boolean(true), And, unknown())).is_none());
        assert!(fold(&binary(nil(), Or, unknown())).is_none());
        assert_value(&unary(UnaryOperation::Not, nil()), Value::Boolean(true));
        assert_value(&unary(UnaryOperation::Not, number(0.0)), Value::Boolean(false));
        assert_value(&unary(UnaryOperation::Not, string("x")), Value::Boolean(false));
        assert_number(&select(boolean(true), number(1.0), number(2.0)), 1.0);
        assert_number(&select(nil(), number(1.0), number(2.0)), 2.0);
        assert_value(&select(number(0.0), string("a"), unknown()), Value::String(b"a".to_vec()));
    }

    fn helper(params: Vec<RcLocal>, value: RValue) -> PureHelper {
        PureHelper { params, value, proto: None }
    }

    fn call(callee: &RcLocal, arguments: Vec<RValue>) -> RValue {
        RValue::Call(Call::new(RValue::Local(callee.clone()), arguments))
    }

    #[test]
    fn calls_of_pure_helpers_fold_through_their_bodies_and_builtins_never() {
        let n = RcLocal::default();
        let frames = RcLocal::default();
        let mut helpers = PureHelpers::default();
        helpers.0.insert(frames.clone(), helper(vec![n.clone()], binary(RValue::Local(n.clone()), Div, number(60.0))));
        let value = call(&frames, vec![number(13.0)]);
        let folded = evaluate(&value, &|_| None, &helpers).unwrap();
        assert!(folded.same(&Value::Number(0.21666666666666667)));
        // A global's call (`math.floor(1.5)`, a library a script may have
        // replaced) folds to nothing.
        let floor = RValue::Call(Call::new(
            RValue::Index(crate::Index::new(RValue::Global(crate::Global::new(b"math".to_vec())), string("floor"))),
            vec![number(1.5)],
        ));
        assert!(evaluate(&floor, &|_| None, &helpers).is_none());
        // A call of an unknown local folds to nothing.
        assert!(evaluate(&call(&RcLocal::default(), vec![number(1.0)]), &|_| None, &helpers).is_none());
    }

    #[test]
    fn solving_prefers_the_shortest_argument_that_folds_bit_for_bit() {
        let n = RcLocal::default();
        let frames = RcLocal::default();
        let mut helpers = PureHelpers::default();
        helpers.0.insert(frames.clone(), helper(vec![n.clone()], binary(RValue::Local(n.clone()), Div, number(60.0))));
        let body = binary(RValue::Local(n.clone()), Div, number(60.0));
        let solve_frames = |goal: f64| solve(&body, &Value::Number(goal), &n, &|_| None, &helpers);
        for (goal, argument) in [(13.0 / 60.0, 13.0), (143.0 / 60.0, 143.0), (0.35, 21.0), (0.1, 6.0), (0.5, 30.0), (0.0, 0.0)] {
            match solve_frames(goal) {
                Some(Value::Number(found)) => assert_eq!(found.to_bits(), f64::to_bits(argument), "{goal}"),
                other => panic!("{goal}: {other:?}"),
            }
        }
        // `-0` is `frames(-0)`, bit for bit.
        assert!(matches!(solve_frames(-0.0), Some(Value::Number(found)) if found.to_bits() == (-0.0f64).to_bits()));
        // Through a call: `linear(p) = { frames(p) ... }`'s value.
        let p = RcLocal::default();
        let through = call(&frames, vec![RValue::Local(p.clone())]);
        let found = solve(&through, &Value::Number(13.0 / 60.0), &p, &|_| None, &helpers).unwrap();
        assert!(found.same(&Value::Number(13.0)));
        // `1 - k = 0.5` and `k * 10 = 5` (fade(part, 0.5)).
        let k = RcLocal::default();
        let equations = vec![
            (binary(number(1.0), Sub, RValue::Local(k.clone())), Literal::Number(0.5)),
            (binary(RValue::Local(k.clone()), Mul, number(10.0)), Literal::Number(5.0)),
        ];
        let solved = solve_equations(&equations, &FxHashMap::default(), std::slice::from_ref(&k), &helpers).unwrap();
        assert!(solved[&k].same(&Value::Number(0.5)));
        // Two equations no one value satisfies.
        let contradiction = vec![
            (binary(number(1.0), Sub, RValue::Local(k.clone())), Literal::Number(0.5)),
            (binary(RValue::Local(k.clone()), Mul, number(10.0)), Literal::Number(6.0)),
        ];
        assert!(solve_equations(&contradiction, &FxHashMap::default(), std::slice::from_ref(&k), &helpers).is_none());
        // Strings: `"Slot" .. n = "Slot7"`.
        let s = RcLocal::default();
        let slot = binary(string("Slot"), Concat, RValue::Local(s.clone()));
        assert!(solve(&slot, &Value::String(b"Slot7".to_vec()), &s, &|_| None, &helpers).unwrap().same(&Value::String(b"7".to_vec())));
        // No inverse: `n // 2 = 3`, and `n` read twice.
        let idiv = binary(RValue::Local(n.clone()), IDiv, number(2.0));
        assert!(solve(&idiv, &Value::Number(3.0), &n, &|_| None, &helpers).is_none());
        let twice = binary(RValue::Local(n.clone()), Add, RValue::Local(n.clone()));
        assert!(solve(&twice, &Value::Number(4.0), &n, &|_| None, &helpers).is_none());
        // `^` is refused even where it could be inverted.
        let pow = binary(RValue::Local(n.clone()), Pow, number(2.0));
        assert!(solve(&pow, &Value::Number(4.0), &n, &|_| None, &helpers).is_none());
    }

    #[test]
    fn pure_helpers_are_single_returns_of_foldable_values() {
        use crate::{Function, Return};
        let n = RcLocal::default();
        let make = |value: RValue, variadic: bool| {
            triomphe::Arc::new(parking_lot::Mutex::new(Function {
                parameters: vec![n.clone()],
                is_variadic: variadic,
                body: crate::Block(vec![Return::new(vec![value]).into()]),
                ..Function::default()
            }))
        };
        let frames = RcLocal::default();
        let twice = RcLocal::default();
        let global = RcLocal::default();
        let variadic = RcLocal::default();
        let reassigned = RcLocal::default();
        let declarations = vec![
            (frames.clone(), make(binary(RValue::Local(n.clone()), Div, number(60.0)), false)),
            (twice.clone(), make(binary(call(&frames, vec![RValue::Local(n.clone())]), Mul, number(2.0)), false)),
            (global.clone(), make(binary(RValue::Global(crate::Global::new(b"x".to_vec())), Div, RValue::Local(n.clone())), false)),
            (variadic.clone(), make(RValue::Local(n.clone()), true)),
            (reassigned.clone(), make(RValue::Local(n.clone()), false)),
        ];
        let counts: FxHashMap<RcLocal, usize> = [(frames.clone(), 1), (twice.clone(), 1), (global.clone(), 1), (variadic.clone(), 1), (reassigned.clone(), 2)]
            .into_iter()
            .collect();
        let helpers = PureHelpers::of(&declarations, &counts);
        assert!(helpers.get(&frames).is_some());
        assert!(helpers.get(&twice).is_some(), "calls a pure helper");
        assert!(helpers.get(&global).is_none(), "reads a global");
        assert!(helpers.get(&variadic).is_none());
        assert!(helpers.get(&reassigned).is_none());
    }
}
