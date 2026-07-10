use std::fmt;

use crate::{Literal, LocalRw, RValue, RcLocal, Reduce, SideEffects, Traverse};

use super::{Unary, UnaryOperation};

#[derive(Debug, PartialEq, Eq, PartialOrd, Copy, Clone)]
pub enum BinaryOperation {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Concat,
    Equal,
    NotEqual,
    LessThanOrEqual,
    GreaterThanOrEqual,
    LessThan,
    GreaterThan,
    And,
    Or,
    IDiv,
}

impl BinaryOperation {
    pub fn is_comparator(&self) -> bool {
        matches!(
            self,
            BinaryOperation::Equal
                | BinaryOperation::NotEqual
                | BinaryOperation::LessThanOrEqual
                | BinaryOperation::GreaterThanOrEqual
                | BinaryOperation::LessThan
                | BinaryOperation::GreaterThan
        )
    }
}

/// True when `r` is statically known to evaluate to a `boolean` value, so that
/// wrapping/unwrapping it in `and`/`or`/`not` cannot change which concrete value
/// it produces (only its truthiness, which is already pinned by being boolean).
///
/// Conservative by design: an `_ => false` under-approximation is always safe —
/// callers only use a `true` result to *skip* a coercion that would be a no-op,
/// never to perform a rewrite that depends on a `false` answer.
///
/// `not X` is always boolean regardless of `X`. Method names alone are not type
/// proofs: an arbitrary table can expose an `IsA` method returning any value.
pub(crate) fn is_boolean(r: &RValue) -> bool {
    match r {
        RValue::Binary(binary) if binary.operation.is_comparator() => true,
        RValue::Binary(Binary {
            left,
            right,
            operation: BinaryOperation::And | BinaryOperation::Or,
        }) => is_boolean(left) && is_boolean(right),
        RValue::Unary(unary) if unary.operation == crate::UnaryOperation::Not => true,
        RValue::Literal(Literal::Boolean(_)) => true,
        // strings, numbers and tables are intentionally not matched: callers run
        // after reduce_condition, so a constant would already be folded.
        _ => false,
    }
}

impl fmt::Display for BinaryOperation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                BinaryOperation::Add => "+",
                BinaryOperation::Sub => "-",
                BinaryOperation::Mul => "*",
                BinaryOperation::Div => "/",
                BinaryOperation::Mod => "%",
                BinaryOperation::Pow => "^",
                BinaryOperation::Concat => "..",
                BinaryOperation::Equal => "==",
                BinaryOperation::NotEqual => "~=",
                BinaryOperation::LessThanOrEqual => "<=",
                BinaryOperation::GreaterThanOrEqual => ">=",
                BinaryOperation::LessThan => "<",
                BinaryOperation::GreaterThan => ">",
                BinaryOperation::And => "and",
                BinaryOperation::Or => "or",
                BinaryOperation::IDiv => "//",
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binary {
    pub left: Box<RValue>,
    pub right: Box<RValue>,
    pub operation: BinaryOperation,
}

impl Traverse for Binary {
    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.left, &mut self.right]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.left, &self.right]
    }
}

impl SideEffects for Binary {
    fn has_side_effects(&self) -> bool {
        // An operator on operands that are themselves effect-free is treated as
        // effect-free. Arithmetic/concat/comparison can in principle invoke a
        // metamethod, but real (non-adversarial) code operates on numbers and
        // strings, so propagating effects from the operands matches the And/Or
        // arm's existing behavior and lets single-use temporaries inline back
        // into their use site instead of exploding into `local vN = ...` chains.
        self.left.has_side_effects() || self.right.has_side_effects()
    }
}

impl<'a: 'b, 'b> Reduce for Binary {
    fn reduce(self) -> RValue {
        // TODO: true == true, true == false, etc.
        // really anything without side effects should be true if l == r
        match (self.left.reduce(), self.right.reduce(), self.operation) {
            (
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: left,
                }),
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: right,
                }),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Unary {
                value: Box::new(
                    Binary {
                        left,
                        right,
                        operation: if self.operation == BinaryOperation::And {
                            BinaryOperation::Or
                        } else {
                            BinaryOperation::And
                        },
                    }
                    .into(),
                ),
                operation: UnaryOperation::Not,
            }
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                RValue::Literal(Literal::Boolean(right)),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Literal::Boolean(if self.operation == BinaryOperation::And {
                left && right
            } else {
                left || right
            })
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                right,
                BinaryOperation::And | BinaryOperation::Or,
            ) => match self.operation {
                BinaryOperation::And if !left => RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::And => right.reduce(),
                BinaryOperation::Or if left => RValue::Literal(Literal::Boolean(true)),
                BinaryOperation::Or => right.reduce(),
                _ => unreachable!(),
            },
            (left, right, BinaryOperation::And)
                if !left.has_side_effects() && !right.has_side_effects() && left == right =>
            {
                left
            }
            (
                RValue::Binary(Binary {
                    left:
                        box value @ RValue::Unary(Unary {
                            operation: UnaryOperation::Not,
                            ..
                        }),
                    right: box RValue::Literal(Literal::Boolean(true)),
                    operation: BinaryOperation::And,
                }),
                RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::Or,
            ) => value,
            (left, right, BinaryOperation::Or)
                if !left.has_side_effects() && !right.has_side_effects() && left == right =>
            {
                left
            }
            // TODO: concat numbers
            (
                RValue::Literal(Literal::String(left)),
                RValue::Literal(Literal::String(right)),
                BinaryOperation::Concat,
            ) => RValue::Literal(Literal::String(
                left.into_iter().chain(right.into_iter()).collect(),
            )),
            (left, right, operation) => Self {
                left: Box::new(left),
                right: Box::new(right),
                operation,
            }
            .into(),
        }
    }

    fn reduce_condition(self) -> RValue {
        let (left, right) = if matches!(self.operation, BinaryOperation::And | BinaryOperation::Or)
        {
            (self.left.reduce_condition(), self.right.reduce_condition())
        } else {
            (self.left.reduce(), self.right.reduce())
        };
        match (left, right, self.operation) {
            (
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: left,
                }),
                RValue::Unary(Unary {
                    operation: UnaryOperation::Not,
                    value: right,
                }),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Unary {
                value: Box::new(
                    Binary {
                        left,
                        right,
                        operation: if self.operation == BinaryOperation::And {
                            BinaryOperation::Or
                        } else {
                            BinaryOperation::And
                        },
                    }
                    .into(),
                ),
                operation: UnaryOperation::Not,
            }
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                RValue::Literal(Literal::Boolean(right)),
                BinaryOperation::And | BinaryOperation::Or,
            ) => Literal::Boolean(if self.operation == BinaryOperation::And {
                left && right
            } else {
                left || right
            })
            .into(),
            (
                RValue::Literal(Literal::Boolean(left)),
                right,
                BinaryOperation::And | BinaryOperation::Or,
            ) => match self.operation {
                BinaryOperation::And if !left => RValue::Literal(Literal::Boolean(false)),
                BinaryOperation::And => right.reduce(),
                BinaryOperation::Or if left => RValue::Literal(Literal::Boolean(true)),
                BinaryOperation::Or => right.reduce(),
                _ => unreachable!(),
            },
            (
                left,
                RValue::Literal(Literal::Boolean(right)),
                BinaryOperation::And | BinaryOperation::Or,
            ) => match self.operation {
                // `X and true` -> X, `X or false` -> X: the literal is the
                // identity here, so X is preserved.
                BinaryOperation::And if right => left.reduce(),
                BinaryOperation::Or if !right => left.reduce(),
                // `X and false` -> false, `X or true` -> true: the literal decides
                // the result, but X (the LEFT operand) is ALWAYS evaluated in Lua,
                // so it may only be dropped when it is side-effect-free; otherwise
                // keep `X <op> <literal>` so X still runs.
                _ if !left.has_side_effects() => RValue::Literal(Literal::Boolean(right)),
                operation => {
                    Binary::new(left, RValue::Literal(Literal::Boolean(right)), operation).into()
                }
            },
            // TODO: concat numbers
            (
                RValue::Literal(Literal::String(left)),
                RValue::Literal(Literal::String(right)),
                BinaryOperation::Concat,
            ) => RValue::Literal(Literal::String(
                left.into_iter().chain(right.into_iter()).collect(),
            )),
            (left, right, operation) => Self {
                left: Box::new(left),
                right: Box::new(right),
                operation,
            }
            .into(),
        }
    }
}

impl Binary {
    pub fn new(left: RValue, right: RValue, operation: BinaryOperation) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            operation,
        }
    }

    pub fn precedence(&self) -> usize {
        match self.operation {
            BinaryOperation::Pow => 8,
            BinaryOperation::Mul
            | BinaryOperation::Div
            | BinaryOperation::Mod
            | BinaryOperation::IDiv => 6,
            BinaryOperation::Add | BinaryOperation::Sub => 5,
            BinaryOperation::Concat => 4,
            BinaryOperation::LessThan
            | BinaryOperation::GreaterThan
            | BinaryOperation::LessThanOrEqual
            | BinaryOperation::GreaterThanOrEqual
            | BinaryOperation::Equal
            | BinaryOperation::NotEqual => 3,
            BinaryOperation::And => 2,
            BinaryOperation::Or => 1,
        }
    }

    pub fn right_associative(&self) -> bool {
        matches!(
            self.operation,
            BinaryOperation::Pow | BinaryOperation::Concat
        )
    }

    pub fn left_group(&self) -> bool {
        self.precedence() > self.left.precedence()
            || (self.precedence() == self.left.precedence() && self.right_associative())
    }

    pub fn right_group(&self) -> bool {
        self.precedence() > self.right.precedence()
            || (self.precedence() == self.right.precedence() && !self.right_associative())
    }
}

impl LocalRw for Binary {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.left
            .values_read()
            .into_iter()
            .chain(self.right.values_read().into_iter())
            .collect()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.left
            .values_read_mut()
            .into_iter()
            .chain(self.right.values_read_mut().into_iter())
            .collect()
    }
}

impl fmt::Display for Binary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let parentheses = |group: bool, rvalue: &RValue| {
            if group {
                format!("({})", rvalue)
            } else {
                format!("{}", rvalue)
            }
        };

        write!(
            f,
            "{} {} {}",
            parentheses(self.left_group(), self.left.as_ref()),
            self.operation,
            parentheses(self.right_group(), self.right.as_ref()),
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{Binary, BinaryOperation, Global, Literal, RValue, RcLocal, Reduce};

    fn local() -> RValue {
        RValue::Local(RcLocal::default())
    }
    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }
    fn boolean(b: bool) -> RValue {
        RValue::Literal(Literal::Boolean(b))
    }
    fn is_binary(r: &RValue, op: BinaryOperation) -> bool {
        matches!(r, RValue::Binary(b) if b.operation == op)
    }

    // P1: `a or a` collapses to `a` only when `a` is side-effect-free; a
    // side-effecting `a` (e.g. a global read) must keep both evaluations.
    #[test]
    fn or_idempotence_folds_only_pure_operand() {
        let x = RcLocal::default();
        let folded = Binary::new(
            RValue::Local(x.clone()),
            RValue::Local(x.clone()),
            BinaryOperation::Or,
        )
        .reduce();
        assert_eq!(folded, RValue::Local(x));

        let kept = Binary::new(global("foo"), global("foo"), BinaryOperation::Or).reduce();
        assert!(is_binary(&kept, BinaryOperation::Or));
    }

    // P2 (reduce_condition): `X and false` -> false and `X or true` -> true drop the
    // ALWAYS-evaluated left operand, so they may only fold when X is side-effect-free.
    #[test]
    fn and_false_or_true_fold_only_pure_left() {
        assert_eq!(
            Binary::new(local(), boolean(false), BinaryOperation::And).reduce_condition(),
            boolean(false)
        );
        assert_eq!(
            Binary::new(local(), boolean(true), BinaryOperation::Or).reduce_condition(),
            boolean(true)
        );

        let and_kept =
            Binary::new(global("foo"), boolean(false), BinaryOperation::And).reduce_condition();
        assert!(is_binary(&and_kept, BinaryOperation::And));
        let or_kept =
            Binary::new(global("foo"), boolean(true), BinaryOperation::Or).reduce_condition();
        assert!(is_binary(&or_kept, BinaryOperation::Or));
    }

    // The identity sub-cases are unchanged: `X and true` -> X, `X or false` -> X.
    #[test]
    fn and_true_or_false_keep_left() {
        let x = RcLocal::default();
        assert_eq!(
            Binary::new(
                RValue::Local(x.clone()),
                boolean(true),
                BinaryOperation::And
            )
            .reduce_condition(),
            RValue::Local(x.clone())
        );
        // A side-effecting left is preserved here too (it is the result).
        let kept =
            Binary::new(global("foo"), boolean(false), BinaryOperation::Or).reduce_condition();
        assert_eq!(kept, global("foo"));
    }
}
