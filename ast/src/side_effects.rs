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
        // `not x` cannot raise; total iff its operand is.
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => {
            is_total_pure(&unary.value)
        }
        // `==`/`~=` never raise on a type mismatch (equality is total) and
        // `and`/`or` only short-circuit; total iff both operands are total. Every
        // other binary operator can raise, so it is NOT total.
        RValue::Binary(binary)
            if matches!(
                binary.operation,
                BinaryOperation::Equal
                    | BinaryOperation::NotEqual
                    | BinaryOperation::And
                    | BinaryOperation::Or
            ) =>
        {
            is_total_pure(&binary.left) && is_total_pure(&binary.right)
        }
        // A table constructor is total iff every key and value is total. (A bare
        // `{}` / `{1, 2}` / `{x = 1}` never raises; a `{ f() }` is non-total via
        // its element.) NB: an exotic `{[nil]=1}` would raise, but the compiler
        // never emits a nil/NaN constant key, so it cannot occur here.
        RValue::Table(table) => table
            .0
            .iter()
            .all(|(k, v)| k.as_ref().map_or(true, is_total_pure) && is_total_pure(v)),
        // An if-expression raises only if one of its (evaluated) parts does.
        RValue::IfExpression(if_expression) => {
            is_total_pure(&if_expression.condition)
                && is_total_pure(&if_expression.then_value)
                && is_total_pure(&if_expression.else_value)
        }
        _ => false,
    }
}
