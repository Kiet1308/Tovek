use core::num;

use nom::{
    complete::take,
    number::complete::{le_u32, le_u8},
    IResult,
};
use nom_leb128::leb128_usize;

use super::{
    constant::Constant,
    list::{parse_list, parse_list_len},
};

use crate::{instruction::*, op_code::OpCode};

#[derive(Debug)]
pub struct Function {
    pub max_stack_size: u8,
    pub num_parameters: u8,
    pub num_upvalues: u8,
    pub is_vararg: bool,
    //pub instructions: Vec<u32>,
    pub instructions: Vec<Instruction>,
    pub constants: Vec<Constant>,
    pub functions: Vec<usize>,
    pub line_defined: usize,
    pub function_name: usize,
    pub line_gap_log2: Option<u8>,
    pub line_info_delta: Option<Vec<u8>>,
    pub abs_line_info_delta: Option<Vec<u32>>,
}

impl Function {
    fn parse_instructions(vec: &Vec<u32>, encode_key: u8) -> Vec<Instruction> {
        let mut v: Vec<Instruction> = Vec::new();
        let mut pc = 0;

        loop {
            let ins = Instruction::parse(vec[pc], encode_key).unwrap();
            let op = match ins {
                Instruction::BC { op_code, .. } => op_code,
                Instruction::AD { op_code, .. } => op_code,
                Instruction::E { op_code, .. } => op_code,
            };

            // handle ops with aux values
            match op {
                OpCode::LOP_GETGLOBAL
                | OpCode::LOP_SETGLOBAL
                | OpCode::LOP_GETIMPORT
                | OpCode::LOP_GETTABLEKS
                | OpCode::LOP_SETTABLEKS
                | OpCode::LOP_NAMECALL
                | OpCode::LOP_JUMPIFEQ
                | OpCode::LOP_JUMPIFLE
                | OpCode::LOP_JUMPIFLT
                | OpCode::LOP_JUMPIFNOTEQ
                | OpCode::LOP_JUMPIFNOTLE
                | OpCode::LOP_JUMPIFNOTLT
                | OpCode::LOP_NEWTABLE
                | OpCode::LOP_SETLIST
                | OpCode::LOP_FORGLOOP
                | OpCode::LOP_LOADKX
                | OpCode::LOP_FASTCALL2
                | OpCode::LOP_FASTCALL2K
                | OpCode::LOP_FASTCALL3
                | OpCode::LOP_JUMPXEQKNIL
                | OpCode::LOP_JUMPXEQKB
                | OpCode::LOP_JUMPXEQKN
                | OpCode::LOP_JUMPXEQKS
                // v9/v10/v11 aux-bearing opcodes (getOpLength == 2). Omitting any of
                // these would desync the instruction stream of every proto that uses them.
                | OpCode::LOP_GETUDATAKS
                | OpCode::LOP_SETUDATAKS
                | OpCode::LOP_NAMECALLUDATA
                | OpCode::LOP_NEWCLASSMEMBER
                | OpCode::LOP_CALLFB
                | OpCode::LOP_CMPPROTO => {
                    let aux = vec[pc + 1];
                    pc += 2;
                    match ins {
                        Instruction::BC {
                            op_code, a, b, c, ..
                        } => {
                            v.push(Instruction::BC {
                                op_code,
                                a,
                                b,
                                c,
                                aux,
                            });
                        }
                        Instruction::AD { op_code, a, d, .. } => {
                            v.push(Instruction::AD { op_code, a, d, aux });
                        }
                        _ => unreachable!(),
                    }
                    v.push(Instruction::BC {
                        op_code: OpCode::LOP_NOP,
                        a: 0,
                        b: 0,
                        c: 0,
                        aux: 0,
                    });
                }
                _ => {
                    v.push(ins);
                    pc += 1;
                }
            }

            if pc == vec.len() {
                break;
            }
        }

        v
    }

    pub(crate) fn parse(input: &[u8], encode_key: u8, version: u8) -> IResult<&[u8], Self> {
        let (input, max_stack_size) = le_u8(input)?;
        let (input, num_parameters) = le_u8(input)?;
        let (input, num_upvalues) = le_u8(input)?;
        let (input, is_vararg) = le_u8(input)?;

        let (input, flags) = le_u8(input)?;
        let (input, _) = parse_list(input, le_u8)?;

        let (input, u32_instructions) = parse_list(input, le_u32)?;
        //let (input, instructions) = parse_list(input, Function::parse_instrution)?;
        let instructions = Self::parse_instructions(&u32_instructions, encode_key);
        let (input, constants) = parse_list(input, Constant::parse)?;
        let (input, functions) = parse_list(input, leb128_usize)?;
        let (input, line_defined) = leb128_usize(input)?;
        let (input, function_name) = leb128_usize(input)?;
        let (input, has_line_info) = le_u8(input)?;
        let (input, line_gap_log2) = match has_line_info {
            0 => (input, None),
            _ => {
                let (input, line_gap_log2) = le_u8(input)?;
                (input, Some(line_gap_log2))
            }
        };
        let (input, line_info_delta) = match has_line_info {
            0 => (input, None),
            _ => {
                let (input, line_info_delta) =
                    parse_list_len(input, le_u8, u32_instructions.len())?;
                (input, Some(line_info_delta))
            }
        };
        let (input, abs_line_info_delta) = match has_line_info {
            0 => (input, None),
            _ => {
                let (input, abs_line_info_delta) = parse_list_len(
                    input,
                    le_u32,
                    ((u32_instructions.len() - 1) >> line_gap_log2.unwrap()) + 1,
                )?;
                (input, Some(abs_line_info_delta))
            }
        };
        let input = match le_u8(input)? {
            (input, 0) => input,
            (input, _) => {
                panic!("we have debug info");
                let (mut input, num_locvars) = leb128_usize(input)?;
                for _ in 0..num_locvars {
                    (input, _) = leb128_usize(input)?;
                    (input, _) = leb128_usize(input)?;
                    (input, _) = leb128_usize(input)?;
                    (input, _) = le_u8(input)?;
                }
                let (mut input, num_upvalues) = leb128_usize(input)?;
                for _ in 0..num_upvalues {
                    (input, _) = leb128_usize(input)?;
                }
                input
            }
        };
        // Bytecode v11+ appends a per-proto "feedback vector" (runtime call-target
        // profiling) here, after debuginfo. It carries no source-level meaning, but the
        // bytes must be consumed or the next proto / the main-id varint will desync.
        // Layout (lvmload.cpp): varint count, then per slot a raw u8 slot type
        // (LFT_CALLTARGET == 0) followed by a varint pc. Only LFT_CALLTARGET exists today;
        // fail loudly on anything else rather than risk misreading an unknown slot layout.
        let input = if version >= 11 {
            let (mut input, feedback_count) = leb128_usize(input)?;
            for _ in 0..feedback_count {
                let (rest, slot_type) = le_u8(input)?;
                if slot_type != 0 {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Tag,
                    )));
                }
                let (rest, _call_target_pc) = leb128_usize(rest)?;
                input = rest;
            }
            input
        } else {
            input
        };
        Ok((
            input,
            Self {
                max_stack_size,
                num_parameters,
                num_upvalues,
                is_vararg: is_vararg != 0u8,
                instructions,
                constants,
                functions,
                line_defined,
                function_name,
                line_gap_log2,
                line_info_delta,
                abs_line_info_delta,
            },
        ))
    }
}
