use crate::instruction::layout::LayoutDiscriminants;
use nom::{
    error::{Error, ErrorKind, ParseError},
    number::complete::le_u8,
    Err, IResult,
};
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;

#[derive(Debug, FromPrimitive, ToPrimitive)]
pub enum OperationCode {
    Move = 0,
    LoadConstant,
    LoadBoolean,
    LoadNil,
    GetUpvalue,
    GetGlobal,
    GetIndex,
    SetGlobal,
    SetUpvalue,
    SetIndex,
    NewTable,
    PrepMethodCall,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
    Minus,
    Not,
    Length,
    Concatenate,
    Jump,
    Equal,
    LessThan,
    LessThanOrEqual,
    Test,
    TestSet,
    Call,
    TailCall,
    Return,
    IterateNumericForLoop,
    InitNumericForLoop,
    IterateGenericForLoop,
    SetList,
    Close,
    Closure,
    VarArg,
}

impl OperationCode {
    pub fn parse(input: &[u8]) -> IResult<&[u8], Self> {
        let (input, operation_code) = le_u8(input)?;
        let operation_code = operation_code & 0x3F;

        Ok((
            input,
            match FromPrimitive::from_u8(operation_code) {
                None => {
                    return Err(Err::Failure(Error::from_error_kind(
                        input,
                        ErrorKind::Switch,
                    )))
                }
                Some(operation_code) => operation_code,
            },
        ))
    }

    pub fn instruction_layout(&self) -> LayoutDiscriminants {
        /*
           0 = BC
           1 = BX
           2 = BSx
        */

        match self {
            Self::Move => LayoutDiscriminants::BC,
            Self::LoadConstant => LayoutDiscriminants::BX,
            Self::LoadBoolean => LayoutDiscriminants::BC,
            Self::LoadNil => LayoutDiscriminants::BC,
            Self::GetUpvalue => LayoutDiscriminants::BC,
            Self::GetGlobal => LayoutDiscriminants::BX,
            Self::GetIndex => LayoutDiscriminants::BC,
            Self::SetGlobal => LayoutDiscriminants::BX,
            Self::SetUpvalue => LayoutDiscriminants::BC,
            Self::SetIndex => LayoutDiscriminants::BC,
            Self::NewTable => LayoutDiscriminants::BC,
            Self::PrepMethodCall => LayoutDiscriminants::BC,
            Self::Add => LayoutDiscriminants::BC,
            Self::Subtract => LayoutDiscriminants::BC,
            Self::Multiply => LayoutDiscriminants::BC,
            Self::Divide => LayoutDiscriminants::BC,
            Self::Modulo => LayoutDiscriminants::BC,
            Self::Power => LayoutDiscriminants::BC,
            Self::Minus => LayoutDiscriminants::BC,
            Self::Not => LayoutDiscriminants::BC,
            Self::Length => LayoutDiscriminants::BC,
            Self::Concatenate => LayoutDiscriminants::BC,
            Self::Jump => LayoutDiscriminants::BSx,
            Self::Equal => LayoutDiscriminants::BC,
            Self::LessThan => LayoutDiscriminants::BC,
            Self::LessThanOrEqual => LayoutDiscriminants::BC,
            Self::Test => LayoutDiscriminants::BC,
            Self::TestSet => LayoutDiscriminants::BC,
            Self::Call => LayoutDiscriminants::BC,
            Self::TailCall => LayoutDiscriminants::BC,
            Self::Return => LayoutDiscriminants::BC,
            Self::IterateNumericForLoop => LayoutDiscriminants::BSx,
            Self::InitNumericForLoop => LayoutDiscriminants::BSx,
            Self::IterateGenericForLoop => LayoutDiscriminants::BC,
            Self::SetList => LayoutDiscriminants::BC,
            Self::Close => LayoutDiscriminants::BC,
            Self::Closure => LayoutDiscriminants::BX,
            Self::VarArg => LayoutDiscriminants::BC,
        }
    }
}
