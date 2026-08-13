use super::{function::Function, list::parse_list, parse_string};
use nom::IResult;
use nom::character::complete::char;
use nom::multi::many_till;
use nom::number::complete::le_u8;
use nom_leb128::leb128_usize;

#[derive(Debug)]
pub struct Chunk {
    pub version: u8,
    pub string_table: Vec<Vec<u8>>,
    pub functions: Vec<Function>,
    pub main: usize,
}

impl Chunk {
    pub(crate) fn parse(input: &[u8], encode_key: u8, version: u8) -> IResult<&[u8], Self> {
        let (input, types_version) = if version >= 4 {
            le_u8(input)?
        } else {
            (input, 0)
        };
        if types_version > 3 {
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }
        let (input, string_table) = parse_list(input, parse_string)?;
        let input = if types_version == 3 {
            many_till(leb128_usize, char('\0'))(input)?.0
        } else {
            input
        };
        let (input, functions) = parse_list(input, |i| {
            // Bytecode v12+ (cost model / vector doubles) prefixes every proto
            // with a varint size so the reader can skip a proto wholesale; the
            // deserializer verifies the body via its own grammar, so the size is
            // consumed and discarded (mirrors lvmload.cpp: version >= 12).
            let (i, _proto_size) = if version >= 12 {
                leb128_usize(i)?
            } else {
                (i, 0)
            };
            Function::parse(i, encode_key, version)
        })?;
        let (input, main) = leb128_usize(input)?;

        Ok((
            input,
            Self {
                version,
                string_table,
                functions,
                main,
            },
        ))
    }
}
