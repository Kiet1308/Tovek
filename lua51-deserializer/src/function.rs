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
        let (input, code) = parse_code(input, code_length as usize)?;
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

fn parse_code(input: &[u8], length: usize) -> IResult<&[u8], Vec<Instruction>> {
    let (input, words) = count(le_u32, length)(input)?;
    let mut code = Vec::with_capacity(length);
    let mut pc = 0;
    while pc < words.len() {
        let word = words[pc].to_le_bytes();
        let mut instruction = Instruction::parse(&word)
            .map_err(|_| nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify)))?.1;
        if let Instruction::SetList { block_number, .. } = &mut instruction {
            if *block_number == 0 {
                *block_number = *words.get(pc + 1).filter(|&&n| n != 0)
                    .ok_or_else(|| nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify)))?;
                code.push(instruction);
                code.push(Instruction::ExtraArgument);
                pc += 2;
                continue;
            }
        }
        code.push(instruction);
        pc += 1;
    }
    Ok((input, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setlist_keeps_nine_bits_and_consumes_extended_word() {
        for block in [255, 256, 257, 511] {
            let word: u32 = 34 | (1 << 23) | (block << 14);
            let (_, code) = parse_code(&word.to_le_bytes(), 1).unwrap();
            assert!(matches!(code[0], Instruction::SetList { block_number, .. } if block_number == block));
        }
        let words = [34u32 | (1 << 23), 512, 30 | (1 << 23)];
        let bytes: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let (_, code) = parse_code(&bytes, 3).unwrap();
        assert!(matches!(code[0], Instruction::SetList { block_number: 512, .. }));
        assert!(matches!(code[1], Instruction::ExtraArgument));
        assert!(parse_code(&bytes[..4], 1).is_err());
    }
}
