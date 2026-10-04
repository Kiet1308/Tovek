use enum_as_inner::EnumAsInner;
use nom::{
    bytes::complete::take,
    error::{Error, ErrorKind, ParseError},
    multi::count,
    number::complete::{le_f64, le_u32, le_u8},
    Err, IResult,
};

#[derive(Debug, EnumAsInner)]
pub enum Value<'a> {
    Nil,
    Boolean(bool),
    Number(f64),
    String(&'a [u8]),
}

impl<'a> Value<'a> {
    pub fn parse(input: &'a [u8]) -> IResult<&'a [u8], Self> {
        let (input, kind) = le_u8(input)?;

        match kind {
            0 => Ok((input, Self::Nil)),
            1 => {
                let (input, value) = le_u8(input)?;

                Ok((input, Self::Boolean(value != 0)))
            }
            3 => {
                let (input, value) = le_f64(input)?;

                Ok((input, Self::Number(value)))
            }
            4 => {
                let (input, value) = parse_string(input)?;
                // A zero length is the absent-name sentinel, not a string
                // constant. Keep malformed data inside the parser's error
                // boundary instead of panicking or guessing an empty value.
                let string = value.strip_suffix(&[0]).ok_or_else(||
                    Err::Failure(Error::from_error_kind(input, ErrorKind::Verify)))?;
                Ok((input, Self::String(string)))
            }
            _ => Err(Err::Failure(Error::from_error_kind(
                input,
                ErrorKind::Switch,
            ))),
        }
    }
}

pub fn parse_string(input: &[u8]) -> IResult<&[u8], &[u8]> {
    let (input, string_length) = le_u32(input)?;
    let (input, value) = take(string_length as usize)(input)?;
    if !value.is_empty() && value.last() != Some(&0) {
        return Err(Err::Failure(Error::from_error_kind(input, ErrorKind::Verify)));
    }
    Ok((input, value))
}

pub fn parse_strings(input: &[u8]) -> IResult<&[u8], Vec<&[u8]>> {
    let (input, string_count) = le_u32(input)?;
    if string_count as usize > input.len() / 4 {
        return Err(Err::Failure(Error::from_error_kind(input, ErrorKind::Count)));
    }
    let (input, strings) = count(parse_string, string_count as usize)(input)?;

    Ok((input, strings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_constants_distinguish_empty_absent_and_unterminated_bytes() {
        assert!(matches!(Value::parse(&[4, 1, 0, 0, 0, 0]), Ok(([], Value::String([])))));
        assert!(Value::parse(&[4, 0, 0, 0, 0]).is_err());
        assert!(Value::parse(&[4, 1, 0, 0, 0, b'x']).is_err());
        // The source-name field may be absent and inherit its parent's name.
        assert!(matches!(parse_string(&[0, 0, 0, 0]), Ok(([], []))));
        assert!(parse_strings(&[255, 255, 255, 255]).is_err());
    }
}
