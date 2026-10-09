//! A closure reads its by-value captures after its statement wrote its
//! targets (invariant I1 of [`Destructor::assert_captures_keep_their_values`]).
//! Each test runs the whole destructor, then evaluates the function before
//! and after: a returned closure resolves its captures when the function
//! returns, which is what the printed closure (capturing by reference) sees.

use super::*;
use ast::{Assign, Binary, BinaryOperation, Closure, If, Literal, RValue, Return, Upvalue};

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    Boolean(bool),
    /// A closure by creation order, with the locals it captures.
    Closure(usize, Vec<RcLocal>),
}

/// What a returned value is once the function has returned: a closure's
/// captures read their locals then, one level deep.
#[derive(Debug, PartialEq)]
enum Resolved {
    Number(f64),
    Boolean(bool),
    Closure(usize, Vec<Resolved>),
}

fn closure(upvalues: Vec<Upvalue>) -> RValue {
    Closure { node_origin: Default::default(), function: Default::default(), upvalues }.into()
}

fn number(value: f64) -> RValue {
    Literal::Number(value).into()
}

fn edge(arguments: Vec<(RcLocal, RValue)>) -> BlockEdge {
    BlockEdge { branch_type: BranchType::Unconditional, arguments }
}

fn evaluate(function: &Function, input: &[Value]) -> Vec<Resolved> {
    let mut created = 0;
    let mut value = |rvalue: &RValue, env: &FxHashMap<RcLocal, Value>| match rvalue {
        RValue::Local(local) => env.get(local).expect("read of an unassigned local").clone(),
        RValue::Literal(Literal::Number(n)) => Value::Number(*n),
        RValue::Binary(Binary { left, right, operation: BinaryOperation::Add, .. }) => {
            match (&**left, &**right) {
                (RValue::Local(local), RValue::Literal(Literal::Number(n))) => match env[local] {
                    Value::Number(m) => Value::Number(m + n),
                    _ => panic!("arithmetic on a non-number"),
                },
                _ => panic!("unexpected operands"),
            }
        }
        RValue::Closure(closure) => {
            created += 1;
            Value::Closure(created, closure.upvalues.iter().map(|upvalue| {
                let (Upvalue::Copy(local) | Upvalue::Ref(local)) = upvalue;
                local.clone()
            }).collect())
        }
        _ => panic!("unexpected expression {rvalue}"),
    };
    fn resolve(value: &Value, env: &FxHashMap<RcLocal, Value>, depth: usize) -> Resolved {
        match value {
            Value::Number(n) => Resolved::Number(*n),
            Value::Boolean(b) => Resolved::Boolean(*b),
            Value::Closure(id, captures) if depth == 0 => Resolved::Closure(*id, captures.iter()
                .map(|local| resolve(&env[local], env, 1)).collect()),
            Value::Closure(id, _) => Resolved::Closure(*id, Vec::new()),
        }
    }
    let mut env: FxHashMap<RcLocal, Value> = function.parameters.iter().cloned().zip(input.iter().cloned()).collect();
    let mut node = function.entry().unwrap();
    for _ in 0..32 {
        let mut branch = BranchType::Unconditional;
        for statement in function.block(node).unwrap().iter() {
            match statement {
                ast::Statement::Assign(assign) => {
                    // A closure statement writes before its captures are read.
                    let values = assign.right.iter().map(|rvalue| value(rvalue, &env)).collect::<Vec<_>>();
                    for (left, value) in assign.left.iter().zip(values) {
                        env.insert(left.as_local().unwrap().clone(), value);
                    }
                }
                ast::Statement::If(r#if) => {
                    branch = if value(&r#if.condition, &env) != Value::Boolean(false) {
                        BranchType::Then
                    } else {
                        BranchType::Else
                    };
                }
                ast::Statement::Return(r#return) => {
                    let values = r#return.values.iter().map(|rvalue| value(rvalue, &env)).collect::<Vec<_>>();
                    return values.iter().map(|value| resolve(value, &env, 0)).collect();
                }
                ast::Statement::Empty(_) => {}
                _ => panic!("unexpected statement {statement}"),
            }
        }
        let edge = function.edges(node).find(|edge| edge.weight().branch_type == branch).unwrap();
        let arguments = edge.weight().arguments.iter().map(|(param, argument)| (param.clone(), value(argument, &env)))
            .collect::<Vec<_>>();
        env.extend(arguments);
        node = edge.target();
    }
    panic!("too many steps")
}

/// `entry: if flag` with one block per arm, both jumping to `join`, which
/// returns `returned`. The arms carry their statements and edge arguments.
fn diamond(
    parameters: Vec<RcLocal>,
    flag: &RcLocal,
    then_arm: (Vec<ast::Statement>, Vec<(RcLocal, RValue)>),
    else_arm: (Vec<ast::Statement>, Vec<(RcLocal, RValue)>),
    returned: Vec<RValue>,
) -> Function {
    let mut function = Function::new(0);
    function.parameters = parameters;
    let entry = function.new_block();
    let then_block = function.new_block();
    let else_block = function.new_block();
    let join = function.new_block();
    function.set_entry(entry);
    function.block_mut(entry).unwrap().push(If::new(flag.clone().into(), Default::default(), Default::default()).into());
    function.block_mut(then_block).unwrap().extend(then_arm.0);
    function.block_mut(else_block).unwrap().extend(else_arm.0);
    function.block_mut(join).unwrap().push(Return::new(returned).into());
    function.set_edges(entry, vec![
        (then_block, BlockEdge::new(BranchType::Then)),
        (else_block, BlockEdge::new(BranchType::Else)),
    ]);
    function.set_edges(then_block, vec![(join, edge(then_arm.1))]);
    function.set_edges(else_block, vec![(join, edge(else_arm.1))]);
    function
}

/// Destructs `function`, asserting it computes what it did for every input.
fn destruct_preserving(mut function: Function, inputs: &[Vec<Value>]) -> Function {
    let before = inputs.iter().map(|input| evaluate(&function, input)).collect::<Vec<_>>();
    Destructor::new(&mut function, IndexMap::default(), FxHashSet::default(), 32).destruct();
    let after = inputs.iter().map(|input| evaluate(&function, input)).collect::<Vec<_>>();
    assert_eq!(before, after, "destruct changed what the function returns:\n{:?}", function);
    function
}

/// Every closure statement: its targets and its by-value captures.
fn closure_statements(function: &Function) -> Vec<(Vec<RcLocal>, Vec<RcLocal>)> {
    let mut found = Vec::new();
    for (_, block) in function.blocks() {
        for statement in block.iter() {
            let mut captures = Vec::new();
            statement.traverse_rvalues_ref(&mut |rvalue| {
                if let RValue::Closure(closure) = rvalue {
                    captures.extend(closure.upvalues.iter().filter_map(|upvalue| match upvalue {
                        Upvalue::Copy(local) => Some(local.clone()),
                        Upvalue::Ref(_) => None,
                    }));
                }
            });
            if !captures.is_empty() {
                found.push((statement.values_written().into_iter().cloned().collect(), captures));
            }
        }
    }
    found
}

fn inputs() -> Vec<Vec<Value>> {
    vec![
        vec![Value::Number(7.0), Value::Boolean(true), Value::Number(9.0)],
        vec![Value::Number(7.0), Value::Boolean(false), Value::Number(9.0)],
    ]
}

/// `local failure = reject; if tag then failure = function() reject() end`:
/// the phi's closure argument captures the value the other edge passes.
#[test]
fn closure_on_an_edge_keeps_its_capture_apart_from_the_phi() {
    let (p, flag, other, phi) = (RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default());
    let function = diamond(
        vec![p.clone(), flag.clone(), other],
        &flag,
        (vec![], vec![(phi.clone(), closure(vec![Upvalue::Copy(p.clone())]))]),
        (vec![], vec![(phi.clone(), p.clone().into())]),
        vec![phi.into()],
    );
    let function = destruct_preserving(function, &inputs());
    for (targets, captures) in closure_statements(&function) {
        assert!(captures.iter().all(|capture| !targets.contains(capture)), "{targets:?} {captures:?}");
    }
}

/// `d = function() p() end` keeps its own statement and a transport copies
/// `d` into the phi.
#[test]
fn closure_statement_keeps_its_capture_apart_from_its_target() {
    let (p, flag, other, d, phi) =
        (RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default());
    let function = diamond(
        vec![p.clone(), flag.clone(), other],
        &flag,
        (
            vec![Assign::new(vec![d.clone().into()], vec![closure(vec![Upvalue::Copy(p.clone())])]).into()],
            vec![(phi.clone(), d.into())],
        ),
        (vec![], vec![(phi.clone(), p.clone().into())]),
        vec![phi.into()],
    );
    let function = destruct_preserving(function, &inputs());
    for (targets, captures) in closure_statements(&function) {
        assert!(captures.iter().all(|capture| !targets.contains(capture)), "{targets:?} {captures:?}");
    }
}

/// `x, y <- function() b() end, 5` on one edge and `x, y <- a, b` on the
/// other: `b` must not become `y`, which the same parallel copy writes.
#[test]
fn parallel_transport_keeps_a_capture_apart_from_the_other_target() {
    let (a, b, flag, x, y) =
        (RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default());
    let function = diamond(
        vec![a.clone(), flag.clone(), b.clone()],
        &flag,
        (vec![], vec![(x.clone(), closure(vec![Upvalue::Copy(b.clone())])), (y.clone(), number(5.0))]),
        (vec![], vec![(x.clone(), a.clone().into()), (y.clone(), b.clone().into())]),
        vec![x.into(), y.into()],
    );
    let function = destruct_preserving(function, &inputs());
    for (targets, captures) in closure_statements(&function) {
        assert!(captures.iter().all(|capture| !targets.contains(capture)), "{targets:?} {captures:?}");
    }
}

/// The self capture `d = function() d() end` is the closure's own value: it
/// still joins the phi with the parameter, with no copy of `d`.
#[test]
fn self_capture_still_coalesces_with_the_phi() {
    let (p, flag, other, d, phi) =
        (RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default());
    let function = diamond(
        vec![p.clone(), flag.clone(), other],
        &flag,
        (
            vec![Assign::new(vec![d.clone().into()], vec![closure(vec![Upvalue::Copy(d.clone())])]).into()],
            vec![(phi.clone(), d.into())],
        ),
        (vec![], vec![(phi.clone(), p.clone().into())]),
        vec![phi.into()],
    );
    let function = destruct_preserving(function, &inputs());
    let statements = closure_statements(&function);
    assert_eq!(statements.len(), 1);
    let (targets, captures) = &statements[0];
    assert_eq!(targets, captures, "the closure still captures its own variable");
    // One variable for `p`, `d` and the phi: no statement besides the closure.
    let copies = function.blocks().map(|(_, block)| block.iter()
        .filter(|statement| statement.as_assign().is_some()).count()).sum::<usize>();
    assert_eq!(copies, 1, "{function:?}");
}

/// An ordinary read at the defining statement (`x2 = x1 + 1`) still lets
/// `x1` and `x2` share one variable.
#[test]
fn ordinary_read_at_the_definition_still_coalesces() {
    let (x1, flag, other, x2, phi) =
        (RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default());
    let function = diamond(
        vec![x1.clone(), flag.clone(), other],
        &flag,
        (
            vec![Assign::new(vec![x2.clone().into()],
                vec![Binary::new(x1.clone().into(), number(1.0), BinaryOperation::Add).into()]).into()],
            vec![(phi.clone(), x2.into())],
        ),
        (vec![], vec![(phi.clone(), x1.clone().into())]),
        vec![phi.into()],
    );
    let function = destruct_preserving(function, &inputs());
    let update = function.blocks().flat_map(|(_, block)| block.iter())
        .find_map(|statement| statement.as_assign().filter(|assign| matches!(assign.right[0], RValue::Binary(_))))
        .expect("the update survives");
    let reads = update.right[0].values_read();
    assert_eq!(update.left[0].as_local(), Some(reads[0]), "`x = x + 1` in one variable");
}

/// `v', x <- v, function() v() end`: the target holding `v`'s own value may
/// share `v`'s variable, as the closure sees the same value either way.
#[test]
fn value_equal_target_may_share_the_captured_variable() {
    let (v, flag, other, copy, x) =
        (RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default(), RcLocal::default());
    let function = diamond(
        vec![v.clone(), flag.clone(), other.clone()],
        &flag,
        (vec![], vec![(copy.clone(), v.clone().into()), (x.clone(), closure(vec![Upvalue::Copy(v.clone())]))]),
        (vec![], vec![(copy.clone(), v.clone().into()), (x.clone(), other.into())]),
        vec![copy.into(), x.into()],
    );
    let function = destruct_preserving(function, &inputs());
    let join = function.blocks().find_map(|(_, block)| match block.last() {
        Some(ast::Statement::Return(r#return)) => Some(r#return.values.clone()),
        _ => None,
    }).unwrap();
    assert_eq!(join[0], RValue::Local(v), "the copy of `v` is `v` itself");
}

/// Only by-value captures are recorded; a cell (`Ref`) is one variable by
/// construction and keeps the cell rules.
#[test]
fn reference_captures_are_not_value_captures() {
    for by_value in [false, true] {
        let (p, d) = (RcLocal::default(), RcLocal::default());
        let mut function = Function::new(0);
        function.parameters = vec![p.clone()];
        let entry = function.new_block();
        function.set_entry(entry);
        let upvalue = if by_value { Upvalue::Copy(p.clone()) } else { Upvalue::Ref(p.clone()) };
        function.block_mut(entry).unwrap().extend([
            Assign::new(vec![d.clone().into()], vec![closure(vec![upvalue])]).into(),
            Return::new(vec![d.into()]).into(),
        ]);
        let mut destructor = Destructor::new(&mut function, IndexMap::default(), FxHashSet::default(), 4);
        destructor.build_def_use();
        assert_eq!(destructor.value_captures.contains(&(p, 0, 0)), by_value);
        assert_eq!(destructor.value_captures.len(), usize::from(by_value));
    }
}
