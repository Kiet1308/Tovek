use enum_dispatch::enum_dispatch;

use crate::Traverse;

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
        // `not x` cannot raise; total iff its operand is.
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => {
            is_total_pure(&unary.value)
        }
        // `and`/`or` only short-circuit; total iff both operands are total.
        // Equality is intentionally excluded: tables can invoke `__eq`, and a
        // metamethod can observe state or raise even when both operands are
        // otherwise literals. Every other binary operator can raise too.
        RValue::Binary(binary)
            if matches!(binary.operation, BinaryOperation::And | BinaryOperation::Or) =>
        {
            is_total_pure(&binary.left) && is_total_pure(&binary.right)
        }
        // A table constructor with a computed key can raise for nil/NaN even
        // though evaluating the key itself has no side effect. Only literal
        // keys known to be valid table keys are total; all computed keys are
        // conservatively retained.
        RValue::Table(table) => is_total_table(table),
        // An if-expression raises only if one of its (evaluated) parts does.
        RValue::IfExpression(if_expression) => {
            is_total_pure(&if_expression.condition)
                && is_total_pure(&if_expression.then_value)
                && is_total_pure(&if_expression.else_value)
        }
        _ => false,
    }
}

/// Whether a literal key is guaranteed not to raise when inserted into a
/// table.  Keep this predicate shared with table-reconstruction passes: a
/// local/global key may evaluate to nil or NaN even when its expression has no
/// explicit side effects, so it is not safe to move the constructor across a
/// call merely because the key is a leaf read.
pub(crate) fn is_total_table_key(key: &crate::RValue) -> bool {
    use crate::{Literal, RValue};
    match key {
        RValue::Literal(Literal::Nil) => false,
        RValue::Literal(Literal::Number(number)) => !number.is_nan(),
        RValue::Literal(Literal::Vector(x, y, z)) => {
            !x.is_nan() && !y.is_nan() && !z.is_nan()
        }
        RValue::Literal(Literal::VectorD(x, y, z)) => {
            !x.is_nan() && !y.is_nan() && !z.is_nan()
        }
        RValue::Literal(_) => true,
        _ => false,
    }
}

/// Total-purity check for a table constructor, including key validity.
pub(crate) fn is_total_table(table: &crate::Table) -> bool {
    table.0.iter().all(|(key, value)| {
        key.as_ref().map_or(true, is_total_table_key)
            && is_total_pure(value)
            && key.as_ref().map_or(true, is_total_pure)
    })
}

/// Whether evaluating an expression is observable, either because it performs
/// a side effect or because it can raise. This is the common gate for transforms
/// that move one expression across another; using `has_side_effects()` alone is
/// unsound for pure-looking operations such as a dynamic table key.
pub fn is_observable(value: &crate::RValue) -> bool {
    value.has_side_effects() || !is_total_pure(value)
}

/// Statement-level counterpart of [`is_observable`]. `rvalues()` returns the
/// statement's direct expression roots; `is_total_pure` recursively inspects
/// each root, so nested calls/keys are covered without a second traversal.
pub fn statement_is_observable(statement: &crate::Statement) -> bool {
    statement.has_side_effects() || statement.rvalues().into_iter().any(is_observable)
}

#[cfg(test)]
mod tests {
    use super::{is_observable, is_total_pure};
    use crate::{Binary, BinaryOperation, Literal, Local, RValue, RcLocal, Table};

    #[test]
    fn dynamic_table_keys_are_not_total_or_reorderable() {
        let key = RcLocal::new(Local::new(Some("key".to_owned())));
        let table = RValue::Table(Table(vec![(
            Some(RValue::Local(key)),
            RValue::Literal(Literal::Number(1.0)),
        )]));
        assert!(!is_total_pure(&table));
        assert!(is_observable(&table));
    }

    #[test]
    fn equality_is_not_total_because_of_metamethods() {
        let comparison = RValue::Binary(Binary::new(
            RValue::Literal(Literal::Number(1.0)),
            RValue::Literal(Literal::Number(2.0)),
            BinaryOperation::Equal,
        ));
        assert!(!is_total_pure(&comparison));
        assert!(is_observable(&comparison));
    }
}
