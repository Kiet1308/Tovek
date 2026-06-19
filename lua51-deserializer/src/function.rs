use nom::{
    combinator::opt,
    multi::count,
    number::complete::{le_u32, le_u8},
    IResult,
};

use crate::{
    instruction::{position::Position, Instruction},
    local::Local,
    value::{self, Value},
};

#[derive(Debug)]
pub struct Function<'a> {
    pub name: &'a [u8],
    pub line_defined: u32,
    pub last_line_defined: u32,
    pub number_of_upvalues: u8,
    pub vararg_flag: u8,
    pub maximum_stack_size: u8,
    pub code: Vec<Instruction>,
    pub constants: Vec<Value<'a>>,
    pub closures: Vec<Function<'a>>,
    pub positions: Vec<Position>,
    pub locals: Vec<Local<'a>>,
    pub upvalues: Vec<&'a [u8]>,
    pub number_of_parameters: u8,
}

impl<'a> Function<'a> {
    pub fn parse(input: &'a [u8]) -> IResult<&'a [u8], Self> {
        let (input, name) = value::parse_string(input)?;
        let (input, line_defined) = le_u32(input)?;
        let (input, last_line_defined) = le_u32(input)?;
        let (input, number_of_upvalues) = le_u8(input)?;
        let (input, number_of_parameters) = le_u8(input)?;
        let (input, vararg_flag) = le_u8(input)?;
        let (input, maximum_stack_size) = le_u8(input)?;
        let (input, code_length) = le_u32(input)?;
        let (input, code) = count(Instruction::parse, code_length as usize)(input)?;
        let (input, constants_length) = le_u32(input)?;
        let (input, constants) = count(Value::parse, constants_length as usize)(input)?;
        let (input, closures_length) = le_u32(input)?;
        let (input, closures) = count(Self::parse, closures_length as usize)(input)?;
        let (input, positions) = opt(Position::parse)(input)?;
        let (input, locals) = opt(Local::parse_list)(input)?;
        let (input, upvalues) = opt(value::parse_strings)(input)?;

        Ok((
            input,
            Self {
                name,
                line_defined,
                last_line_defined,
                number_of_upvalues,
                vararg_flag,
                maximum_stack_size,
                code,
                constants,
                closures,
                positions: positions.unwrap_or_default(),
                locals: locals.unwrap_or_default(),
                upvalues: upvalues.unwrap_or_default(),
                number_of_parameters,
            },
        ))
    }
}
