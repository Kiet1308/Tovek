//! Validate bytecode indices before any analysis follows them. Parser success
//! only proves serialization, not that register/constant/branch operands exist.
use crate::{
    deserializer::{chunk::Chunk, constant::Constant},
    instruction::Instruction,
    op_code::OpCode::*,
};

/// The constant a DUPTABLE copies must be a table template whose keys are
/// earlier scalar constants (the loader builds constants in order) and whose
/// values are scalar constants, `nil` or no value. Luau bakes nothing else into
/// one: a nested table or a closure would be shared by every copy, which no
/// constructor spells, so such a chunk is refused here rather than by a panic
/// in the lifter.
fn template(id: usize, pc: usize, constants: &[Constant], index: usize) -> Result<(), String> {
    let scalar = |at: usize| matches!(constants.get(at), Some(Constant::Boolean(_)
        | Constant::Number(_) | Constant::Integer(_) | Constant::String(_)
        | Constant::Vector(..) | Constant::VectorD(..)));
    let error = |what: &str| format!("prototype {id}, pc {pc}: DUPTABLE {what}");
    let keys: Vec<(usize, Option<i32>)> = match constants.get(index) {
        Some(Constant::Table(keys)) => keys.iter().map(|&key| (key, None)).collect(),
        Some(Constant::TableWithConstants(pairs)) => pairs.iter().map(|&(key, value)| (key, Some(value))).collect(),
        Some(_) => return Err(error("constant is not a table template")),
        None => return Err(error("constant index")),
    };
    for (key, value) in keys {
        if key >= index || !scalar(key) {
            return Err(error("template key is not an earlier scalar constant"));
        }
        if let Some(Ok(value)) = value.map(usize::try_from)
            && value < index
            && !matches!(constants.get(value), Some(Constant::Nil))
            && !scalar(value)
        {
            return Err(error("template value is not a scalar constant"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{deserializer::function::Function, op_code::OpCode};

    fn bc(op_code: OpCode, a: u8, b: u8, c: u8) -> Instruction {
        Instruction::BC { op_code, a, b, c, aux: 0 }
    }

    fn ad(op_code: OpCode, a: u8, d: i16, aux: u32) -> Instruction {
        Instruction::AD { op_code, a, d, aux }
    }

    fn check(instructions: Vec<Instruction>, constants: Vec<Constant>) -> Result<(), String> {
        validate(&Chunk { version: 9, main: 0, string_table: vec![], userdata_type_names: vec![],
            functions: vec![Function { max_stack_size: 8, num_parameters: 0, num_upvalues: 0,
                is_vararg: true, flags: 0, instructions, constants, functions: vec![], line_defined: 0,
                function_name: 0, line_gap_log2: None, line_info_delta: None, abs_line_info_delta: None,
                has_debug_info: false, debug_locals: vec![], debug_upvalue_name_indices: vec![], type_info: None }] })
    }

    #[test]
    fn refuses_debugger_break_and_short_concat_before_lifting() {
        assert!(check(vec![bc(LOP_BREAK, 0, 0, 0)], vec![]).unwrap_err().contains("debugger BREAK"));
        for (b, c) in [(1, 1), (2, 1)] {
            assert!(check(vec![bc(LOP_CONCAT, 0, b, c)], vec![]).unwrap_err().contains("CONCAT register range"));
        }
        assert!(check(vec![bc(LOP_CONCAT, 0, 1, 2), bc(LOP_RETURN, 0, 2, 0)], vec![]).is_ok());
    }

    #[test]
    fn refuses_nonliteral_constant_operands() {
        assert!(check(vec![ad(LOP_LOADK, 0, 0, 0)], vec![Constant::Table(vec![])])
            .unwrap_err().contains("literal constant"));
        assert!(check(vec![bc(LOP_ADDK, 0, 1, 0)], vec![Constant::Import(0)])
            .unwrap_err().contains("literal constant"));
        assert!(check(vec![ad(LOP_LOADK, 0, 0, 0), bc(LOP_RETURN, 0, 2, 0)], vec![Constant::Number(1.0)]).is_ok());
    }

    #[test]
    fn requires_one_matching_prep_per_generic_step() {
        let nop = || bc(LOP_NOP, 0, 0, 0);
        let ret = || bc(LOP_RETURN, 0, 1, 0);
        assert!(check(vec![ad(LOP_FORGPREP, 0, 0, 0), ret()], vec![])
            .unwrap_err().contains("FORGPREP/FORGLOOP pair"));
        assert!(check(vec![ad(LOP_FORGPREP, 0, 0, 0), ad(LOP_FORGLOOP, 1, 1, 1), nop(), ret()], vec![])
            .unwrap_err().contains("FORGPREP/FORGLOOP pair"));
        assert!(check(vec![ad(LOP_FORGPREP, 0, 1, 0), ad(LOP_FORGPREP, 0, 0, 0),
            ad(LOP_FORGLOOP, 0, 1, 1), nop(), ret()], vec![]).unwrap_err().contains("FORGPREP/FORGLOOP pair"));
        assert!(check(vec![ad(LOP_FORGLOOP, 0, 1, 1), nop(), ret()], vec![])
            .unwrap_err().contains("FORGLOOP without FORGPREP"));
        assert!(check(vec![ad(LOP_FORGPREP, 0, 0, 0), ad(LOP_FORGLOOP, 0, 1, 1), nop(), ret()], vec![]).is_ok());
        // Large loops use JUMP +1; JUMPX target; FORGPREP -2.
        assert!(check(vec![ad(LOP_JUMP, 0, 1, 0), Instruction::E { op_code: LOP_JUMPX, e: 1 },
            ad(LOP_FORGPREP, 0, -2, 0), ad(LOP_FORGLOOP, 0, 1, 1), nop(), ret()], vec![]).is_ok());
    }

    #[test]
    fn refuses_open_results_without_a_same_block_producer() {
        for instruction in [bc(LOP_RETURN, 0, 0, 0), bc(LOP_CALL, 0, 0, 1),
            Instruction::BC { op_code: LOP_SETLIST, a: 0, b: 1, c: 0, aux: 1 }] {
            assert!(check(vec![instruction, bc(LOP_NOP, 0, 0, 0)], vec![])
                .unwrap_err().contains("open result list"));
        }
        assert!(check(vec![bc(LOP_GETVARARGS, 1, 0, 0), bc(LOP_CALL, 0, 0, 0), bc(LOP_RETURN, 0, 0, 0)], vec![]).is_ok());
        assert!(check(vec![bc(LOP_GETVARARGS, 0, 0, 0), ad(LOP_JUMP, 0, 0, 0), bc(LOP_RETURN, 0, 0, 0)], vec![])
            .unwrap_err().contains("open result list"));
    }
}

pub(crate) fn validate(chunk: &Chunk) -> Result<(), String> {
    for (id, function) in chunk.functions.iter().enumerate() {
        let code = &function.instructions;
        if code.is_empty() {
            return Err(format!("prototype {id}: empty instruction stream"));
        }
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
        // Branch destinations, for instruction groups that must not be entered midway.
        let mut targets = vec![false; code.len()];
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
            let literal = |index: usize| match constant(index)? {
                Constant::Nil | Constant::Boolean(_) | Constant::Number(_)
                | Constant::Integer(_) | Constant::String(_) | Constant::Vector(..)
                | Constant::VectorD(..) => Ok(()),
                _ => Err(error("literal constant")),
            };
            let mut target = |offset: isize| match (pc + 1).checked_add_signed(offset) {
                Some(to) if starts.get(to) == Some(&true) => {
                    targets[to] = true;
                    Ok(())
                }
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
                        LOP_LOADK => {
                            reg(a)?;
                            literal(d as usize)?;
                        }
                        LOP_DUPCLOSURE => {
                            reg(a)?;
                            constant(d as usize)?;
                        }
                        LOP_DUPTABLE => {
                            reg(a)?;
                            template(id, pc, &function.constants, d as usize)?;
                        }
                        LOP_LOADKX => {
                            reg(a)?;
                            literal(aux as usize)?;
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
                            match (op_code, constant((aux & 0xffffff) as usize)?) {
                                (LOP_JUMPXEQKN, Constant::Number(_))
                                | (LOP_JUMPXEQKS, Constant::String(_)) => {}
                                _ => return Err(error("comparison constant")),
                            }
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
                        LOP_BREAK => return Err(error("debugger BREAK (original instruction unavailable)")),
                        LOP_NOP | LOP_PREPVARARGS | LOP_FASTCALL | LOP_FASTCALL1
                        | LOP_FASTCALL2 | LOP_FASTCALL2K | LOP_FASTCALL3 | LOP_FASTPCALL => {}
                        LOP_LOADNIL | LOP_NEWTABLE => reg(a)?,
                        LOP_LOADKX => {
                            reg(a)?;
                            literal(aux as usize)?;
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
                            literal(c)?;
                        }
                        LOP_SUBRK | LOP_DIVRK => {
                            reg(a)?;
                            literal(b)?;
                            reg(c)?;
                        }
                        LOP_GETTABLE | LOP_SETTABLE | LOP_ADD | LOP_SUB | LOP_MUL | LOP_DIV
                        | LOP_MOD | LOP_POW | LOP_AND | LOP_OR | LOP_IDIV | LOP_CONCAT => {
                            reg(a)?;
                            reg(b)?;
                            reg(c)?;
                            if op_code == LOP_CONCAT && b >= c {
                                return Err(error("CONCAT register range (requires at least two values)"));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Use the same long-jump trampoline rule as lifting before it builds
        // the generic-for provenance map, which requires one prep per step.
        let mut paired = vec![false; code.len()];
        for pc in (0..code.len()).filter(|pc| starts[*pc]) {
            if let Instruction::AD { op_code: LOP_FORGPREP | LOP_FORGPREP_INEXT | LOP_FORGPREP_NEXT, a, d, .. } = code[pc] {
                let step = crate::lifter::loop_jump_target(code, pc, d);
                match code.get(step) {
                    Some(Instruction::AD { op_code: LOP_FORGLOOP, a: base, .. })
                        if *base == a && starts[step] && !paired[step] => paired[step] = true,
                    _ => return Err(format!("prototype {id}, pc {pc}: invalid FORGPREP/FORGLOOP pair")),
                }
            }
        }
        for pc in (0..code.len()).filter(|pc| starts[*pc]) {
            if matches!(code[pc], Instruction::AD { op_code: LOP_FORGLOOP, .. }) && !paired[pc] {
                return Err(format!("prototype {id}, pc {pc}: FORGLOOP without FORGPREP"));
            }
        }
        // An open result list belongs to one basic block. Lifting consumes it
        // as a single expression; a branch cannot supply it from another path.
        let mut top = None;
        for pc in (0..code.len()).filter(|pc| starts[*pc]) {
            if targets[pc] { top = None; }
            let error = || format!("prototype {id}, pc {pc}: invalid open result list");
            match code[pc] {
                Instruction::BC { op_code: LOP_CALL | LOP_CALLFB, a, b, c, .. } => {
                    if b == 0 && !top.take().is_some_and(|base| base > a) { return Err(error()); }
                    if c == 0 { top = Some(a); }
                }
                Instruction::BC { op_code: LOP_GETVARARGS, a, b, .. } => {
                    if !function.is_vararg { return Err(error()); }
                    if b == 0 { top = Some(a); }
                }
                Instruction::BC { op_code: LOP_RETURN, a, b, .. } => {
                    if b == 0 && !top.take().is_some_and(|base| base >= a) { return Err(error()); }
                    top = None;
                }
                Instruction::BC { op_code: LOP_SETLIST, b, c: 0, .. } => {
                    if !top.take().is_some_and(|base| base >= b) { return Err(error()); }
                }
                Instruction::BC { op_code: LOP_LOADB, c, .. } if c != 0 => top = None,
                Instruction::AD { op_code: LOP_JUMP | LOP_JUMPBACK | LOP_JUMPIF | LOP_JUMPIFNOT
                    | LOP_JUMPIFEQ | LOP_JUMPIFLE | LOP_JUMPIFLT | LOP_JUMPIFNOTEQ | LOP_JUMPIFNOTLE
                    | LOP_JUMPIFNOTLT | LOP_JUMPXEQKNIL | LOP_JUMPXEQKB | LOP_JUMPXEQKN | LOP_JUMPXEQKS
                    | LOP_FORNPREP | LOP_FORNLOOP | LOP_FORGPREP | LOP_FORGPREP_INEXT | LOP_FORGPREP_NEXT
                    | LOP_FORGLOOP, .. } | Instruction::E { op_code: LOP_JUMPX, .. } => top = None,
                _ => {}
            }
        }
        // The lifter reads a closure with its CAPTURE list, and NAMECALL with the
        // CALL that uses it, as single units. Compilers always emit them intact.
        let mut captured = vec![false; code.len()];
        for pc in (0..code.len()).filter(|pc| starts[*pc]) {
            let error = |what: &str| format!("prototype {id}, pc {pc}: invalid {what}");
            match code[pc] {
                Instruction::AD {
                    op_code: op_code @ (LOP_NEWCLOSURE | LOP_DUPCLOSURE),
                    d,
                    ..
                } => {
                    let child = if op_code == LOP_NEWCLOSURE {
                        function.functions.get(d as usize).copied()
                    } else {
                        match function.constants.get(d as usize) {
                            Some(&Constant::Closure(child)) => Some(child),
                            _ => None,
                        }
                    };
                    let child = child
                        .and_then(|child| chunk.functions.get(child))
                        .ok_or_else(|| error("closure prototype"))?;
                    for at in pc + 1..=pc + usize::from(child.num_upvalues) {
                        match code.get(at) {
                            Some(Instruction::BC { op_code: LOP_CAPTURE, .. }) if !targets[at] => {
                                captured[at] = true;
                            }
                            _ => return Err(error("closure capture list")),
                        }
                    }
                }
                Instruction::BC {
                    op_code: LOP_NAMECALL | LOP_NAMECALLUDATA,
                    a,
                    ..
                } => match code.get(pc + 2) {
                    Some(&Instruction::BC {
                        op_code: LOP_CALL | LOP_CALLFB,
                        a: call_base,
                        ..
                    }) if call_base == a && !targets[pc + 2] => {}
                    _ => return Err(error("method call")),
                },
                Instruction::BC { op_code: LOP_CAPTURE, .. } if !captured[pc] => {
                    return Err(error("capture outside a closure"));
                }
                _ => {}
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
