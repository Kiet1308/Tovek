use super::{function::Function, list::parse_list, parse_string};
use nom::{IResult, bytes::complete::take};
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
            if version < 12 {
                return Function::parse(i, encode_key, version);
            }

            // v12+ prefixes every prototype with its exact encoded size. Parse
            // only within that slice, then resume at the declared boundary so
            // unknown trailing fields remain forward-compatible.
            let (input, proto_size) = leb128_usize(i)?;
            if proto_size > input.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::LengthValue,
                )));
            }
            let (rest, proto) = take(proto_size)(input)?;
            let (unconsumed, function) = Function::parse(proto, encode_key, version)?;
            // A declared size smaller than the known proto body is rejected by
            // Function::parse above. Any remaining bytes are unknown extension
            // fields and are intentionally skipped at the proto boundary.
            let _extension = unconsumed;
            Ok((rest, function))
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
