//! Validate the operands and instruction protocols the lifter relies on before
//! allocating locals, CFGs, or repeated closure instances.
use std::{collections::HashMap, fmt};

use either::Either;

use crate::{argument::RegisterOrConstant, function, Function, Instruction, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    pub prototype: usize,
    pub pc: Option<usize>,
    pub reason: &'static str,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lua 5.1 prototype {}", self.prototype)?;
        if let Some(pc) = self.pc { write!(f, ", pc {pc}")?; }
        write!(f, ": {}", self.reason)
    }
}

impl std::error::Error for ValidationError {}

/// Child tables are trees on the wire, but multiple CLOSURE sites can expand
/// the same child repeatedly. Count those sites without constructing instances.
pub fn validate(root: &Function<'_>) -> Result<(), ValidationError> {
    let mut pending = vec![(root, 0usize)];
    let mut prototypes = Vec::new();
    let mut instruction_words = 0usize;
    while let Some((prototype, depth)) = pending.pop() {
        let id = prototypes.len();
        let fail = |reason| ValidationError { prototype: id, pc: None, reason };
        if depth > function::MAX_FUNCTION_DEPTH { return Err(fail("prototype depth budget exceeded")); }
        if id >= function::MAX_FUNCTIONS { return Err(fail("prototype count budget exceeded")); }
        instruction_words = instruction_words.saturating_add(prototype.code.len());
        if instruction_words > function::MAX_INSTRUCTION_WORDS {
            return Err(fail("instruction budget exceeded"));
        }
        validate_function(prototype, id)?;
        prototypes.push(prototype);
        pending.extend(prototype.closures.iter().rev().map(|child| (child, depth + 1)));
    }

    let mut costs = HashMap::<usize, (usize, usize)>::new();
    for (id, prototype) in prototypes.into_iter().enumerate().rev() {
        let mut instances = 1usize;
        let mut words = prototype.code.len();
        for (pc, instruction) in prototype.code.iter().enumerate() {
            let Instruction::Closure { function: child, .. } = instruction else { continue; };
            // Operand validation above checked every child index; reversed
            // preorder visits every serialized child before its parent.
            let key = &prototype.closures[child.0 as usize] as *const Function<'_> as usize;
            let (child_instances, child_words) = costs[&key];
            instances = instances.saturating_add(child_instances);
            words = words.saturating_add(child_words);
            let reason = if instances > function::MAX_FUNCTIONS {
                Some("expanded function instance budget exceeded")
            } else if words > function::MAX_INSTRUCTION_WORDS {
                Some("expanded instruction budget exceeded")
            } else { None };
            if let Some(reason) = reason { return Err(ValidationError { prototype: id, pc: Some(pc), reason }); }
        }
        costs.insert(prototype as *const Function<'_> as usize, (instances, words));
    }
    Ok(())
}

fn mark_target(
    starts: &[bool], blocks: &mut [bool], destination: Option<usize>, prototype: usize, pc: usize,
) -> Result<(), ValidationError> {
    let Some(destination) = destination.filter(|&destination| starts.get(destination) == Some(&true)) else {
        return Err(ValidationError { prototype, pc: Some(pc), reason: "branch target is outside executable instructions" });
    };
    blocks[destination] = true;
    Ok(())
}

fn validate_function(function: &Function<'_>, prototype: usize) -> Result<(), ValidationError> {
    let fail = |pc, reason| ValidationError { prototype, pc, reason };
    if !matches!(function.code.last(), Some(Instruction::Return(..))) {
        return Err(fail(None, "function must end with RETURN"));
    }
    if function.number_of_parameters > function.maximum_stack_size {
        return Err(fail(None, "parameters exceed the register stack"));
    }
    if function.vararg_flag & !7 != 0 || function.needs_arg_table() && !function.is_variadic() {
        return Err(fail(None, "invalid variadic flags"));
    }
    if !function.positions.is_empty() && function.positions.len() != function.code.len() {
        return Err(fail(None, "debug line count does not match instructions"));
    }
    if !function.upvalues.is_empty() && function.upvalues.len() != usize::from(function.number_of_upvalues) {
        return Err(fail(None, "debug upvalue count does not match slots"));
    }
    if function.locals.iter().any(|local| local.range.start > local.range.end || local.range.end as usize > function.code.len()) {
        return Err(fail(None, "debug local lifetime is outside instructions"));
    }

    let mut starts: Vec<_> = function.code.iter().map(|instruction| !matches!(instruction, Instruction::ExtraArgument)).collect();
    for (pc, instruction) in function.code.iter().enumerate() {
        if matches!(instruction, Instruction::ExtraArgument)
            && (pc == 0 || !matches!(function.code[pc - 1], Instruction::SetList { .. }))
        {
            return Err(fail(Some(pc), "orphan SETLIST argument word"));
        }
        let Instruction::Closure { function: child, .. } = instruction else { continue; };
        let Some(child) = function.closures.get(child.0 as usize) else {
            return Err(fail(Some(pc), "closure child index is out of range"));
        };
        for capture in pc + 1..pc + 1 + usize::from(child.number_of_upvalues) {
            let valid = match function.code.get(capture) {
                Some(Instruction::Move { source, .. }) => source.0 < function.maximum_stack_size,
                Some(Instruction::GetUpvalue { upvalue, .. }) => upvalue.0 < function.number_of_upvalues,
                _ => false,
            };
            if !valid || starts.get(capture) != Some(&true) {
                return Err(fail(Some(pc), "malformed or overlapping closure capture group"));
            }
            starts[capture] = false;
        }
    }

    let mut blocks = vec![false; function.code.len() + 1];
    blocks[0] = true;
    for (pc, instruction) in function.code.iter().enumerate().filter(|(pc, _)| starts[*pc]) {
        let fail = |reason| fail(Some(pc), reason);
        let reg = |register: usize| if register < usize::from(function.maximum_stack_size) {
            Ok(())
        } else { Err(fail("register is outside the stack")) };
        let constant = |index: u32| function.constants.get(index as usize).ok_or_else(|| fail("constant index is out of range"));
        let rk = |operand: &RegisterOrConstant| match operand.0 {
            Either::Left(register) => reg(usize::from(register.0)),
            Either::Right(index) => constant(index.0).map(|_| ()),
        };
        let target = |blocks: &mut [bool], destination| mark_target(&starts, blocks, destination, prototype, pc);
        match instruction {
            Instruction::Move { destination, source } => { reg(destination.0.into())?; reg(source.0.into())?; }
            Instruction::LoadConstant { destination, source } => { reg(destination.0.into())?; constant(source.0)?; }
            Instruction::LoadBoolean { destination, skip_next, .. } => {
                reg(destination.0.into())?;
                if *skip_next { target(&mut blocks, pc.checked_add(2))?; blocks[pc + 1] = true; }
            }
            Instruction::LoadNil(registers) => {
                if registers.is_empty() { return Err(fail("empty LOADNIL range")); }
                for register in registers { reg(register.0.into())?; }
            }
            Instruction::GetUpvalue { destination, upvalue } => {
                reg(destination.0.into())?;
                if upvalue.0 >= function.number_of_upvalues { return Err(fail("upvalue slot is out of range")); }
            }
            Instruction::SetUpvalue { destination, source } => {
                reg(source.0.into())?;
                if destination.0 >= function.number_of_upvalues { return Err(fail("upvalue slot is out of range")); }
            }
            Instruction::GetGlobal { destination, global } | Instruction::SetGlobal { destination: global, value: destination } => {
                reg(destination.0.into())?;
                if !matches!(constant(global.0)?, Value::String(_)) { return Err(fail("global name is not a string")); }
            }
            Instruction::GetIndex { destination, object, key } => { reg(destination.0.into())?; reg(object.0.into())?; rk(key)?; }
            Instruction::SetIndex { object, key, value } => { reg(object.0.into())?; rk(key)?; rk(value)?; }
            Instruction::NewTable { destination, .. } | Instruction::Closure { destination, .. } => { reg(destination.0.into())?; }
            Instruction::PrepMethodCall { destination, self_arg, object, method } => {
                reg(destination.0.into())?; reg(self_arg.0.into())?; reg(object.0.into())?; rk(method)?;
            }
            Instruction::Add { destination, lhs, rhs } | Instruction::Sub { destination, lhs, rhs }
            | Instruction::Mul { destination, lhs, rhs } | Instruction::Div { destination, lhs, rhs }
            | Instruction::Mod { destination, lhs, rhs } | Instruction::Pow { destination, lhs, rhs } => {
                reg(destination.0.into())?; rk(lhs)?; rk(rhs)?;
            }
            Instruction::Minus { destination, operand } | Instruction::Not { destination, operand }
            | Instruction::Length { destination, operand } => { reg(destination.0.into())?; reg(operand.0.into())?; }
            Instruction::Concatenate { destination, operands } => {
                reg(destination.0.into())?;
                if operands.len() < 2 { return Err(fail("CONCAT needs at least two operands")); }
                for operand in operands { reg(operand.0.into())?; }
            }
            Instruction::Jump(skip) => {
                target(&mut blocks, (pc + 1).checked_add_signed(*skip as isize))?; blocks[pc + 1] = true;
            }
            Instruction::Equal { lhs, rhs, .. } | Instruction::LessThan { lhs, rhs, .. }
            | Instruction::LessThanOrEqual { lhs, rhs, .. } => {
                rk(lhs)?; rk(rhs)?; target(&mut blocks, Some(pc + 1))?; target(&mut blocks, Some(pc + 2))?;
            }
            Instruction::Test { value, .. } => {
                reg(value.0.into())?; target(&mut blocks, Some(pc + 1))?; target(&mut blocks, Some(pc + 2))?;
            }
            Instruction::TestSet { destination, value, .. } => {
                reg(destination.0.into())?; reg(value.0.into())?;
                target(&mut blocks, Some(pc + 1))?; target(&mut blocks, Some(pc + 2))?;
            }
            Instruction::Call { function: base, arguments, return_values } => {
                reg(base.0.into())?;
                if *arguments > 1 { reg(usize::from(base.0) + usize::from(*arguments) - 1)?; }
                if *return_values > 1 { reg(usize::from(base.0) + usize::from(*return_values) - 2)?; }
            }
            Instruction::TailCall { function: base, arguments } => {
                reg(base.0.into())?;
                if *arguments > 1 { reg(usize::from(base.0) + usize::from(*arguments) - 1)?; }
                if !matches!(function.code.get(pc + 1), Some(Instruction::Return(register, 0)) if register == base) {
                    return Err(fail("TAILCALL needs its matching open RETURN"));
                }
            }
            Instruction::Return(base, count) | Instruction::VarArg(base, count) => {
                reg(base.0.into())?;
                if *count > 1 { reg(usize::from(base.0) + usize::from(*count) - 2)?; }
                if matches!(instruction, Instruction::Return(..)) { blocks[pc + 1] = true; }
                else if !function.is_variadic() { return Err(fail("VARARG in a fixed-arity function")); }
            }
            Instruction::InitNumericForLoop { control, skip } | Instruction::IterateNumericForLoop { control, skip } => {
                if control.len() != 4 { return Err(fail("numeric loop needs four control registers")); }
                for register in control { reg(register.0.into())?; }
                target(&mut blocks, (pc + 1).checked_add_signed(*skip as isize))?;
                if matches!(instruction, Instruction::IterateNumericForLoop { .. }) { target(&mut blocks, Some(pc + 1))?; }
                blocks[pc + 1] = true;
            }
            Instruction::IterateGenericForLoop { generator, state, internal_control, vars } => {
                reg(generator.0.into())?; reg(state.0.into())?; reg(internal_control.0.into())?;
                if vars.is_empty() { return Err(fail("generic loop needs a result register")); }
                for register in vars { reg(register.0.into())?; }
                target(&mut blocks, Some(pc + 1))?; target(&mut blocks, Some(pc + 2))?;
            }
            Instruction::SetList { table, number_of_elements, block_number } => {
                reg(table.0.into())?;
                if block_number.checked_sub(1).and_then(|n| (n as usize).checked_mul(50)).and_then(|n| n.checked_add(1)).is_none() {
                    return Err(fail("SETLIST block index is out of range"));
                }
                if *number_of_elements > 0 { reg(usize::from(table.0) + usize::from(*number_of_elements))?; }
            }
            Instruction::Close(base) => {
                if base.0 > function.maximum_stack_size { return Err(fail("CLOSE register is outside the stack")); }
            }
            Instruction::ExtraArgument => unreachable!("argument words were excluded above"),
        }
    }
    validate_open_results(function, prototype, &starts, &blocks)
}

fn validate_open_results(function: &Function<'_>, prototype: usize, starts: &[bool], blocks: &[bool]) -> Result<(), ValidationError> {
    let mut top: Option<usize> = None;
    for (pc, instruction) in function.code.iter().enumerate().filter(|(pc, _)| starts[*pc]) {
        let fail = |reason| ValidationError { prototype, pc: Some(pc), reason };
        if blocks[pc] && top.is_some() { return Err(fail("open results cross a control-flow boundary")); }
        let consumer = match instruction {
            Instruction::Call { function, arguments: 0, .. } | Instruction::TailCall { function, arguments: 0 } => Some(usize::from(function.0) + 1),
            Instruction::Return(base, 0) => Some(usize::from(base.0)),
            Instruction::SetList { table, number_of_elements: 0, .. } => Some(usize::from(table.0) + 1),
            _ => None,
        };
        if let Some(first) = consumer {
            if !top.take().is_some_and(|base| base >= first) { return Err(fail("open result consumer has no matching producer")); }
        } else if top.is_some() {
            // The lifter represents the open producer as a deferred expression;
            // moving it past another instruction could reorder side effects.
            return Err(fail("open results are not consumed by the next instruction"));
        }
        top = match instruction {
            Instruction::Call { function, return_values: 0, .. } | Instruction::TailCall { function, .. } => Some(usize::from(function.0)),
            Instruction::VarArg(base, 0) => Some(usize::from(base.0)),
            _ => None,
        };
    }
    if top.is_some() { return Err(ValidationError { prototype, pc: None, reason: "unconsumed open results at function end" }); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argument::{self, Register};

    fn prototype(code: Vec<Instruction>) -> Function<'static> {
        Function { name: b"", line_defined: 0, last_line_defined: 0, number_of_upvalues: 0,
            vararg_flag: 2, maximum_stack_size: 4, code, constants: vec![], closures: vec![],
            positions: vec![], locals: vec![], upvalues: vec![], number_of_parameters: 0 }
    }

    #[test]
    fn indexed_operands_targets_and_open_results_fail_before_lifting() {
        let ret = Instruction::Return(Register(0), 1);
        for instructions in [
            vec![Instruction::Move { destination: Register(0), source: Register(4) }, ret.clone()],
            vec![Instruction::LoadConstant { destination: Register(0), source: argument::Constant(0) }, ret.clone()],
            vec![Instruction::Jump(-2), ret.clone()],
            vec![Instruction::Test { value: Register(0), invert: false }, ret.clone()],
            vec![Instruction::Return(Register(0), 0)],
            vec![Instruction::VarArg(Register(0), 0), Instruction::Move { destination: Register(1), source: Register(0) }, ret],
        ] { assert!(validate(&prototype(instructions)).is_err()); }
        assert!(validate(&prototype(vec![Instruction::VarArg(Register(1), 0),
            Instruction::Call { function: Register(0), arguments: 0, return_values: 0 },
            Instruction::Return(Register(0), 0)])).is_ok());
    }

    #[test]
    fn branches_cannot_enter_capture_descriptors() {
        let mut parent = prototype(vec![Instruction::Jump(1),
            Instruction::Closure { destination: Register(0), function: argument::Function(0) },
            Instruction::Move { destination: Register(255), source: Register(1) },
            Instruction::Return(Register(0), 2)]);
        let mut child = prototype(vec![Instruction::Return(Register(0), 1)]);
        child.number_of_upvalues = 1;
        parent.closures.push(child);
        assert!(validate(&parent).unwrap_err().reason.contains("branch target"));
        parent.code[0] = Instruction::Move { destination: Register(1), source: Register(2) };
        // Capture descriptor A is metadata and need not be a live register.
        assert!(validate(&parent).is_ok());
    }

    #[test]
    fn repeated_constructor_sites_are_budgeted_without_expanding_instances() {
        let mut root = prototype(vec![Instruction::Return(Register(0), 1)]);
        for _ in 0..16 {
            let mut parent = prototype(vec![
                Instruction::Closure { destination: Register(0), function: argument::Function(0) },
                Instruction::Closure { destination: Register(1), function: argument::Function(0) },
                Instruction::Return(Register(0), 1)]);
            parent.closures.push(root);
            root = parent;
        }
        assert_eq!(validate(&root).unwrap_err().reason, "expanded function instance budget exceeded");
    }
}
