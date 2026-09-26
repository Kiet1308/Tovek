//! Validate bytecode indices before any analysis follows them. Parser success
//! only proves serialization, not that register/constant/branch operands exist.
use crate::{
    deserializer::{chunk::Chunk, constant::Constant},
    instruction::Instruction,
    op_code::OpCode::*,
};

pub(crate) fn validate(chunk: &Chunk) -> Result<(), String> {
    for (id, function) in chunk.functions.iter().enumerate() {
        let code = &function.instructions;
        let mut starts = vec![true; code.len()];
        let mut pc = 0;
        while pc < code.len() {
            let op = match code[pc] {
                Instruction::BC { op_code, .. }
                | Instruction::AD { op_code, .. }
                | Instruction::E { op_code, .. } => op_code,
            };
            if op.has_aux() {
                let slot = starts
                    .get_mut(pc + 1)
                    .ok_or_else(|| format!("prototype {id}: missing AUX at {pc}"))?;
                *slot = false;
                pc += 2;
            } else {
                pc += 1;
            }
        }
        if function.num_parameters > function.max_stack_size {
            return Err(format!("prototype {id}: parameters exceed register stack"));
        }
        for (pc, instruction) in code.iter().enumerate().filter(|(pc, _)| starts[*pc]) {
            let error = |what: &str| format!("prototype {id}, pc {pc}: invalid {what}");
            let reg = |index: usize| {
                if index < usize::from(function.max_stack_size) {
                    Ok(())
                } else {
                    Err(error("register"))
                }
            };
            let constant = |index: usize| {
                function
                    .constants
                    .get(index)
                    .ok_or_else(|| error("constant index"))
            };
            let string = |index: usize| match constant(index)? {
                Constant::String(_) => Ok(()),
                _ => Err(error("string constant")),
            };
            let target = |offset: isize| match (pc + 1).checked_add_signed(offset) {
                Some(to) if starts.get(to) == Some(&true) => Ok(()),
                _ => Err(error("jump target")),
            };
            match *instruction {
                Instruction::E {
                    op_code: LOP_JUMPX,
                    e,
                } => target(e as isize)?,
                Instruction::E { .. } => {}
                Instruction::AD { op_code, a, d, aux } => {
                    let a = usize::from(a);
                    match op_code {
                        LOP_JUMP | LOP_JUMPBACK => target(d as isize)?,
                        LOP_LOADN => reg(a)?,
                        LOP_LOADK | LOP_DUPTABLE | LOP_DUPCLOSURE => {
                            reg(a)?;
                            constant(d as usize)?;
                        }
                        LOP_LOADKX => {
                            reg(a)?;
                            constant(aux as usize)?;
                        }
                        LOP_GETIMPORT => {
                            reg(a)?;
                            constant(d as usize)?;
                            let count = aux >> 30;
                            if count == 0 {
                                return Err(error("import path"));
                            }
                            for index in 0..count {
                                string(((aux >> (20 - index * 10)) & 1023) as usize)?;
                            }
                        }
                        LOP_NEWCLOSURE => {
                            reg(a)?;
                            function
                                .functions
                                .get(d as usize)
                                .ok_or_else(|| error("child prototype"))?;
                        }
                        LOP_FORNPREP | LOP_FORNLOOP | LOP_FORGPREP | LOP_FORGPREP_INEXT
                        | LOP_FORGPREP_NEXT => {
                            reg(a + 2)?;
                            target(d as isize)?;
                        }
                        LOP_FORGLOOP => {
                            let count = (aux & 255) as usize;
                            if count == 0 {
                                return Err(error("generic-for result count"));
                            }
                            reg(a + 2 + count)?;
                            target(d as isize)?;
                        }
                        LOP_JUMPIFEQ | LOP_JUMPIFLE | LOP_JUMPIFLT | LOP_JUMPIFNOTEQ
                        | LOP_JUMPIFNOTLE | LOP_JUMPIFNOTLT => {
                            reg(a)?;
                            reg(aux as usize)?;
                            target(d as isize)?;
                        }
                        LOP_JUMPXEQKN | LOP_JUMPXEQKS => {
                            reg(a)?;
                            constant((aux & 0xffffff) as usize)?;
                            target(d as isize)?;
                        }
                        LOP_JUMPIF | LOP_JUMPIFNOT | LOP_JUMPXEQKNIL | LOP_JUMPXEQKB
                        | LOP_CMPPROTO => {
                            reg(a)?;
                            target(d as isize)?;
                        }
                        _ => {}
                    }
                }
                Instruction::BC {
                    op_code,
                    a,
                    b,
                    c,
                    aux,
                } => {
                    let (a, b, c) = (usize::from(a), usize::from(b), usize::from(c));
                    match op_code {
                        LOP_NOP | LOP_BREAK | LOP_PREPVARARGS | LOP_FASTCALL | LOP_FASTCALL1
                        | LOP_FASTCALL2 | LOP_FASTCALL2K | LOP_FASTCALL3 | LOP_FASTPCALL => {}
                        LOP_LOADNIL | LOP_NEWTABLE => reg(a)?,
                        LOP_LOADKX => {
                            reg(a)?;
                            constant(aux as usize)?;
                        }
                        LOP_LOADB => {
                            reg(a)?;
                            if c != 0 {
                                target(c as isize)?;
                            }
                        }
                        LOP_CLOSEUPVALS => {
                            if a > usize::from(function.max_stack_size) {
                                return Err(error("close register"));
                            }
                        }
                        LOP_GETUPVAL | LOP_SETUPVAL => {
                            reg(a)?;
                            if b >= usize::from(function.num_upvalues) {
                                return Err(error("upvalue"));
                            }
                        }
                        LOP_CAPTURE => match a {
                            0 | 1 => reg(b)?,
                            2 if b < usize::from(function.num_upvalues) => {}
                            _ => return Err(error("capture operand")),
                        },
                        LOP_GETGLOBAL | LOP_SETGLOBAL => {
                            reg(a)?;
                            string(aux as usize)?;
                        }
                        LOP_GETTABLEKS | LOP_SETTABLEKS | LOP_GETUDATAKS | LOP_SETUDATAKS => {
                            reg(a)?;
                            reg(b)?;
                            string(
                                (aux & if matches!(op_code, LOP_GETUDATAKS | LOP_SETUDATAKS) {
                                    0xffff
                                } else {
                                    u32::MAX
                                }) as usize,
                            )?;
                        }
                        LOP_NAMECALL | LOP_NAMECALLUDATA => {
                            reg(a + 1)?;
                            reg(b)?;
                            string(
                                (aux & if op_code == LOP_NAMECALLUDATA {
                                    0xffff
                                } else {
                                    u32::MAX
                                }) as usize,
                            )?;
                        }
                        LOP_MOVE | LOP_NOT | LOP_MINUS | LOP_LENGTH | LOP_GETTABLEN
                        | LOP_SETTABLEN => {
                            reg(a)?;
                            reg(b)?;
                        }
                        LOP_CALL | LOP_CALLFB | LOP_NATIVECALL => {
                            reg(a)?;
                            if b > 1 {
                                reg(a + b - 1)?;
                            }
                            if c > 1 {
                                reg(a + c - 2)?;
                            }
                        }
                        LOP_RETURN | LOP_GETVARARGS => {
                            if b > 1 {
                                reg(a + b - 2)?;
                            }
                        }
                        LOP_SETLIST => {
                            reg(a)?;
                            if aux == 0 {
                                return Err(error("SETLIST index (must be positive)"));
                            }
                            if c > 1 {
                                reg(b + c - 2)?;
                            }
                        }
                        LOP_ADDK | LOP_SUBK | LOP_MULK | LOP_DIVK | LOP_MODK | LOP_POWK
                        | LOP_ANDK | LOP_ORK | LOP_IDIVK => {
                            reg(a)?;
                            reg(b)?;
                            constant(c)?;
                        }
                        LOP_SUBRK | LOP_DIVRK => {
                            reg(a)?;
                            constant(b)?;
                            reg(c)?;
                        }
                        LOP_GETTABLE | LOP_SETTABLE | LOP_ADD | LOP_SUB | LOP_MUL | LOP_DIV
                        | LOP_MOD | LOP_POW | LOP_AND | LOP_OR | LOP_IDIV | LOP_CONCAT => {
                            reg(a)?;
                            reg(b)?;
                            reg(c)?;
                        }
                        _ => {}
                    }
                }
            }
        }
        for value in &function.constants {
            match value {
                Constant::String(index) if *index > chunk.string_table.len() => {
                    return Err(format!("prototype {id}: invalid string index"));
                }
                Constant::Closure(index) if *index >= chunk.functions.len() => {
                    return Err(format!("prototype {id}: invalid closure index"));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
