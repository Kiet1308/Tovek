use super::list::{parse_list, parse_list_len};
use nom::{
    number::complete::{le_f32, le_f64, le_i32, le_u32, le_u8},
    IResult,
};
use nom_leb128::leb128_usize;

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
}

impl Constant {
    pub(crate) fn parse(input: &[u8]) -> IResult<&[u8], Self> {
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
                let (input, magnitude) = leb128_usize(input)?;
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
                let (input, _members) =
                    parse_list_len(input, leb128_usize, num_properties + num_methods)?;
                Ok((input, Constant::ClassShape))
            }
            _ => panic!("{}", tag),
        }
    }
}
