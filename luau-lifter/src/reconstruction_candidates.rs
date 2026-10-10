//! Preserve prospective scalar helper binders before parallel child lifting.
//! The later AST matcher independently proves every accepted reconstruction.
use crate::{deserializer::{constant::Constant, function::Function}, instruction::Instruction, op_code::OpCode};

/// The last line the tables indexed by line read: past it, line info is
/// pathological (no source has 4M lines; forged bytecode may name any line
/// up to 2^31) and gives no evidence.
const LINE_LIMIT: usize = 4 * 1024 * 1024;

/// The most lines all the helpers' spans and code ranges may hold together,
/// for the same reason.
const SPAN_LIMIT: usize = 8 * 1024 * 1024;

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
    if last as usize > LINE_LIMIT {
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
        lines: LineSet,
        first: u32,
        /// The line a copy ends on: the line of the helper's last
        /// instruction but its return, which a copy never has (a void
        /// helper's `RETURN` on its `end` line).
        last: u32,
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
    // Every table below is indexed by line, or holds one bit per line of a
    // helper's code: pathological lines (forged line info) give no evidence
    // rather than tables as large as the lines they name.
    let mut code_lines = 0usize;
    for (id, (function, pcs)) in functions.iter().zip(lines).enumerate() {
        let Some((min, max)) = line_range(pcs) else { continue };
        if max as usize > LINE_LIMIT || function.line_defined > LINE_LIMIT {
            return None;
        }
        if id != main {
            code_lines += (max - min) as usize + 1;
            if code_lines > SPAN_LIMIT {
                return None;
            }
        }
    }
    let mut helpers: Vec<Option<Helper>> = Vec::with_capacity(functions.len());
    for (id, (function, pcs)) in functions.iter().zip(lines).enumerate() {
        if id == main {
            helpers.push(None);
            continue;
        }
        let Some(own) = LineSet::new(pcs) else {
            helpers.push(None);
            continue;
        };
        let start = u32::try_from(function.line_defined).ok()?;
        // A function whose code is all copies of helpers defined before it
        // (`return function(k) return frames(k) end`) has no line of its
        // own at or after its first: its span is that line alone.
        let end = own.max().max(start);
        let first = (start..=end).find(|&line| own.contains(line)).unwrap_or(start);
        let last = function
            .instructions
            .iter()
            .zip(pcs)
            .rev()
            .filter(|(instruction, _)| opcode_of(instruction) != LOP_RETURN)
            .find_map(|(_, line)| line.filter(|&line| line >= first))
            .unwrap_or(end);
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
        helpers.push(Some(Helper { span: (start, end), lines: own, first, last, single }));
    }
    // The helpers whose fully folded copies are counted (plan E2): one line
    // of arithmetic on parameters returning one value, and no constant load
    // of their own, so a number constant loaded on that line in a caller is
    // a whole copy whose computation the compiler folded.
    let folds: Vec<bool> = functions
        .iter()
        .zip(&helpers)
        .map(|(function, helper)| helper.as_ref().is_some_and(|helper| helper.single.is_some()) && folding_helper(function))
        .collect();
    let last = helpers.iter().flatten().map(|helper| helper.span.1).max()?;
    // Pathological spans would make the table large: no evidence then.
    let spanned: usize = helpers.iter().flatten().map(|helper| (helper.span.1 - helper.span.0) as usize + 1).sum();
    if spanned > SPAN_LIMIT {
        return None;
    }
    // Every line's helpers, innermost first (the narrowest span, then the
    // lowest prototype), as one flat table indexed by line.
    let mut starts = vec![0u32; last as usize + 2];
    for helper in helpers.iter().flatten() {
        let (start, end) = helper.span;
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
        for line in start..=end {
            owners[filled[line as usize] as usize] = id as u32;
            filled[line as usize] += 1;
        }
    }
    // Each helper's span, read where every PC needs one.
    let spans: Vec<(u32, u32)> = helpers.iter().map(|helper| helper.as_ref().map_or((u32::MAX, 0), |helper| helper.span)).collect();
    let width = |id: u32| {
        let (start, end) = spans[id as usize];
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
    // The callers with a fully folded copy or a constant inside a copy,
    // whose constant references are counted once every copy is.
    let mut folded_callers: Vec<usize> = Vec::new();
    for (caller, pcs) in lines.iter().enumerate() {
        if pcs.is_empty() {
            continue;
        }
        let caller_id = caller as u32;
        let own = helpers[caller].as_ref().map(|helper| helper.span);
        let instructions = &functions[caller].instructions;
        let constants = &functions[caller].constants;
        // A number constant loaded at `pc` on a line of `owner`, a helper
        // whose copies fold: a fully folded copy of it, the whole copy,
        // inside a copy of `around` if that is another helper. Whether it
        // is one.
        let mut folded_any = false;
        // The number constants the instruction at `pc` refers to, inside a
        // copy of `outer` (and of `inner`, the helper whose code it is,
        // where that copy is inside the other one).
        let mut copied_any = false;
        let mut copy_refs = |copies: &mut ast::deinline::evidence::Copies, outer: u32, inner: Option<u32>, pc: usize| {
            let Some(instruction) = instructions.get(pc) else { return };
            instruction_numbers(instruction, constants, &mut |value| {
                for helper in std::iter::once(outer).chain(inner.filter(|&inner| inner != outer)) {
                    *copies.copy_refs.entry((caller_id, helper, value.to_bits())).or_default() += 1;
                }
                copied_any = true;
            });
        };
        let mut fold_copy = |copies: &mut ast::deinline::evidence::Copies, owner: u32, around: u32, pc: usize| -> bool {
            if !folds[owner as usize] {
                return false;
            }
            let Some(value) = instructions.get(pc).and_then(|instruction| loaded_number(instruction, constants)) else { return false };
            let entry = copies.constant_copies.entry((caller_id, owner, value.to_bits())).or_default();
            if owner == around {
                entry.outermost += 1;
            } else {
                entry.nested += 1;
            }
            folded_any = true;
            true
        };
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
            let owners = helper_at(line);
            // The caller's own code: strictly inside its span, where only a
            // function nested in it, narrower, could come before it.
            if owners.first() == Some(&caller_id) && own.is_some_and(|(own_start, own_end)| own_start < line && line < own_end) {
                return None;
            }
            owners.iter().copied().find(|&id| {
                let (start, end) = spans[id as usize];
                let defines = own.is_none_or(|(own_start, _)| own_start <= start) && line == start;
                id != caller_id
                    && !own.is_some_and(|(own_start, own_end)| start <= own_start && own_end <= end)
                    && !(defines && (definition(opcode) || creating_at(pc)))
            })
        };
        // The answer for the line of the PC before, where it holds for any
        // PC on that line: no helper, or one not starting on it (the
        // definition rules read the operation only on a helper's first line).
        let mut last_line: Option<(u32, Option<u32>)> = None;
        // The copy going on: its helper, the last of its lines reached, and
        // for a one-line helper, how many of its operations the copy passed.
        let mut current: Option<(u32, u32, usize)> = None;
        // The copy going on is one load of a constant, folded whole: the
        // next operation on the helper's line is another copy.
        let mut complete = false;
        // Where a one-line helper's copy goes on with the operation at `pc`
        // on its line: the operations it passed then, or `None` where the
        // copy went past the helper's first operation and that operation
        // comes again, on no value the instruction before it made, followed
        // by another of the helper's operations if it has more (a new copy,
        // perhaps with some folded away). Any other
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
                    // Constant arguments may have folded operations away
                    // (`map(y, 50, 100, 0, 1)` loses `outMax - outMin`):
                    // any later operation of the helper will do.
                    if second.is_some_and(|second| single[1..].contains(&second)) { None } else { Some(passed) }
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
                if copy.lines.contains(line) {
                    let (start, end) = copy.span;
                    let inside = start <= line && line <= end;
                    let mut restarts = inside && line == copy.first && *reached == copy.last && copy.first != copy.last;
                    if let Some(single) = &copy.single
                        && line == copy.first
                    {
                        match advance(single, *passed, pc) {
                            Some(_) if complete && kind(opcode).is_some() => restarts = true,
                            Some(next) => *passed = next,
                            None => restarts = true,
                        }
                    }
                    if !restarts {
                        if inside {
                            *reached = line;
                        }
                        // A helper inlined inside this copy (the copy's own
                        // lines have the copy's helper innermost).
                        let mut nested_in = None;
                        if helper_at(line).first() != Some(helper) {
                            if let Some(inner) = innermost(line, opcode, pc).filter(|inner| inner != helper) {
                                copies.present.insert((caller_id, inner));
                                copies.nested.insert((caller_id, *helper, inner));
                                fold_copy(&mut copies, inner, *helper, pc);
                                nested_in = Some(inner);
                            }
                        } else if fold_copy(&mut copies, *helper, *helper, pc) {
                            // Another copy, folded whole, right after one
                            // on the helper's line (`f(frames(k), frames(2))`):
                            // the helper's code loads no constant of its own.
                            *copies.copies.entry((caller_id, *helper)).or_default() += 1;
                            complete = true;
                        }
                        copy_refs(&mut copies, *helper, nested_in, pc);
                        pc += step;
                        continue;
                    }
                }
            }
            let found = match last_line {
                Some((seen, found)) if seen == line => found,
                _ => {
                    let found = innermost(line, opcode, pc);
                    let reusable = helper_at(line).iter().all(|&id| spans[id as usize].0 != line);
                    last_line = reusable.then_some((line, found));
                    found
                }
            };
            current = found.map(|helper| {
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
            // A copy folded whole is that one load: an operation after it on
            // the helper's line is another copy (`f(frames(2), frames(k))`).
            complete = matches!(current, Some((helper, ..)) if fold_copy(&mut copies, helper, helper, pc));
            if let Some((helper, ..)) = current {
                copy_refs(&mut copies, helper, None, pc);
            }
            pc += step;
        }
        if folded_any || copied_any {
            folded_callers.push(caller);
        }
    }
    for caller in folded_callers {
        number_references(&functions[caller], |value| {
            *copies.constant_refs.entry((caller as u32, value.to_bits())).or_default() += 1;
        });
    }
    Some(copies)
}

/// Whether a helper's copies may fold whole to one constant (plan E2): a
/// fixed number of parameters, arithmetic on them (`n / 60`, `-(a + b)`;
/// `^` aside), and one value returned. Its own code then loads no
/// constant, so one loaded on its line in a caller is a whole copy.
fn folding_helper(function: &Function) -> bool {
    use OpCode::*;
    if function.is_vararg || function.num_parameters == 0 {
        return false;
    }
    let mut operations = 0;
    for instruction in &function.instructions {
        match instruction {
            Instruction::BC { op_code: LOP_RETURN, b: 2, .. } => {}
            Instruction::BC { op_code: LOP_MOVE | LOP_NOP, .. } | Instruction::AD { op_code: LOP_MOVE | LOP_NOP, .. } => {}
            Instruction::BC {
                op_code:
                    LOP_ADD | LOP_SUB | LOP_MUL | LOP_DIV | LOP_MOD | LOP_IDIV | LOP_ADDK | LOP_SUBK | LOP_MULK | LOP_DIVK
                    | LOP_MODK | LOP_IDIVK | LOP_SUBRK | LOP_DIVRK | LOP_MINUS,
                ..
            } => operations += 1,
            _ => return false,
        }
    }
    operations > 0
}

/// The number `instruction` loads into a register: `LOADN`, or `LOADK` and
/// `LOADKX` of a number constant.
fn loaded_number(instruction: &Instruction, constants: &[Constant]) -> Option<f64> {
    use OpCode::*;
    let index = match instruction {
        Instruction::AD { op_code: LOP_LOADN, d, .. } => return Some(f64::from(*d)),
        Instruction::AD { op_code: LOP_LOADK, d, .. } => usize::try_from(*d).ok()?,
        Instruction::BC { op_code: LOP_LOADKX, aux, .. } => *aux as usize,
        _ => return None,
    };
    match constants.get(index) {
        Some(Constant::Number(value)) => Some(*value),
        _ => None,
    }
}

/// Calls `found` with every number constant `function` refers to, once per
/// reference: loads (`LOADN`, `LOADK`, `LOADKX`), constant operands
/// (`ADDK`...`IDIVK`, `ANDK`, `ORK`, `SUBRK`, `DIVRK`), comparisons
/// (`JUMPXEQKN`), table indexes (`GETTABLEN`, `SETTABLEN`) and the values
/// of a `DUPTABLE` template. A `FASTCALL2K` constant is loaded again for
/// the call it stands before, and counted there.
fn number_references(function: &Function, mut found: impl FnMut(f64)) {
    for instruction in &function.instructions {
        instruction_numbers(instruction, &function.constants, &mut found);
    }
}

/// [`number_references`] of one instruction.
fn instruction_numbers(instruction: &Instruction, constants: &[Constant], found: &mut dyn FnMut(f64)) {
    use OpCode::*;
    let constant = |index: usize, found: &mut dyn FnMut(f64)| {
        if let Some(Constant::Number(value)) = constants.get(index) {
            found(*value);
        }
    };
    if let Some(value) = loaded_number(instruction, constants) {
        found(value);
        return;
    }
    {
        match instruction {
            Instruction::BC {
                op_code: LOP_ADDK | LOP_SUBK | LOP_MULK | LOP_DIVK | LOP_MODK | LOP_POWK | LOP_IDIVK | LOP_ANDK | LOP_ORK,
                c,
                ..
            } => constant(usize::from(*c), found),
            Instruction::BC { op_code: LOP_SUBRK | LOP_DIVRK, b, .. } => constant(usize::from(*b), found),
            Instruction::BC { op_code: LOP_GETTABLEN | LOP_SETTABLEN, c, .. } => found(f64::from(*c) + 1.0),
            Instruction::AD { op_code: LOP_JUMPXEQKN, aux, .. } => constant((*aux & 0x00FF_FFFF) as usize, found),
            Instruction::AD { op_code: LOP_DUPTABLE, d, .. } => {
                if let Ok(index) = usize::try_from(*d)
                    && let Some(Constant::TableWithConstants(entries)) = constants.get(index)
                {
                    for &(_, value) in entries {
                        if let Ok(value) = usize::try_from(value) {
                            constant(value, found);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// The least and the last line of a prototype's code, `None` without lines.
fn line_range(lines: &[Option<u32>]) -> Option<(u32, u32)> {
    lines.iter().flatten().fold(None, |range, &line| match range {
        None => Some((line, line)),
        Some((min, max)) => Some((min.min(line), max.max(line))),
    })
}

/// The lines a prototype's code has, one bit per line from the least.
struct LineSet {
    min: u32,
    bits: Vec<u64>,
}

impl LineSet {
    /// `None` without lines.
    fn new(lines: &[Option<u32>]) -> Option<LineSet> {
        let (min, max) = line_range(lines)?;
        let mut bits = vec![0u64; ((max - min) as usize >> 6) + 1];
        for &line in lines.iter().flatten() {
            let at = (line - min) as usize;
            bits[at >> 6] |= 1 << (at & 63);
        }
        Some(LineSet { min, bits })
    }

    fn contains(&self, line: u32) -> bool {
        line.checked_sub(self.min).is_some_and(|at| {
            let at = at as usize;
            self.bits.get(at >> 6).is_some_and(|word| word & (1 << (at & 63)) != 0)
        })
    }

    /// The last line.
    fn max(&self) -> u32 {
        let (word, bits) = self.bits.iter().enumerate().rev().find(|(_, bits)| **bits != 0).expect("a line");
        self.min + (word as u32) * 64 + (63 - bits.leading_zeros())
    }
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
    fn forged_lines_give_no_evidence_instead_of_tables_as_large_as_the_lines_they_name() {
        use OpCode::*;
        let helper = || prototype(1, &[LOP_JUMPIFNOT, LOP_CALL, LOP_RETURN]);
        let caller = || prototype(5, &[LOP_GETUPVAL, LOP_JUMPIFNOT, LOP_CALL, LOP_RETURN]);
        let chunk = || prototype(1, &[LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_RETURN]);
        let honest = vec![vec![Some(2), Some(2), Some(3)], vec![Some(6), Some(2), Some(2), Some(7)], vec![Some(1), Some(5), Some(9)]];
        assert_eq!(inlined_copies(&[helper(), caller(), chunk()], &honest, 2).unwrap().copies(Some(1), 0), 1);
        // A helper's last line, a caller's line, a definition line: each
        // forged to 2 billion, which the line tables would have to reach
        // (8 GB) or the helper's line bits span (250 MB).
        let mut last = honest.clone();
        last[0][2] = Some(2_000_000_000);
        assert!(inlined_copies(&[helper(), caller(), chunk()], &last, 2).is_none());
        let mut caller_line = honest.clone();
        caller_line[1][3] = Some(2_000_000_000);
        assert!(inlined_copies(&[helper(), caller(), chunk()], &caller_line, 2).is_none());
        assert!(inlined_copies(&[prototype(2_000_000_000, &[LOP_JUMPIFNOT, LOP_CALL, LOP_RETURN]), caller(), chunk()], &honest, 2).is_none());
        // Lines within the limit whose ranges together exceed it.
        let wide: Vec<Function> = (0..4).map(|_| helper()).chain([chunk()]).collect();
        let ranges: Vec<Vec<Option<u32>>> = (0..4).map(|_| vec![Some(1), Some(3_000_000), Some(3_000_001)]).chain([vec![Some(1), Some(1), Some(1)]]).collect();
        assert!(inlined_copies(&wide, &ranges, 4).is_none());
        assert!(inlined_prototypes(&[helper(), caller(), chunk()], &last).iter().all(|inlined| !inlined));
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

    /// Plan E2: `frames(n) = n / 60` on line 2. Each load of a number on its
    /// line in a caller is a whole copy, folded: counted per value, outside
    /// other copies or inside one (`linear`, lines 4-6, holding a copy), and
    /// every reference to the value in that caller beside.
    #[test]
    fn fully_folded_copies_are_counted_per_value_and_every_reference_beside() {
        use OpCode::*;
        let bc = |op_code, a, b, c| Instruction::BC { op_code, a, b, c, aux: 0 };
        let ad = |op_code, a, d| Instruction::AD { op_code, a, d, aux: 0 };
        let with = |line_defined, instructions: Vec<Instruction>, constants: Vec<Constant>| Function {
            instructions,
            constants,
            ..prototype(line_defined, &[])
        };
        let frames = with(1, vec![bc(LOP_DIVK, 1, 0, 0), bc(LOP_RETURN, 1, 2, 0)], vec![Constant::Number(60.0)]);
        // (`NEWTABLE` and `SETTABLEKS` have an auxiliary word.)
        let nop = || bc(LOP_NOP, 0, 0, 0);
        let linear = with(4, vec![bc(LOP_NEWTABLE, 1, 0, 0), nop(), bc(LOP_DIVK, 2, 0, 0), bc(LOP_SETTABLEKS, 2, 1, 0), nop(), bc(LOP_RETURN, 1, 2, 0)], vec![]);
        // p2 at line 8: `wait(frames(15))`, `wait(0.25)` by hand, a copy of
        // `linear(15)` (its `frames` copy folded inside), `wait(frames(30))`
        // twice back to back, and `x * 0.25` by hand.
        let caller = with(8, vec![
            ad(LOP_LOADK, 1, 0),
            ad(LOP_LOADK, 1, 0),
            bc(LOP_NEWTABLE, 1, 0, 0), nop(), ad(LOP_LOADK, 2, 0), bc(LOP_SETTABLEKS, 2, 1, 0), nop(), bc(LOP_CALL, 0, 2, 1),
            ad(LOP_LOADK, 1, 1), ad(LOP_LOADK, 2, 1),
            bc(LOP_MULK, 3, 0, 0),
            bc(LOP_RETURN, 0, 1, 0),
        ], vec![Constant::Number(0.25), Constant::Number(0.5)]);
        let chunk = prototype(1, &[LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_RETURN]);
        let lines = vec![
            vec![Some(2), Some(2)],
            vec![Some(5), Some(5), Some(2), Some(5), Some(5), Some(6)],
            vec![Some(2), Some(9), Some(5), Some(5), Some(2), Some(5), Some(5), Some(9), Some(2), Some(2), Some(10), Some(11)],
            vec![Some(1), Some(4), Some(8), Some(12)],
        ];
        let copies = inlined_copies(&[frames, linear, caller, chunk], &lines, 3).unwrap();
        let quarter = 0.25f64.to_bits();
        let half = 0.5f64.to_bits();
        let counted = |helper: u32, bits: u64| copies.constant_copies.get(&(2, helper, bits)).copied().unwrap_or_default();
        assert_eq!(counted(0, quarter), ast::deinline::evidence::ConstantCopies { outermost: 1, nested: 1 });
        assert_eq!(counted(0, half), ast::deinline::evidence::ConstantCopies { outermost: 2, nested: 0 });
        assert_eq!(copies.constant_refs.get(&(2, quarter)), Some(&4), "two copies, a load and a MULK by hand");
        assert_eq!(copies.constant_refs.get(&(2, half)), Some(&2));
        // The two folded copies back to back are two copies of `frames`.
        assert_eq!(copies.copies(Some(2), 0), 3);
        assert_eq!(copies.copies(Some(2), 1), 1);
        // `linear`'s own code loads no constant on `frames`' line: nothing
        // folded there.
        assert!(copies.constant_copies.keys().all(|&(caller, ..)| caller == 2));
    }

    /// A copy of a one-line helper and one folded whole on its line, either
    /// way round, are two copies; the caller's own `return` after a folded
    /// copy is no copy. A function whose code is all copies of helpers
    /// defined before it (`return function(k) return frames(k), frames(90)
    /// end`) holds them: its span is its first line alone.
    #[test]
    fn folded_copies_end_where_they_load_and_functions_made_of_copies_hold_theirs() {
        use OpCode::*;
        let bc = |op_code, a, b, c| Instruction::BC { op_code, a, b, c, aux: 0 };
        let ad = |op_code, a, d| Instruction::AD { op_code, a, d, aux: 0 };
        let with = |line_defined, instructions: Vec<Instruction>, constants: Vec<Constant>| Function {
            instructions,
            constants,
            ..prototype(line_defined, &[])
        };
        let frames = || with(1, vec![bc(LOP_DIVK, 1, 0, 0), bc(LOP_RETURN, 1, 2, 0)], vec![Constant::Number(60.0)]);
        let chunk = || prototype(1, &[LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_RETURN]);
        // p1 at line 10, every PC on `frames`' line 2.
        for instructions in [
            vec![bc(LOP_DIVK, 1, 0, 0), ad(LOP_LOADK, 2, 0), bc(LOP_RETURN, 1, 3, 0)],
            vec![ad(LOP_LOADK, 2, 0), bc(LOP_DIVK, 1, 0, 0), bc(LOP_RETURN, 1, 3, 0)],
        ] {
            let caller = with(10, instructions, vec![Constant::Number(1.5)]);
            let lines = vec![vec![Some(2), Some(2)], vec![Some(2), Some(2), Some(2)], vec![Some(1), Some(10), Some(12)]];
            let copies = inlined_copies(&[frames(), caller, chunk()], &lines, 2).unwrap();
            assert_eq!(copies.copies(Some(1), 0), 2);
            assert_eq!(copies.constant_copies.get(&(1, 0, 1.5f64.to_bits())).map(|counted| counted.outermost), Some(1));
        }
        let caller = with(10, vec![ad(LOP_LOADK, 2, 0), bc(LOP_RETURN, 2, 2, 0)], vec![Constant::Number(1.5)]);
        let lines = vec![vec![Some(2), Some(2)], vec![Some(2), Some(2)], vec![Some(1), Some(10), Some(12)]];
        let copies = inlined_copies(&[frames(), caller, chunk()], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 1);
        // A helper with a constant load or a `^` of its own never folds
        // whole as far as the census tells.
        let loads = with(1, vec![ad(LOP_LOADN, 1, 2), bc(LOP_DIV, 1, 1, 0), bc(LOP_RETURN, 1, 2, 0)], vec![]);
        let pow = with(1, vec![bc(LOP_POWK, 1, 0, 0), bc(LOP_RETURN, 1, 2, 0)], vec![Constant::Number(2.0)]);
        for helper in [loads, pow] {
            let caller = with(10, vec![ad(LOP_LOADN, 2, 9), bc(LOP_RETURN, 2, 2, 0)], vec![]);
            let lines = vec![vec![Some(2); helper.instructions.len()], vec![Some(2), Some(11)], vec![Some(1), Some(10), Some(12)]];
            let copies = inlined_copies(&[helper, caller, chunk()], &lines, 2).unwrap();
            assert!(copies.constant_copies.is_empty());
        }
    }

    /// Copies back to back with no code of the caller between them: a void
    /// helper's copies end at its last statement (its `RETURN`, alone on its
    /// `end` line, is never copied), and a one-line helper's copies with
    /// operations folded away for constant arguments start again where its
    /// first operation comes with another of its operations after it.
    #[test]
    fn copies_back_to_back_split_without_the_return_and_with_folded_operations() {
        use OpCode::*;
        let bc = |op_code, a, b, c| Instruction::BC { op_code, a, b, c, aux: 0 };
        let with = |line_defined, instructions: Vec<Instruction>| Function { instructions, ..prototype(line_defined, &[]) };
        let chunk = || prototype(1, &[LOP_NEWCLOSURE, LOP_NEWCLOSURE, LOP_RETURN]);
        // p0, lines 1-4: `local function fade(p, k) p.A = 1 - k; p.B = k * 10 end`,
        // its statements on lines 2-3 and its RETURN on line 4.
        let fade = with(1, vec![bc(LOP_SUBRK, 2, 0, 1), bc(LOP_SETTABLE, 2, 0, 0), bc(LOP_MULK, 2, 1, 0), bc(LOP_SETTABLE, 2, 0, 0), bc(LOP_RETURN, 0, 1, 0)]);
        // p1 at line 6: three copies, two with `k` folded (constant loads).
        let caller = with(6, vec![
            Instruction::AD { op_code: LOP_LOADN, a: 3, d: 5, aux: 0 }, bc(LOP_SETTABLE, 3, 0, 0), Instruction::AD { op_code: LOP_LOADN, a: 3, d: 5, aux: 0 }, bc(LOP_SETTABLE, 3, 0, 0),
            Instruction::AD { op_code: LOP_LOADN, a: 3, d: 5, aux: 0 }, bc(LOP_SETTABLE, 3, 1, 0), Instruction::AD { op_code: LOP_LOADN, a: 3, d: 5, aux: 0 }, bc(LOP_SETTABLE, 3, 1, 0),
            bc(LOP_SUBRK, 3, 0, 2), bc(LOP_SETTABLE, 3, 2, 0), bc(LOP_MULK, 3, 2, 0), bc(LOP_SETTABLE, 3, 2, 0),
            bc(LOP_RETURN, 0, 1, 0),
        ]);
        let lines = vec![
            vec![Some(2), Some(2), Some(3), Some(3), Some(4)],
            vec![Some(2), Some(2), Some(3), Some(3), Some(2), Some(2), Some(3), Some(3), Some(2), Some(2), Some(3), Some(3), Some(7)],
            vec![Some(1), Some(6), Some(8)],
        ];
        let copies = inlined_copies(&[fade, caller, chunk()], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 3);
        // p0, lines 1-2: `map(x, a, b, c, d) return (x - a) * (d - c) / (b - a) + c`;
        // p1 at line 4: `f(map(y, 100, 350, 0.28, 1), map(y, 50, 100, 0, 1))`,
        // each copy `SUBK, MULK, DIVK, ADDK` (two subtractions folded).
        let map = with(1, vec![
            bc(LOP_SUB, 5, 0, 1), bc(LOP_SUB, 6, 4, 3), bc(LOP_MUL, 5, 5, 6), bc(LOP_SUB, 6, 2, 1), bc(LOP_DIV, 5, 5, 6),
            bc(LOP_ADD, 5, 5, 3), bc(LOP_RETURN, 5, 2, 0),
        ]);
        let caller = with(4, vec![
            bc(LOP_SUBK, 2, 0, 0), bc(LOP_MULK, 2, 2, 1), bc(LOP_DIVK, 2, 2, 2), bc(LOP_ADDK, 2, 2, 3),
            bc(LOP_SUBK, 3, 0, 4), bc(LOP_MULK, 3, 3, 5), bc(LOP_DIVK, 3, 3, 4), bc(LOP_ADDK, 3, 3, 6),
            bc(LOP_RETURN, 2, 3, 0),
        ]);
        let lines = vec![vec![Some(2); 7], vec![Some(2), Some(2), Some(2), Some(2), Some(2), Some(2), Some(2), Some(2), Some(5)], vec![Some(1), Some(4), Some(7)]];
        let copies = inlined_copies(&[map, caller, chunk()], &lines, 2).unwrap();
        assert_eq!(copies.copies(Some(1), 0), 2);
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
