//! Independent legacy oracle for capture proofs, target order and emitted metadata.
use super::*;
use crate::{Assign, Binary, BinaryOperation as Op, Closure, Global, If, IfExpression,
    Index, Literal, Local, Return, Select, Unary, UnaryOperation, Upvalue};
use by_address::ByAddress;
fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
fn global(name: &str) -> RValue { Global::from(name).into() }
fn number(value: f64) -> RValue { Literal::Number(value).into() }
fn assign(local: &RcLocal, value: RValue) -> Statement {
    let mut result = Assign::new(vec![local.clone().into()], vec![value]); result.prefix = true; result.into()
}
fn predicate(value: RValue) -> RValue {
    Binary::new(
        Binary::new(Call::new(global("typeof"), vec![value.clone()]).into(),
            Literal::String(b"number".to_vec()).into(), Op::Equal).into(),
        Binary::new(value, number(0.0), Op::GreaterThan).into(), Op::And).into()
}
fn arithmetic(value: RValue) -> RValue {
    Binary::new(Binary::new(Binary::new(value, number(2.0), Op::Mul).into(),
        number(3.0), Op::Add).into(), number(4.0), Op::Mul).into()
}
fn closure(function: &Arc<Mutex<Function>>, upvalues: Vec<Upvalue>) -> RValue {
    Closure { node_origin: Default::default(), function: ByAddress(function.clone()), upvalues }.into()
}
struct Fixture { block: Block, locals: Vec<RcLocal>, functions: Vec<Arc<Mutex<Function>>> }
impl Fixture {
    fn owners(&self) -> Vec<usize> {
        self.locals.iter().map(|local| Arc::strong_count(&local.0.0))
            .chain(self.functions.iter().map(Arc::strong_count)).collect()
    }
}
fn fixture(seed: usize) -> Fixture {
    crate::reset_local_ids();
    let locals = ["predicate", "arithmetic", "argument", "result", "parameter", "captured", "rival"].map(local).to_vec();
    let [legacy, arith, arg, result, param, captured, rival] = locals.as_slice() else { unreachable!() };
    let legacy_fn = Arc::new(Mutex::new(Function {
        name: Some("predicate".into()), parameters: vec![param.clone()],
        body: Block(vec![Return::new(vec![predicate(param.clone().into())]).into()]), ..Default::default()
    }));
    let arithmetic_fn = Arc::new(Mutex::new(Function {
        bytecode_proto_id: Some(7), name: Some("arithmetic".into()), parameters: vec![param.clone()],
        body: Block(vec![Return::new(vec![arithmetic(param.clone().into())]).into()]), ..Default::default()
    }));
    let child_fn = Arc::new(Mutex::new(Function {
        body: Block(vec![Return::new(vec![predicate(arg.clone().into()), arithmetic(arg.clone().into())]).into()]),
        ..Default::default()
    }));
    let argument = match seed % 10 {
        0 => number(-0.0), 1 => number(f64::from_bits(0x7ff8_0000_0000_0023)),
        2 => number(std::f64::consts::PI), 3 => number(f64::INFINITY),
        4 => Literal::Boolean(false).into(), 5 => Literal::Nil.into(),
        6 => Literal::String(vec![0, 255]).into(),
        7 => Binary::new(arg.clone().into(), number(2.0), Op::Add).into(),
        _ => arg.clone().into(),
    };
    let mut block = Block(vec![
        assign(legacy, closure(&legacy_fn, vec![])),
        assign(arith, closure(&arithmetic_fn, vec![])),
    ]);
    if seed % 7 == 0 { block.0.push(assign(rival, closure(&arithmetic_fn, vec![]))); }
    if seed % 11 == 0 { block.0.push(Assign::new(vec![legacy.clone().into()], vec![global("replacement")]).into()); }
    let captures = if seed & 1 != 0 { vec![Upvalue::Ref(arg.clone()), Upvalue::Copy(captured.clone())] }
        else { vec![Upvalue::Copy(arg.clone()), Upvalue::Ref(captured.clone())] };
    // Indexed LHS roots, table keys and closure aliases are included by both
    // capture censuses, but closure body writes retain occurrence semantics.
    block.0.push(Assign::new(vec![Index::new(
        closure(&child_fn, captures), closure(&child_fn, vec![Upvalue::Copy(arg.clone())])).into()],
        vec![crate::Table::new(vec![(Some(closure(&child_fn, vec![])), number(1.0))]).into()]).into());
    block.0.push(assign(result, Unary::new(predicate(argument.clone()), UnaryOperation::Not).into()));
    block.0.push(assign(result, arithmetic(argument)));
    block.0.push(If::new(global("condition"), Block(vec![
        Return::new(vec![predicate(arg.clone().into())]).into()
    ]), Block(vec![Return::new(vec![arithmetic(arg.clone().into())]).into()])).into());
    if seed % 5 == 0 { block.0.rotate_left(2); }
    annotate_block(&mut block, &mut 0);
    Fixture { block, locals, functions: vec![legacy_fn, arithmetic_fn, child_fn] }
}
fn annotate(origin: &mut crate::node_origins::Origin, index: &mut usize) {
    *index += 1;
    *origin = crate::node_origins::Origin::input(crate::node_origins::Input {
        function: "cleanup_differential".into(), block: *index / 13,
        statement: *index, value: Some(*index % 17),
    });
    let data = origin.0.as_mut().unwrap();
    data.inlined = *index & 1 != 0;
    data.cloned = *index & 2 != 0;
    data.incomplete = *index & 4 != 0;
    data.synthesized = (*index & 8 != 0).then_some("fixture");
    if *index & 16 != 0 { data.inputs.push(data.inputs[0].clone()); }
}
fn annotate_value(value: &mut RValue, index: &mut usize) {
    if let Some(origin) = crate::node_origins::value_mut(value) { annotate(origin, index); }
    if let RValue::Closure(closure) = value { annotate_block(&mut closure.function.lock().body, index); }
    for child in value.rvalues_mut() { annotate_value(child, index); }
}
fn annotate_block(block: &mut Block, index: &mut usize) {
    for statement in &mut block.0 {
        if let Some(origin) = crate::node_origins::statement_mut(statement) { annotate(origin, index); }
        for value in crate::deinline::stmt_rvalues_mut(statement) { annotate_value(value, index); }
        match statement {
            Statement::If(node) => {
                annotate_block(&mut node.then_block.lock(), index);
                annotate_block(&mut node.else_block.lock(), index);
            }
            Statement::While(node) => annotate_block(&mut node.block.lock(), index),
            Statement::Repeat(node) => annotate_block(&mut node.block.lock(), index),
            Statement::NumericFor(node) => annotate_block(&mut node.block.lock(), index),
            Statement::GenericFor(node) => annotate_block(&mut node.block.lock(), index),
            _ => {}
        }
    }
}
type OriginSnapshot = Option<(Vec<std::sync::Arc<crate::node_origins::Input>>, bool, bool, bool, Option<&'static str>)>;
#[derive(Debug, Default, PartialEq)]
struct Snapshot {
    origins: Vec<OriginSnapshot>, bits: Vec<u64>, locals: Vec<u64>,
    closure_aliases: Vec<usize>, events: Vec<u32>,
    // Kept outside PartialEq: only the occurrence classes above are compared.
    functions: Vec<usize>,
}
impl Snapshot {
    fn origin(&mut self, origin: &crate::node_origins::Origin) {
        self.origins.push(origin.0.as_ref().map(|data| (data.inputs.clone(), data.inlined,
            data.cloned, data.incomplete, data.synthesized)));
    }
    fn value(&mut self, value: &RValue) {
        if let Some(origin) = crate::node_origins::value(value) { self.origin(origin); }
        match value {
            RValue::Call(call) | RValue::Select(Select::Call(call)) => self.events.push(call.reconstruction_event),
            RValue::Literal(Literal::Number(number)) => self.bits.push(number.to_bits()),
            RValue::Local(local) => self.locals.push(local.stable_id()),
            RValue::Closure(closure) => {
                let address = Arc::as_ptr(&closure.function.0) as usize;
                let identity = self.functions.iter().position(|seen| *seen == address)
                    .unwrap_or_else(|| { self.functions.push(address); self.functions.len() - 1 });
                self.closure_aliases.push(identity);
                self.block(&closure.function.lock().body);
            }
            _ => {}
        }
        for child in value.rvalues() { self.value(child); }
    }
    fn block(&mut self, block: &Block) {
        for statement in &block.0 {
            if let Some(origin) = crate::node_origins::statement(statement) { self.origin(origin); }
            for value in crate::deinline::stmt_rvalues(statement) { self.value(value); }
            self.locals.extend(statement.values_written().into_iter().map(RcLocal::stable_id));
            match statement {
                Statement::Call(call) => self.events.push(call.reconstruction_event),
                _ => {}
            }
            match statement {
                Statement::If(node) => {
                    self.block(&node.then_block.lock());
                    self.block(&node.else_block.lock());
                }
                Statement::While(node) => self.block(&node.block.lock()),
                Statement::Repeat(node) => self.block(&node.block.lock()),
                Statement::NumericFor(node) => self.block(&node.block.lock()),
                Statement::GenericFor(node) => self.block(&node.block.lock()),
                _ => {}
            }
        }
    }
}
fn snapshot(block: &Block) -> Snapshot {
    let mut result = Snapshot::default();
    result.block(block);
    result.functions.clear();
    result
}
// Independent fixtures have different addresses. Number addresses by first
// occurrence to retain the complete identity/alias pattern in the Debug shape.
fn shape(block: &Block) -> String {
    let debug = format!("{block:?}");
    let mut remaining = debug.as_str();
    let mut result = String::new();
    let mut identities = FxHashMap::default();
    while let Some(index) = remaining.find(" @ 0x") {
        result.push_str(&remaining[..index]);
        remaining = &remaining[index + 5..];
        let end = remaining.bytes().take_while(u8::is_ascii_hexdigit).count();
        let next = identities.len();
        let identity = *identities.entry(&remaining[..end]).or_insert(next);
        result.push_str(&format!(" @ #{identity}"));
        remaining = &remaining[end..];
    }
    result.push_str(remaining);
    result
}


#[derive(Debug, PartialEq)]
struct TargetSnapshot { binding: u64, function_class: usize, parameters: Vec<u64>, arithmetic: bool,
    protect_definition: bool, shape: String, source: String, metadata: Snapshot }
macro_rules! target_snapshot {
    ($targets:expr) => {{
        let mut functions = FxHashMap::default();
        $targets.into_iter().map(|target| {
            let next = functions.len();
            let function_class = *functions.entry(target.func_ptr as usize).or_insert(next);
            let block = Block(vec![Return::new(vec![target.expr]).into()]);
            TargetSnapshot { binding: target.f_local.stable_id(), function_class,
                parameters: target.param_order.iter().map(RcLocal::stable_id).collect(),
                arithmetic: target.arithmetic.is_some(), protect_definition: target.protect_definition,
                shape: shape(&block), source: block.to_string(), metadata: snapshot(&block) }
        }).collect::<Vec<_>>()
    }}
}

#[test]
fn expression_pass_matches_legacy_targets_rewrites_origins_and_events() {
    for seed in 0..64 {
        for arithmetic_only in [false, true] {
            let mut actual = fixture(seed);
            let scope = crate::call_origins::enter(true);
            let target_keys = target_snapshot!(collect_expr_targets(&actual.block, false));
            run(&mut actual.block, arithmetic_only);
            let actual_report = format!("{:?}", scope.take_report());
            let actual_shape = shape(&actual.block);
            let actual_source = actual.block.to_string();
            let actual_snapshot = snapshot(&actual.block);
            let actual_owners = actual.owners();
            let mut expected = fixture(seed);
            let scope = crate::call_origins::enter(true);
            assert_eq!(target_snapshot!(reference::collect_expr_targets(&expected.block)), target_keys,
                "target eligibility/order seed {seed}");
            if arithmetic_only { reference::arithmetic_deinline_early(&mut expected.block); }
            else { reference::expr_deinline(&mut expected.block); }
            assert_eq!(format!("{:?}", scope.take_report()), actual_report, "events seed {seed}");
            assert_eq!(shape(&expected.block), actual_shape, "shape seed {seed}, early {arithmetic_only}");
            assert_eq!(expected.block.to_string(), actual_source);
            assert_eq!(snapshot(&expected.block), actual_snapshot, "origins seed {seed}, early {arithmetic_only}");
            assert_eq!(expected.owners(), actual_owners);
        }
    }
}

#[test]
fn complete_shared_capture_proof_implies_legacy_arithmetic_gate() {
    for seed in 0..64 {
        let fixture = fixture(seed);
        let common = crate::deinline_safety::CaptureSafety::new(&fixture.block);
        assert!(common.complete());
        let legacy = reference::arithmetic::Safety::new(&fixture.block);
        // The legacy gate refused every reference capture. A cell written
        // only by its declaration never changes, so the shared proof admits
        // the ones never assigned again.
        let rebound: Vec<u64> = fixture.block.0.iter().filter_map(|statement| match statement {
            Statement::Assign(assign) if !assign.prefix => Some(assign.left.iter().filter_map(|left| left.as_local().map(RcLocal::stable_id))),
            _ => None,
        }).flatten().collect();
        for local in &fixture.locals {
            let value = RValue::Local(local.clone());
            assert_eq!(common.stable(&value), legacy.stable(&value) || !rebound.contains(&local.stable_id()),
                "local capture set seed {seed}");
        }
        for value in [Literal::Nil, Literal::Boolean(false), Literal::String(vec![0, 255]),
            Literal::Number(-0.0), Literal::Number(f64::NAN), Literal::Number(f64::INFINITY),
            Literal::Number(std::f64::consts::PI), Literal::Vector(1.0, 2.0, 3.0), Literal::VectorD(1.0, 2.0, 3.0)] {
            let value = RValue::Literal(value);
            assert!(!common.stable(&value) || legacy.stable(&value));
        }
        for value in [global("source"), Call::new(global("source"), vec![]).into(),
            crate::Table::default().into(), arithmetic(number(1.0))] {
            assert!(!common.stable(&value));
        }
    }
}

#[test]
fn target_count_caps_preserve_every_legacy_eligibility_decision() {
    for arithmetic_family in [false, true] {
        for count in [31, 32, 33, 255, 256, 257] {
            let make = || {
                crate::reset_local_ids();
                let mut block = Block::default();
                for index in 0..count {
                    let binder = local(&format!("helper{index}"));
                    let parameter = local("parameter");
                    let function = Arc::new(Mutex::new(Function {
                        bytecode_proto_id: arithmetic_family.then_some(index),
                        name: Some(format!("helper{index}")), parameters: vec![parameter.clone()],
                        body: Block(vec![Return::new(vec![if arithmetic_family {
                            arithmetic(parameter.into())
                        } else { predicate(parameter.into()) }]).into()]), ..Default::default()
                    }));
                    block.0.push(assign(&binder, closure(&function, vec![])));
                }
                block
            };
            let actual = make();
            let keys = target_snapshot!(collect_expr_targets(&actual, false));
            let expected = make();
            assert_eq!(keys, target_snapshot!(reference::collect_expr_targets(&expected)),
                "arithmetic {arithmetic_family}, count {count}");
        }
    }
}

#[test]
fn common_budget_refusal_still_precedes_target_collection() {
    for depth in [126, 127, 128, 129] {
        let make = || {
            let mut fixture = fixture(8);
            let mut value = global("depth");
            for _ in 0..depth { value = Unary::new(value, UnaryOperation::Not).into(); }
            fixture.block.0.push(Return::new(vec![value]).into());
            fixture
        };
        let actual = make();
        let keys = target_snapshot!(collect_expr_targets(&actual.block, false));
        let expected = make();
        assert_eq!(keys, target_snapshot!(reference::collect_expr_targets(&expected.block)), "depth {depth}");
    }
}

#[test]
fn capture_implication_preserves_all_shallow_statement_domains() {
    let captured = local("captured");
    let result = local("result");
    let empty = Arc::new(Mutex::new(Function::default()));
    for location in 0..15 {
        let captured_value = || closure(&empty, vec![Upvalue::Ref(captured.clone())]);
        let statement: Statement = match location {
            0 => Return::new(vec![captured_value()]).into(),
            1 => Assign::new(vec![Index::new(captured_value(), number(1.0)).into()], vec![number(1.0)]).into(),
            2 => Assign::new(vec![Index::new(global("table"), captured_value()).into()], vec![number(1.0)]).into(),
            3 => Call::new(captured_value(), vec![]).into(),
            4 => crate::MethodCall::new(global("obj"), "method".into(), vec![captured_value()]).into(),
            5 => Return::new(vec![crate::Table::new(vec![(Some(captured_value()), number(1.0))]).into()]).into(),
            6 => Return::new(vec![IfExpression::new(captured_value(), number(1.0), number(2.0)).into()]).into(),
            7 => Return::new(vec![Select::Call(Call::new(captured_value(), vec![])).into()]).into(),
            8 => crate::NumericFor::new(captured_value(), number(3.0), number(1.0), result.clone(), Block::default()).into(),
            9 => crate::GenericFor::new(vec![result.clone()], vec![captured_value()], Block::default()).into(),
            10 => crate::SetList::new(result.clone(), 1, vec![], Some(captured_value())).into(),
            11 => crate::While::new(global("looping"), Block(vec![Return::new(vec![captured_value()]).into()])).into(),
            12 => {
                let nested = Arc::new(Mutex::new(Function {
                    body: Block(vec![Return::new(vec![captured_value()]).into()]), ..Default::default()
                }));
                Return::new(vec![closure(&nested, vec![]), closure(&nested, vec![])]).into()
            }
            13 => crate::NumForInit {
                counter: (result.clone().into(), captured_value()),
                limit: (result.clone().into(), number(2.0)), step: (result.clone().into(), number(1.0)),
            }.into(),
            _ => If::new(global("condition"), Block::default(), Block(vec![Return::new(vec![captured_value()]).into()])).into(),
        };
        // Rebound, so a reference capture anywhere makes it unstable.
        let block = Block(vec![Assign::new(vec![captured.clone().into()], vec![number(0.0)]).into(), statement]);
        let common = crate::deinline_safety::CaptureSafety::new(&block);
        let legacy = reference::arithmetic::Safety::new(&block);
        assert!(common.complete());
        let value = RValue::Local(captured.clone());
        assert_eq!(common.stable(&value), legacy.stable(&value), "location {location}");
        assert_eq!(common.stable(&value), location == 13, "internal markers stay outside both selectors");
    }
}

#[test]
fn arithmetic_attempt_boundary_preserves_late_match_refusal() {
    for failed_attempts in [8191, 8192] {
        let make = || {
            crate::reset_local_ids();
            let binder = local("arithmetic");
            let parameter = local("parameter");
            let argument = local("argument");
            let function = Arc::new(Mutex::new(Function {
                bytecode_proto_id: Some(7), name: Some("arithmetic".into()), parameters: vec![parameter.clone()],
                body: Block(vec![Return::new(vec![arithmetic(parameter.into())]).into()]), ..Default::default()
            }));
            let mut values: Vec<RValue> = (0..failed_attempts).map(|_| {
                Binary::new(argument.clone().into(), number(0.0), Op::Equal).into()
            }).collect();
            values.push(arithmetic(argument.into()));
            Block(vec![assign(&binder, closure(&function, vec![])), Return::new(values).into()])
        };
        let mut actual = make();
        run(&mut actual, false);
        let mut expected = make();
        reference::run(&mut expected, false);
        assert_eq!(shape(&actual), shape(&expected));
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(snapshot(&actual), snapshot(&expected));
        let Statement::Return(result) = actual.0.last().unwrap() else { panic!("return must remain"); };
        assert_eq!(matches!(result.values.last(), Some(RValue::Call(_))), failed_attempts == 8191);
    }
}

#[test]
fn repeated_root_priority_cache_preserves_late_rivals_scopes_and_metadata() {
    fn make(count: usize, rival: bool) -> (Block, Vec<Arc<Mutex<Function>>>) {
        crate::reset_local_ids();
        let mut block = Block::default();
        let mut functions = Vec::new();
        let argument = local("argument");
        let expression = |value: RValue, tag: usize| -> RValue {
            Binary::new(predicate(value), Literal::String(format!("tag{tag}").into_bytes()).into(), Op::And).into()
        };
        for index in 0..count {
            let parameter = local("parameter");
            let tag = if rival && index + 1 == count { 0 } else { index };
            let function = Arc::new(Mutex::new(Function {
                parameters: vec![parameter.clone()],
                body: Block(vec![Return::new(vec![expression(parameter.into(), tag)]).into()]), ..Default::default()
            }));
            block.0.push(assign(&local(&format!("helper{index}")), closure(&function, vec![])));
            functions.push(function);
        }
        let caller = Arc::new(Mutex::new(Function {
            body: Block(vec![Return::new((0..24).map(|index| expression(argument.clone().into(), index % count)).collect()).into()]),
            ..Default::default()
        }));
        block.0.push(assign(&local("caller"), closure(&caller, vec![Upvalue::Copy(argument)])));
        functions.push(caller);
        annotate_block(&mut block, &mut 0);
        (block, functions)
    }
    for count in [2, 8, 32] {
        for rival in [false, true] {
            let lines: Vec<_> = (0..=count).map(|index| vec![Some(if index + 1 >= count { 7 } else { 3 })]).collect();
            let (mut actual, functions) = make(count, rival);
            {
                let _scope = crate::reconstruction_search::enter(lines.clone());
                for (index, function) in functions.iter().enumerate() {
                    crate::reconstruction_search::register_function(Arc::as_ptr(function) as usize, index);
                }
                priority_tests::reset_builds();
                run(&mut actual, false);
                assert!(priority_tests::builds() <= functions.len(), "one root bucket per visited function");
            }
            let (mut expected, functions) = make(count, rival);
            {
                let _scope = crate::reconstruction_search::enter(lines);
                for (index, function) in functions.iter().enumerate() {
                    crate::reconstruction_search::register_function(Arc::as_ptr(function) as usize, index);
                }
                reference::run(&mut expected, false);
            }
            assert_eq!(actual.to_string(), expected.to_string(), "{count} helpers, rival {rival}");
            assert_eq!(shape(&actual), shape(&expected));
            assert_eq!(snapshot(&actual), snapshot(&expected));
        }
    }
}
