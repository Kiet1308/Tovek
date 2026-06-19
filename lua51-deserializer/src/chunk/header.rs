use nom::{
    bytes::complete::tag,
    error::{Error, ErrorKind, ParseError},
    number::complete::le_u8,
    Err, IResult,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Endianness {
    Big,
    Little,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Format {
    Official,
}

#[derive(Debug)]
pub struct Header {
    pub(crate) version_number: u8,
    pub(crate) format: Format,
    pub(crate) endianness: Endianness,
    pub(crate) int_width: u8,
    pub(crate) size_t_width: u8,
    pub(crate) instr_width: u8,
    pub(crate) number_width: u8,
    pub(crate) number_is_integral: bool,
}

impl Header {
    pub fn parse(input: &[u8]) -> IResult<&[u8], Self> {
        let (input, _) = tag("\x1BLua")(input)?;
        let (input, version_number) = le_u8(input)?;
        let (input, format) = match le_u8(input)? {
            (input, 0) => Ok((input, Format::Official)),
            _ => Err(Err::Failure(Error::from_error_kind(
                input,
                ErrorKind::Switch,
            ))),
        }?;
        // TODO: try_into instead
        let (input, endianness) = match le_u8(input)? {
            (input, 0) => Ok((input, Endianness::Big)),
            (input, 1) => Ok((input, Endianness::Little)),
            _ => Err(Err::Failure(Error::from_error_kind(
                input,
                ErrorKind::Switch,
            ))),
        }?;
        let (input, int_width) = le_u8(input)?;
        let (input, size_t_width) = le_u8(input)?;
        let (input, instr_width) = le_u8(input)?;
        let (input, number_width) = le_u8(input)?;
        let (input, number_is_integral) = match le_u8(input)? {
            (input, 0) => Ok((input, false)),
            (input, 1) => Ok((input, true)),
            _ => Err(Err::Failure(Error::from_error_kind(
                input,
                ErrorKind::Switch,
            ))),
        }?;

        Ok((
            input,
            Self {
                version_number,
                format,
                endianness,
                int_width,
                size_t_width,
                instr_width,
                number_width,
                number_is_integral,
            },
        ))
    }
}
