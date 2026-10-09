//! Number constants the compiler folded from a fraction, printed as that
//! fraction: `x * 0.7272727272727273` reads `x * (8 / 11)`.
//!
//! IEEE division is correctly rounded, so `p / q` evaluates to the double
//! nearest the rational: Luau folds it to the identical constant at `-O1`
//! and above, and divides at run time to the same value at `-O0`. Both
//! operands are numbers, so no metamethod and no global is involved; the
//! spelling assumes nothing about the environment. A literal is spelled only
//! when `p / q` is bit-equal to it (`-0`, infinities and NaN never are) and
//! prints shorter than its decimal.
//!
//! Only `NUMBER` constants: integer constants (`Literal::Integer`) and vector
//! components are other literal kinds and stay as they are. A literal table
//! or index key (`t[0.5]`) keeps its literal too: the table passes after this
//! one read such keys as slots (`rebuild_table_literals`, `compound_bases`).
//!
//! The fractions come from the chunk's constant table ([`Fractions`]), so a
//! chunk without one costs no walk. The pass runs late (`luau-lifter`, before
//! `library_constants` and the register check): passes before it compare and
//! count raw literals. The constant-folded argument recovery of the
//! de-inliner (plan item E2, which must see raw literals) belongs to the
//! de-inline stage, which runs long before this pass; `rehoist_constants`,
//! which runs between them, consults [`exact_fraction`] instead of the
//! spelled tree.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Binary, BinaryOperation, Block, LValue, Literal, RValue, Select, Statement, Traverse};

/// The largest denominator spelled.
const MAX_DENOMINATOR: u64 = 1000;
/// Numerators stay below 2^31.
const MAX_NUMERATOR: u64 = 1 << 31;
/// A decimal with fewer significant digits reads as well as any fraction
/// (`0.35`, `0.125`).
const MIN_SIGNIFICANT_DIGITS: usize = 10;

/// `(p, q)` with `p / q` bit-equal to `value`, `q <= 1000`, `|p| < 2^31`, in
/// lowest terms, when that spelling is shorter than the shortest decimal and
/// the decimal has at least ten significant digits.
///
/// Any rational `p / q` that rounds to `value` lies within half an ulp of it,
/// which for `|value| < 2^31` and `q <= 1000` is closer than `1 / (2 q^2)`:
/// by Legendre's theorem it is a convergent of the exact binary value. The
/// first bit-equal convergent is the one with the smallest denominator.
pub fn exact_fraction(value: f64) -> Option<(i64, u64)> {
    let magnitude = value.abs();
    // `p >= 1` and `q <= 1000` reach no lower than 1/1000; `p < 2^31` no higher.
    if !(1.0 / MAX_DENOMINATOR as f64..MAX_NUMERATOR as f64).contains(&magnitude) || magnitude.fract() == 0.0 {
        return None;
    }
    let mut buffer = ryu::Buffer::new();
    let decimal = buffer.format_finite(magnitude);
    if significant_digits(decimal) < MIN_SIGNIFICANT_DIGITS {
        return None;
    }
    let (p, q) = convergent(magnitude)?;
    let spelled = digits(p) + " / ".len() + digits(q);
    (spelled < decimal.len()).then(|| (if value < 0.0 { -(p as i64) } else { p as i64 }, q))
}

/// Digits of a shortest decimal (`0.016666666666666666`, `1.5e-7`) from its
/// first nonzero one, the exponent aside.
fn significant_digits(decimal: &str) -> usize {
    let mantissa = decimal.split('e').next().unwrap_or(decimal);
    mantissa.bytes().filter(u8::is_ascii_digit).skip_while(|&digit| digit == b'0').count()
}

fn digits(value: u64) -> usize {
    value.checked_ilog10().map_or(1, |log| log as usize + 1)
}

/// The first continued-fraction convergent of `magnitude` (a positive normal
/// double in range) whose division gives `magnitude` back exactly.
fn convergent(magnitude: f64) -> Option<(u64, u64)> {
    // magnitude = mantissa / 2^shift exactly; in range, 22 <= shift <= 62.
    let bits = magnitude.to_bits();
    let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
    let shift = 1075 - (bits >> 52) as u32;
    let (mut a, mut b) = (u128::from(mantissa), 1u128 << shift);
    let (mut h0, mut h1, mut k0, mut k1) = (0u128, 1u128, 1u128, 0u128);
    while b != 0 {
        let quotient = a / b;
        (a, b) = (b, a % b);
        (h0, h1) = (h1, quotient * h1 + h0);
        (k0, k1) = (k1, quotient * k1 + k0);
        if k1 > u128::from(MAX_DENOMINATOR) {
            return None;
        }
        if h1 < u128::from(MAX_NUMERATOR) && (h1 as f64 / k1 as f64).to_bits() == bits {
            return Some((h1 as u64, k1 as u64));
        }
    }
    None
}

/// The exact fractions of a chunk's number constants, by magnitude. Every
/// number literal that is not an integer comes from a `NUMBER` constant (an
/// integer one is `Literal::Integer`, a `LOADN` an integer), so the table
/// holds every literal the pass can spell, and a chunk with none (most of
/// them) is not walked at all.
#[derive(Default)]
pub struct Fractions(FxHashMap<u64, (u64, u64)>);

impl Fractions {
    pub fn of_constants(constants: impl IntoIterator<Item = f64>) -> Self {
        let mut fractions = FxHashMap::default();
        for constant in constants {
            if let Some((p, q)) = exact_fraction(constant) {
                fractions.insert(constant.abs().to_bits(), (p.unsigned_abs(), q));
            }
        }
        Self(fractions)
    }

    /// Spell every literal of the tree whose magnitude has a fraction here.
    pub fn spell(&self, body: &mut Block) {
        if !self.0.is_empty() {
            self.spell_block(body, &mut FxHashSet::default());
        }
    }

    fn spell_block(&self, block: &mut Block, visited: &mut FxHashSet<usize>) {
        for statement in &mut block.0 {
            statement.visit_rvalues_mut(&mut |value| {
                self.spell_value(value, visited);
                true
            });
            statement.visit_lvalues_mut(&mut |target| {
                if let LValue::Index(index) = target {
                    self.spell_value(&mut index.left, visited);
                    self.spell_key(&mut index.right, visited);
                }
                true
            });
            for_each_child_block(statement, &mut |child| self.spell_block(child, visited));
        }
    }

    /// Matched out by kind, not through `Traverse`, so a leaf costs no call.
    fn spell_value(&self, value: &mut RValue, visited: &mut FxHashSet<usize>) {
        match value {
            RValue::Literal(Literal::Number(number)) => {
                if let Some(&(p, q)) = self.0.get(&number.abs().to_bits()) {
                    let p = if number.is_sign_negative() { -(p as f64) } else { p as f64 };
                    *value = Binary::new(Literal::Number(p).into(), Literal::Number(q as f64).into(), BinaryOperation::Div).into();
                }
            }
            RValue::Closure(closure) => {
                // De-inline copies can share one body: spell it once.
                if visited.insert(triomphe::Arc::as_ptr(&closure.function.0) as usize) {
                    self.spell_block(&mut closure.function.lock().body, visited);
                }
            }
            RValue::Table(table) => {
                for (key, field) in &mut table.0 {
                    if let Some(key) = key {
                        self.spell_key(key, visited);
                    }
                    self.spell_value(field, visited);
                }
            }
            RValue::Index(index) => {
                self.spell_value(&mut index.left, visited);
                self.spell_key(&mut index.right, visited);
            }
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                self.spell_value(&mut call.value, visited);
                call.arguments.iter_mut().for_each(|argument| self.spell_value(argument, visited));
            }
            RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
                self.spell_value(&mut call.value, visited);
                call.arguments.iter_mut().for_each(|argument| self.spell_value(argument, visited));
            }
            RValue::Unary(unary) => self.spell_value(&mut unary.value, visited),
            RValue::Binary(binary) => {
                self.spell_value(&mut binary.left, visited);
                self.spell_value(&mut binary.right, visited);
            }
            RValue::IfExpression(expression) => {
                self.spell_value(&mut expression.condition, visited);
                self.spell_value(&mut expression.then_value, visited);
                self.spell_value(&mut expression.else_value, visited);
            }
            RValue::Local(_) | RValue::Global(_) | RValue::Literal(_) | RValue::VarArg(_) | RValue::Select(Select::VarArg(_)) => {}
        }
    }

    /// A key: a literal names a slot and stays; a computed key is a value.
    fn spell_key(&self, key: &mut RValue, visited: &mut FxHashSet<usize>) {
        if !matches!(key, RValue::Literal(_)) {
            self.spell_value(key, visited);
        }
    }
}

fn for_each_child_block(statement: &mut Statement, visit: &mut impl FnMut(&mut Block)) {
    match statement {
        Statement::If(r#if) => {
            visit(&mut r#if.then_block.lock());
            visit(&mut r#if.else_block.lock());
        }
        Statement::While(r#while) => visit(&mut r#while.block.lock()),
        Statement::Repeat(repeat) => visit(&mut repeat.block.lock()),
        Statement::NumericFor(numeric_for) => visit(&mut numeric_for.block.lock()),
        Statement::GenericFor(generic_for) => visit(&mut generic_for.block.lock()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Call, Global, Index, RcLocal, Table, Unary, UnaryOperation};

    fn fraction(p: f64, q: f64) -> f64 {
        p / q
    }

    #[test]
    fn repeating_decimals_spell_as_their_fraction() {
        assert_eq!(exact_fraction(fraction(8.0, 11.0)), Some((8, 11)));
        assert_eq!(exact_fraction(fraction(11.0, 30.0)), Some((11, 30)));
        assert_eq!(exact_fraction(fraction(1.0, 60.0)), Some((1, 60)));
        assert_eq!(exact_fraction(fraction(69.0, 56.0)), Some((69, 56)));
        assert_eq!(exact_fraction(-fraction(13.0, 60.0)), Some((-13, 60)));
        assert_eq!(exact_fraction(fraction(999.0, 1000.0) + 0.0), None, "0.999 is short");
        assert_eq!(exact_fraction(fraction(1999.0, 997.0)), Some((1999, 997)));
        // Lowest terms: 22/60 is 11/30.
        assert_eq!(exact_fraction(fraction(22.0, 60.0)), Some((11, 30)));
    }

    #[test]
    fn denominators_over_1000_numerators_over_2_31_and_near_misses_are_refused() {
        assert_eq!(exact_fraction(fraction(1000.0, 1001.0)), None);
        assert_eq!(exact_fraction(fraction(2.0, 1001.0)), None);
        assert_eq!(exact_fraction(fraction(4_294_967_295.0, 7.0)), None);
        // One ulp away from 8/11 is no fraction.
        assert_eq!(exact_fraction(f64::from_bits(fraction(8.0, 11.0).to_bits() + 1)), None);
    }

    #[test]
    fn short_decimals_integers_and_non_finite_values_stay() {
        for value in [0.35, 0.1, 0.0001, 0.125, 0.03125, 5.0, -3.0, 0.0, -0.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 1e-7] {
            assert_eq!(exact_fraction(value), None, "{value}");
        }
    }

    /// A single-precision constant widened to double (a Roblox property set
    /// from a `float`) has no short exact fraction.
    #[test]
    fn widened_floats_stay_raw() {
        for value in [0.3400000035762787, 0.8705882430076599, f64::from(1.0f32 / 3.0), f64::from(0.1f32)] {
            assert_eq!(exact_fraction(value), None, "{value}");
        }
    }

    #[test]
    fn spelled_only_where_shorter_than_the_decimal() {
        assert_eq!(significant_digits("0.016666666666666666"), 17);
        assert_eq!(significant_digits("1.5e-7"), 2);
        assert_eq!(significant_digits("120.25"), 5);
        // 1/3: "1 / 3" against "0.3333333333333333".
        assert_eq!(exact_fraction(fraction(1.0, 3.0)), Some((1, 3)));
        // 999.999 with a denominator of 1000 would need "999999 / 1000".
        assert_eq!(exact_fraction(999.999), None);
    }

    /// The fractions of the constants the tests use.
    fn fractions() -> Fractions {
        Fractions::of_constants([8.0 / 11.0, 1.0 / 3.0, 7.0 / 60.0, 0.5, 2.0])
    }

    fn spelled(value: RValue) -> String {
        let mut body = Block(vec![Statement::Return(crate::Return::new(vec![value]))]);
        fractions().spell(&mut body);
        body.to_string()
    }

    fn number(value: f64) -> RValue {
        Literal::Number(value).into()
    }

    fn local() -> RValue {
        let local = RcLocal::new(crate::Local::new(Some("x".into())));
        local.into()
    }

    /// The spelling groups like any division: `x * (8 / 11)` keeps its
    /// parentheses, `8 / 11 * x` needs none.
    #[test]
    fn spellings_take_the_parentheses_division_needs() {
        let eight_elevenths = fraction(8.0, 11.0);
        let binary = |left, right, operation| -> RValue { Binary::new(left, right, operation).into() };
        assert_eq!(spelled(binary(local(), number(eight_elevenths), BinaryOperation::Mul)), "return x * (8 / 11)");
        assert_eq!(spelled(binary(number(eight_elevenths), local(), BinaryOperation::Mul)), "return 8 / 11 * x");
        assert_eq!(spelled(binary(local(), number(eight_elevenths), BinaryOperation::IDiv)), "return x // (8 / 11)");
        assert_eq!(spelled(binary(number(2.0), number(fraction(1.0, 3.0)), BinaryOperation::Pow)), "return 2 ^ (1 / 3)");
        assert_eq!(spelled(binary(number(fraction(1.0, 3.0)), number(2.0), BinaryOperation::Pow)), "return (1 / 3) ^ 2");
        assert_eq!(spelled(binary(local(), number(-eight_elevenths), BinaryOperation::Sub)), "return x - -8 / 11");
        assert_eq!(spelled(binary(local(), number(eight_elevenths), BinaryOperation::Add)), "return x + 8 / 11");
        assert_eq!(spelled(Unary::new(number(eight_elevenths), UnaryOperation::Negate).into()), "return -(8 / 11)");
        assert_eq!(spelled(binary(local(), number(eight_elevenths), BinaryOperation::LessThan)), "return x < 8 / 11");
    }

    /// Call arguments and table values are spelled; table keys and index
    /// keys keep their literal; vector constants and integers are untouched.
    #[test]
    fn values_are_spelled_and_keys_vectors_and_integers_are_not() {
        let third = fraction(1.0, 3.0);
        let call = Call::new(Global::from("wait").into(), vec![number(fraction(7.0, 60.0))]);
        assert_eq!(spelled(call.into()), "return wait(7 / 60)");
        let table = Table::new(vec![(None, number(third)), (Some(number(third)), number(third))]);
        assert_eq!(spelled(table.into()), "return {\n\t1 / 3,\n\t[0.3333333333333333] = 1 / 3\n}");
        assert_eq!(spelled(Index::new(local(), number(third)).into()), "return x[0.3333333333333333]");
        let computed = Binary::new(local(), number(third), BinaryOperation::Mul);
        assert_eq!(spelled(Index::new(local(), computed.into()).into()), "return x[x * (1 / 3)]");
        let vector = Literal::VectorD(third, 0.5, 2.0);
        assert_eq!(spelled(vector.clone().into()), format!("return {vector}"));
        assert_eq!(spelled(Literal::Integer(7).into()), "return 7i");
    }

    /// Spelling twice changes nothing: `8` and `11` are integers. A literal
    /// that is no constant of the chunk is left alone.
    #[test]
    fn spelling_is_idempotent_and_limited_to_the_chunk_constants() {
        let mut body = Block(vec![Statement::Return(crate::Return::new(vec![number(fraction(8.0, 11.0))]))]);
        fractions().spell(&mut body);
        let once = body.to_string();
        fractions().spell(&mut body);
        assert_eq!(body.to_string(), once);
        assert_eq!(spelled(number(fraction(11.0, 30.0))), "return 0.36666666666666664");
        assert!(Fractions::of_constants([0.5, 3.0, f64::NAN]).0.is_empty());
    }
}
