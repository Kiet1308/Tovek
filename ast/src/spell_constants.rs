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
//! A fraction prints over the denominator its source most likely divided by
//! when that is not its lowest one: `190 / 255` (a color channel) rather than
//! `38 / 51`, `28 / 60` in a chunk whose times are in sixtieths ([`Fractions`]
//! has the rule). The rationals are equal, so the double is the same.
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
/// Denominators sources divide by for a reason of their own, in order of
/// preference: frames or seconds per minute, color channels, degrees, screen
/// sizes. (A denominator of 100 or 1000 never shows: such a fraction is a
/// short decimal.)
const PREFERRED_DENOMINATORS: [u64; 5] = [60, 255, 360, 1080, 1920];

/// `(p, q)` with `p / q` bit-equal to `value`, `q <= 1000`, `|p| < 2^31`, in
/// lowest terms, when that spelling is shorter than the shortest decimal and
/// the decimal has at least ten significant digits.
///
/// Any rational `p / q` that rounds to `value` lies within half an ulp of it,
/// which for `|value| < 2^31` and `q <= 1000` is closer than `1 / (2 q^2)`:
/// by Legendre's theorem it is a convergent of the exact binary value. The
/// first bit-equal convergent is the one with the smallest denominator.
pub fn exact_fraction(value: f64) -> Option<(i64, u64)> {
    let (p, q, _) = lowest_fraction(value.abs())?;
    Some((if value < 0.0 { -(p as i64) } else { p as i64 }, q))
}

/// [`exact_fraction`] of a magnitude, with the length of its decimal.
fn lowest_fraction(magnitude: f64) -> Option<(u64, u64, usize)> {
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
    (spelled_length(p, q) < decimal.len()).then_some((p, q, decimal.len()))
}

fn spelled_length(p: u64, q: u64) -> usize {
    digits(p) + " / ".len() + digits(q)
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
///
/// A fraction prints over a multiple `D` of its lowest denominator `q` when
/// that is the denominator its source most likely divided by. A unit
/// fraction never moves: `1 / 30` and `1 / 240` are how sources write rates
/// and steps. Of the others, call a fraction odd when `q` is above 60 or has
/// a prime factor above 5 (`38 / 51`, `373 / 480`: lowest forms nobody
/// writes), plain otherwise (`7 / 15`).
/// 1. An odd fraction takes a [`PREFERRED_DENOMINATORS`] multiple: when `q`
///    has a prime factor above 5 (`38 / 51` is `190 / 255`), or when at least
///    two odd fractions of the chunk divide `D` (a layout in 1920x1080
///    ratios: `373 / 480` is `1492 / 1920` beside `589 / 960`). The most odd
///    fractions dividing it, then the preferred order, choose among several.
/// 2. Then a fraction alone with its lowest denominator follows a preferred
///    or odd denominator at least two other fractions of the chunk now print
///    with (a cutscene timed in sixtieths: `7 / 15` is `28 / 60` beside
///    `11 / 60` and `67 / 60`); the most used one wins, then the smaller. A
///    plain denominator is no source divisor however often it is used:
///    `2 / 3` beside `1 / 9` and `2 / 9` stays.
///
/// The spelling must still be shorter than the decimal. The rationals are
/// equal, so `p·k / (q·k)` divides to the same double (checked anyway).
#[derive(Default)]
pub struct Fractions(FxHashMap<u64, (u64, u64)>);

impl Fractions {
    pub fn of_constants(constants: impl IntoIterator<Item = f64>) -> Self {
        // (magnitude bits, lowest p, lowest q, decimal length, spelled p, spelled q)
        let mut fractions = Vec::new();
        let mut seen = FxHashSet::default();
        for constant in constants {
            let magnitude = constant.abs();
            if seen.insert(magnitude.to_bits())
                && let Some((p, q, decimal)) = lowest_fraction(magnitude)
            {
                fractions.push((magnitude.to_bits(), p, q, decimal, p, q));
            }
        }
        if fractions.len() > 1 || fractions.first().is_some_and(|&(_, _, q, ..)| !five_smooth(q)) {
            Self::prefer_source_denominators(&mut fractions);
        }
        Self(fractions.into_iter().map(|(bits, .., p, q)| (bits, (p, q))).collect())
    }

    /// The two steps of the rule above, in place on `fractions`' spellings.
    fn prefer_source_denominators(fractions: &mut [(u64, u64, u64, usize, u64, u64)]) {
        // `p·(d/q) / d` when that is a shorter, bit-equal spelling.
        let respelled = |&(bits, p, q, decimal, ..): &(u64, u64, u64, usize, u64, u64), d: u64| {
            let k = d / q;
            (p > 1 && d % q == 0 && k >= 2 && p * k < MAX_NUMERATOR && spelled_length(p * k, d) < decimal
                && ((p * k) as f64 / d as f64).to_bits() == bits)
                .then_some((p * k, d))
        };
        let odd = |q: u64| q > 60 || !five_smooth(q);
        // The lowest denominators of the odd fractions that may move.
        let mut odd_lowest: FxHashMap<u64, usize> = FxHashMap::default();
        for &(_, p, q, ..) in fractions.iter() {
            if p > 1 && odd(q) {
                *odd_lowest.entry(q).or_default() += 1;
            }
        }
        // Step 1.
        for fraction in fractions.iter_mut().filter(|fraction| odd(fraction.2)) {
            let factor_above_5 = !five_smooth(fraction.2);
            let best = PREFERRED_DENOMINATORS
                .iter()
                .enumerate()
                .filter_map(|(order, &d)| {
                    let support: usize = odd_lowest.iter().filter(|&(&q, _)| d % q == 0).map(|(_, &n)| n).sum();
                    let spelled = respelled(fraction, d).filter(|_| factor_above_5 || support >= 2)?;
                    Some((support, usize::MAX - order, spelled))
                })
                .max_by_key(|&(support, order, _)| (support, order));
            if let Some((.., (p, d))) = best {
                (fraction.4, fraction.5) = (p, d);
            }
        }
        // Step 2.
        let mut used: FxHashMap<u64, usize> = FxHashMap::default();
        for &(.., d) in fractions.iter() {
            *used.entry(d).or_default() += 1;
        }
        let mut denominators: Vec<(usize, u64)> = used
            .iter()
            .filter(|&(&d, &n)| n >= 2 && (odd(d) || PREFERRED_DENOMINATORS.contains(&d)))
            .map(|(&d, &n)| (n, d))
            .collect();
        // The most used first, then the smaller.
        denominators.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for fraction in fractions.iter_mut().filter(|fraction| fraction.5 == fraction.2 && used[&fraction.2] == 1) {
            if let Some((p, d)) = denominators.iter().find_map(|&(_, d)| respelled(fraction, d)) {
                (fraction.4, fraction.5) = (p, d);
            }
        }
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

/// Whether `n` has no prime factor above 5: the denominators people write
/// (`3`, `12`, `60`), against `51` or `7`.
fn five_smooth(mut n: u64) -> bool {
    for factor in [2, 3, 5] {
        while n % factor == 0 {
            n /= factor;
        }
    }
    n == 1
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

    fn spelled_over(constants: &[f64], value: f64) -> Option<(u64, u64)> {
        Fractions::of_constants(constants.iter().copied()).0.get(&value.to_bits()).copied()
    }

    /// A color channel prints over 255 on its own; a layout ratio over the
    /// screen size once two odd fractions of the chunk agree on it.
    #[test]
    fn a_lowest_form_nobody_writes_takes_the_preferred_denominator() {
        assert_eq!(spelled_over(&[190.0 / 255.0], 190.0 / 255.0), Some((190, 255)));
        assert_eq!(spelled_over(&[9.0 / 255.0], 9.0 / 255.0), Some((9, 255)));
        let layout = [1492.0 / 1920.0, 1178.0 / 1920.0, 814.0 / 1080.0, 274.0 / 1080.0, 970.0 / 1080.0, 13.0 / 60.0];
        assert_eq!(spelled_over(&layout, 1492.0 / 1920.0), Some((1492, 1920)));
        assert_eq!(spelled_over(&layout, 1178.0 / 1920.0), Some((1178, 1920)));
        assert_eq!(spelled_over(&layout, 814.0 / 1080.0), Some((814, 1080)));
        assert_eq!(spelled_over(&layout, 970.0 / 1080.0), Some((970, 1080)));
        // ... and a plain fraction beside them follows the most used one.
        assert_eq!(spelled_over(&layout, 13.0 / 60.0), Some((234, 1080)));
        // Alone, a smooth denominator is as likely the source's own; a unit
        // fraction never moves, nor counts for another.
        assert_eq!(spelled_over(&[1492.0 / 1920.0], 1492.0 / 1920.0), Some((373, 480)));
        assert_eq!(spelled_over(&[1.0 / 240.0, 7.0 / 120.0], 7.0 / 120.0), Some((7, 120)));
        assert_eq!(spelled_over(&[1.0 / 240.0, 7.0 / 120.0], 1.0 / 240.0), Some((1, 240)));
        assert_eq!(spelled_over(&[1.0 / 51.0], 1.0 / 51.0), Some((1, 51)));
        // No preferred multiple: the lowest form stays.
        assert_eq!(spelled_over(&[8.0 / 11.0], 8.0 / 11.0), Some((8, 11)));
        assert_eq!(spelled_over(&[1.0 / 7.0], 1.0 / 7.0), Some((1, 7)));
    }

    /// `1 / 3` stays a third unless the chunk divides by one denominator
    /// again and again.
    #[test]
    fn a_plain_fraction_follows_only_a_denominator_the_chunk_keeps_using() {
        assert_eq!(spelled_over(&[1.0 / 3.0], 1.0 / 3.0), Some((1, 3)));
        assert_eq!(spelled_over(&[1.0 / 3.0, 1.0 / 60.0], 1.0 / 3.0), Some((1, 3)));
        let cutscene = [7.0 / 15.0, 11.0 / 60.0, 67.0 / 60.0, 2.0 / 3.0, 107.0 / 30.0];
        assert_eq!(spelled_over(&cutscene, 7.0 / 15.0), Some((28, 60)));
        assert_eq!(spelled_over(&cutscene, 2.0 / 3.0), Some((40, 60)));
        assert_eq!(spelled_over(&cutscene, 107.0 / 30.0), Some((214, 60)));
        assert_eq!(spelled_over(&cutscene, 11.0 / 60.0), Some((11, 60)));
        // Colors in 255ths carry two thirds along, unless a third is there
        // too (thirds the chunk divides by); a plain denominator others use
        // is no source divisor.
        let colors = [190.0 / 255.0, 200.0 / 255.0, 2.0 / 3.0];
        assert_eq!(spelled_over(&colors, 2.0 / 3.0), Some((170, 255)));
        assert_eq!(spelled_over(&[190.0 / 255.0, 200.0 / 255.0, 2.0 / 3.0, 1.0 / 3.0], 2.0 / 3.0), Some((2, 3)));
        assert_eq!(spelled_over(&[2.0 / 9.0, 4.0 / 9.0, 2.0 / 3.0], 2.0 / 3.0), Some((2, 3)));
    }

    /// Every respelled fraction is the same double, and shorter than the
    /// decimal it replaces.
    #[test]
    fn respelled_fractions_stay_exact_and_short() {
        let chunk = [190.0 / 255.0, 254.0 / 255.0, 13.0 / 51.0, 1.0 / 3.0, 1492.0 / 1920.0, 1178.0 / 1920.0, 13.0 / 60.0];
        let fractions = Fractions::of_constants(chunk);
        for value in chunk {
            let (p, q) = fractions.0[&value.to_bits()];
            assert_eq!((p as f64 / q as f64).to_bits(), value.to_bits(), "{p} / {q}");
            assert!(spelled_length(p, q) < ryu::Buffer::new().format_finite(value).len(), "{p} / {q}");
        }
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
