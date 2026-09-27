use derive_more::From;
use enum_as_inner::EnumAsInner;
use std::fmt;

use crate::{
    formatter::Formatter, type_system::Infer, LocalRw, Reduce, SideEffects, Traverse, Type,
    TypeSystem,
};

#[derive(Debug, From, PartialEq, PartialOrd, EnumAsInner)]
#[cfg_attr(not(feature = "byte-storage-trace"), derive(Clone))]
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

// Keep the default derived implementation intact. The attribution build uses
// the same field copies (including all floating-point bits) and Vec::clone;
// only string clone counts and byte lengths are recorded, never contents.
#[cfg(feature = "byte-storage-trace")]
impl Clone for Literal {
    fn clone(&self) -> Self {
        match self {
            Self::Nil => Self::Nil,
            Self::Boolean(value) => Self::Boolean(*value),
            Self::Number(value) => Self::Number(*value),
            Self::Integer(value) => Self::Integer(*value),
            Self::String(value) => {
                crate::telemetry::count("byte_literal_clone_calls", 1);
                crate::telemetry::count("byte_literal_clone_bytes", value.len() as u64);
                crate::telemetry::count("byte_literal_clone_nonempty", u64::from(!value.is_empty()));
                Self::String(value.clone())
            }
            Self::Vector(x, y, z) => Self::Vector(*x, *y, *z),
            Self::VectorD(x, y, z) => Self::VectorD(*x, *y, *z),
        }
    }
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
    fn long_string(value: &[u8]) -> Option<(&str, usize)> {
        let text = std::str::from_utf8(value).ok()?;
        let newlines = value.iter().filter(|&&byte| byte == b'\n').count();
        if newlines == 0 || (newlines < 2 && value.len() < 80)
            || text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return None;
        }
        // A delimiter ]=...=] is unavailable when it occurs in the payload,
        // or when its prefix ]=...= ends the payload: the appended closing ]
        // would finish an earlier delimiter across that boundary. Record all
        // 17 permitted widths in one scan, without copying the payload per try.
        let mut forbidden = 0u32;
        let mut index = 0;
        while index < value.len() {
            if value[index] != b']' { index += 1; continue; }
            let start = index + 1;
            index = start;
            while value.get(index) == Some(&b'=') { index += 1; }
            let count = index - start;
            if count <= 16 && (index == value.len() || value[index] == b']') {
                forbidden |= 1 << count;
            }
            // A closing ] can also begin the next collision (for example ]]]).
        }
        let count = (!forbidden & ((1 << 17) - 1)).trailing_zeros() as usize;
        (count <= 16).then_some((text, count))
    }
    pub(crate) fn format_number(value: f64) -> String {
        NumberText(value).to_string()
    }

}

/// Display adapters keep temporary numeric text on the stack. The owned
/// format_number API uses the same spelling when a caller needs a String.
struct NumberText(f64);
impl fmt::Display for NumberText {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0.is_infinite() {
            f.write_str(if self.0.is_sign_positive() { "1e999" } else { "-1e999" })
        } else if self.0.is_nan() {
            f.write_str("(0 / 0)")
        } else {
            // Constants cannot depend on a shadowed global (including math.pi).
            let mut buffer = ryu::Buffer::new();
            let printed = buffer.format_finite(self.0);
            f.write_str(printed.strip_suffix(".0").unwrap_or(printed))
        }
    }
}

struct VectorComponentText(f32);
impl fmt::Display for VectorComponentText {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.0.is_infinite() {
            f.write_str(if self.0.is_sign_positive() { "1e999" } else { "-1e999" })
        } else if self.0.is_nan() {
            f.write_str("(0 / 0)")
        } else {
            // Keep standard f32 spelling; widening to f64 expands e.g. 0.1.
            write!(f, "{}", self.0)
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Literal::Nil => write!(f, "nil"),
            Literal::Boolean(value) => write!(f, "{}", value),
            &Literal::Number(value) => write!(f, "{}", NumberText(value)),
            // Decimal tokens are parsed as positive i64 before unary minus;
            // MIN's magnitude overflows. Hex tokens preserve all 64 bits.
            Literal::Integer(i64::MIN) => write!(f, "0x8000000000000000i"),
            Literal::Integer(value) => write!(f, "{value}i"),
            Literal::String(value) => {
                if let Some((text, count)) = Self::long_string(value) {
                    let equals = &"================"[..count];
                    f.write_str("[")?;
                    f.write_str(equals)?;
                    f.write_str("[\n")?;
                    f.write_str(text)?;
                    f.write_str("]")?;
                    f.write_str(equals)?;
                    return f.write_str("]");
                }
                f.write_str("\"")?;
                Formatter::<fmt::Formatter>::write_escaped_string(value, f)?;
                f.write_str("\"")
            }
            Literal::Vector(x, y, z) => write!(
                f,
                "vector.create({}, {}, {})",
                VectorComponentText(*x),
                VectorComponentText(*y),
                VectorComponentText(*z)
            ),
            Literal::VectorD(x, y, z) => write!(
                f,
                "vector.create({}, {}, {})",
                NumberText(*x),
                NumberText(*y),
                NumberText(*z)
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Literal;


    use std::{borrow::Cow, fmt::{self, Write}, iter};
    use crate::formatter::{Formatter, IndentationMode, format_with_emission_map};

    // The original allocating delimiter and escaping implementation is kept
    // independently: neither production streaming helper participates here.
    struct LegacyString;
    impl LegacyString {
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
        fn is_printable_string_char(c: char) -> bool {
            // A bare `'` is left unescaped: the string delimiter is always `"`, so an
            // apostrophe is the same byte whether written `'` or `\'` and re-lexes to
            // the same constant. Source never escapes it inside `"..."`.
            !c.is_control() && c != '\\' && c != '"'
        }

        fn push_escaped_byte(output: &mut String, byte: u8, next: Option<u8>) {
            match byte {
                b'\n' => output.push_str(r"\n"),
                b'\r' => output.push_str(r"\r"),
                b'\t' => output.push_str(r"\t"),
                b'\"' => output.push_str(r#"\""#),
                b'\\' => output.push_str(r"\\"),
                12 => output.push_str(r"\f"),
                _ => {
                    let mut buffer = itoa::Buffer::new();
                    let printed = buffer.format(byte);
                    output.push('\\');
                    if printed.len() != 3 && next.is_some_and(|next| next.is_ascii_digit()) {
                        output.extend(iter::repeat('0').take(3 - printed.len()));
                    }
                    output.push_str(printed);
                }
            };
        }

        fn escape_utf8_string<'s>(string: &'s [u8], text: &'s str) -> Cow<'s, str> {
            let mut owned: Option<String> = None;
            let mut iter = text.char_indices().peekable();
            while let Some((i, c)) = iter.next() {
                if Self::is_printable_string_char(c) {
                    if let Some(owned) = &mut owned {
                        owned.push(c);
                    }
                } else {
                    if owned.is_none() {
                        let mut output = text[..i].to_string();
                        output.reserve((string.len() - i) * 2);
                        owned = Some(output);
                    }

                    let owned = owned.as_mut().unwrap();
                    match c {
                        '\n' => owned.push_str(r"\n"),
                        '\r' => owned.push_str(r"\r"),
                        '\t' => owned.push_str(r"\t"),
                        '"' => owned.push_str(r#"\""#),
                        '\\' => owned.push_str(r"\\"),
                        '\u{000C}' => owned.push_str(r"\f"),
                        _ => {
                            let end = iter
                                .peek()
                                .map(|(next_i, _)| *next_i)
                                .unwrap_or(string.len());
                            for byte_i in i..end {
                                Self::push_escaped_byte(
                                    owned,
                                    string[byte_i],
                                    string.get(byte_i + 1).copied(),
                                );
                            }
                        }
                    };
                }
            }

            if let Some(owned) = owned {
                owned.into()
            } else {
                text.into()
            }
        }

        fn escape_bytes<'s>(string: &'s [u8]) -> Cow<'s, str> {
            let mut owned: Option<String> = None;
            for (i, &byte) in string.iter().enumerate() {
                if byte == b' ' || (byte.is_ascii_graphic() && byte != b'\\' && byte != b'\"') {
                    if let Some(owned) = &mut owned {
                        owned.push(byte as char);
                    }
                } else {
                    if owned.is_none() {
                        let mut output = std::str::from_utf8(&string[..i]).unwrap().to_string();
                        output.reserve((string.len() - i) * 2);
                        owned = Some(output);
                    }

                    Self::push_escaped_byte(owned.as_mut().unwrap(), byte, string.get(i + 1).copied());
                }
            }

            owned
                .map(Cow::Owned)
                .unwrap_or_else(|| std::str::from_utf8(string).unwrap().into())
        }

        pub(crate) fn escape_string<'s>(string: &'s [u8]) -> Cow<'s, str> {
            if let Ok(text) = std::str::from_utf8(string) {
                Self::escape_utf8_string(string, text)
            } else {
                Self::escape_bytes(string)
            }
        }

    }

    fn reference_string(bytes: &[u8]) -> String {
        LegacyString::long_string(bytes).unwrap_or_else(|| format!("\"{}\"", LegacyString::escape_string(bytes)))
    }

    #[test]
    fn stack_number_formatting_matches_original_float_bits_and_vector_tokens() {
        fn reference64(value: f64) -> String {
            if value.is_infinite() {
                if value.is_sign_positive() { "1e999".into() } else { "-1e999".into() }
            } else if value.is_nan() { "(0 / 0)".into() }
            else {
                let mut buffer = ryu::Buffer::new();
                let printed = buffer.format_finite(value);
                printed.strip_suffix(".0").unwrap_or(printed).to_string()
            }
        }
        fn reference32(value: f32) -> String {
            if value.is_infinite() {
                if value.is_sign_positive() { "1e999".into() } else { "-1e999".into() }
            } else if value.is_nan() { "(0 / 0)".into() }
            else { value.to_string() }
        }
        fn check(bits64: u64, bits32: u32) {
            let number = f64::from_bits(bits64);
            let component = f32::from_bits(bits32);
            let expected = reference64(number);
            assert_eq!(Literal::format_number(number), expected, "owned f64 {bits64:x}");
            assert_eq!(Literal::Number(number).to_string(), expected, "f64 {bits64:x}");
            // Negation, signed zero and both vector precision families retain
            // exactly the prior token grouping and non-finite expression form.
            let values64 = [number, -number, -0.0];
            let values32 = [component, -component, -0.0];
            assert_eq!(Literal::VectorD(values64[0], values64[1], values64[2]).to_string(),
                format!("vector.create({}, {}, {})", reference64(values64[0]), reference64(values64[1]), reference64(values64[2])));
            assert_eq!(Literal::Vector(values32[0], values32[1], values32[2]).to_string(),
                format!("vector.create({}, {}, {})", reference32(values32[0]), reference32(values32[1]), reference32(values32[2])));
        }
        let special64 = [0, 1, (1u64 << 52) - 1, 1u64 << 52, 0x3ff0_0000_0000_0000,
            0x7fef_ffff_ffff_ffff, 0x7ff0_0000_0000_0000, 0x7ff0_0000_0000_0001, 0x7ff8_0000_0000_1234,
            0x8000_0000_0000_0000, 0xfff0_0000_0000_0000, 0xfff8_0000_0000_1234];
        let special32 = [0, 1, (1u32 << 23) - 1, 1u32 << 23, 0x3f80_0000, 0x7f7f_ffff,
            0x7f80_0000, 0x7f80_0001, 0x7fc0_1234, 0x8000_0000, 0xff80_0000, 0xffc0_1234];
        for (bits64, bits32) in special64.into_iter().zip(special32) { check(bits64, bits32); }
        for number in [0.1, 1.0, -1.0, 1e-300, 1e300, std::f64::consts::PI, 9007199254740992.0] {
            check(number.to_bits(), (number as f32).to_bits());
        }
        let mut state = 7u64;
        for _ in 0..8192 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            check(state, (state >> 32) as u32);
        }
    }

    #[test]
    fn streaming_delimiter_matches_allocating_reference_exhaustively() {
        for length in 0..=9u32 {
            for mut code in 0..3usize.pow(length) {
                let mut bytes = b"a\nb\n".to_vec();
                for _ in 0..length {
                    bytes.push([b']', b'=', b'x'][code % 3]);
                    code /= 3;
                }
                let expected = reference_string(&bytes);
                assert_eq!(Literal::String(bytes.clone()).to_string(), expected, "{bytes:?}");
                let (text, _) = Literal::long_string(&bytes).unwrap();
                assert_eq!(text.as_ptr(), bytes.as_ptr(), "payload remains borrowed");
            }
        }
        let mut collisions = String::from("a\nb\n");
        for count in 0..=17 {
            let chosen = Literal::long_string(collisions.as_bytes()).map(|(_, count)| count);
            assert_eq!(chosen, (count < 17).then_some(count));
            assert_eq!(Literal::String(collisions.as_bytes().to_vec()).to_string(), reference_string(collisions.as_bytes()));
            // A suffix can collide with the delimiter that would otherwise be
            // selected, even though it has no closing ] inside the payload.
            let suffix = format!("{collisions}]{}", "=".repeat(count));
            assert_eq!(Literal::String(suffix.as_bytes().to_vec()).to_string(), reference_string(suffix.as_bytes()));
            collisions.push_str(&format!("]{}]x", "=".repeat(count)));
        }
    }

    #[test]
    fn streaming_literals_match_all_byte_pairs_and_unicode_escape_boundaries() {
        for first in 0..=255u8 {
            for second in 0..=255u8 {
                let bytes = [first, second];
                assert_eq!(Literal::String(bytes.to_vec()).to_string(), reference_string(&bytes), "{bytes:?}");
                let mut escaped = String::new();
                Formatter::<String>::write_escaped_string(&bytes, &mut escaped).unwrap();
                assert_eq!(escaped, LegacyString::escape_string(&bytes));
            }
        }
        let samples = [
            "đăng nhập 7 ngày - ô_ngày_giờ ✓", "café\n1", "界\u{0085}7\u{009f}8é", "\0\u{000c}\t\r\n\"\\'0",
            "a\nb", "\na\nb\n", "]=================]\n1\n2", "a\nb\n]\u{2028}界",
        ];
        for text in samples {
            assert_eq!(Literal::String(text.as_bytes().to_vec()).to_string(), reference_string(text.as_bytes()));
        }
        for count in [0, 1, 2, 77, 78, 79, 80, 81, 1024] {
            let mut bytes = vec![b'x'; count];
            bytes.push(b'\n');
            assert_eq!(Literal::String(bytes.clone()).to_string(), reference_string(&bytes));
        }
        let mut seed = 7u64;
        for length in 0..256 {
            let bytes: Vec<_> = (0..length).map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 32) as u8
            }).collect();
            assert_eq!(Literal::String(bytes.clone()).to_string(), reference_string(&bytes));
        }
    }

    #[test]
    fn streaming_literal_stops_at_writer_failure_with_exact_emitted_prefix() {
        struct FailAfter { bytes: Vec<u8>, remaining: usize, failed: bool }
        impl fmt::Write for FailAfter {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                assert!(!self.failed, "no writes after the first failure");
                let count = self.remaining.min(text.len());
                self.bytes.extend_from_slice(&text.as_bytes()[..count]);
                self.remaining -= count;
                self.failed = count != text.len();
                if self.failed { Err(fmt::Error) } else { Ok(()) }
            }
        }
        for bytes in [b"a\nb\nc]".as_slice(), b"\0\x079\xff7\\\"", "界\u{0085}7é".as_bytes(), b"", b"plain"] {
            let expected = reference_string(bytes);
            for limit in 0..=expected.len() + 1 {
                let mut writer = FailAfter { bytes: Vec::new(), remaining: limit, failed: false };
                let result = write!(&mut writer, "{}", Literal::String(bytes.to_vec()));
                assert_eq!(result.is_err(), limit < expected.len());
                assert_eq!(writer.bytes, &expected.as_bytes()[..limit.min(expected.len())]);
            }
        }
    }

    #[test]
    fn streaming_literal_passes_borrowed_payload_to_writer() {
        struct BorrowProbe<'a> { payload: &'a [u8], seen: bool, bytes: usize }
        impl fmt::Write for BorrowProbe<'_> {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                self.seen |= text.len() == self.payload.len() && text.as_ptr() == self.payload.as_ptr();
                self.bytes += text.len();
                Ok(())
            }
        }
        for length in [64, 256, 1024, 16384] {
            for multiline in [false, true] {
                let mut bytes = vec![b'a'; length];
                if multiline { bytes[1] = b'\n'; bytes[2] = b'\n'; }
                let value = Literal::String(bytes);
                let Literal::String(payload) = &value else { unreachable!() };
                let mut probe = BorrowProbe { payload, seen: false, bytes: 0 };
                write!(&mut probe, "{value}").unwrap();
                assert!(probe.seen, "length {length}, multiline {multiline}");
                assert_eq!(probe.bytes, reference_string(payload).len());
            }
        }
    }

    #[test]
    fn streaming_literals_preserve_layout_and_emission_map_offsets() {
        use crate::{Block, Call, Global, Local, RValue, RcLocal, Return};
        let after = RcLocal::new(Local::new(Some("after".into())));
        let payloads = ["界\nfirst]]\nsecond]=".as_bytes(), b"\xff7\0\"\\", "café\t7".as_bytes()];
        let block = Block(vec![Call::new(RValue::Global(Global::from("observe")),
            payloads.iter().map(|bytes| Literal::String(bytes.to_vec()).into()).collect()).into(),
            Return::new(vec![after.clone().into()]).into()]);
        for mode in [IndentationMode::Tab, IndentationMode::Spaces(4)] {
            let ordinary_mode = match mode { IndentationMode::Tab => IndentationMode::Tab,
                IndentationMode::Spaces(count) => IndentationMode::Spaces(count) };
            let (source, _, map) = format_with_emission_map(&block, mode, true).unwrap();
            let mut ordinary = String::new();
            Formatter::format(&block, &mut ordinary, ordinary_mode).unwrap();
            assert_eq!(source, ordinary);
            let regions: Vec<_> = map.regions.iter().filter(|region| region.kind == "literal").collect();
            assert_eq!(regions.len(), payloads.len());
            for (region, payload) in regions.iter().zip(&payloads) {
                assert_eq!(&source[region.span.start.byte_offset..region.span.end.byte_offset], reference_string(payload));
            }
            for span in map.regions.iter().map(|region| region.span).chain(map.bindings.iter().map(|binding| binding.span)) {
                for position in [span.start, span.end] {
                    let prefix = &source[..position.byte_offset];
                    assert_eq!(position.line_one_based, prefix.bytes().filter(|&byte| byte == b'\n').count() + 1);
                    assert_eq!(position.column_one_based, prefix.rsplit('\n').next().unwrap().chars().count() + 1);
                }
            }
            let binding = map.bindings.iter().find(|binding| binding.binding_id == after.stable_id()).unwrap();
            assert_eq!(&source[binding.span.start.byte_offset..binding.span.end.byte_offset], "after");
        }
    }

    #[test]
    fn literal_clone_keeps_variant_bits_and_independent_arbitrary_bytes() {
        let nan64 = f64::from_bits(0x7ff8_0000_0000_1234);
        let nan32 = f32::from_bits(0x7fc0_1234);
        let values = [
            Literal::Nil, Literal::Boolean(false), Literal::Boolean(true),
            Literal::Number(nan64), Literal::Number(-0.0), Literal::Number(f64::NEG_INFINITY),
            Literal::Integer(i64::MIN), Literal::Integer(i64::MAX),
            Literal::String(Vec::new()), Literal::String((0..=255).collect()),
            Literal::Vector(nan32, -0.0, f32::INFINITY),
            Literal::VectorD(nan64, -0.0, f64::NEG_INFINITY),
        ];
        for value in values {
            let mut cloned = value.clone();
            assert_eq!(std::mem::discriminant(&value), std::mem::discriminant(&cloned));
            match (&value, &mut cloned) {
                (Literal::Number(expected), Literal::Number(actual)) => assert_eq!(actual.to_bits(), expected.to_bits()),
                (Literal::Vector(x, y, z), Literal::Vector(cx, cy, cz)) => {
                    assert_eq!([cx.to_bits(), cy.to_bits(), cz.to_bits()], [x.to_bits(), y.to_bits(), z.to_bits()]);
                }
                (Literal::VectorD(x, y, z), Literal::VectorD(cx, cy, cz)) => {
                    assert_eq!([cx.to_bits(), cy.to_bits(), cz.to_bits()], [x.to_bits(), y.to_bits(), z.to_bits()]);
                }
                (Literal::String(expected), Literal::String(actual)) => {
                    assert_eq!(actual.as_slice(), expected.as_slice());
                    if !expected.is_empty() {
                        assert_ne!(actual.as_ptr(), expected.as_ptr());
                        actual[0] ^= 0xff;
                        assert_ne!(actual[0], expected[0]);
                    }
                    actual.push(0);
                    assert_eq!(actual.len(), expected.len() + 1);
                }
                (expected, actual) => assert_eq!(expected, &*actual),
            }
        }
    }

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
