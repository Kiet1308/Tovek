//! Preserve prospective scalar helper binders before parallel child lifting.
//! The later AST matcher independently proves every accepted reconstruction.
use crate::{deserializer::function::Function, instruction::Instruction, op_code::OpCode};

pub(crate) fn retain(function: &Function, name: Option<&str>) -> bool {
    name.is_some_and(|name| name.len() <= 256 && ast::valid_source_name(name))
        && !function.is_vararg
        && function.num_upvalues == 0
        && (1..=8).contains(&function.num_parameters)
        && scalar_body(&function.instructions)
}

/// For each prototype, whether line info shows its code inside another
/// prototype: Luau `-O2` inlined it there. A prototype's lines run from its
/// `linedefined` to the last line its own code has; a line belongs to the
/// innermost such span. A PC of one prototype on a line of another one's
/// span is that other one's code. On the first or last line of the span,
/// which the definer's own code may share (`t.f = function() ... end, g =
/// 1`, `task.spawn(function() ... end)`), it counts only as an operation the
/// function has itself, never as the creation, a move or a store of a
/// value, nor within an enclosing function's run of PCs on that line that
/// creates a closure. This only lets the SSA inliner keep a function's
/// binder for
/// the de-inliner ([`ast::Function::inlined_by_compiler`]); the de-inliner
/// still proves every call it rebuilds. Without line info nothing is.
/// O(lines spanned + PCs).
pub(crate) fn inlined_prototypes(functions: &[Function], lines: &[Vec<Option<u32>>]) -> Vec<bool> {
    let mut inlined = vec![false; functions.len()];
    let spans: Vec<Option<(u32, u32)>> = functions
        .iter()
        .zip(lines)
        .map(|(function, lines)| {
            let start = u32::try_from(function.line_defined).ok().filter(|&line| line != 0)?;
            let end = lines.iter().flatten().copied().max()?.max(start);
            Some((start, end))
        })
        .collect();
    let Some(last) = spans.iter().flatten().map(|&(_, end)| end).max() else { return inlined };
    // Pathological line numbers would make the owner table large; such
    // input keeps every binder as before.
    if last as usize > 4 * 1024 * 1024 {
        return inlined;
    }
    let opcode_of = |instruction: &Instruction| match instruction {
        Instruction::BC { op_code, .. } | Instruction::AD { op_code, .. } | Instruction::E { op_code, .. } => *op_code,
    };
    // What a definer may write on those lines: the creation of the
    // function, moves and stores, its return.
    let shared = |opcode: OpCode| {
        matches!(
            opcode,
            OpCode::LOP_NEWCLOSURE | OpCode::LOP_DUPCLOSURE | OpCode::LOP_CAPTURE | OpCode::LOP_MOVE | OpCode::LOP_NOP
                | OpCode::LOP_SETTABLEKS | OpCode::LOP_SETTABLE | OpCode::LOP_SETTABLEN | OpCode::LOP_SETGLOBAL
                | OpCode::LOP_SETUPVAL | OpCode::LOP_RETURN | OpCode::LOP_PREPVARARGS | OpCode::LOP_COVERAGE
        )
    };
    // The operations each function has itself, one bit per opcode, read
    // where a first or last line needs them.
    let mut own: Vec<Option<[u64; 4]>> = vec![None; functions.len()];
    let mut owner = vec![u32::MAX; last as usize + 1];
    let mut order: Vec<usize> = (0..functions.len()).filter(|&p| spans[p].is_some()).collect();
    // Outer spans first, so an inner span overwrites the lines it holds.
    order.sort_by_key(|&p| {
        let (start, end) = spans[p].unwrap();
        (std::cmp::Reverse(end - start), p)
    });
    for &p in &order {
        let (start, end) = spans[p].unwrap();
        owner[start as usize..=end as usize].fill(p as u32);
    }
    for (caller, lines) in lines.iter().enumerate() {
        let instructions = &functions[caller].instructions;
        // For each PC, whether the run of consecutive PCs on its line
        // creates a closure: read once, where a first or last line needs it.
        let mut creating: Option<Vec<bool>> = None;
        let mut creating_at = |pc: usize| {
            creating.get_or_insert_with(|| {
                let mut creating = vec![false; lines.len()];
                let mut run = 0;
                while run < lines.len() {
                    let mut next = run + 1;
                    while next < lines.len() && lines[next] == lines[run] {
                        next += 1;
                    }
                    let creates = instructions.get(run..next).is_some_and(|run| {
                        run.iter().any(|instruction| matches!(opcode_of(instruction), OpCode::LOP_NEWCLOSURE | OpCode::LOP_DUPCLOSURE))
                    });
                    creating[run..next].fill(creates);
                    run = next;
                }
                creating
            })[pc]
        };
        for (pc, line) in lines.iter().enumerate() {
            let Some(line) = *line else { continue };
            let Some(&helper) = owner.get(line as usize) else { continue };
            if helper == u32::MAX || helper as usize == caller || inlined[helper as usize] {
                continue;
            }
            let helper = helper as usize;
            let Some((start, end)) = spans[helper] else { continue };
            let own_code = (start < line && line < end) || {
                let Some(opcode) = instructions.get(pc).map(opcode_of) else { continue };
                let bit = opcode as usize & 255;
                let bits = own[helper].get_or_insert_with(|| {
                    let mut bits = [0u64; 4];
                    for instruction in &functions[helper].instructions {
                        let opcode = opcode_of(instruction) as usize & 255;
                        bits[opcode / 64] |= 1 << (opcode % 64);
                    }
                    bits
                });
                let encloses = spans[caller].is_some_and(|(outer_start, outer_end)| outer_start <= start && end <= outer_end);
                !shared(opcode) && bits[bit / 64] & (1 << (bit % 64)) != 0 && !(encloses && creating_at(pc))
            };
            if own_code {
                if ast::env_flag!("MEDAL_TRACE_INLINED") {
                    eprintln!("INLINED helper=p{helper} start={start} end={end} caller=p{caller} pc={pc} line={line}");
                }
                inlined[helper] = true;
            }
        }
    }
    inlined
}

fn scalar_body(instructions: &[Instruction]) -> bool {
    use OpCode::*;
    if instructions.len() > 128 { return false; }
    let mut operations = 0;
    let mut returns = 0;
    for instruction in instructions {
        let opcode = match instruction {
            Instruction::BC { op_code, .. } | Instruction::AD { op_code, .. }
            | Instruction::E { op_code, .. } => *op_code,
        };
        match opcode {
            LOP_RETURN => {
                if !matches!(instruction, Instruction::BC { b: 2, .. }) { return false; }
                returns += 1;
            }
            LOP_ADD | LOP_SUB | LOP_MUL | LOP_DIV | LOP_MOD | LOP_POW
            | LOP_ADDK | LOP_SUBK | LOP_MULK | LOP_DIVK | LOP_MODK | LOP_POWK
            | LOP_SUBRK | LOP_DIVRK | LOP_IDIV | LOP_IDIVK
            | LOP_AND | LOP_OR | LOP_ANDK | LOP_ORK | LOP_NOT | LOP_MINUS
            | LOP_JUMPIFEQ | LOP_JUMPIFLE | LOP_JUMPIFLT | LOP_JUMPIFNOTEQ
            | LOP_JUMPIFNOTLE | LOP_JUMPIFNOTLT | LOP_JUMPXEQKNIL
            | LOP_JUMPXEQKB | LOP_JUMPXEQKN | LOP_JUMPXEQKS => operations += 1,
            LOP_NOP | LOP_LOADNIL | LOP_LOADB | LOP_LOADN | LOP_LOADK
            | LOP_LOADKX | LOP_MOVE | LOP_JUMP | LOP_JUMPIF | LOP_JUMPIFNOT
            | LOP_JUMPX | LOP_COVERAGE => {}
            _ => return false,
        }
    }
    returns > 0 && operations >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bc(op_code: OpCode, b: u8) -> Instruction {
        Instruction::BC { op_code, a: 0, b, c: 0, aux: 0 }
    }

    fn prototype(line_defined: usize, opcodes: &[OpCode]) -> Function {
        Function { max_stack_size: 8, num_parameters: 1, num_upvalues: 0, is_vararg: false, flags: 0,
            instructions: opcodes.iter().map(|&op_code| bc(op_code, 0)).collect(), constants: vec![], functions: vec![],
            line_defined, function_name: 0, line_gap_log2: None, line_info_delta: None, abs_line_info_delta: None,
            has_debug_info: false, debug_locals: vec![], debug_upvalue_name_indices: vec![], type_info: None }
    }

    #[test]
    fn line_info_shows_a_function_inlined_into_another() {
        use OpCode::*;
        // p0, lines 1-3: `local function f(x) local y = math.pi * x; return y + 1 end`.
        let helper = || prototype(1, &[LOP_GETIMPORT, LOP_ADDK, LOP_RETURN]);
        // p1 at line 5 runs p0's lines 2-3 (an inlined copy) between its own.
        let caller = || prototype(5, &[LOP_GETIMPORT, LOP_GETIMPORT, LOP_ADDK, LOP_RETURN]);
        // p2, the chunk, creates p0 on line 1, in one run with an import of
        // its own (`task.spawn(function(x) ... end)`).
        let chunk = || prototype(1, &[LOP_GETIMPORT, LOP_NEWCLOSURE, LOP_CALL, LOP_RETURN]);
        let lines = vec![
            vec![Some(1), Some(2), Some(3)],
            vec![Some(6), Some(2), Some(3), Some(7)],
            vec![Some(1), Some(1), Some(1), Some(8)],
        ];
        assert_eq!(inlined_prototypes(&[helper(), caller(), chunk()], &lines), vec![true, false, false]);
        // Without the copy, the definer's run on line 1 shows nothing.
        let lines = vec![lines[0].clone(), vec![Some(6), Some(6), Some(7), Some(7)], lines[2].clone()];
        assert_eq!(inlined_prototypes(&[helper(), caller(), chunk()], &lines), vec![false, false, false]);
        // No line info, no evidence.
        assert_eq!(inlined_prototypes(&[helper(), chunk()], &[vec![], vec![]]), vec![false, false]);
    }

    #[test]
    fn scalar_candidate_requires_cost_and_fixed_return_arity() {
        use OpCode::*;
        let mut body = vec![bc(LOP_MULK, 0), bc(LOP_ADDK, 0), bc(LOP_MULK, 0), bc(LOP_RETURN, 2)];
        assert!(scalar_body(&body));
        for arity in [0, 1, 3] {
            body[3] = bc(LOP_RETURN, arity);
            assert!(!scalar_body(&body));
        }
        assert!(!scalar_body(&[bc(LOP_ADDK, 0), bc(LOP_RETURN, 2)]));
    }

    #[test]
    fn calls_captures_storage_loops_and_budget_refuse() {
        use OpCode::*;
        let base = vec![bc(LOP_MULK, 0), bc(LOP_ADDK, 0), bc(LOP_MULK, 0), bc(LOP_RETURN, 2)];
        for opcode in [LOP_CALL, LOP_GETUPVAL, LOP_GETTABLE, LOP_SETGLOBAL, LOP_NEWCLOSURE, LOP_FORNLOOP, LOP_JUMPBACK] {
            let mut body = base.clone();
            body.insert(0, bc(opcode, 0));
            assert!(!scalar_body(&body), "{opcode:?}");
        }
        let mut body = base;
        body.resize(129, bc(LOP_NOP, 0));
        assert!(!scalar_body(&body));
    }
}
