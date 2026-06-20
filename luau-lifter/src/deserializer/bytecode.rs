use nom::{bytes::complete::take, number::complete::le_u8, IResult};

use super::chunk::Chunk;

#[derive(Debug)]
pub enum Bytecode {
    Error(String),
    Chunk(Chunk),
}

impl Bytecode {
    pub fn parse(input: &[u8], encode_key: u8) -> IResult<&[u8], Bytecode> {
        let (input, status_code) = le_u8(input)?;
        match status_code {
            0 => {
                let (input, error_msg) = take(input.len())(input)?;
                Ok((
                    input,
                    Bytecode::Error(String::from_utf8_lossy(error_msg).to_string()),
                ))
            }
            // 4..=11: bytecode versions 4 through 11 (LBC_VERSION_MAX). v10 adds
            // LBC_CONSTANT_CLASS_SHAPE + NEWCLASSMEMBER; v11 adds CALLFB/CMPPROTO
            // and a per-proto feedback-vector section (read in function.rs).
            4..=11 => {
                let (input, chunk) = Chunk::parse(input, encode_key, status_code)?;
                Ok((input, Bytecode::Chunk(chunk)))
            }
            _ => panic!("Unsupported bytecode version: {}", status_code),
        }
    }
}
