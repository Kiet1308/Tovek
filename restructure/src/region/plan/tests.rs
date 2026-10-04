use super::*;
use ast::{Call, Global, Return};

struct Restore(bool);
impl Drop for Restore {
    fn drop(&mut self) { REFERENCE_REGION_PLAN.with(|flag| flag.set(self.0)); }
}

fn reference(function: &Function) -> StructureAttempt {
    let _restore = Restore(REFERENCE_REGION_PLAN.with(|flag| flag.replace(true)));
    lift_attempt_borrowed_with_ignored_locals(function, &FxHashSet::default())
}

fn call(name: &str) -> Call { Call::new(Global::from(name).into(), Vec::new()) }
fn event(name: &str) -> Statement { call(name).into() }
fn block(function: &mut Function, statements: Vec<Statement>) -> NodeIndex {
    let node = function.new_block();
    *function.block_mut(node).unwrap() = statements.into();
    node
}
fn link(function: &mut Function, from: NodeIndex, to: NodeIndex) {
    function.set_edges(from, vec![(to, BlockEdge::default())]);
}
fn arm(function: &mut Function, name: &str, length: usize, tail: NodeIndex) -> NodeIndex {
    let mut next = tail;
    for index in (0..length).rev() {
        let node = block(function, vec![event(&format!("{name}{index}"))]);
        link(function, node, next);
        next = node;
    }
    next
}
fn diamond(then_length: usize, else_length: usize, reverse_edges: bool, holes: usize) -> (Function, NodeIndex, NodeIndex) {
    let mut function = Function::new(9);
    let removed = (0..holes).map(|_| function.new_block()).collect_vec();
    let entry = block(&mut function, vec![event("entry")]);
    let branch = block(&mut function, vec![event("prefix"), If::new(call("condition").into(), Block::default(), Block::default()).into()]);
    for node in removed { function.remove_block(node); }
    let tail = block(&mut function, vec![event("join"), Return::new(vec![Literal::Number(42.0).into()]).into()]);
    let then_node = arm(&mut function, "then", then_length, tail);
    let else_node = arm(&mut function, "else", else_length, tail);
    let mut edges = vec![(then_node, BlockEdge::new(BranchType::Then)), (else_node, BlockEdge::new(BranchType::Else))];
    if reverse_edges { edges.reverse(); }
    function.set_edges(branch, edges);
    link(&mut function, entry, branch);
    function.set_entry(entry);
    (function, branch, tail)
}

fn condition(value: &RValue, truth: bool, events: &mut Vec<String>) -> bool {
    match value {
        RValue::Call(node) => { record_call(node, events); truth }
        RValue::Unary(node) if node.operation == UnaryOperation::Not => !condition(&node.value, truth, events),
        RValue::Literal(Literal::Boolean(value)) => *value,
        other => panic!("unexpected test condition {other:?}"),
    }
}
fn record_call(call: &Call, events: &mut Vec<String>) {
    let RValue::Global(name) = call.value.as_ref() else { panic!("test calls are named effects") };
    events.push(String::from_utf8(name.0.clone()).unwrap());
}
fn linear(statement: &Statement, events: &mut Vec<String>) -> bool {
    match statement {
        Statement::Call(node) => record_call(node, events),
        Statement::Return(node) => { events.push(format!("return:{:?}", node.values)); return true; }
        Statement::Comment(_) | Statement::Empty(_) => {}
        other => panic!("unexpected test statement {other:?}"),
    }
    false
}
fn ast_trace(block: &Block, truth: bool) -> Vec<String> {
    fn walk(block: &Block, truth: bool, events: &mut Vec<String>) -> bool {
        for statement in block.iter() {
            if let Statement::If(node) = statement {
                let taken = condition(&node.condition, truth, events);
                let branch = if taken { &node.then_block } else { &node.else_block };
                if walk(&branch.lock(), truth, events) { return true; }
            } else if linear(statement, events) { return true; }
        }
        false
    }
    let mut events = Vec::new();
    walk(block, truth, &mut events);
    events
}
fn cfg_trace(function: &Function, truth: bool) -> Vec<String> {
    let mut events = Vec::new();
    let mut node = *function.entry().as_ref().unwrap();
    for _ in 0..MAX_NODES + 1 {
        let mut taken = None;
        for statement in function.block(node).unwrap().iter() {
            if let Statement::If(branch) = statement { taken = Some(condition(&branch.condition, truth, &mut events)); }
            else if linear(statement, &mut events) { return events; }
        }
        let next = if let Some(taken) = taken {
            let (then_edge, else_edge) = function.conditional_edges(node).unwrap();
            Some(if taken { then_edge.target() } else { else_edge.target() })
        } else { function.unconditional_edge(node).map(|edge| edge.target()) };
        let Some(next) = next else { return events; };
        node = next;
    }
    panic!("test CFG did not terminate");
}

fn compare(function: &Function) {
    let base = ast::current_local_id();
    let before = format!("{function:?}");
    let StructureAttempt::Structured(expected) = reference(function) else { panic!("reference refused a pilot shape") };
    REGION_PLAN_ADMISSIONS.with(|count| count.set(0));
    let StructureAttempt::Structured(actual) = lift_attempt_borrowed_with_ignored_locals(function, &FxHashSet::default()) else {
        panic!("pilot refused a proven shape")
    };
    assert_eq!(REGION_PLAN_ADMISSIONS.with(std::cell::Cell::get), 1);
    for truth in [false, true] {
        let expected_trace = cfg_trace(function, truth);
        assert_eq!(ast_trace(&expected, truth), expected_trace);
        assert_eq!(ast_trace(&actual, truth), expected_trace);
        assert_eq!(expected_trace.iter().filter(|event| event.as_str() == "condition").count(), 1);
    }
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(ast::current_local_id(), base);
    assert_eq!(format!("{function:?}"), before);
}

#[test]
fn pilot_matches_cfg_effects_returns_and_reference_with_shared_arms_and_sparse_slots() {
    for then_length in [0, 1, 4] {
        for else_length in [0, 1, 3] {
            for reverse in [false, true] {
                for holes in [0, 17] {
                    let (function, _, _) = diamond(then_length, else_length, reverse, holes);
                    compare(&function);
                }
            }
        }
    }
    let (at_limit, _, _) = diamond(31, 30, false, 0);
    assert_eq!(at_limit.graph().node_count(), MAX_NODES);
    compare(&at_limit);
    let (over_limit, _, _) = diamond(32, 30, false, 0);
    assert!(RegionPlan::prove(&over_limit).is_none());
}

#[test]
fn terminal_arms_keep_return_and_fallthrough_behavior() {
    for explicit_return in [false, true] {
        let (mut function, branch, join) = diamond(1, 2, false, 0);
        let then_node = function.conditional_edges(branch).unwrap().0.target();
        function.set_edges(then_node, Vec::new());
        if explicit_return { function.block_mut(then_node).unwrap().push(Return::new(vec![Literal::Number(7.0).into()]).into()); }
        // The else path retains the original terminal suffix. The branches
        // have separate exits, so no return is inferred for the then arm.
        assert!(function.block(join).is_some());
        compare(&function);
    }
}

#[test]
fn planning_retains_no_local_or_closure_owners_and_clones_only_after_admission() {
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;
    let local = RcLocal::default();
    let child = Arc::new(Mutex::new(ast::Function::default()));
    let mut assignment = Assign::new(vec![local.clone().into()], vec![ast::Closure {
        node_origin: Default::default(), function: ByAddress(child.clone()), upvalues: vec![ast::Upvalue::Ref(local.clone())],
    }.into()]);
    assignment.node_origin = ast::node_origins::Origin::input(ast::node_origins::Input {
        function: "plan-test".into(), block: 0, statement: 0, value: None,
    });
    let mut function = Function::new(1);
    let entry = block(&mut function, vec![assignment.into()]);
    let exit = block(&mut function, vec![Return::new(vec![local.clone().into()]).into()]);
    link(&mut function, entry, exit);
    function.set_entry(entry);
    let local_owners = Arc::count(&local.0.0);
    let closure_owners = Arc::strong_count(&child);
    let base = ast::current_local_id();
    let plan = RegionPlan::prove(&function).unwrap();
    assert_eq!(Arc::count(&local.0.0), local_owners);
    assert_eq!(Arc::strong_count(&child), closure_owners);
    assert_eq!(ast::current_local_id(), base);
    let output = plan.materialize();
    let origin = ast::node_origins::statement(&output[0]).unwrap().0.as_ref().unwrap();
    assert!(origin.cloned);
    assert_eq!(origin.inputs[0].function.as_ref(), "plan-test");
    let source_origin = ast::node_origins::statement(&function.block(entry).unwrap()[0]).unwrap().0.as_ref().unwrap();
    assert!(!source_origin.cloned);
    assert_eq!(Arc::strong_count(&child), closure_owners + 1);
    let closure = output[0].as_assign().unwrap().right[0].as_closure().unwrap();
    assert!(Arc::ptr_eq(&child, &closure.function.0));
    assert!(matches!(&closure.upvalues[0], ast::Upvalue::Ref(captured) if captured == &local));
    assert_eq!(ast::current_local_id(), base);
    drop(output);
    assert_eq!(Arc::count(&local.0.0), local_owners);
    assert_eq!(Arc::strong_count(&child), closure_owners);
}

#[test]
fn refusals_preserve_typed_fallback_and_do_not_materialize_candidates() {
    for case in 0..10 {
        let (mut function, branch, tail) = diamond(1, 1, false, 0);
        let then_node = function.conditional_edges(branch).unwrap().0.target();
        let local = RcLocal::default();
        match case {
            0 => { link(&mut function, tail, branch); function.block_mut(tail).unwrap().pop(); }
            1 => {
                let edge = function.edges(then_node).next().unwrap().id();
                function.graph_mut().edge_weight_mut(edge).unwrap().arguments.push((local.clone(), Literal::Nil.into()));
            }
            2 => {
                function.block_mut(then_node).unwrap().push(If::new(Literal::Boolean(true).into(), Block::default(), Block::default()).into());
                function.set_edges(then_node, vec![(tail, BlockEdge::new(BranchType::Then)), (tail, BlockEdge::new(BranchType::Else))]);
            }
            3 => { function.new_block(); }
            4 => { function.block(branch).unwrap().last().unwrap().as_if().unwrap().then_block.lock().push(event("unowned")); }
            5 => { function.block_mut(tail).unwrap().insert(0, Return::default().into()); }
            6 => { function.block_mut(then_node).unwrap().push(ast::Close { locals: vec![local.clone()] }.into()); }
            7 => { function.iteration_capture_obligations.insert(local.clone(), [ast::ForId { prep_pc: 1, step_pc: 4 }].into_iter().collect()); }
            8 => {
                let mut assignment = Assign::new(vec![local.clone().into()], vec![Literal::Nil.into()]);
                assignment.prefix = true;
                function.block_mut(then_node).unwrap().push(assignment.into());
            }
            9 => {
                let child = ast::Function { body: vec![ast::NumForInit::new(local.clone(), local.clone(), local.clone()).into()].into(), ..Default::default() };
                let value = ast::Closure { node_origin: Default::default(),
                    function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(child))), upvalues: vec![] };
                function.block_mut(then_node).unwrap().push(Assign::new(vec![local.clone().into()], vec![value.into()]).into());
            }
            _ => unreachable!(),
        }
        let source = format!("{function:?}");
        let owners = triomphe::Arc::count(&local.0.0);
        let base = ast::current_local_id();
        assert!(RegionPlan::prove(&function).is_none(), "case {case}");
        assert_eq!(triomphe::Arc::count(&local.0.0), owners);
        assert_eq!(ast::current_local_id(), base);
        let expected = format!("{:?}", reference(&function));
        ast::set_local_id_base(base);
        REGION_PLAN_ADMISSIONS.with(|count| count.set(0));
        let actual = lift_attempt_borrowed_with_ignored_locals(&function, &FxHashSet::default());
        assert_eq!(format!("{actual:?}"), expected, "typed fallback changed for case {case}");
        assert_eq!(REGION_PLAN_ADMISSIONS.with(std::cell::Cell::get), 0);
        assert_eq!(format!("{function:?}"), source);
    }
}
