use nom::{
    multi::count,
    number::complete::{le_u32, le_u8},
    IResult,
};

use crate::{
    instruction::{position::Position, Instruction},
    local::Local,
    value::{self, Value},
};

pub const MAX_FUNCTION_DEPTH: usize = 256;
pub const MAX_FUNCTIONS: usize = 65_536;
pub const MAX_INSTRUCTION_WORDS: usize = 8_000_000;

struct ParseBudget { functions: usize, instruction_words: usize }

fn refusal(input: &[u8], code: nom::error::ErrorKind) -> nom::Err<nom::error::Error<&[u8]>> {
    nom::Err::Failure(nom::error::Error::new(input, code))
}

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
    pub fn is_variadic(&self) -> bool {
        self.vararg_flag & 2 != 0 // VARARG_ISVARARG
    }

    pub fn needs_arg_table(&self) -> bool {
        self.vararg_flag & 4 != 0 // VARARG_NEEDSARG (HASARG alone only reserves a slot)
    }

    pub fn parse(input: &'a [u8]) -> IResult<&'a [u8], Self> {
        let mut budget = ParseBudget {
            functions: MAX_FUNCTIONS, instruction_words: MAX_INSTRUCTION_WORDS,
        };
        let (mut input, root) = Self::parse_prefix(input, 0, &mut budget)?;
        let mut pending = vec![root];
        loop {
            if pending.last().is_some_and(|(_, remaining)| *remaining > 0) {
                let (rest, child) = Self::parse_prefix(input, pending.len(), &mut budget)?;
                input = rest;
                pending.push(child);
                continue;
            }
            let (mut function, _) = pending.pop().expect("root is returned when its frame completes");
            // Stripped debug sections still encode their zero counts. Missing
            // sections must not consume a sibling prototype as metadata.
            let (rest, positions) = Position::parse(input)?;
            let (rest, locals) = Local::parse_list(rest)?;
            let (rest, upvalues) = value::parse_strings(rest)?;
            function.positions = positions;
            function.locals = locals;
            function.upvalues = upvalues;
            input = rest;
            if let Some((parent, remaining)) = pending.last_mut() {
                parent.closures.push(function);
                *remaining -= 1;
            } else {
                return Ok((input, function));
            }
        }
    }

    // Children precede their parent's debug tail on the wire. Keep that parse
    // state explicitly instead of spending native stack for each prototype.
    fn parse_prefix(input: &'a [u8], depth: usize, budget: &mut ParseBudget) -> IResult<&'a [u8], (Self, usize)> {
        if depth > MAX_FUNCTION_DEPTH || budget.functions == 0 {
            return Err(refusal(input, nom::error::ErrorKind::TooLarge));
        }
        budget.functions -= 1;
        let (input, name) = value::parse_string(input)?;
        let (input, line_defined) = le_u32(input)?;
        let (input, last_line_defined) = le_u32(input)?;
        let (input, number_of_upvalues) = le_u8(input)?;
        let (input, number_of_parameters) = le_u8(input)?;
        let (input, vararg_flag) = le_u8(input)?;
        let (input, maximum_stack_size) = le_u8(input)?;
        let (input, code_length) = le_u32(input)?;
        if code_length == 0 || code_length as usize > budget.instruction_words {
            return Err(refusal(input, nom::error::ErrorKind::TooLarge));
        }
        budget.instruction_words -= code_length as usize;
        let (input, code) = parse_code(input, code_length as usize)?;
        let (input, constants_length) = le_u32(input)?;
        if constants_length as usize > input.len() {
            return Err(refusal(input, nom::error::ErrorKind::Count));
        }
        let (input, constants) = count(Value::parse, constants_length as usize)(input)?;
        let (input, closures_length) = le_u32(input)?;
        if closures_length as usize > budget.functions || closures_length as usize > input.len() / 40 {
            return Err(refusal(input, nom::error::ErrorKind::Count));
        }
        Ok((
            input,
            (Self {
                name,
                line_defined,
                last_line_defined,
                number_of_upvalues,
                vararg_flag,
                maximum_stack_size,
                code,
                constants,
                closures: Vec::new(),
                positions: Vec::new(),
                locals: Vec::new(),
                upvalues: Vec::new(),
                number_of_parameters,
            }, closures_length as usize),
        ))
    }
}

fn parse_code(input: &[u8], length: usize) -> IResult<&[u8], Vec<Instruction>> {
    let byte_count = length.checked_mul(4).ok_or_else(|| {
        nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Count))
    })?;
    let (input, raw_words) = nom::bytes::complete::take(byte_count)(input)?;
    let mut words = raw_words.chunks_exact(4);
    let mut code = Vec::with_capacity(length);
    while let Some(word) = words.next() {
        let mut instruction = Instruction::parse(word)
            .map_err(|_| nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify)))?.1;
        if let Instruction::SetList { block_number, .. } = &mut instruction {
            if *block_number == 0 {
                *block_number = words.next().map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())).filter(|&n| n != 0)
                    .ok_or_else(|| nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify)))?;
                code.push(instruction);
                code.push(Instruction::ExtraArgument);
                continue;
            }
        }
        code.push(instruction);
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

    fn nested_prototypes(depth: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for level in 0..=depth {
            bytes.extend([0; 12]); // absent source name and line range
            bytes.extend([0, 0, 0, 2]);
            bytes.extend(1u32.to_le_bytes());
            bytes.extend((30u32 | (1 << 23)).to_le_bytes()); // RETURN no values
            bytes.extend(0u32.to_le_bytes()); // constants
            bytes.extend(u32::from(level < depth).to_le_bytes());
        }
        for _ in 0..=depth { bytes.extend([0; 12]); }
        bytes
    }

    #[test]
    fn debug_sections_are_mandatory_even_when_stripped() {
        let bytes = nested_prototypes(1);
        assert!(matches!(Function::parse(&bytes), Ok(([], _))));
        for cut in 0..bytes.len() {
            assert!(Function::parse(&bytes[..cut]).is_err(), "accepted truncation at {cut}");
        }
        let mut huge_count = nested_prototypes(0);
        let debug_start = huge_count.len() - 12;
        huge_count[debug_start..debug_start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Function::parse(&huge_count).is_err());
    }

    #[test]
    fn nesting_budget_is_checked_without_recursive_parsing() {
        let at_limit = nested_prototypes(MAX_FUNCTION_DEPTH);
        assert!(matches!(Function::parse(&at_limit), Ok(([], _))));
        for depth in [MAX_FUNCTION_DEPTH + 1, 4096] {
            let bytes = nested_prototypes(depth);
            assert!(Function::parse(&bytes).is_err());
        }
    }
}
