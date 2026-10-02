use super::*;
use crate::{Assign, Binary, BinaryOperation, Closure, Function, Global, IfExpression, Index, Literal, Local, Return, Table, Upvalue};
use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
struct OriginState {
    inputs: Vec<crate::node_origins::Input>, inlined: bool, cloned: bool,
    synthesized: Option<&'static str>, incomplete: bool,
}

fn origin_state(origin: Option<&crate::node_origins::Origin>) -> Option<OriginState> {
    origin.and_then(|origin| origin.0.as_ref()).map(|data| OriginState {
        inputs: data.inputs.iter().map(|input| (**input).clone()).collect(),
        inlined: data.inlined, cloned: data.cloned,
        synthesized: data.synthesized, incomplete: data.incomplete,
    })
}

fn origin(index: usize) -> crate::node_origins::Origin {
    let mut tag = crate::node_origins::Origin::input(crate::node_origins::Input {
        function: "owned:p0".into(), block: 0, statement: index, value: Some(index),
    });
    let data = tag.0.as_mut().unwrap();
    data.inlined = index % 3 == 1;
    data.cloned = index % 4 == 2;
    data.incomplete = index % 5 == 3;
    data.synthesized = Some("owned_fixture");
    tag
}

fn seed(value: &mut RValue, index: &mut usize) {
    if let Some(tag) = crate::node_origins::value_mut(value) { *tag = origin(*index); }
    *index += 1;
    value.visit_rvalues_mut(&mut |child| { seed(child, index); true });
}

#[derive(Debug, PartialEq, Eq)]
struct State {
    source: String, origins: Vec<Option<OriginState>>, numbers: Vec<u64>,
    locals: Vec<u64>, closures: Vec<(usize, Vec<(bool, u64)>)>,
}

impl State {
    fn new(source: String) -> Self { Self { source, origins: vec![], numbers: vec![], locals: vec![], closures: vec![] } }
    fn value(&mut self, value: &RValue) {
        self.origins.push(origin_state(crate::node_origins::value(value)));
        match value {
            RValue::Literal(Literal::Number(number)) => self.numbers.push(number.to_bits()),
            RValue::Local(local) => self.locals.push(local.stable_id()),
            RValue::Closure(closure) => self.closures.push((Arc::as_ptr(&closure.function.0) as usize,
                closure.upvalues.iter().map(|upvalue| match upvalue {
                    Upvalue::Copy(local) => (true, local.stable_id()),
                    Upvalue::Ref(local) => (false, local.stable_id()),
                }).collect())),
            _ => {}
        }
        value.visit_rvalues(&mut |child| { self.value(child); true });
    }
    fn expression(value: &RValue) -> Self {
        let mut state = Self::new(value.to_string());
        state.value(value);
        state
    }
    fn block(block: &Block) -> Self {
        let mut state = Self::new(block.to_string());
        for statement in &block.0 {
            state.origins.push(origin_state(crate::node_origins::statement(statement)));
            statement.visit_local_writes(&mut |local| { state.locals.push(local.stable_id()); true });
            statement.visit_lvalues(&mut |left| { left.visit_rvalues(&mut |value| { state.value(value); true }); true });
            statement.visit_rvalues(&mut |value| { state.value(value); true });
        }
        state
    }
}

fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
fn call(name: &str) -> RValue { Call::new(Global::from(name).into(), vec![]).into() }
fn declare(local: &RcLocal, mut value: RValue) -> Statement {
    seed(&mut value, &mut 0);
    let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
    assign.prefix = true;
    assign.node_origin = origin(1000);
    assign.into()
}
fn finish(value: RValue) -> Statement {
    let mut result = Return::new(vec![value]);
    result.node_origin = origin(1001);
    result.into()
}
fn owners(locals: &[&RcLocal], function: &Arc<Mutex<Function>>) -> Vec<usize> {
    locals.iter().map(|local| Arc::strong_count(&local.0.0)).chain([Arc::strong_count(function)]).collect()
}

fn complex_initializer(function: &Arc<Mutex<Function>>, capture: &RcLocal, root_tag: bool) -> RValue {
    let mut value: RValue = Table::new(vec![
        (None, Table::new(vec![
            (None, Literal::Number(f64::from_bits(0x7ff8_0000_0000_00a5)).into()),
            (None, Literal::Number(-0.0).into()),
            (None, RValue::Select(Select::MethodCall(MethodCall::new(capture.clone().into(), "Step".into(), vec![])))),
        ]).into()),
        (None, Closure { node_origin: Default::default(), function: ByAddress(function.clone()), upvalues: vec![Upvalue::Copy(capture.clone())] }.into()),
        (None, Closure { node_origin: Default::default(), function: ByAddress(function.clone()), upvalues: vec![Upvalue::Ref(capture.clone())] }.into()),
    ]).into();
    seed(&mut value, &mut 0);
    if !root_tag { *crate::node_origins::value_mut(&mut value).unwrap() = Default::default(); }
    value
}

#[test]
fn accepted_transfer_preserves_storage_child_origins_capture_aliases_and_numeric_bits() {
    for root_tag in [false, true] {
        let temporary = local("v0");
        let capture = local("captured");
        let mut body_value: RValue = Table::new(vec![(None, Literal::Number(-0.0).into())]).into();
        seed(&mut body_value, &mut 37);
        let function = Arc::new(Mutex::new(Function { body: Block(vec![finish(body_value)]), ..Default::default() }));
        let body_before = State::block(&function.lock().body);
        let value = complex_initializer(&function, &capture, root_tag);
        let storage = value.as_table().unwrap().0.as_ptr();
        let mut specified_move = State::expression(&value);
        if let Some(tag) = &mut specified_move.origins[0] { tag.inlined = true; }
        let mut assign = Assign::new(vec![temporary.clone().into()], vec![value]);
        assign.prefix = true;
        let mut actual = Block(vec![assign.into(), finish(temporary.clone().into())]);
        assert!(inline_single_use_temps(&mut actual));
        assert_eq!(actual.len(), 1);
        let value = &actual[0].as_return().unwrap().values[0];
        assert_eq!(State::expression(value), specified_move);
        assert_eq!(value.as_table().unwrap().0.as_ptr(), storage, "owned constructor storage was copied");
        assert_eq!(State::block(&function.lock().body), body_before, "shared function body was changed");
        let after_owners = owners(&[&temporary, &capture], &function);
        let source = actual.to_string();
        drop(actual);

        let value = complex_initializer(&function, &capture, root_tag);
        let mut specified_clone = State::expression(&value);
        for tag in specified_clone.origins.iter_mut().flatten() { tag.cloned = true; }
        if let Some(tag) = &mut specified_clone.origins[0] { tag.inlined = true; }
        let mut assign = Assign::new(vec![temporary.clone().into()], vec![value]);
        assign.prefix = true;
        let mut legacy = Block(vec![assign.into(), finish(temporary.clone().into())]);
        assert!(legacy_clone::run(|| inline_single_use_temps(&mut legacy)));
        assert_eq!(State::expression(&legacy[0].as_return().unwrap().values[0]), specified_clone);
        assert_eq!(legacy.to_string(), source);
        assert_eq!(owners(&[&temporary, &capture], &function), after_owners);
        assert_eq!(State::block(&function.lock().body), body_before);
    }
}

#[test]
fn refused_candidates_leave_source_origins_bits_captures_and_owners_untouched() {
    for case in 0..11 {
        let temporary = local("v0");
        let source = local("source");
        let function = Arc::new(Mutex::new(Function::default()));
        if case == 6 {
            temporary.0.lock().add_source_binding(crate::SourceBinding {
                name: "recorded".into(), origin: crate::BindingOrigin::DebugLocal {
                    prototype: 0, register: 0, start_pc: 0, end_pc: 20,
                },
            });
        }
        let build = || {
            let mut value: RValue = Table::new(vec![(None, call("effect")),
                (None, Literal::Number(f64::from_bits(0x7ff8_0000_0000_00b6)).into()),
                (None, Literal::Number(-0.0).into())]).into();
            if case == 4 { value = call("returnsMany"); }
            if case == 5 { value = source.clone().into(); }
            if case == 9 { value = Table::new(vec![(None, temporary.clone().into())]).into(); }
            if case == 10 {
                value = Literal::Number(-0.0).into();
                for _ in 0..128 { value = Table::new(vec![(None, value)]).into(); }
            }
            let mut block = Block(vec![declare(&temporary, value)]);
            match case {
                0 => block.push(finish(Binary::new(source.clone().into(), temporary.clone().into(), BinaryOperation::And).into())),
                1 => block.push(finish(IfExpression::new(source.clone().into(), temporary.clone().into(), Literal::Nil.into()).into())),
                2 => block.push(Return::new(vec![call("before"), temporary.clone().into()]).into()),
                3 | 5 => {
                    if case == 5 {
                        block.push(Call::new(Global::from("publish").into(), vec![Closure {
                            node_origin: origin(44), function: ByAddress(function.clone()),
                            upvalues: vec![Upvalue::Ref(source.clone())],
                        }.into()]).into());
                    }
                    block.push(Call::new(Global::from("mutate").into(), vec![]).into());
                    block.push(finish(temporary.clone().into()));
                }
                7 => block.push(Assign::new(vec![Index::new(call("lhs"), Literal::String(b"key".to_vec()).into()).into()], vec![temporary.clone().into()]).into()),
                8 => block.push(finish(source.clone().into())), // No destination.
                _ => block.push(finish(temporary.clone().into())),
            }
            block
        };
        for ui in [false, true] {
            let mut actual = build();
            let before = State::block(&actual);
            let before_owners = owners(&[&temporary, &source], &function);
            let changed = if ui { rebuild_ui_expression_trees(&mut actual) } else { inline_single_use_temps(&mut actual) };
            assert!(!changed, "case {case}, UI {ui}");
            assert_eq!(State::block(&actual), before, "case {case}, UI {ui}");
            assert_eq!(owners(&[&temporary, &source], &function), before_owners);
            drop(actual);
            let mut legacy = build();
            let changed = legacy_clone::run(|| if ui { rebuild_ui_expression_trees(&mut legacy) } else { inline_single_use_temps(&mut legacy) });
            assert!(!changed, "legacy case {case}, UI {ui}");
            assert_eq!(State::block(&legacy), before);
            assert_eq!(owners(&[&temporary, &source], &function), before_owners);
        }
    }
}

#[test]
fn late_destination_refusals_do_not_detach_the_initializer() {
    let temporary = local("v0");
    let source = local("source");
    let function = Arc::new(Mutex::new(Function::default()));
    let facts = collect_motion_facts(&Block::default(), true);
    for case in 0..4 {
        let mut replacement = complex_initializer(&function, &source, true);
        let before_value = State::expression(&replacement);
        let mut statement = match case {
            0 => finish(source.clone().into()),
            1 => finish(Binary::new(source.clone().into(), temporary.clone().into(), BinaryOperation::Or).into()),
            2 => finish(IfExpression::new(source.clone().into(), Literal::Nil.into(), temporary.clone().into()).into()),
            _ => Return::new(vec![call("prior"), temporary.clone().into()]).into(),
        };
        let before_statement = statement.to_string();
        let before_owners = owners(&[&temporary, &source], &function);
        assert!(!replace_direct_rvalue_use(&mut statement, &temporary, &mut replacement, &facts, &Default::default()));
        assert_eq!(State::expression(&replacement), before_value);
        assert_eq!(statement.to_string(), before_statement);
        assert_eq!(owners(&[&temporary, &source], &function), before_owners);
    }
}

#[test]
fn small_index_key_specialization_keeps_legacy_clone_contract() {
    let temporary = local("v0");
    let object = local("object");
    let source = local("source");
    let build = || Block(vec![declare(&temporary, source.clone().into()),
        Assign::new(vec![Index::new(object.clone().into(), temporary.clone().into()).into()], vec![Literal::Number(1.0).into()]).into()]);
    let mut actual = build();
    assert!(rebuild_ui_expression_trees(&mut actual));
    let mut legacy = build();
    assert!(legacy_clone::run(|| rebuild_ui_expression_trees(&mut legacy)));
    assert_eq!(State::block(&actual), State::block(&legacy));
}
