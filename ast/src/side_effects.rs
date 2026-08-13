use enum_dispatch::enum_dispatch;

#[enum_dispatch]
pub trait SideEffects {
    fn has_side_effects(&self) -> bool {
        false
    }
}

macro_rules! has_side_effects {
    ($($name:ty),*) => {
        $(
            impl $crate::SideEffects for $name {
                fn has_side_effects(&self) -> bool {
                    true
                }
            }
        )*
    };
}

pub(crate) use has_side_effects;

/// True when evaluating `value` can have NO observable effect — it neither has a
/// side effect nor can raise a runtime error — so it is safe to drop entirely
/// when its result is unused (the body of an `if cond then end` that the
/// structurer is collapsing).
///
/// This is deliberately STRICTER than `!has_side_effects()`: the relational
/// (`< <= > >=`), arithmetic and concat operators, indexing, length and unary
/// minus are reported effect-free by `has_side_effects` (so single-use temps can
/// inline back into their use site), yet they RAISE on type-mismatched operands
/// (`{} < {}`, `nil.x`, `#5`, `-{}`). Dropping such a condition would silently
/// swallow that runtime error (bug C11), so anything that is not provably total
/// must be kept (as `local _ = cond`).
pub fn is_total_pure(value: &crate::RValue) -> bool {
    use crate::{BinaryOperation, RValue, UnaryOperation};
    match value {
        // Leaf reads that cannot raise and have no effect. `Global` is excluded
        // on purpose: it is modelled as side-effecting elsewhere and is kept as a
        // statement, so excluding it here preserves that existing behaviour.
        RValue::Local(_) | RValue::Literal(_) | RValue::VarArg(_) => true,
        // Constructing a closure never raises and has no effect.
        RValue::Closure(_) => true,
        // `not x` cannot raise and has no metamethod (it is total on any
        // operand); total iff its operand is. The audit's stricter rule for `not`
        // is unnecessary exactly because `not` itself can never error.
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => {
            is_total_pure(&unary.value)
        }
        // `and`/`or` only short-circuit; total iff both operands are total.
        //
        // `==`/`~=` are EXCLUDED (FIX(OPT-001)): equality on tables can invoke an
        // `__eq` metamethod, which can raise or observe state, so a comparison is
        // not provably total — and a comparison dropped as "pure" would swallow
        // that error (also covers FIX(OPT-002): NaN comparisons stay undroppable).
        // Every other binary operator can raise, so it is NOT total either.
        RValue::Binary(binary)
            if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) =>
        {
            is_total_pure(&binary.left) && is_total_pure(&binary.right)
        }
        // A table constructor is total iff every value is total and every key is
        // a provably non-nil, non-NaN literal (FIX(OPT-003)): a constructor whose
        // `[expr]` key could be nil/NaN at runtime raises "table index is
        // nil/NaN", so a variable-key constructor must not be dropped.
        RValue::Table(table) => table.0.iter().all(|(k, v)| match k {
            // A positional entry has no key (nil VALUE is legal), so it is total
            // iff its value is.
            None => is_total_pure(v),
            Some(RValue::Literal(literal)) => {
                !matches!(literal, crate::Literal::Nil)
                    && !matches!(literal, crate::Literal::Number(n) if n.is_nan())
                    && is_total_pure(v)
            }
            Some(_) => false,
        }),
        // An if-expression raises only if one of its (evaluated) parts does.
        RValue::IfExpression(if_expression) => {
            is_total_pure(&if_expression.condition)
                && is_total_pure(&if_expression.then_value)
                && is_total_pure(&if_expression.else_value)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_total_pure;
    use crate::{Binary, BinaryOperation, Literal, Local, RValue, RcLocal, Table};

    #[test]
    fn equality_comparisons_are_not_total() {
        let one = RValue::Literal(Literal::Number(1.0));
        let two = RValue::Literal(Literal::Number(2.0));
        // FIX(OPT-001): `==`/`~=` can run an `__eq` metamethod that raises or
        // observes state, so even a literal comparison is not droppable.
        assert!(!is_total_pure(&RValue::Binary(Binary::new(
            one.clone(),
            two.clone(),
            BinaryOperation::Equal
        ))));
        assert!(!is_total_pure(&RValue::Binary(Binary::new(
            one.clone(),
            two.clone(),
            BinaryOperation::NotEqual
        ))));
        // `and`/`or` on total operands short-circuit and stay droppable.
        assert!(is_total_pure(&RValue::Binary(Binary::new(
            one,
            RValue::Literal(Literal::Boolean(true)),
            BinaryOperation::And
        ))));
        assert!(is_total_pure(&RValue::Binary(Binary::new(
            two,
            RValue::Literal(Literal::Boolean(false)),
            BinaryOperation::Or
        ))));
    }

    #[test]
    fn constructor_with_variable_key_is_not_total() {
        let key = RcLocal::new(Local::new(Some("k".to_string())));
        // FIX(OPT-003): a `[expr]` key that could be nil at runtime makes the
        // constructor raise, so it is not droppable even with total values.
        let variable_key = RValue::Table(Table(vec![(
            Some(RValue::Local(key)),
            RValue::Literal(Literal::Number(1.0)),
        )]));
        assert!(!is_total_pure(&variable_key));
        // A positional or literal-keyed constructor with total values is.
        let literal_keyed = RValue::Table(Table(vec![(
            Some(RValue::Literal(Literal::Number(1.0))),
            RValue::Literal(Literal::Number(1.0)),
        )]));
        assert!(is_total_pure(&literal_keyed));
        let positional = RValue::Table(Table(vec![(
            None,
            RValue::Literal(Literal::Number(1.0)),
        )]));
        assert!(is_total_pure(&positional));
    }
}
