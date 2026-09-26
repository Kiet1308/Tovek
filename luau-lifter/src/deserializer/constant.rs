use super::list::{parse_list, parse_list_len};
use nom::{
    IResult,
    number::complete::{le_f32, le_f64, le_i32, le_u8, le_u32},
};
use nom_leb128::{leb128_u64, leb128_usize};

const CONSTANT_NIL: u8 = 0;
const CONSTANT_BOOLEAN: u8 = 1;
const CONSTANT_NUMBER: u8 = 2;
const CONSTANT_STRING: u8 = 3;
const CONSTANT_IMPORT: u8 = 4;
const CONSTANT_TABLE: u8 = 5;
const CONSTANT_CLOSURE: u8 = 6;
const CONSTANT_VECTOR: u8 = 7;
// Added in Luau bytecode version 7+.
const CONSTANT_TABLE_WITH_CONSTANTS: u8 = 8;
const CONSTANT_INTEGER: u8 = 9;
const CONSTANT_CLASS_SHAPE: u8 = 10;
const CONSTANT_VECTORD: u8 = 11;

#[derive(Debug)]
pub enum Constant {
    Nil,
    Boolean(bool),
    Number(f64),
    String(usize),
    Import(usize),
    Table(Vec<usize>),
    Closure(usize),
    Vector(f32, f32, f32, f32),
    // A DUPTABLE template that also carries constant values: (key index, constant index).
    TableWithConstants(Vec<(usize, i32)>),
    Integer(i64),
    ClassShape,
    VectorD(f64, f64, f64, f64),
}

impl Constant {
    pub(crate) fn parse(input: &[u8], version: u8) -> IResult<&[u8], Self> {
        let (input, tag) = le_u8(input)?;
        match tag {
            CONSTANT_NIL => Ok((input, Constant::Nil)),
            CONSTANT_BOOLEAN => {
                let (input, value) = le_u8(input)?;
                Ok((input, Constant::Boolean(value != 0u8)))
            }
            CONSTANT_NUMBER => {
                let (input, value) = le_f64(input)?;
                Ok((input, Constant::Number(value)))
            }
            CONSTANT_STRING => {
                let (input, string_index) = leb128_usize(input)?;
                Ok((input, Constant::String(string_index)))
            }
            CONSTANT_IMPORT => {
                let (input, import_index) = le_u32(input)?;
                Ok((input, Constant::Import(import_index as usize)))
            }
            CONSTANT_TABLE => {
                let (input, keys) = parse_list(input, leb128_usize)?;
                Ok((input, Constant::Table(keys)))
            }
            CONSTANT_CLOSURE => {
                let (input, f_id) = leb128_usize(input)?;
                Ok((input, Constant::Closure(f_id)))
            }
            CONSTANT_VECTOR => {
                let (input, x) = le_f32(input)?;
                let (input, y) = le_f32(input)?;
                let (input, z) = le_f32(input)?;
                let (input, w) = le_f32(input)?;
                Ok((input, Constant::Vector(x, y, z, w)))
            }
            CONSTANT_VECTORD => {
                if version < 13 {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Verify,
                    )));
                }
                let (input, x) = le_f64(input)?;
                let (input, y) = le_f64(input)?;
                let (input, z) = le_f64(input)?;
                let (input, w) = le_f64(input)?;
                Ok((input, Constant::VectorD(x, y, z, w)))
            }
            // count, then per key: varint key index + int32 constant index
            CONSTANT_TABLE_WITH_CONSTANTS => {
                let (input, pairs) = parse_list(input, |i| {
                    let (i, key) = leb128_usize(i)?;
                    let (i, value) = le_i32(i)?;
                    Ok((i, (key, value)))
                })?;
                Ok((input, Constant::TableWithConstants(pairs)))
            }
            // isNegative byte, then varint magnitude
            CONSTANT_INTEGER => {
                let (input, is_negative) = le_u8(input)?;
                // nom-leb128 0.2 accepts overflowing payload bits in the last
                // byte. Reject them before a truncated magnitude can pass the
                // signed-range check (e.g. 2^64 becoming zero).
                if input.iter().take(9).all(|byte| byte & 0x80 != 0)
                    && input.get(9).is_some_and(|byte| *byte > 1)
                {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        input, nom::error::ErrorKind::TooLarge,
                    )));
                }
                let (input, magnitude) = leb128_u64(input)?;
                if magnitude > (i64::MAX as u64) + u64::from(is_negative != 0) {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        input, nom::error::ErrorKind::TooLarge,
                    )));
                }
                let value = if is_negative != 0 {
                    (magnitude as i64).wrapping_neg()
                } else {
                    magnitude as i64
                };
                Ok((input, Constant::Integer(value)))
            }
            // class name id, property count, method count, then one varint per member
            CONSTANT_CLASS_SHAPE => {
                let (input, _class_name_id) = leb128_usize(input)?;
                let (input, num_properties) = leb128_usize(input)?;
                let (input, num_methods) = leb128_usize(input)?;
                let member_count = num_properties.checked_add(num_methods).ok_or_else(|| {
                    nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify))
                })?;
                let (input, _members) = parse_list_len(input, leb128_usize, member_count)?;
                Ok((input, Constant::ClassShape))
            }
            _ => Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            ))),
        }
    }
}

#[cfg(test)]
mod integer_tests {
    use super::*;

    fn encoded(negative: u8, mut magnitude: u64) -> Vec<u8> {
        let mut bytes = vec![9, negative];
        loop {
            let low = (magnitude & 127) as u8;
            magnitude >>= 7;
            bytes.push(low | if magnitude == 0 { 0 } else { 128 });
            if magnitude == 0 { return bytes; }
        }
    }

    #[test]
    fn integer_width_and_signed_limits_do_not_depend_on_usize() {
        for value in [0, 127, 128, 1 << 32, (1 << 32) + 1, i64::MAX as u64] {
            assert!(matches!(Constant::parse(&encoded(0, value), 9),
                Ok(([], Constant::Integer(parsed))) if parsed == value as i64));
        }
        assert!(matches!(Constant::parse(&encoded(1, 1 << 63), 9), Ok(([], Constant::Integer(i64::MIN)))));
        assert!(Constant::parse(&encoded(0, 1 << 63), 9).is_err());
        let mut overflow = vec![9, 0];
        overflow.extend([0x80; 9]); overflow.push(2);
        assert!(Constant::parse(&overflow, 9).is_err());
    }
}
