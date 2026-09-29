//! The value-exact `and`/`or` form of a two-way select.
//!
//! `if c then x = a else x = b end` (or its returns) reads as one expression
//! only when `and`/`or` can yield exactly `a` or `b`: the arm selected through
//! `and` must be truthy. SSA structuring folds register selects with this rule,
//! and the de-inliners fold a helper's `return` diamond with it, so a helper
//! and the copy Luau inlined into a caller reach the same shape.

use crate::{Binary, BinaryOperation, Literal, RValue, Reduce, Unary, UnaryOperation};

/// Whether `value` is known truthy (`Some(true)`), known falsy, or unknown.
fn is_truthy(value: RValue) -> Option<bool> {
    match value.reduce_condition() {
        // __len has to return number, but __unm can return any value
        RValue::Unary(Unary {
            operation: UnaryOperation::Length,
            ..
        }) => Some(true),
        RValue::Literal(
            Literal::Boolean(true)
            | Literal::Number(_)
            | Literal::String(_)
            | Literal::Vector(..)
            | Literal::VectorD(..),
        )
        | RValue::Table(_)
        | RValue::Closure(_) => Some(true),
        RValue::Literal(Literal::Nil | Literal::Boolean(_)) => Some(false),
        _ => None,
    }
}

/// `if condition then then_value else else_value` as an `and`/`or` value, or
/// `None` when no such form is exact. On success `condition` has been moved
/// out (left as `nil`); on failure it is untouched.
pub fn select_value(
    condition: &mut RValue,
    mut then_value: RValue,
    mut else_value: RValue,
) -> Option<RValue> {
    if let RValue::Literal(Literal::Boolean(then_bool)) = then_value
        && let RValue::Literal(Literal::Boolean(else_bool)) = else_value
        && then_bool != else_bool
    {
        let cond = Unary::new(
            std::mem::replace(condition, Literal::Nil.into()),
            UnaryOperation::Not,
        );
        let cond = if then_bool {
            Unary::new(cond.into(), UnaryOperation::Not)
        } else {
            cond
        };
        return Some(cond.reduce());
    }
    // If the then-value is exactly the condition, the whole select collapses to
    // `condition or else_value`: when the condition is truthy it evaluates to
    // then_value (which *is* the condition), so the result is then_value;
    // otherwise it is else_value. This is the clean form for
    // `x = (a and b and c) or d`-style code, and it handles whole `and`-chains
    // that the right-operand-only check below misses.
    //
    // The condition is used here in value position (exactly as the original
    // then-branch used it), so it stays a plain value-preserving `reduce()`,
    // never `reduce_condition()`. Emitting `condition or else_value` directly
    // (rather than routing it through the `then_truthy` path, which would build
    // `not(cond) and X or cond` and lean on the `X and X` collapse rule) keeps
    // De Morgan chains clean: `(not a and not b) or d` reduces to
    // `not (a or b) or d`, not the verbose `not (a or b or (a or b)) or d`.
    //
    // The cheap discriminant/id `==` is tested before the recursive
    // `has_side_effects` walk so the common non-matching call short-circuits
    // without walking. The guard ensures the condition (== then_value) is
    // effect-free, so evaluating it once here matches the original.
    if *condition == then_value && crate::is_total_pure(&then_value) {
        let condition = std::mem::replace(condition, Literal::Nil.into());
        return Some(Binary::new(condition, else_value, BinaryOperation::Or).reduce());
    }
    // Symmetric case: when the else-value is the condition, `if c then X else c`
    // collapses to `c and X` (c truthy -> X; c falsy -> short-circuits to c).
    // Unlike the then-case, X need not be truthy — `and` yields its right
    // operand verbatim when c is truthy. Same value-position / effect-free
    // reasoning as above (the original evaluates c twice on the falsy path, this
    // once), and the cheap `==` gates the recursive side-effect walk.
    if *condition == else_value && crate::is_total_pure(&else_value) {
        let condition = std::mem::replace(condition, Literal::Nil.into());
        return Some(Binary::new(condition, then_value, BinaryOperation::And).reduce());
    }
    // TODO: for `v0 and v1 and v2` only the right operand v2 (and the whole
    // chain, handled above) is recognised as truthy; inner left operands are
    // intentionally not — `(a and b) and a or c` would not collapse and reads
    // worse than the original if/else.
    let then_truthy = match is_truthy(then_value.clone()) {
        Some(truthy) => truthy,
        None if crate::is_total_pure(&then_value) => {
            let value = match &*condition {
                RValue::Binary(binary) if binary.operation == BinaryOperation::And => {
                    binary.right.as_ref()
                }
                value => value,
            };
            crate::is_total_pure(value) && *value == then_value
        }
        None => false,
    };
    // TODO: if condition is `and not else_value` or `not else_value` then truthy?
    let else_truthy = is_truthy(else_value.clone()).is_some_and(|v| v);
    let cond = if !then_truthy && !else_truthy {
        return None;
    } else if !then_truthy {
        std::mem::swap(&mut then_value, &mut else_value);
        Unary::new(
            std::mem::replace(condition, Literal::Nil.into()),
            UnaryOperation::Not,
        )
        .reduce_condition()
    } else if !else_truthy {
        std::mem::replace(condition, Literal::Nil.into()).reduce_condition()
    } else {
        let cond = std::mem::replace(condition, Literal::Nil.into()).reduce_condition();
        if let RValue::Unary(Unary {
            box value,
            operation: UnaryOperation::Not,
            ..
        }) = cond
        {
            std::mem::swap(&mut then_value, &mut else_value);
            value
        } else {
            cond
        }
    };

    Some(
        Binary::new(
            Binary::new(cond, then_value, BinaryOperation::And).into(),
            else_value,
            BinaryOperation::Or,
        )
        .reduce(),
    )
}
