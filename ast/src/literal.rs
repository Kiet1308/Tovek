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
    /// Preserve Luau's signed integer type and all 64 bits (the `i` suffix).
    Integer(i64),
    String(Vec<u8>),
    Vector(f32, f32, f32),
    /// A Luau vector constant whose components were encoded as doubles.
    ///
    /// Keep this distinct from [`Vector`] so legacy f32 constants retain their
    /// compact formatting while v13+ constants are never narrowed.
    VectorD(f64, f64, f64),
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
            | Literal::Integer(_)
            | Literal::String(_)
            | Literal::Vector(..)
            | Literal::VectorD(..) => true,
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
            Literal::Integer(_) => Type::Integer,
            Literal::String(_) => Type::String,
            Literal::Vector(..) | Literal::VectorD(..) => Type::Vector,
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
    /// Long brackets normalize CR/LF and discard the first newline. Emit one
    /// framing LF (which the lexer discards), then the exact payload. Decline
    /// CR, control bytes and invalid UTF-8 rather than changing constant bytes.
    fn long_string(value: &[u8]) -> Option<String> {
        let text = std::str::from_utf8(value).ok()?;
        let newlines = value.iter().filter(|&&byte| byte == b'\n').count();
        if newlines == 0 || (newlines < 2 && value.len() < 80)
            || text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return None;
        }
        for count in 0..=16 {
            let equals = "=".repeat(count);
            let close = format!("]{equals}]");
            // Include the closing delimiter in the search: a payload ending in
            // `]` would otherwise terminate `[[...]]]` one byte too early.
            if format!("{text}{close}").find(&close) == Some(text.len()) {
                return Some(format!("[{equals}[\n{text}{close}"));
            }
        }
        None
    }
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
                "1e999".to_string()
            } else {
                "-1e999".to_string()
            }
        } else if value.is_nan() {
            "(0 / 0)".to_string()
        } else {
            // Constants must not acquire a dependency on a shadowed or mutated
            // global (including math.pi). An overflowing decimal exponent is
            // also an atomic Luau number token for infinities above.
            Self::format_finite_f64(value)
        }
    }

    fn format_vector_component(value: f32) -> String {
        if value.is_infinite() {
            if value.is_sign_positive() {
                "1e999".to_string()
            } else {
                "-1e999".to_string()
            }
        } else if value.is_nan() {
            "(0 / 0)".to_string()
        } else {
            Self::format_finite_f32(value)
        }
    }

    fn format_vector_component_d(value: f64) -> String {
        Self::format_number(value)
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Literal::Nil => write!(f, "nil"),
            Literal::Boolean(value) => write!(f, "{}", value),
            &Literal::Number(value) => write!(f, "{}", Self::format_number(value)),
            // Decimal tokens are parsed as positive i64 before unary minus;
            // MIN's magnitude overflows. Hex tokens preserve all 64 bits.
            Literal::Integer(i64::MIN) => write!(f, "0x8000000000000000i"),
            Literal::Integer(value) => write!(f, "{value}i"),
            Literal::String(value) => {
                if let Some(long) = Self::long_string(value) {
                    return write!(f, "{long}");
                }
                write!(
                    f,
                    "\"{}\"",
                    Formatter::<fmt::Formatter>::escape_string(value)
                )
            }
            Literal::Vector(x, y, z) => write!(
                f,
                "vector.create({}, {}, {})",
                Self::format_vector_component(*x),
                Self::format_vector_component(*y),
                Self::format_vector_component(*z)
            ),
            Literal::VectorD(x, y, z) => write!(
                f,
                "vector.create({}, {}, {})",
                Self::format_vector_component_d(*x),
                Self::format_vector_component_d(*y),
                Self::format_vector_component_d(*z)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Literal;

    #[test]
    fn integer_tokens_preserve_type_precision_and_signed_boundaries() {
        assert_eq!(Literal::Integer(9007199254740993).to_string(), "9007199254740993i");
        assert_eq!(Literal::Integer(i64::MIN).to_string(), "0x8000000000000000i");
        assert_eq!(Literal::Integer(i64::MAX).to_string(), "9223372036854775807i");
        let value = crate::Unary::new(Literal::Integer(-42).into(), crate::UnaryOperation::Negate);
        assert_eq!(value.to_string(), "-(-42i)");
    }

    #[test]
    fn long_strings_keep_leading_newline_and_choose_delimiters() {
        assert_eq!(Literal::String(b"\nfirst\nsecond\n".to_vec()).to_string(), "[[\n\nfirst\nsecond\n]]");
        assert_eq!(Literal::String(b"a]]\nb]=]\nc".to_vec()).to_string(), "[==[\na]]\nb]=]\nc]==]");
        assert_eq!(Literal::String(b"a\nb\nc]".to_vec()).to_string(), "[=[\na\nb\nc]]=]");
    }

    #[test]
    fn long_strings_decline_normalizing_or_nonprintable_bytes() {
        for value in [b"a\r\nb\nc".as_slice(), b"a\nb\n\0", b"a\nb\n\xff"] {
            assert!(Literal::String(value.to_vec()).to_string().starts_with('"'));
        }
    }

    #[test]
    fn format_number_pi() {
        assert_eq!(Literal::format_number(std::f64::consts::PI), "3.141592653589793");
    }

    #[test]
    fn format_number_negative_pi() {
        assert_eq!(Literal::format_number(-std::f64::consts::PI), "-3.141592653589793");
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

    #[test]
    fn vectord_format_preserves_double_components() {
        let value = Literal::VectorD(1.0000000000000002, 1e-300, 16777217.0);
        assert_eq!(
            value.to_string(),
            "vector.create(1.0000000000000002, 1e-300, 16777217)"
        );
        let huge = Literal::VectorD(1e300, f64::INFINITY, f64::NAN);
        assert_eq!(
            huge.to_string(),
            "vector.create(1e300, 1e999, (0 / 0))"
        );
    }
}
