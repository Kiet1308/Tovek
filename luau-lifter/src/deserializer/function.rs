use nom::{
    IResult,
    number::complete::{le_u8, le_u32},
};
use nom_leb128::leb128_usize;

use super::{
    constant::Constant,
    list::{parse_list, parse_list_len},
};

use crate::{instruction::*, op_code::OpCode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugLocal {
    /// One-based index into the chunk string table; zero means no name.
    pub name_index: usize,
    /// Inclusive bytecode PC where the local becomes visible.
    pub start_pc: usize,
    /// Exclusive bytecode PC where the local stops being visible.
    pub end_pc: usize,
    pub register: u8,
}

const LPF_INLINABLE: u8 = 1 << 3;

/// Bytecode type tags (`LuauBytecodeType` in Luau's `Bytecode.h`).
pub const LBC_TYPE_NIL: u8 = 0;
pub const LBC_TYPE_BOOLEAN: u8 = 1;
pub const LBC_TYPE_NUMBER: u8 = 2;
pub const LBC_TYPE_STRING: u8 = 3;
pub const LBC_TYPE_TABLE: u8 = 4;
pub const LBC_TYPE_FUNCTION: u8 = 5;
pub const LBC_TYPE_THREAD: u8 = 6;
pub const LBC_TYPE_USERDATA: u8 = 7;
pub const LBC_TYPE_VECTOR: u8 = 8;
pub const LBC_TYPE_BUFFER: u8 = 9;
pub const LBC_TYPE_INTEGER: u8 = 10;
pub const LBC_TYPE_ANY: u8 = 15;
pub const LBC_TYPE_TAGGED_USERDATA_BASE: u8 = 64;
pub const LBC_TYPE_TAGGED_USERDATA_END: u8 = 64 + 32;
pub const LBC_TYPE_OPTIONAL_BIT: u8 = 1 << 7;

/// A typed local register range recorded by the compiler (types version >= 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypedLocal {
    pub type_tag: u8,
    pub register: u8,
    pub start_pc: usize,
    pub end_pc: usize,
}

/// Compiler-recorded type information for one prototype.
///
/// `parameter_types` is the function signature (`LBC_TYPE_FUNCTION`, count,
/// then one tag per parameter, `self` included); it is present only when the
/// source annotated at least one parameter with a non-`any` type, so every
/// entry here is source-recoverable.  Local and upvalue tags are inferred by
/// the compiler from annotations and literal/builtin expressions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FunctionTypeInfo {
    pub parameter_types: Vec<u8>,
    pub upvalue_types: Vec<u8>,
    pub local_types: Vec<TypedLocal>,
}

impl FunctionTypeInfo {
    fn parse(raw: &[u8]) -> Option<Self> {
        fn varint(input: &[u8], pos: &mut usize) -> Option<usize> {
            let mut result = 0usize;
            let mut shift = 0;
            loop {
                let byte = *input.get(*pos)?;
                *pos += 1;
                result |= usize::from(byte & 0x7f) << shift;
                shift += 7;
                if byte & 0x80 == 0 {
                    return Some(result);
                }
            }
        }
        if raw.is_empty() {
            return None;
        }
        let mut pos = 0;
        let function_size = varint(raw, &mut pos)?;
        let upvalue_count = varint(raw, &mut pos)?;
        let local_count = varint(raw, &mut pos)?;
        let function = raw.get(pos..pos + function_size)?;
        pos += function_size;
        let parameter_types = match function {
            [] => Vec::new(),
            [LBC_TYPE_FUNCTION, count, rest @ ..] if rest.len() == usize::from(*count) => {
                rest.to_vec()
            }
            _ => return None,
        };
        let upvalue_types = raw.get(pos..pos + upvalue_count)?.to_vec();
        pos += upvalue_count;
        let mut local_types = Vec::with_capacity(local_count);
        for _ in 0..local_count {
            let type_tag = *raw.get(pos)?;
            let register = *raw.get(pos + 1)?;
            pos += 2;
            let start_pc = varint(raw, &mut pos)?;
            let length = varint(raw, &mut pos)?;
            local_types.push(TypedLocal {
                type_tag,
                register,
                start_pc,
                end_pc: start_pc + length,
            });
        }
        Some(Self {
            parameter_types,
            upvalue_types,
            local_types,
        })
    }
}

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
    pub has_debug_info: bool,
    pub debug_locals: Vec<DebugLocal>,
    /// Ordered one-based string-table indices, one entry per upvalue slot.
    pub debug_upvalue_name_indices: Vec<usize>,
    /// Compiler-recorded type information (bytecode version >= 4, types
    /// version 2/3), when the prototype carries any.
    pub type_info: Option<FunctionTypeInfo>,
}

impl Function {
    fn parse_instructions(vec: &[u32], encode_key: u8) -> Result<Vec<Instruction>, ()> {
        let mut v: Vec<Instruction> = Vec::new();
        let mut pc = 0;

        while pc < vec.len() {
            let ins = Instruction::parse(vec[pc], encode_key).map_err(|_| ())?;
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
                    let aux = *vec.get(pc + 1).ok_or(())?;
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
                        _ => return Err(()),
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
        }

        Ok(v)
    }

    pub(crate) fn parse(input: &[u8], encode_key: u8, version: u8) -> IResult<&[u8], Self> {
        let (input, max_stack_size) = le_u8(input)?;
        let (input, num_parameters) = le_u8(input)?;
        let (input, num_upvalues) = le_u8(input)?;
        let (input, is_vararg) = le_u8(input)?;

        let (input, flags) = le_u8(input)?;
        let (input, raw_type_info) = parse_list(input, le_u8)?;
        let type_info = FunctionTypeInfo::parse(&raw_type_info);

        let (input, u32_instructions) = parse_list(input, le_u32)?;
        if u32_instructions.is_empty() {
            return Err(nom::Err::Failure(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Verify,
            )));
        }
        //let (input, instructions) = parse_list(input, Function::parse_instrution)?;
        let instructions =
            Self::parse_instructions(&u32_instructions, encode_key).map_err(|_| {
                nom::Err::Failure(nom::error::Error::new(input, nom::error::ErrorKind::Verify))
            })?;
        let (input, constants) = parse_list(input, |i| Constant::parse(i, version))?;
        let (input, functions) = parse_list(input, leb128_usize)?;
        let (input, line_defined) = leb128_usize(input)?;
        let (input, function_name) = leb128_usize(input)?;
        let (input, has_line_info) = le_u8(input)?;
        let (input, line_gap_log2) = match has_line_info {
            0 => (input, None),
            _ => {
                let (input, line_gap_log2) = le_u8(input)?;
                if usize::from(line_gap_log2) >= usize::BITS as usize {
                    return Err(nom::Err::Failure(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Verify,
                    )));
                }
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
        let (input, has_debug_info_raw) = le_u8(input)?;
        let has_debug_info = has_debug_info_raw != 0;
        let (input, debug_locals, debug_upvalue_name_indices) = if !has_debug_info {
            (input, Vec::new(), Vec::new())
        } else {
            let (mut input, num_locals) = leb128_usize(input)?;
            if num_locals > input.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Count,
                )));
            }
            let mut debug_locals = Vec::with_capacity(num_locals);
            for _ in 0..num_locals {
                let (rest, name_index) = leb128_usize(input)?;
                let (rest, start_pc) = leb128_usize(rest)?;
                let (rest, end_pc) = leb128_usize(rest)?;
                let (rest, register) = le_u8(rest)?;
                input = rest;
                debug_locals.push(DebugLocal {
                    name_index,
                    start_pc,
                    end_pc,
                    register,
                });
            }

            let (mut input, num_debug_upvalues) = leb128_usize(input)?;
            if num_debug_upvalues > input.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Count,
                )));
            }
            let mut debug_upvalue_name_indices = Vec::with_capacity(num_debug_upvalues);
            for _ in 0..num_debug_upvalues {
                let (rest, name_index) = leb128_usize(input)?;
                input = rest;
                debug_upvalue_name_indices.push(name_index);
            }

            (input, debug_locals, debug_upvalue_name_indices)
        };
        // Bytecode v11+ appends a per-proto "feedback vector" (runtime call-target
        // profiling) here, after debuginfo. It carries no source-level meaning, but the
        // bytes must be consumed or the next proto / the main-id varint will desync.
        // Layout (lvmload.cpp): varint count, then per slot a raw u8 slot type
        // (LFT_CALLTARGET == 0) followed by a varint pc. Only LFT_CALLTARGET exists today;
        // fail loudly on anything else rather than risk misreading an unknown slot layout.
        let input = if version >= 11 {
            let (mut input, feedback_count) = leb128_usize(input)?;
            if feedback_count > input.len() {
                return Err(nom::Err::Failure(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Count,
                )));
            }
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
        // v12+ stores a cost-model varint for inlinable prototypes. It has no
        // source-level meaning, but must be consumed before the next prototype.
        let input = if version >= 12 && flags & LPF_INLINABLE != 0 {
            let (input, _) = leb128_usize(input)?;
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
                has_debug_info,
                debug_locals,
                debug_upvalue_name_indices,
                type_info,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{Function, FunctionTypeInfo, TypedLocal};

    #[test]
    fn parses_type_info_block() {
        // typeinfo: function sig size=5, 1 upvalue, 2 locals
        // sig: FUNCTION, 3 params: number, string?, tagged#2
        let raw = [
            5u8, 1, 2, // sizes
            super::LBC_TYPE_FUNCTION, 3, super::LBC_TYPE_NUMBER,
            super::LBC_TYPE_STRING | super::LBC_TYPE_OPTIONAL_BIT,
            super::LBC_TYPE_TAGGED_USERDATA_BASE + 2,
            super::LBC_TYPE_TABLE, // upvalue
            super::LBC_TYPE_VECTOR, 4, 10, 5, // local: vector in r4, pc 10..15
            super::LBC_TYPE_BUFFER, 7, 0x80, 0x01, 3, // local: buffer in r7, pc 128..131
        ];
        let info = FunctionTypeInfo::parse(&raw).expect("typeinfo");
        assert_eq!(
            info.parameter_types,
            vec![
                super::LBC_TYPE_NUMBER,
                super::LBC_TYPE_STRING | super::LBC_TYPE_OPTIONAL_BIT,
                super::LBC_TYPE_TAGGED_USERDATA_BASE + 2
            ]
        );
        assert_eq!(info.upvalue_types, vec![super::LBC_TYPE_TABLE]);
        assert_eq!(
            info.local_types,
            vec![
                TypedLocal { type_tag: super::LBC_TYPE_VECTOR, register: 4, start_pc: 10, end_pc: 15 },
                TypedLocal { type_tag: super::LBC_TYPE_BUFFER, register: 7, start_pc: 128, end_pc: 131 },
            ]
        );
        // Empty block and truncated block.
        assert!(FunctionTypeInfo::parse(&[]).is_none());
        assert!(FunctionTypeInfo::parse(&[5, 0, 0, super::LBC_TYPE_FUNCTION, 3]).is_none());
    }

    fn leb128(mut value: usize) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                return out;
            }
        }
    }

    #[test]
    fn parses_debug_locals_and_ordered_upvalue_names() {
        let mut bytes = vec![3, 0, 2, 0, 0];
        bytes.extend(leb128(0)); // type info
        bytes.extend(leb128(1)); // instruction count
        bytes.extend(0u32.to_le_bytes()); // NOP
        bytes.extend(leb128(0)); // constants
        bytes.extend(leb128(0)); // child protos
        bytes.extend(leb128(12)); // line defined
        bytes.extend(leb128(1)); // function debug name
        bytes.push(0); // no line info
        bytes.push(1); // has debug info
        bytes.extend(leb128(1)); // local count
        bytes.extend(leb128(2)); // local name
        bytes.extend(leb128(0)); // start pc
        bytes.extend(leb128(1)); // end pc
        bytes.push(2); // register
        bytes.extend(leb128(2)); // upvalue count
        bytes.extend(leb128(3));
        bytes.extend(leb128(4));

        let (rest, function) = Function::parse(&bytes, 1, 9).expect("function must parse");
        assert!(rest.is_empty());
        assert_eq!(function.debug_locals.len(), 1);
        let local = &function.debug_locals[0];
        assert_eq!(local.name_index, 2);
        assert_eq!(local.start_pc, 0);
        assert_eq!(local.end_pc, 1);
        assert_eq!(local.register, 2);
        assert_eq!(function.debug_upvalue_name_indices, vec![3, 4]);
    }

    #[test]
    fn rejects_empty_instruction_stream() {
        let mut bytes = vec![1, 0, 0, 0, 0];
        bytes.extend(leb128(0)); // type info
        bytes.extend(leb128(0)); // instruction count

        assert!(Function::parse(&bytes, 1, 9).is_err());
    }

    #[test]
    fn rejects_truncated_aux_instruction() {
        // GETGLOBAL has a two-word encoding, but only its opcode word is present.
        assert!(Function::parse_instructions(&[7], 1).is_err());
    }

    #[test]
    fn rejects_invalid_opcode_without_panicking() {
        assert!(Function::parse_instructions(&[96], 1).is_err());
    }

    #[test]
    fn rejects_oversized_line_gap() {
        let mut bytes = vec![1, 0, 0, 0, 0];
        bytes.extend(leb128(0)); // type info
        bytes.extend(leb128(1)); // instruction count
        bytes.extend(0u32.to_le_bytes()); // NOP
        bytes.extend(leb128(0)); // constants
        bytes.extend(leb128(0)); // child protos
        bytes.extend(leb128(0)); // line defined
        bytes.extend(leb128(0)); // function name
        bytes.push(1); // has line info
        bytes.push(u8::MAX); // invalid shift amount

        assert!(Function::parse(&bytes, 1, 9).is_err());
    }
}
