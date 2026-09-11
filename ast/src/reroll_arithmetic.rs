//! Synthesize a finite numeric loop from an exact ordered accumulation.
//! This is equivalence-based presentation, not evidence of a unique source loop.

use rustc_hash::FxHashMap;

use crate::inline_temps::{collect_closures_in_statement, collect_usage, Usage};
use crate::{
    Assign, Binary, BinaryOperation, Block, Comment, LValue, Literal, Local, NumericFor, RValue,
    RcLocal, Return, Statement, Traverse,
};

const MIN_ITERATIONS: usize = 4;
const MAX_ITERATIONS: usize = 8;
const MAX_LOOPS: usize = 64;
const MARKER: &str = "equivalent fixed-count loop synthesized; original loop unknown";

#[derive(Clone)]
struct Term {
    value: RcLocal,
    index_on_left: bool,
}

struct Candidate {
    consumed: usize,
    result: Option<RcLocal>,
    term: Term,
    iterations: usize,
    returns: bool,
}

pub fn reroll_arithmetic(body: &mut Block) {
    // The later arithmetic de-inliner has stronger source evidence: retain its
    // patterns and their possible sites instead of hiding them inside loops.
    // A module-wide shape veto deliberately over-refuses outside lexical scope.
    let mut protected = [false; MAX_ITERATIONS + 1];
    crate::deinline::each_closure_decl(&body.0, &mut |_, function| {
        if let Some(pattern) = crate::expr_deinline::arithmetic::pattern(&function.lock()) {
            protect_expressions(&pattern, &mut protected);
        }
    });
    let usage = collect_usage(body);
    let mut remaining = MAX_LOOPS;
    walk(body, &usage, &protected, &mut remaining);
}

fn protect_expressions(value: &RValue, protected: &mut [bool; MAX_ITERATIONS + 1]) {
    if let Some((_, count)) = expression(value) {
        protected[count] = true;
    }
    for child in value.rvalues() {
        protect_expressions(child, protected);
    }
}

fn walk(
    body: &mut Block,
    usage: &FxHashMap<RcLocal, Usage>,
    protected: &[bool; MAX_ITERATIONS + 1],
    remaining: &mut usize,
) {
    for statement in &mut body.0 {
        let mut closures = Vec::new();
        collect_closures_in_statement(statement, &mut |c| closures.push(c.function.clone()));
        for closure in closures {
            walk(&mut closure.lock().body, usage, protected, remaining);
        }
        match statement {
            Statement::If(s) => {
                walk(&mut s.then_block.lock(), usage, protected, remaining);
                walk(&mut s.else_block.lock(), usage, protected, remaining);
            }
            Statement::While(s) => walk(&mut s.block.lock(), usage, protected, remaining),
            Statement::Repeat(s) => walk(&mut s.block.lock(), usage, protected, remaining),
            Statement::NumericFor(s) => walk(&mut s.block.lock(), usage, protected, remaining),
            Statement::GenericFor(s) => walk(&mut s.block.lock(), usage, protected, remaining),
            _ => {}
        }
    }
    let mut index = 0;
    while index < body.0.len() && *remaining != 0 {
        let candidate = candidate(&body.0[index..], usage);
        if let Some(candidate) = candidate.filter(|c| !protected[c.iterations]) {
            let consumed = candidate.consumed;
            let replacement = emit(candidate);
            let count = replacement.len();
            body.0.splice(index..index + consumed, replacement);
            index += count;
            *remaining -= 1;
            crate::telemetry::count("synthesized_loops", 1);
        } else {
            index += 1;
        }
    }
}

fn declaration(statement: &Statement) -> Option<(&RcLocal, &RValue)> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 || assign.right.len() != 1 {
        return None;
    }
    let LValue::Local(local) = &assign.left[0] else {
        return None;
    };
    Some((local, &assign.right[0]))
}

fn private_once(local: &RcLocal, usage: &FxHashMap<RcLocal, Usage>) -> bool {
    usage
        .get(local)
        .is_some_and(|u| u.writes == 1 && !u.captured)
}

fn zero(value: &RValue) -> bool {
    matches!(value, RValue::Literal(Literal::Number(n)) if n.to_bits() == 0.0f64.to_bits())
}

fn term(value: &RValue, index: usize) -> Option<Term> {
    let RValue::Binary(binary) = value else {
        return None;
    };
    if binary.operation != BinaryOperation::Mul {
        return None;
    }
    let expected = (index as f64).to_bits();
    match (&*binary.left, &*binary.right) {
        (RValue::Local(value), RValue::Literal(Literal::Number(n))) if n.to_bits() == expected => {
            Some(Term {
                value: value.clone(),
                index_on_left: false,
            })
        }
        (RValue::Literal(Literal::Number(n)), RValue::Local(value)) if n.to_bits() == expected => {
            Some(Term {
                value: value.clone(),
                index_on_left: true,
            })
        }
        _ => None,
    }
}

fn same_term(a: &Term, b: &Term) -> bool {
    a.value == b.value && a.index_on_left == b.index_on_left
}

fn expression(value: &RValue) -> Option<(Term, usize)> {
    let mut terms = Vec::new();
    let mut cursor = value;
    while let RValue::Binary(binary) = cursor {
        if binary.operation != BinaryOperation::Add || terms.len() == MAX_ITERATIONS {
            return None;
        }
        terms.push(&*binary.right);
        cursor = &binary.left;
    }
    if !zero(cursor) || terms.len() < MIN_ITERATIONS {
        return None;
    }
    terms.reverse();
    let first = term(terms[0], 1)?;
    for (index, value) in terms.iter().enumerate().skip(1) {
        if !same_term(&first, &term(value, index + 1)?) {
            return None;
        }
    }
    Some((first, terms.len()))
}

fn candidate(stmts: &[Statement], usage: &FxHashMap<RcLocal, Usage>) -> Option<Candidate> {
    if let Statement::Return(ret) = &stmts[0] {
        if ret.values.len() != 1 {
            return None;
        }
        let (term, iterations) = expression(&ret.values[0])?;
        return Some(Candidate {
            consumed: 1,
            result: None,
            term,
            iterations,
            returns: true,
        });
    }
    let (first, value) = declaration(&stmts[0])?;
    if !private_once(first, usage) {
        return None;
    }
    if let Some((term, iterations)) = expression(value) {
        if term.value == *first {
            return None;
        }
        return Some(Candidate {
            consumed: 1,
            result: Some(first.clone()),
            term,
            iterations,
            returns: false,
        });
    }
    if !zero(value) {
        return None;
    }
    let mut previous = first;
    let mut locals = vec![first];
    let mut matched_term = None;
    let mut iterations = 0;
    for statement in stmts.iter().skip(1) {
        let Some((local, value)) = declaration(statement) else {
            break;
        };
        let RValue::Binary(binary) = value else {
            break;
        };
        if binary.operation != BinaryOperation::Add
            || !matches!(&*binary.left, RValue::Local(l) if l == previous)
        {
            break;
        }
        if iterations == MAX_ITERATIONS
            || !private_once(local, usage)
            || usage.get(previous)?.reads != 1
            || locals.contains(&local)
        {
            return None;
        }
        let current = term(&binary.right, iterations + 1)?;
        if matched_term
            .as_ref()
            .is_some_and(|first| !same_term(first, &current))
        {
            return None;
        }
        matched_term = Some(current);
        previous = local;
        locals.push(local);
        iterations += 1;
    }
    if iterations < MIN_ITERATIONS {
        return None;
    }
    let matched_term = matched_term?;
    if locals.contains(&&matched_term.value) {
        return None;
    }
    // Do not erase intentionally distinct source names. Unrolled updates of one
    // debug binding share its spelling; absent names supply no stronger evidence.
    let final_names: Vec<String> = previous.0.lock().2.iter().map(|b| b.name.clone()).collect();
    for local in &locals[..locals.len() - 1] {
        let names = &local.0.lock().2;
        if names.iter().any(|binding| {
            final_names.is_empty() || final_names.iter().any(|name| *name != binding.name)
        }) {
            return None;
        }
    }
    Some(Candidate {
        consumed: iterations + 1,
        result: Some(previous.clone()),
        term: matched_term,
        iterations,
        returns: false,
    })
}

fn emit(candidate: Candidate) -> Vec<Statement> {
    let result_local = candidate
        .result
        .unwrap_or_else(|| RcLocal::new(Local::default()));
    let counter = RcLocal::new(Local::default());
    let index = RValue::Local(counter.clone());
    let value = RValue::Local(candidate.term.value);
    let (left, right) = if candidate.term.index_on_left {
        (index, value)
    } else {
        (value, index)
    };
    let product = Binary::new(left, right, BinaryOperation::Mul).into();
    let update = Assign::new(
        vec![LValue::Local(result_local.clone())],
        vec![Binary::new(
            RValue::Local(result_local.clone()),
            product,
            BinaryOperation::Add,
        )
        .into()],
    );
    let mut declaration = Assign::new(
        vec![LValue::Local(result_local.clone())],
        vec![Literal::Number(0.0).into()],
    );
    declaration.prefix = true;
    let mut result = vec![
        Comment::new(MARKER.to_string()).into(),
        declaration.into(),
        NumericFor::new(
            Literal::Number(1.0).into(),
            Literal::Number(candidate.iterations as f64).into(),
            Literal::Number(1.0).into(),
            counter,
            Block(vec![update.into()]),
        )
        .into(),
    ];
    if candidate.returns {
        result.push(Return::new(vec![RValue::Local(result_local)]).into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BindingOrigin, Closure, Function, SourceBinding, Upvalue};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.into())))
    }
    fn number(n: f64) -> RValue {
        Literal::Number(n).into()
    }
    fn bin(a: RValue, b: RValue, op: BinaryOperation) -> RValue {
        Binary::new(a, b, op).into()
    }
    fn product(x: &RcLocal, n: usize, left: bool) -> RValue {
        let value = RValue::Local(x.clone());
        let index = number(n as f64);
        if left {
            bin(index, value, BinaryOperation::Mul)
        } else {
            bin(value, index, BinaryOperation::Mul)
        }
    }
    fn sum(x: &RcLocal, n: usize, left: bool) -> RValue {
        (1..=n).fold(number(0.0), |a, i| {
            bin(a, product(x, i, left), BinaryOperation::Add)
        })
    }
    fn decl(x: &RcLocal, value: RValue) -> Statement {
        let mut a = Assign::new(vec![LValue::Local(x.clone())], vec![value]);
        a.prefix = true;
        a.into()
    }
    fn chain(x: &RcLocal, count: usize) -> (Block, Vec<RcLocal>) {
        let locals: Vec<_> = (0..=count).map(|i| local(&format!("result{i}"))).collect();
        let mut body = vec![decl(&locals[0], number(0.0))];
        for i in 1..=count {
            body.push(decl(
                &locals[i],
                bin(
                    RValue::Local(locals[i - 1].clone()),
                    product(x, i, false),
                    BinaryOperation::Add,
                ),
            ));
        }
        body.push(Return::new(vec![RValue::Local(locals[count].clone())]).into());
        (Block(body), locals)
    }
    fn loops(body: &Block) -> usize {
        body.0
            .iter()
            .filter(|s| matches!(s, Statement::NumericFor(_)))
            .count()
    }

    #[test]
    fn rerolls_return_and_preserves_multiplication_orientation() {
        for left in [false, true] {
            let x = local("x");
            let mut body = Block(vec![Return::new(vec![sum(&x, 4, left)]).into()]);
            reroll_arithmetic(&mut body);
            assert_eq!(loops(&body), 1);
            let Statement::NumericFor(nf) = &body.0[2] else {
                unreachable!()
            };
            assert_eq!(nf.initial, number(1.0));
            assert_eq!(nf.limit, number(4.0));
            assert_eq!(nf.step, number(1.0));
            let inner = nf.block.lock();
            let Statement::Assign(a) = &inner.0[0] else {
                unreachable!()
            };
            let RValue::Binary(add) = &a.right[0] else {
                unreachable!()
            };
            let RValue::Binary(mul) = &*add.right else {
                unreachable!()
            };
            let expected_left = RValue::Local(if left { nf.counter.clone() } else { x.clone() });
            assert_eq!(&*mul.left, &expected_left);
            drop(inner);
            let before = body.to_string();
            reroll_arithmetic(&mut body);
            assert_eq!(before, body.to_string());
        }
    }

    #[test]
    fn preserves_final_binding_and_same_source_name() {
        let x = local("x");
        let (mut body, locals) = chain(&x, 4);
        for (index, local) in locals.iter().enumerate() {
            local.0.lock().add_source_binding(SourceBinding {
                name: "result".into(),
                origin: BindingOrigin::DebugLocal {
                    prototype: 0,
                    register: index as u8,
                    start_pc: 0,
                    end_pc: 20,
                },
            });
        }
        reroll_arithmetic(&mut body);
        assert_eq!(loops(&body), 1);
        assert_eq!(declaration(&body.0[1]).unwrap().0, &locals[4]);
    }

    #[test]
    fn refuses_observed_intermediate_and_distinct_source_name() {
        for named in [false, true] {
            let x = local("x");
            let (mut body, locals) = chain(&x, 4);
            if named {
                locals[1].0.lock().add_source_binding(SourceBinding {
                    name: "firstContribution".into(),
                    origin: BindingOrigin::DebugLocal {
                        prototype: 0,
                        register: 1,
                        start_pc: 0,
                        end_pc: 20,
                    },
                });
            } else {
                body.0
                    .push(Return::new(vec![RValue::Local(locals[1].clone())]).into());
            }
            let before = body.to_string();
            reroll_arithmetic(&mut body);
            assert_eq!(before, body.to_string());
        }
    }

    #[test]
    fn refuses_captured_result_for_both_capture_modes() {
        for by_ref in [false, true] {
            let x = local("x");
            let result = local("result");
            let closure = Closure {
                function: ByAddress(Arc::new(Mutex::new(Function {
                    body: Block(vec![Return::new(vec![RValue::Local(result.clone())]).into()]),
                    ..Default::default()
                }))),
                upvalues: vec![if by_ref {
                    Upvalue::Ref(result.clone())
                } else {
                    Upvalue::Copy(result.clone())
                }],
            };
            let mut body = Block(vec![
                decl(&result, sum(&x, 4, false)),
                Return::new(vec![closure.into()]).into(),
            ]);
            let before = body.to_string();
            reroll_arithmetic(&mut body);
            assert_eq!(before, body.to_string());
        }
    }

    #[test]
    fn refuses_reassociation_gaps_and_multiple_results() {
        let x = local("x");
        let changed = bin(
            sum(&x, 3, false),
            product(&x, 5, false),
            BinaryOperation::Add,
        );
        let regrouped = bin(
            sum(&x, 2, false),
            bin(
                product(&x, 3, false),
                product(&x, 4, false),
                BinaryOperation::Add,
            ),
            BinaryOperation::Add,
        );
        for values in [
            vec![changed],
            vec![regrouped],
            vec![sum(&x, 4, false), number(7.0)],
        ] {
            let mut body = Block(vec![Return::new(values).into()]);
            let before = body.to_string();
            reroll_arithmetic(&mut body);
            assert_eq!(before, body.to_string());
        }
    }

    #[test]
    fn iteration_bounds_refuse_three_and_nine() {
        let x = local("x");
        for n in [3, 4, 8, 9] {
            let mut body = Block(vec![Return::new(vec![sum(&x, n, false)]).into()]);
            reroll_arithmetic(&mut body);
            assert_eq!(loops(&body), usize::from((4..=8).contains(&n)));
        }
    }

    #[test]
    fn existing_named_helper_has_priority_over_synthesized_loop() {
        let x = local("x");
        let parameter = local("parameter");
        let helper = local("weightedSum");
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(Function {
                bytecode_proto_id: Some(1),
                name: Some("weightedSum".into()),
                parameters: vec![parameter.clone()],
                body: Block(vec![Return::new(vec![sum(&parameter, 4, false)]).into()]),
                ..Default::default()
            }))),
            upvalues: vec![],
        };
        let mut body = Block(vec![
            decl(&helper, closure.into()),
            decl(&local("result"), sum(&x, 4, false)),
        ]);
        let before = body.to_string();
        reroll_arithmetic(&mut body);
        assert_eq!(before, body.to_string());
        crate::expr_deinline::expr_deinline(&mut body);
        let (_, value) = declaration(body.0.last().unwrap()).unwrap();
        assert!(matches!(value,RValue::Call(call) if call.value.as_ref()==&RValue::Local(helper)));
    }

    #[test]
    fn generated_counter_does_not_shadow_a_source_parameter() {
        let parameter = local("i");
        parameter.0.lock().add_source_binding(SourceBinding {
            name: "i".into(),
            origin: BindingOrigin::DebugLocal {
                prototype: 0,
                register: 0,
                start_pc: 0,
                end_pc: 20,
            },
        });
        let function = Arc::new(Mutex::new(Function {
            parameters: vec![parameter.clone()],
            body: Block(vec![Return::new(vec![sum(&parameter, 4, false)]).into()]),
            ..Default::default()
        }));
        let closure = Closure {
            function: ByAddress(function.clone()),
            upvalues: vec![],
        };
        let mut body = Block(vec![Return::new(vec![closure.into()]).into()]);
        reroll_arithmetic(&mut body);
        crate::name_locals::name_locals(&mut body, true);
        let function = function.lock();
        let Statement::NumericFor(nf) = &function.body.0[2] else {
            unreachable!()
        };
        assert_eq!(parameter.0.lock().0.as_deref(), Some("i"));
        let counter_name = nf.counter.0.lock().0.clone();
        assert_ne!(counter_name, parameter.0.lock().0);
    }
}
