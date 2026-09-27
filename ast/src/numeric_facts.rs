//! Small runtime proofs for late motion, independent of names/type hints.
//! Only single-write declarations and checked numeric-for counters seed facts.
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;
use crate::{BinaryOperation, Block, LValue, Literal, RValue, RcLocal, Statement, UnaryOperation};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Primitive { Number, Boolean }

fn primitive(value: &RValue, numbers: &FxHashSet<RcLocal>, remaining: &mut usize, depth: usize) -> Option<Primitive> {
    if *remaining == 0 || depth >= 64 { return None; }
    *remaining -= 1;
    match value {
        // Non-finite numbers and pi are emitted through environment lookups.
        RValue::Literal(Literal::Number(n))
            if n.is_finite() && n.abs().to_bits() != std::f64::consts::PI.to_bits() => Some(Primitive::Number),
        RValue::Local(local) if numbers.contains(local) => Some(Primitive::Number),
        RValue::Unary(unary) if unary.operation == UnaryOperation::Negate =>
            (primitive(&unary.value, numbers, remaining, depth + 1)? == Primitive::Number).then_some(Primitive::Number),
        RValue::Binary(binary) => {
            if primitive(&binary.left, numbers, remaining, depth + 1)? != Primitive::Number
                || primitive(&binary.right, numbers, remaining, depth + 1)? != Primitive::Number { return None; }
            match binary.operation {
                BinaryOperation::Add | BinaryOperation::Sub | BinaryOperation::Mul | BinaryOperation::Div
                | BinaryOperation::IDiv | BinaryOperation::Mod | BinaryOperation::Pow => Some(Primitive::Number),
                op if op.is_comparator() => Some(Primitive::Boolean),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(crate) fn total(value: &RValue, numbers: &FxHashSet<RcLocal>) -> bool {
    primitive(value, numbers, &mut 1024, 0).is_some()
}

#[cfg(test)]
fn number_result(value: &RValue, numbers: &FxHashSet<RcLocal>) -> bool {
    // Luau's LEN instruction either produces a number or raises; the VM checks
    // the result of __len too (pinned lvmutils.cpp, luaV_dolen). The evaluation
    // itself remains effectful. Only a completed single-write snapshot seeds
    // this fact; it never grants motion/deletion permission to the LEN node.
    matches!(value, RValue::Unary(unary) if unary.operation == UnaryOperation::Length)
        || primitive(value, numbers, &mut 1024, 0) == Some(Primitive::Number)
}

#[cfg(test)]
fn collect_reference(block: &Block, usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>) -> FxHashSet<RcLocal> {
    fn visit(block: &Block, usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>, numbers: &mut FxHashSet<RcLocal>) {
        for statement in &block.0 {
            if let Statement::Assign(assign) = statement
                && assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1
                && let LValue::Local(local) = &assign.left[0]
                && usage.get(local).is_some_and(|u| u.writes == 1)
                && number_result(&assign.right[0], numbers)
            { numbers.insert(local.clone()); }
            if let Statement::NumericFor(loop_) = statement
                && usage.get(&loop_.counter).is_some_and(|u| u.writes == 1)
            { numbers.insert(loop_.counter.clone()); }
            let mut functions = Vec::new();
            crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| functions.push(closure.function.clone()));
            for function in functions { visit(&function.lock().body, usage, numbers); }
            match statement {
                Statement::If(branch) => {
                    visit(&branch.then_block.lock(), usage, numbers);
                    visit(&branch.else_block.lock(), usage, numbers);
                }
                Statement::NumericFor(loop_) => visit(&loop_.block.lock(), usage, numbers),
                Statement::GenericFor(loop_) => visit(&loop_.block.lock(), usage, numbers),
                Statement::While(loop_) => visit(&loop_.block.lock(), usage, numbers),
                Statement::Repeat(loop_) => visit(&loop_.block.lock(), usage, numbers),
                _ => {}
            }
        }
    }
    let mut numbers = FxHashSet::default();
    // Facts only accumulate. A bounded fallback leaves later aliases unknown.
    for _ in 0..16 {
        let before = numbers.len();
        visit(block, usage, &mut numbers);
        if numbers.len() == before { break; }
    }
    numbers
}

/// Extract the exact numeric proof's prerequisites once. The proof accepts only
/// numeric literals/locals, numeric unary minus and arithmetic, so its outcome
/// is the conjunction of these local facts. LEN is a completed numeric snapshot
/// regardless of its operand, as in the original inference rule.
fn dependencies(value: &RValue, locals: &mut Vec<RcLocal>, remaining: &mut usize, depth: usize) -> bool {
    if *remaining == 0 || depth >= 64 { return false; }
    *remaining -= 1;
    match value {
        RValue::Literal(Literal::Number(number)) => number.is_finite()
            && number.abs().to_bits() != std::f64::consts::PI.to_bits(),
        RValue::Local(local) => { locals.push(local.clone()); true }
        RValue::Unary(unary) if unary.operation == UnaryOperation::Negate =>
            dependencies(&unary.value, locals, remaining, depth + 1),
        RValue::Binary(binary) if matches!(binary.operation,
            BinaryOperation::Add | BinaryOperation::Sub | BinaryOperation::Mul | BinaryOperation::Div
            | BinaryOperation::IDiv | BinaryOperation::Mod | BinaryOperation::Pow) =>
            dependencies(&binary.left, locals, remaining, depth + 1)
                && dependencies(&binary.right, locals, remaining, depth + 1),
        _ => false,
    }
}

pub(crate) fn collect(block: &Block, usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>) -> FxHashSet<RcLocal> {
    fn candidates(block: &Block, usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>, out: &mut Vec<(RcLocal, Vec<RcLocal>)>) {
        for statement in &block.0 {
            if let Statement::Assign(assign) = statement
                && assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1
                && let LValue::Local(local) = &assign.left[0]
                && usage.get(local).is_some_and(|usage| usage.writes == 1)
            {
                let mut reads = Vec::new();
                if matches!(&assign.right[0], RValue::Unary(unary) if unary.operation == UnaryOperation::Length)
                    || dependencies(&assign.right[0], &mut reads, &mut 1024, 0)
                { out.push((local.clone(), reads)); }
            }
            if let Statement::NumericFor(loop_) = statement
                && usage.get(&loop_.counter).is_some_and(|usage| usage.writes == 1)
            { out.push((loop_.counter.clone(), Vec::new())); }
            crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
                candidates(&closure.function.lock().body, usage, out);
            });
            match statement {
                Statement::If(branch) => {
                    candidates(&branch.then_block.lock(), usage, out);
                    candidates(&branch.else_block.lock(), usage, out);
                }
                Statement::NumericFor(loop_) => candidates(&loop_.block.lock(), usage, out),
                Statement::GenericFor(loop_) => candidates(&loop_.block.lock(), usage, out),
                Statement::While(loop_) => candidates(&loop_.block.lock(), usage, out),
                Statement::Repeat(loop_) => candidates(&loop_.block.lock(), usage, out),
                _ => {}
            }
        }
    }
    let mut nodes = Vec::new();
    candidates(block, usage, &mut nodes);
    let mut waiting: FxHashMap<RcLocal, Vec<usize>> = FxHashMap::default();
    let mut missing = Vec::with_capacity(nodes.len());
    let mut pending = BTreeSet::new();
    for (index, (_, reads)) in nodes.iter().enumerate() {
        missing.push(reads.len());
        if reads.is_empty() { pending.insert((0, index)); }
        for local in reads { waiting.entry(local.clone()).or_default().push(index); }
    }
    let mut numbers = FxHashSet::default();
    while let Some((round, index)) = pending.pop_first() {
        if round >= 16 { break; }
        let local = &nodes[index].0;
        if !numbers.insert(local.clone()) { continue; }
        if let Some(dependents) = waiting.remove(local) {
            for dependent in dependents {
                missing[dependent] -= 1;
                if missing[dependent] == 0 {
                    // Earlier candidates observe this fact on the next legacy
                    // scan, later candidates observe it in the current scan.
                    pending.insert((round + usize::from(dependent <= index), dependent));
                }
            }
        }
    }
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, Global, If, Local, Unary};

    #[test]
    fn numeric_dependency_schedule_matches_sixteen_ordered_scans() {
        for seed in 1..120u64 {
            let mut random = seed;
            let mut next = || { random ^= random << 13; random ^= random >> 7; random ^= random << 17; random as usize };
            let locals: Vec<_> = (0..80).map(|index| RcLocal::new(Local::new(Some(format!("v{index}"))))).collect();
            let mut block = Block::default();
            for (index, local) in locals.iter().enumerate() {
                let read = RValue::Local(locals[next() % locals.len()].clone());
                let value = match next() % 8 {
                    0 => Literal::Number(index as f64).into(),
                    1 => Literal::Number(std::f64::consts::PI).into(),
                    2 => Unary::new(read, UnaryOperation::Length).into(),
                    3 => Unary::new(read, UnaryOperation::Negate).into(),
                    4 => Binary::new(read, Literal::Number(3.0).into(), BinaryOperation::Add).into(),
                    5 => Binary::new(read, Literal::Number(2.0).into(), BinaryOperation::Equal).into(),
                    _ => read,
                };
                let mut assign = Assign::new(vec![local.clone().into()], vec![value]); assign.prefix = true;
                if next() % 4 == 0 { block.push(If::new(Global::from("flag").into(), Block(vec![assign.into()]), Block::default()).into()); }
                else { block.push(assign.into()); }
            }
            let usage = crate::inline_temps::collect_usage(&block);
            assert_eq!(collect(&block, &usage), collect_reference(&block, &usage), "seed {seed}");
        }
    }

    #[test]
    fn numeric_dependency_schedule_preserves_backward_round_limit() {
        let locals: Vec<_> = (0..40).map(|_| RcLocal::default()).collect();
        let mut block = Block::default();
        for index in 0..locals.len() {
            let value = locals.get(index + 1).map_or(Literal::Number(1.0).into(), |local| local.clone().into());
            let mut assign = Assign::new(vec![locals[index].clone().into()], vec![value]); assign.prefix = true;
            block.push(assign.into());
        }
        let usage = crate::inline_temps::collect_usage(&block);
        let actual = collect(&block, &usage);
        assert_eq!(actual.len(), 16);
        assert_eq!(actual, collect_reference(&block, &usage));
    }
}
