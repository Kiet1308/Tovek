use std::ops::Range;

use nom::{multi::count, number::complete::le_u32, IResult};

use crate::value::parse_string;

#[derive(Debug)]
pub struct Local<'a> {
    pub name: &'a [u8],
    pub range: Range<u32>,
}

impl<'a> Local<'a> {
    pub fn parse_list(input: &'a [u8]) -> IResult<&'a [u8], Vec<Self>> {
        let (input, length) = le_u32(input)?;
        if length as usize > input.len() / 12 {
            return Err(nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Count)));
        }
        count(Self::parse, length as usize)(input)
    }

    fn parse(input: &'a [u8]) -> IResult<&'a [u8], Self> {
        let (input, name) = parse_string(input)?;
        let (input, start) = le_u32(input)?;
        let (input, end) = le_u32(input)?;
        let name = name.strip_suffix(&[0]).ok_or_else(||
            nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify)))?;

        Ok((
            input,
            Self {
                name,
                range: (start..end),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_local_name_and_oversized_count_are_errors() {
        let mut bytes = 1u32.to_le_bytes().to_vec();
        bytes.extend([0; 12]);
        assert!(Local::parse_list(&bytes).is_err());
        assert!(Local::parse_list(&u32::MAX.to_le_bytes()).is_err());
    }
}
