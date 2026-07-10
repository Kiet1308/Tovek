use std::fmt;

use crate::{Literal, LocalRw, RValue, RcLocal, Reduce, SideEffects, Traverse};

use super::{Binary, BinaryOperation};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum UnaryOperation {
    Not,
    Negate,
    Length,
}

impl fmt::Display for UnaryOperation {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Not => write!(f, "not "),
            Self::Negate => write!(f, "-"),
            Self::Length => write!(f, "#"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Unary {
    pub value: Box<RValue>,
    pub operation: UnaryOperation,
}

impl SideEffects for Unary {
    fn has_side_effects(&self) -> bool {
        // TODO: do this properly
        matches!(
            self.operation,
            UnaryOperation::Negate | UnaryOperation::Length
        ) || self.value.has_side_effects()
    }
}

impl Traverse for Unary {
    fn rvalues_mut(&mut self) -> Vec<&mut RValue> {
        vec![&mut self.value]
    }

    fn rvalues(&self) -> Vec<&RValue> {
        vec![&self.value]
    }
}

impl Reduce for Unary {
    fn reduce(self) -> RValue {
        // TODO: unnecessary clone
        let does_reduce = |r: &RValue| &r.clone().reduce_condition() != r;

        use crate::binary::is_boolean;

        let ensure_boolean = |r| {
            if is_boolean(&r) {
                r
            } else {
                Binary::new(
                    Binary::new(r, Literal::Boolean(true).into(), BinaryOperation::And).into(),
                    Literal::Boolean(false).into(),
                    BinaryOperation::Or,
                )
                .into()
            }
        };

        match (self.value.reduce(), self.operation) {
            (RValue::Literal(Literal::Boolean(value)), UnaryOperation::Not) => {
                RValue::Literal(Literal::Boolean(!value))
            }
            (
                RValue::Unary(Unary {
                    box value,
                    operation: UnaryOperation::Not,
                }),
                UnaryOperation::Not,
            ) => ensure_boolean(value.reduce_condition()),
            (RValue::Literal(Literal::Number(value)), UnaryOperation::Negate) => {
                RValue::Literal(Literal::Number(-value))
            }
            (RValue::Literal(Literal::String(value)), UnaryOperation::Length) => {
                // TODO: is this accurate w/ unicode in Luau?
                RValue::Literal(Literal::Number(value.len() as f64))
            }
            // NOTE (C1): `not (a < b)` is intentionally NOT rewritten to `a >= b`
            // (and the `<=`/`>`/`>=` variants likewise). Those flips are unsound for
            // NaN: `not (nan < 1)` is `true`, but `nan >= 1` is `false`. Ordering
            // comparisons in normal branch conditions already arrive operand-swapped
            // (`a >= b` ⇒ `b <= a`), which is NaN-correct, so dropping these flips only
            // affects an explicit `not (a <rel> b)` expression — kept verbatim here.
            // The equality flips below stay: `not (a == b)` ≡ `a ~= b` even for NaN.
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::Equal,
                }),
                UnaryOperation::Not,
            ) => Binary {
                left,
                right,
                operation: BinaryOperation::NotEqual,
            }
            .reduce(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::NotEqual,
                }),
                UnaryOperation::Not,
            ) => Binary {
                left,
                right,
                operation: BinaryOperation::Equal,
            }
            .reduce(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation,
                }),
                UnaryOperation::Not,
            ) if (operation == BinaryOperation::And || operation == BinaryOperation::Or)
            // TODO: unnecessary clones
                && (does_reduce(&Unary {
                    value: left.clone(),
                    operation: UnaryOperation::Not,
                }.into()) || does_reduce(&Unary {
                    value: right.clone(),
                    operation: UnaryOperation::Not,
                }.into())) =>
            {
                ensure_boolean(
                    Binary {
                        left: Box::new(
                            Unary {
                                value: left,
                                operation: UnaryOperation::Not,
                            }
                            .reduce_condition(),
                        ),
                        right: Box::new(
                            Unary {
                                value: right,
                                operation: UnaryOperation::Not,
                            }
                            .reduce_condition(),
                        ),
                        operation: if operation == BinaryOperation::And {
                            BinaryOperation::Or
                        } else {
                            BinaryOperation::And
                        },
                    }
                    .reduce_condition(),
                )
            }
            (value, operation) => Self {
                value: Box::new(value),
                operation,
            }
            .into(),
        }
    }

    fn reduce_condition(self) -> RValue {
        // `#X` evaluates X as a VALUE (not a condition) and, when it succeeds,
        // yields a number — always truthy. But it can run a `__len` metamethod,
        // raise on a non-lengthable X (`#5`, `#nil`), and X itself may have side
        // effects. Reduce X in value context (so a string/table stays itself) and
        // only fold to `true` for a string or side-effect-free table literal, where
        // none of that applies; otherwise keep `#X` as the condition.
        if self.operation == UnaryOperation::Length {
            let value = self.value.reduce();
            return if !value.has_side_effects()
                && matches!(
                    value,
                    RValue::Literal(Literal::String(_)) | RValue::Table(_)
                ) {
                RValue::Literal(Literal::Boolean(true))
            } else {
                Unary {
                    value: Box::new(value),
                    operation: UnaryOperation::Length,
                }
                .into()
            };
        }

        // TODO: unnecessary clone
        let does_reduce = |r: &RValue| &r.clone().reduce_condition() != r;

        match (self.value.reduce_condition(), self.operation) {
            (RValue::Literal(Literal::Boolean(value)), UnaryOperation::Not) => {
                RValue::Literal(Literal::Boolean(!value))
            }
            (
                RValue::Unary(Unary {
                    box value,
                    operation: UnaryOperation::Not,
                }),
                UnaryOperation::Not,
            ) => value.reduce_condition(),
            (RValue::Literal(Literal::Number(value)), UnaryOperation::Negate) => {
                RValue::Literal(Literal::Number(-value))
            }
            // NOTE (C1): see `reduce` above — the `not (a <rel> b)` → flipped-relation
            // rewrites are omitted here too because they are NaN-unsound, and as a
            // branch condition this is exactly the case that would silently change
            // control flow. The equality flip below is NaN-safe and kept.
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::Equal,
                }),
                UnaryOperation::Not,
            ) => Binary {
                left,
                right,
                operation: BinaryOperation::NotEqual,
            }
            .reduce_condition(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation: BinaryOperation::NotEqual,
                }),
                UnaryOperation::Not,
            ) => Binary {
                left,
                right,
                operation: BinaryOperation::Equal,
            }
            .reduce_condition(),
            (
                RValue::Binary(Binary {
                    left,
                    right,
                    operation,
                }),
                UnaryOperation::Not,
            ) if (operation == BinaryOperation::And || operation == BinaryOperation::Or)
            // TODO: unnecessary clones
                && (does_reduce(&Unary {
                    value: left.clone(),
                    operation: UnaryOperation::Not,
                }.into()) || does_reduce(&Unary {
                    value: right.clone(),
                    operation: UnaryOperation::Not,
                }.into())) =>
            {
                Binary {
                    left: Box::new(
                        Unary {
                            value: left,
                            operation: UnaryOperation::Not,
                        }
                        .reduce_condition(),
                    ),
                    right: Box::new(
                        Unary {
                            value: right,
                            operation: UnaryOperation::Not,
                        }
                        .reduce_condition(),
                    ),
                    operation: if operation == BinaryOperation::And {
                        BinaryOperation::Or
                    } else {
                        BinaryOperation::And
                    },
                }
                .reduce_condition()
            }
            (value, operation) => Self {
                value: Box::new(value),
                operation,
            }
            .into(),
        }
    }
}

impl Unary {
    pub fn new(value: RValue, operation: UnaryOperation) -> Self {
        Self {
            value: Box::new(value),
            operation,
        }
    }

    pub fn precedence(&self) -> usize {
        7
    }

    pub fn group(&self) -> bool {
        (self.precedence() > self.value.precedence())
            || (matches!(self.operation, UnaryOperation::Negate)
                && (matches!(
                    *self.value,
                    RValue::Unary(Unary {
                        operation: UnaryOperation::Negate,
                        ..
                    })
                ) || matches!(
                    *self.value,
                    RValue::Literal(Literal::Number(value))
                        if value.is_sign_negative() && !value.is_nan()
                )))
    }
}

impl LocalRw for Unary {
    fn values_read(&self) -> Vec<&RcLocal> {
        self.value.values_read()
    }

    fn values_read_mut(&mut self) -> Vec<&mut RcLocal> {
        self.value.values_read_mut()
    }
}

impl fmt::Display for Unary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.operation,
            if self.group() {
                format!("({})", self.value)
            } else {
                format!("{}", self.value)
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{Global, Literal, RValue, RcLocal, Reduce, Table, Unary, UnaryOperation};

    fn len_condition(value: RValue) -> RValue {
        Unary::new(value, UnaryOperation::Length).reduce_condition()
    }
    fn is_length(r: &RValue) -> bool {
        matches!(r, RValue::Unary(u) if u.operation == UnaryOperation::Length)
    }

    // P4: `#X` as a condition only folds to `true` for a string/table literal (where
    // `#` cannot error and X has no side effects). For a local/global/call it stays
    // `#X` so X still runs and any error is preserved — never an invalid `#true`.
    #[test]
    fn length_of_local_or_side_effect_stays_a_length() {
        assert!(is_length(&len_condition(RValue::Local(RcLocal::default()))));
        assert!(is_length(&len_condition(RValue::Global(Global::from(
            "foo"
        )))));
    }

    #[test]
    fn length_of_string_or_table_literal_folds_true() {
        assert_eq!(
            len_condition(RValue::Literal(Literal::String(b"abc".to_vec()))),
            RValue::Literal(Literal::Boolean(true))
        );
        assert_eq!(
            len_condition(RValue::Table(Table::default())),
            RValue::Literal(Literal::Boolean(true))
        );
    }
}
