use nom::{
    error::{Error, ErrorKind, ParseError},
    number::complete::le_u32,
    Err, IResult,
};
use num_traits::FromPrimitive;
use strum_macros::EnumDiscriminants;

use super::OperationCode;

#[derive(Debug, EnumDiscriminants)]
pub enum Layout {
    BC { a: u8, b: u16, c: u16 },
    // b extended
    BX { a: u8, b_x: u32 },
    // b signed, extended
    BSx { a: u8, b_sx: i32 },
}

impl Layout {
    pub fn parse(input: &[u8], operation_code: u8) -> IResult<&[u8], Self> {
        let (input, instruction) = le_u32(input)?;

        match OperationCode::from_u8(operation_code).map(|o: OperationCode| o.instruction_layout())
        {
            Some(LayoutDiscriminants::BC) => {
                let a = ((instruction >> 6) & 0xFF) as u8;
                let c = ((instruction >> 14) & 0x1FF) as u16;
                let b = ((instruction >> 23) & 0x1FF) as u16;

                Ok((input, Self::BC { a, b, c }))
            }
            Some(LayoutDiscriminants::BX) => {
                let a = ((instruction >> 6) & 0xFF) as u8;
                let b_x = (instruction >> 14) & 0x3FFFF;

                Ok((input, Self::BX { a, b_x }))
            }
            Some(LayoutDiscriminants::BSx) => {
                let a = ((instruction >> 6) & 0xFF) as u8;
                let b_x = (instruction >> 14) & 0x3FFFF;
                // subtract maximum 18 bit signed int
                let b_sx = b_x as i32 - (((1 << 18) - 1) >> 1);

                Ok((input, Self::BSx { a, b_sx }))
            }
            _ => Err(Err::Failure(Error::from_error_kind(
                input,
                ErrorKind::Switch,
            ))),
        }
    }
}
