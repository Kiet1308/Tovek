use std::mem;

use nom::IResult;

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
    pub fn parse(input: &'a [u8]) -> IResult<&[u8], Self> {
        let (input, header) = Header::parse(input)?;
        // TODO: pass header to Function::parse
        assert_eq!(header.version_number, 0x51);
        assert_eq!(header.format, Format::Official);
        assert_eq!(header.endianness, Endianness::Little);
        assert_eq!(header.int_width as usize, mem::size_of::<i32>());
        assert_eq!(header.size_t_width as usize, mem::size_of::<u32>());
        assert_eq!(header.instr_width as usize, mem::size_of::<u32>());
        assert_eq!(header.number_width as usize, mem::size_of::<f64>());
        assert!(!header.number_is_integral);
        let (input, function) = Function::parse(input)?;

        Ok((input, Self { function }))
    }
}
