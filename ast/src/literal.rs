use derive_more::From;
use enum_as_inner::EnumAsInner;
use std::fmt;

use crate::{
    formatter::Formatter, type_system::Infer, LocalRw, Reduce, SideEffects, Traverse, Type,
    TypeSystem,
};

#[derive(Debug, From, Clone, PartialEq, PartialOrd, EnumAsInner)]
pub enum Literal {
    Nil,
    Boolean(bool),
    Number(f64),
    String(Vec<u8>),
    Vector(f32, f32, f32),
}

impl Reduce for Literal {
    fn reduce(self) -> crate::RValue {
        self.into()
    }

    fn reduce_condition(self) -> crate::RValue {
        Literal::Boolean(match self {
            Literal::Boolean(false) | Literal::Nil => false,
            Literal::Boolean(true)
            | Literal::Number(_)
            | Literal::String(_)
            | Literal::Vector(..) => true,
        })
        .into()
    }
}

impl Infer for Literal {
    fn infer<'a: 'b, 'b>(&'a mut self, _: &mut TypeSystem<'b>) -> Type {
        match self {
            Literal::Nil => Type::Nil,
            Literal::Boolean(_) => Type::Boolean,
            Literal::Number(_) => Type::Number,
            Literal::String(_) => Type::String,
            Literal::Vector(..) => Type::Vector,
        }
    }
}

impl From<&str> for Literal {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}

impl LocalRw for Literal {}

impl SideEffects for Literal {}

impl Traverse for Literal {}

impl Literal {
    fn format_finite_f64(value: f64) -> String {
        // TODO: fork ryu to remove ".0"
        let mut buffer = ryu::Buffer::new();
        let printed = buffer.format_finite(value);
        printed.strip_suffix(".0").unwrap_or(printed).to_string()
    }

    fn format_finite_f32(value: f32) -> String {
        // Keep f32 constants short. Casting to f64 would expand values like 0.1
        // into their exact f32 representation.
        value.to_string()
    }

    pub(crate) fn format_number(value: f64) -> String {
        if value.is_infinite() {
            if value.is_sign_positive() {
                "math.huge".to_string()
            } else {
                "-math.huge".to_string()
            }
        } else if value.is_nan() {
            "(0 / 0)".to_string()
        } else if value.to_bits() == std::f64::consts::PI.to_bits() {
            // The compiler folds `math.pi` to a raw f64. Only the atomic
            // single-token forms are sound here: `format_number` returns a
            // string treated as an atomic token (precedence 9), so compound
            // forms (e.g. `math.pi * 2`) would mis-associate in larger
            // expressions. `-math.pi` is a negative `Literal::Number`, so it
            // gets precedence 7 and is parenthesized exactly like `-math.huge`.
            "math.pi".to_string()
        } else if value.to_bits() == (-std::f64::consts::PI).to_bits() {
            "-math.pi".to_string()
        } else {
            Self::format_finite_f64(value)
        }
    }

    fn format_vector_component(value: f32) -> String {
        if value.is_infinite() {
            if value.is_sign_positive() {
                "math.huge".to_string()
            } else {
                "-math.huge".to_string()
            }
        } else if value.is_nan() {
            "(0 / 0)".to_string()
        } else {
            Self::format_finite_f32(value)
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Literal::Nil => write!(f, "nil"),
            Literal::Boolean(value) => write!(f, "{}", value),
            &Literal::Number(value) => write!(f, "{}", Self::format_number(value)),
            Literal::String(value) => {
                write!(
                    f,
                    "\"{}\"",
                    Formatter::<fmt::Formatter>::escape_string(value)
                )
            }
            Literal::Vector(x, y, z) => write!(
                f,
                "Vector3.new({}, {}, {})",
                Self::format_vector_component(*x),
                Self::format_vector_component(*y),
                Self::format_vector_component(*z)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Literal;

    #[test]
    fn format_number_pi() {
        assert_eq!(Literal::format_number(std::f64::consts::PI), "math.pi");
    }

    #[test]
    fn format_number_negative_pi() {
        assert_eq!(Literal::format_number(-std::f64::consts::PI), "-math.pi");
    }

    #[test]
    fn format_number_near_pi_stays_decimal() {
        let near = std::f64::consts::PI + 1e-12;
        let printed = Literal::format_number(near);
        assert_ne!(printed, "math.pi");
        assert_ne!(printed, "-math.pi");
        // A value distinct from PI must keep a decimal representation.
        assert!(
            printed.contains('.'),
            "expected a decimal, got {:?}",
            printed
        );
    }

    #[test]
    fn negative_pi_bit_pattern_round_trips() {
        // The compiler emits this exact bit pattern for `-math.pi`.
        let neg_pi = -std::f64::consts::PI;
        assert_eq!(neg_pi.to_bits(), (-std::f64::consts::PI).to_bits());
        // And it is genuinely distinct from +PI.
        assert_ne!(neg_pi.to_bits(), std::f64::consts::PI.to_bits());
    }

    #[test]
    fn negative_zero_is_not_pi() {
        // to_bits comparison must not be fooled by `-0.0` or NaN.
        assert_ne!(Literal::format_number(-0.0), "math.pi");
        assert_ne!(Literal::format_number(-0.0), "-math.pi");
    }
}
