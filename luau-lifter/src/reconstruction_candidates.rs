//! Preserve prospective scalar helper binders before parallel child lifting.
//! The later AST matcher independently proves every accepted reconstruction.
use crate::{deserializer::function::Function, instruction::Instruction, op_code::OpCode};

pub(crate) fn retain(function: &Function, name: Option<&str>) -> bool {
    name.is_some_and(|name| name.len() <= 256 && ast::valid_source_name(name))
        && !function.is_vararg
        && function.num_upvalues == 0
        && function.num_parameters >= 1
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

/// The inlined copies line info shows (plan E1, [`ast::deinline::evidence`]):
/// for each caller prototype, how many copies of each helper prototype its
/// code holds. A helper is every prototype with line info but the main one;
/// its span runs from its `linedefined` to the last line its code has.
///
/// Each PC of a caller is the innermost helper span holding its line, but
/// never the caller's own or one around it (the caller's own code), nor the
/// helper whose definition the caller writes on that helper's first line
/// (the closure creation, its captures and the store or move of it). A copy
/// starts at such a PC and goes on while the lines are lines the helper's
/// code has (a helper inlined inside it continues it), until a copy that
/// reached the helper's last line starts again at its first line (two copies
/// back to back). A helper whose code has one line only restarts where the
/// first operation of its own code comes again after the copy went past it
/// (two copies back to back again). Copies that
/// merge undercount, and split ones overcount, which only refuses matches.
/// `None` without line info. O(lines spanned + PCs · nesting depth).
pub(crate) fn inlined_copies(functions: &[Function], lines: &[Vec<Option<u32>>], main: usize) -> Option<ast::deinline::evidence::Copies> {
    use OpCode::*;
    let opcode_of = |instruction: &Instruction| match instruction {
        Instruction::BC { op_code, .. } | Instruction::AD { op_code, .. } | Instruction::E { op_code, .. } => *op_code,
    };
    // Each helper's span, its code's lines (sorted, distinct) and the first
    // of them inside the span, where a copy restarts; for a helper whose
    // code inside its span is one line, the operations on it in order.
    struct Helper {
        span: (u32, u32),
        lines: Vec<u32>,
        first: u32,
        single: Option<Vec<u8>>,
    }
    // An operation, by kind: the constant and register forms alike, a jump
    // or move none (an inlined copy adds and drops those).
    let kind = |opcode: OpCode| -> Option<u8> {
        Some(match opcode {
            LOP_ADD | LOP_ADDK => 1,
            LOP_SUB | LOP_SUBK | LOP_SUBRK => 2,
            LOP_MUL | LOP_MULK => 3,
            LOP_DIV | LOP_DIVK | LOP_DIVRK => 4,
            LOP_MOD | LOP_MODK => 5,
            LOP_POW | LOP_POWK => 6,
            LOP_IDIV | LOP_IDIVK => 7,
            LOP_AND | LOP_ANDK => 8,
            LOP_OR | LOP_ORK => 9,
            LOP_LOADNIL | LOP_LOADB | LOP_LOADN | LOP_LOADK | LOP_LOADKX => 10,
            LOP_JUMPIFEQ | LOP_JUMPIFNOTEQ | LOP_JUMPIFLE | LOP_JUMPIFNOTLE | LOP_JUMPIFLT | LOP_JUMPIFNOTLT | LOP_JUMPXEQKNIL
            | LOP_JUMPXEQKB | LOP_JUMPXEQKN | LOP_JUMPXEQKS => 11,
            LOP_JUMPIF | LOP_JUMPIFNOT => 12,
            LOP_GETTABLE | LOP_GETTABLEKS | LOP_GETTABLEN => 13,
            LOP_SETTABLE | LOP_SETTABLEKS | LOP_SETTABLEN => 14,
            LOP_CALL | LOP_CALLFB => 15,
            LOP_MOVE | LOP_JUMP | LOP_JUMPBACK | LOP_JUMPX | LOP_NOP | LOP_COVERAGE | LOP_RETURN | LOP_FASTCALL
            | LOP_FASTCALL1 | LOP_FASTCALL2 | LOP_FASTCALL2K | LOP_FASTCALL3 => return None,
            other => 16 + (other as u8 & 127),
        })
    };
    let mut helpers: Vec<Option<Helper>> = Vec::with_capacity(functions.len());
    for (id, (function, pcs)) in functions.iter().zip(lines).enumerate() {
        let mut own: Vec<u32> = pcs.iter().flatten().copied().collect();
        if id == main || own.is_empty() {
            helpers.push(None);
            continue;
        }
        own.sort_unstable();
        own.dedup();
        let start = u32::try_from(function.line_defined).ok()?;
        let end = *own.last()?;
        let first = own.iter().copied().find(|&line| start <= line && line <= end).unwrap_or(start);
        let single = (first == end).then(|| {
            let mut operations = Vec::new();
            let mut pc = 0;
            while let Some(instruction) = function.instructions.get(pc) {
                let opcode = opcode_of(instruction);
                if pcs.get(pc).copied().flatten() == Some(first) && let Some(kind) = kind(opcode) {
                    operations.push(kind);
                }
                pc += if opcode.has_aux() { 2 } else { 1 };
            }
            operations
        });
        helpers.push(Some(Helper { span: (start, end), lines: own, first, single }));
    }
    let last = helpers.iter().flatten().map(|helper| helper.span.1).max()?;
    // Every line's helpers, innermost first (the narrowest span, then the
    // lowest prototype), as one flat table indexed by line.
    let mut starts = vec![0u32; last as usize + 2];
    let mut spanned = 0usize;
    for helper in helpers.iter().flatten() {
        let (start, end) = helper.span;
        if start > end {
            continue;
        }
        spanned += (end - start + 1) as usize;
        // Pathological spans would make the table large: no evidence then.
        if spanned > 8 * 1024 * 1024 {
            return None;
        }
        for line in start..=end {
            starts[line as usize + 1] += 1;
        }
    }
    for line in 1..starts.len() {
        starts[line] += starts[line - 1];
    }
    let mut owners = vec![0u32; spanned];
    let mut filled = starts.clone();
    for (id, helper) in helpers.iter().enumerate() {
        let Some(helper) = helper else { continue };
        let (start, end) = helper.span;
        if start > end {
            continue;
        }
        for line in start..=end {
            owners[filled[line as usize] as usize] = id as u32;
            filled[line as usize] += 1;
        }
    }
    let width = |id: u32| {
        let (start, end) = helpers[id as usize].as_ref().unwrap().span;
        end - start
    };
    for line in 0..=last as usize {
        owners[starts[line] as usize..starts[line + 1] as usize].sort_unstable_by_key(|&id| (width(id), id));
    }
    let helper_at = |line: u32| -> &[u32] {
        match starts.get(line as usize..line as usize + 2) {
            Some(&[from, to]) => &owners[from as usize..to as usize],
            _ => &[],
        }
    };
    let definition = |opcode: OpCode| {
        matches!(opcode, LOP_NEWCLOSURE | LOP_DUPCLOSURE | LOP_CAPTURE | LOP_PREPVARARGS | LOP_SETTABLEKS | LOP_SETGLOBAL | LOP_SETUPVAL | LOP_MOVE)
    };
    let mut copies = ast::deinline::evidence::Copies::new(main);
    for (caller, pcs) in lines.iter().enumerate() {
        if pcs.is_empty() {
            continue;
        }
        let caller_id = caller as u32;
        let own = helpers[caller].as_ref().map(|helper| helper.span);
        let instructions = &functions[caller].instructions;
        // For each PC, whether the run of consecutive PCs on its line creates
        // a closure (the statement defining a function there): read once,
        // where a helper's first line needs it.
        let creating: std::cell::OnceCell<Vec<bool>> = std::cell::OnceCell::new();
        let creating_at = |pc: usize| {
            creating.get_or_init(|| {
                let mut creating = vec![false; pcs.len()];
                let mut run = 0;
                while run < pcs.len() {
                    let mut next = run + 1;
                    while next < pcs.len() && pcs[next] == pcs[run] {
                        next += 1;
                    }
                    let creates = instructions.get(run..next).is_some_and(|run| {
                        run.iter().any(|instruction| matches!(opcode_of(instruction), LOP_NEWCLOSURE | LOP_DUPCLOSURE))
                    });
                    creating[run..next].fill(creates);
                    run = next;
                }
                creating
            })[pc]
        };
        // The innermost helper whose code this PC of the caller is. On a
        // helper's first line, a caller around it may have the statement
        // defining it (`self.conn = signal:Connect(function() ... end)`).
        let innermost = |line: u32, opcode: OpCode, pc: usize| {
            helper_at(line).iter().copied().find(|&id| {
                let (start, end) = helpers[id as usize].as_ref().unwrap().span;
                let defines = own.is_none_or(|(own_start, _)| own_start <= start) && line == start;
                id != caller_id
                    && !own.is_some_and(|(own_start, own_end)| start <= own_start && own_end <= end)
                    && !(defines && (definition(opcode) || creating_at(pc)))
            })
        };
        // The copy going on: its helper, the last of its lines reached, and
        // for a one-line helper, how many of its operations the copy passed.
        let mut current: Option<(u32, u32, usize)> = None;
        // Where a one-line helper's copy goes on with the operation at `pc`
        // on its line: the operations it passed then, or `None` where the
        // copy went past the helper's first operation and that operation
        // comes again, on no value the instruction before it made, followed
        // by the helper's second one if it has one (a new copy). Any other
        // operation is the copy's or the code using its value, which Luau
        // leaves on the helper's line (`if active(x) then`, `(c and ratio(a,
        // b) or ratio(b, a)) + 1`).
        let advance = |single: &[u8], passed: usize, pc: usize| -> Option<usize> {
            let Some(here) = instructions.get(pc).and_then(|instruction| kind(opcode_of(instruction))) else { return Some(passed) };
            let passed = passed.min(single.len());
            match single[passed..].iter().position(|&own| own == here) {
                Some(at) => Some(passed + at + 1),
                None if passed > 0 && single[0] == here && reads_previous_result(instructions, pc) => Some(passed),
                None if passed > 0 && single.len() == 1 && single[0] == here => None,
                None if passed > 0 && single.len() >= 2 && single[0] == here => {
                    // The next operation on this line.
                    let mut next = pc + if opcode_of(&instructions[pc]).has_aux() { 2 } else { 1 };
                    let second = loop {
                        let Some(instruction) = instructions.get(next) else { break None };
                        if pcs.get(next).copied().flatten() != pcs[pc] {
                            break None;
                        }
                        let opcode = opcode_of(instruction);
                        if let Some(kind) = kind(opcode) {
                            break Some(kind);
                        }
                        next += if opcode.has_aux() { 2 } else { 1 };
                    };
                    if second == Some(single[1]) { None } else { Some(passed) }
                }
                None => Some(passed),
            }
        };
        let mut pc = 0;
        while pc < pcs.len() {
            let Some(instruction) = instructions.get(pc) else { break };
            let opcode = opcode_of(instruction);
            let step = if opcode.has_aux() { 2 } else { 1 };
            let Some(line) = pcs[pc] else {
                current = None;
                pc += step;
                continue;
            };
            if let Some((helper, reached, passed)) = &mut current {
                let copy = helpers[*helper as usize].as_ref().unwrap();
                if copy.lines.binary_search(&line).is_ok() {
                    let (start, end) = copy.span;
                    let inside = start <= line && line <= end;
                    let mut restarts = inside && line == copy.first && *reached == end && copy.first != end;
                    if let Some(single) = &copy.single
                        && line == copy.first
                    {
                        match advance(single, *passed, pc) {
                            Some(next) => *passed = next,
                            None => restarts = true,
                        }
                    }
                    if !restarts {
                        if inside {
                            *reached = line;
                        }
                        if let Some(inner) = innermost(line, opcode, pc).filter(|inner| inner != helper) {
                            // A helper inlined inside this copy.
                            copies.present.insert((caller_id, inner));
                            copies.nested.insert((caller_id, *helper, inner));
                        }
                        pc += step;
                        continue;
                    }
                }
            }
            current = innermost(line, opcode, pc).map(|helper| {
                if ast::env_flag!("MEDAL_TRACE_COPIES") {
                    eprintln!("COPY caller=p{caller_id} helper=p{helper} pc={pc} line={line} op={opcode:?}");
                }
                *copies.copies.entry((caller_id, helper)).or_default() += 1;
                copies.present.insert((caller_id, helper));
                let copy = helpers[helper as usize].as_ref().unwrap();
                let passed = match &copy.single {
                    Some(single) if line == copy.first => advance(single, 0, pc).unwrap_or(0),
                    _ => 0,
                };
                (helper, line, passed)
            });
            pc += step;
        }
    }
    Some(copies)
}

/// Whether the instruction at `pc` reads the register the instruction right
/// before it wrote: it uses that value rather than starting anew. Only the
/// operations a one-line helper's copy and the code using its value are
/// made of are told apart; any other says no.
fn reads_previous_result(instructions: &[Instruction], pc: usize) -> bool {
    use OpCode::*;
    let previous = match pc.checked_sub(1).and_then(|before| instructions.get(before)) {
        // The word before is the auxiliary word of the instruction before it.
        Some(Instruction::BC { op_code: LOP_NOP, .. }) => pc.checked_sub(2).and_then(|before| instructions.get(before)),
        other => other,
    };
    let written = match previous {
        Some(Instruction::BC { op_code, a, .. } | Instruction::AD { op_code, a, .. })
            if !matches!(
                op_code,
                LOP_JUMP | LOP_JUMPBACK | LOP_JUMPIF | LOP_JUMPIFNOT | LOP_JUMPIFEQ | LOP_JUMPIFNOTEQ | LOP_JUMPIFLE
                    | LOP_JUMPIFNOTLE | LOP_JUMPIFLT | LOP_JUMPIFNOTLT | LOP_JUMPXEQKNIL | LOP_JUMPXEQKB | LOP_JUMPXEQKN
                    | LOP_JUMPXEQKS | LOP_SETTABLE | LOP_SETTABLEKS | LOP_SETTABLEN | LOP_SETGLOBAL | LOP_SETUPVAL
                    | LOP_RETURN | LOP_NOP | LOP_COVERAGE
            ) =>
        {
            *a
        }
        _ => return false,
    };
    match instructions.get(pc) {
        Some(Instruction::BC { op_code, a, b, c, .. }) => match op_code {
            LOP_ADD | LOP_SUB | LOP_MUL | LOP_DIV | LOP_MOD | LOP_POW | LOP_IDIV | LOP_AND | LOP_OR => *b == written || *c == written,
            LOP_ADDK | LOP_SUBK | LOP_MULK | LOP_DIVK | LOP_MODK | LOP_POWK | LOP_IDIVK | LOP_ANDK | LOP_ORK | LOP_NOT | LOP_MINUS
            | LOP_LENGTH | LOP_GETTABLEKS | LOP_GETTABLEN | LOP_NAMECALL => *b == written,
            LOP_SUBRK | LOP_DIVRK => *c == written,
            LOP_GETTABLE => *b == written || *c == written,
            LOP_CALL | LOP_CALLFB => {
                let last = if *b == 0 { u8::MAX } else { a.saturating_add(*b - 1) };
                (*a..=last).contains(&written)
            }
            LOP_CONCAT => (*b..=*c).contains(&written),
            _ => false,
        },
        Some(Instruction::AD { op_code, a, aux, .. }) => match op_code {
            LOP_JUMPIF | LOP_JUMPIFNOT | LOP_JUMPXEQKNIL | LOP_JUMPXEQKB | LOP_JUMPXEQKN | LOP_JUMPXEQKS => *a == written,
            LOP_JUMPIFEQ | LOP_JUMPIFNOTEQ | LOP_JUMPIFLE | LOP_JUMPIFNOTLE | LOP_JUMPIFLT | LOP_JUMPIFNOTLT => {
                *a == written || *aux == written as u32
            }
            _ => false,
        },
        _ => false,
    }
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
    fn copies_are_counted_per_caller_and_never_in_the_statement_defining_a_helper() {
        use OpCode::*;
        // p0, lines 1-3: `local function f(c) if c then c() end end`.
        let helper = prototype(1, &[LOP_JUMPIFNOT, LOP_CALL, LOP_RETURN]);
        // p1 at line 5 runs two copies of p0 (line 2), its own lines between.
        let caller = prototype(5, &[LOP_GETUPVAL, LOP_JUMPIFNOT, LOP_CALL, LOP_GETUPVAL, LOP_JUMPIFNOT, LOP_CALL, LOP_RETURN]);
        // p2, the chunk, defines p0 on line 1 and p1 on line 5, in runs that
        // create closures (`t.on = function(c) ... end`).
        let chunk = prototype(1, &[LOP_GETUPVAL, LOP_NEWCLOSURE, LOP_MOVE, LOP_GETUPVAL, LOP_NEWCLOSURE, LOP_RETURN]);
        let lines = vec![
            vec![Some(2), Some(2), Some(3)],
            vec![Some(6), Some(2), Some(2), Some(7), Some(2), Some(2), Some(8)],
            vec![Some(1), Some(1), Some(1), Some(5), Some(5), Some(9)],
        ];
        let copies = inlined_copies(&[helper, caller, chunk], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 2);
        assert_eq!(copies.copies(None, 0), 0);
        assert_eq!(copies.copies(None, 1), 0);
        assert!(copies.present(Some(1), 0) && !copies.present(None, 0));
        // No line info, no evidence.
        assert!(inlined_copies(&[prototype(1, &[LOP_RETURN]), prototype(1, &[LOP_RETURN])], &[vec![], vec![]], 1).is_none());
    }

    #[test]
    fn copies_of_a_one_line_helper_back_to_back_are_told_apart_from_the_code_using_them() {
        use OpCode::*;
        let op = |op_code, a, b, c| Instruction::BC { op_code, a, b, c, aux: 0 };
        let with = |line_defined, instructions: Vec<Instruction>| Function { instructions, ..prototype(line_defined, &[]) };
        let chunk = || prototype(1, &[LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_RETURN]);
        // p0, lines 1-2: `local function lerp(a, b, t) return a + (b - a) * t end`, its code on line 2.
        let lerp = prototype(1, &[LOP_SUB, LOP_MUL, LOP_ADD, LOP_RETURN]);
        // p1 at line 4: two copies back to back on line 2 (the second with a
        // constant folded into its `MULK`), then a test of the value, which
        // Luau leaves on the helper's line (`if lerp(...) then`).
        let caller = with(4, vec![
            op(LOP_SUB, 4, 1, 0), op(LOP_MUL, 4, 4, 2), op(LOP_ADD, 3, 0, 4),
            op(LOP_SUB, 5, 1, 0), op(LOP_MULK, 5, 5, 0), op(LOP_ADD, 4, 0, 5),
            op(LOP_JUMPIFNOT, 4, 0, 0), op(LOP_RETURN, 0, 1, 0),
        ]);
        let lines = vec![
            vec![Some(2), Some(2), Some(2), Some(2)],
            vec![Some(2), Some(2), Some(2), Some(2), Some(2), Some(2), Some(2), Some(6)],
            vec![Some(1), Some(4), Some(7)],
        ];
        let copies = inlined_copies(&[lerp, caller, chunk()], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 2);
        // `(c and ratio(a, b) or ratio(b, a)) + 1`: the caller's `+ 1` is an
        // `ADDK` on the helper's line, the kind of the helper's first
        // operation, but on the value the copy before it made.
        let ratio = prototype(1, &[LOP_ADD, LOP_DIV, LOP_RETURN]);
        let caller = with(4, vec![
            op(LOP_JUMPIFNOT, 2, 0, 0), op(LOP_ADD, 5, 1, 0), op(LOP_DIV, 4, 0, 5), op(LOP_JUMPIF, 4, 0, 0),
            op(LOP_ADD, 5, 0, 1), op(LOP_DIV, 4, 1, 5), op(LOP_ADDK, 3, 4, 0), op(LOP_RETURN, 3, 2, 0),
        ]);
        let lines = vec![
            vec![Some(2), Some(2), Some(2)],
            vec![Some(5), Some(2), Some(2), Some(2), Some(2), Some(2), Some(2), Some(2)],
            vec![Some(1), Some(4), Some(7)],
        ];
        let copies = inlined_copies(&[ratio, caller, chunk()], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 2);
        // `{ one(n), one(n) }` for `one(x) return x * 2`: a one-operation
        // helper's copies back to back, each on the argument, split; one
        // copy on the other's value (`one(one(n))`) does not (it merges,
        // which only refuses).
        let one = prototype(1, &[LOP_MULK, LOP_RETURN]);
        let caller = with(4, vec![op(LOP_MULK, 2, 0, 0), op(LOP_MULK, 3, 0, 0), op(LOP_MULK, 4, 3, 0), op(LOP_RETURN, 2, 3, 0)]);
        let lines = vec![vec![Some(2), Some(2)], vec![Some(2), Some(2), Some(2), Some(5)], vec![Some(1), Some(4), Some(7)]];
        let copies = inlined_copies(&[one, caller, chunk()], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 2);
    }

    #[test]
    fn a_copy_inside_another_copy_goes_with_it() {
        use OpCode::*;
        // p0, lines 1-2: `inner`; p1, lines 4-7: `outer`, with a copy of p0
        // (line 2) in its code; p2 at line 9 runs a copy of p1 holding it.
        let inner = prototype(1, &[LOP_NOT, LOP_RETURN]);
        let outer = prototype(4, &[LOP_GETUPVAL, LOP_NOT, LOP_CALL, LOP_RETURN]);
        let caller = prototype(9, &[LOP_LOADN, LOP_GETUPVAL, LOP_NOT, LOP_CALL, LOP_RETURN]);
        let chunk = prototype(1, &[LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_RETURN]);
        let lines = vec![
            vec![Some(2), Some(2)],
            vec![Some(5), Some(2), Some(6), Some(7)],
            vec![Some(10), Some(5), Some(2), Some(6), Some(11)],
            vec![Some(1), Some(4), Some(9), Some(12)],
        ];
        let copies = inlined_copies(&[inner, outer, caller, chunk], &lines, 3).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 1);
        assert_eq!(copies.copies(Some(2), 1), 1);
        assert_eq!(copies.copies(Some(2), 0), 0);
        assert!(copies.present(Some(2), 0));
        assert!(copies.nested(Some(2), 1, 0));
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
