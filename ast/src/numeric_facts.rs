//! Small runtime proofs for late motion, independent of names/type hints.
//! Only single-write declarations and checked numeric-for counters seed facts.
use rustc_hash::{FxHashMap, FxHashSet};
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
                BinaryOperation::Add | BinaryOperation::Sub | BinaryOperation::Mul | BinaryOperation::Div => Some(Primitive::Number),
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

pub(crate) fn collect(block: &Block, usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>) -> FxHashSet<RcLocal> {
    fn visit(block: &Block, usage: &FxHashMap<RcLocal, crate::inline_temps::Usage>, numbers: &mut FxHashSet<RcLocal>) {
        for statement in &block.0 {
            if let Statement::Assign(assign) = statement
                && assign.prefix && !assign.parallel && assign.left.len() == 1 && assign.right.len() == 1
                && let LValue::Local(local) = &assign.left[0]
                && usage.get(local).is_some_and(|u| u.writes == 1)
                && primitive(&assign.right[0], numbers, &mut 1024, 0) == Some(Primitive::Number)
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
