use std::mem;

use nom::{
    error::{Error, ErrorKind, ParseError},
    Err, IResult,
};

pub use header::Header;

use crate::{
    chunk::header::{Endianness, Format},
    function::Function,
};

pub mod header;

#[derive(Debug)]
pub struct Chunk<'a> {
    pub function: Function<'a>,
}

impl<'a> Chunk<'a> {
    /// Fails, at the header, on a chunk other than the one `Function::parse`
    /// reads: Lua 5.1, official format, little-endian, 4-byte int, size_t and
    /// instruction, 8-byte floating-point number.
    pub fn parse(input: &'a [u8]) -> IResult<&[u8], Self> {
        let (rest, header) = Header::parse(input)?;
        // TODO: pass header to Function::parse
        let supported = header.version_number == 0x51
            && header.format == Format::Official
            && header.endianness == Endianness::Little
            && header.int_width as usize == mem::size_of::<i32>()
            && header.size_t_width as usize == mem::size_of::<u32>()
            && header.instr_width as usize == mem::size_of::<u32>()
            && header.number_width as usize == mem::size_of::<f64>()
            && !header.number_is_integral;
        if !supported {
            return Err(Err::Failure(Error::from_error_kind(input, ErrorKind::Verify)));
        }
        let (input, function) = Function::parse(rest)?;

        Ok((input, Self { function }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Lua 5.1 header with the given `size_t` width, then no function.
    fn header(size_t_width: u8) -> Vec<u8> {
        let mut bytes = b"Lua".to_vec();
        bytes.extend([0x51, 0, 1, 4, size_t_width, 4, 8, 0]);
        bytes
    }

    #[test]
    fn an_unsupported_header_is_a_parse_error() {
        let bytes = header(8);
        assert!(matches!(Chunk::parse(&bytes), Err(Err::Failure(error))
            if error.code == ErrorKind::Verify && error.input.len() == bytes.len()));
        let mut bytes = header(4);
        bytes[4] = 0x52;
        assert!(Chunk::parse(&bytes).is_err());
        // A supported header goes on to the function, here missing.
        assert!(!matches!(Chunk::parse(&header(4)), Err(Err::Failure(error)) if error.code == ErrorKind::Verify));
    }
}
