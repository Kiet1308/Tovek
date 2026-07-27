use nom::{IResult, multi::count};
use nom_leb128::leb128_usize;

pub(crate) fn parse_list<'a, T>(
    input: &'a [u8],
    parser: impl Fn(&'a [u8]) -> IResult<&'a [u8], T>,
) -> IResult<&'a [u8], Vec<T>> {
    let (input, length) = leb128_usize(input)?;
    if length > input.len() {
        return Err(nom::Err::Failure(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Count,
        )));
    }
    let (input, items) = count(parser, length)(input)?;
    Ok((input, items))
}

pub(crate) fn parse_list_len<'a, T>(
    input: &'a [u8],
    parser: impl Fn(&'a [u8]) -> IResult<&'a [u8], T>,
    length: usize,
) -> IResult<&'a [u8], Vec<T>> {
    if length > input.len() {
        return Err(nom::Err::Failure(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Count,
        )));
    }
    let (input, items) = count(parser, length)(input)?;
    Ok((input, items))
}
