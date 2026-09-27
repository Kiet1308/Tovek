use super::*;
use ast::{Call, Close, Closure, GenericForInit, Global, Local, MethodCall, NumForInit, Return,
    SetList, Upvalue};
use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

struct Restore(bool);
impl Drop for Restore {
    fn drop(&mut self) { REFERENCE_TERMINAL_BLOCK.with(|flag| flag.set(self.0)); }
}

type OriginView = Option<(Vec<ast::node_origins::Input>, bool, bool, Option<&'static str>, bool)>;

#[derive(Debug, PartialEq, Eq)]
struct BlockView {
    text: String,
    origins: Vec<OriginView>,
    slots: Vec<(u64, bool)>,
    closures: Vec<(usize, Vec<(u64, bool)>)>,
}

fn origin_view(origin: Option<&ast::node_origins::Origin>) -> OriginView {
    origin.and_then(|origin| origin.0.as_ref()).map(|data| (
        data.inputs.iter().map(|input| (**input).clone()).collect(), data.inlined,
        data.cloned, data.synthesized, data.incomplete,
    ))
}

fn visit_blocks(statement: &Statement, visit: &mut impl FnMut(&Block)) {
    match statement {
        Statement::If(node) => {
            visit(&node.then_block.lock());
            visit(&node.else_block.lock());
        }
        Statement::While(node) => visit(&node.block.lock()),
        Statement::Repeat(node) => visit(&node.block.lock()),
        Statement::NumericFor(node) => visit(&node.block.lock()),
        Statement::GenericFor(node) => visit(&node.block.lock()),
        _ => {}
    }
}

fn block_view(block: &Block) -> BlockView {
    fn collect(block: &Block, out: &mut BlockView) {
        for statement in block.iter() {
            out.origins.push(origin_view(ast::node_origins::statement(statement)));
            statement.visit_local_reads(&mut |local| { out.slots.push((local.stable_id(), false)); true });
            statement.visit_local_writes(&mut |local| { out.slots.push((local.stable_id(), true)); true });
            statement.traverse_rvalues_ref(&mut |value| {
                out.origins.push(origin_view(ast::node_origins::value(value)));
                if let RValue::Closure(closure) = value {
                    out.closures.push((Arc::as_ptr(&closure.function.0) as usize,
                        closure.upvalues.iter().map(|upvalue| match upvalue {
                            Upvalue::Copy(local) => (local.stable_id(), false),
                            Upvalue::Ref(local) => (local.stable_id(), true),
                        }).collect()));
                    collect(&closure.function.lock().body, out);
                }
            });
            visit_blocks(statement, &mut |child| collect(child, out));
        }
    }
    let mut out = BlockView { text: block.to_string(), origins: Vec::new(), slots: Vec::new(), closures: Vec::new() };
    collect(block, &mut out);
    out
}

fn owners(function: &Function, locals: &[RcLocal]) -> (Vec<usize>, Vec<(usize, usize)>) {
    fn closures(block: &Block, out: &mut Vec<(usize, usize)>) {
        for statement in block.iter() {
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Closure(closure) = value {
                    out.push((Arc::as_ptr(&closure.function.0) as usize, Arc::strong_count(&closure.function.0)));
                    closures(&closure.function.lock().body, out);
                }
            });
            visit_blocks(statement, &mut |child| closures(child, out));
        }
    }
    let mut functions = Vec::new();
    for (_, block) in function.blocks() { closures(block, &mut functions); }
    (locals.iter().map(|local| Arc::count(&local.0.0)).collect(), functions)
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Structured(BlockView),
    Unsupported,
    Unsafe(UnsafeStructureReason),
    Panic(String),
}

fn outcome(result: &Result<StructureAttempt, Box<dyn std::any::Any + Send>>) -> Outcome {
    match result {
        Ok(StructureAttempt::Structured(block)) => Outcome::Structured(block_view(block)),
        Ok(StructureAttempt::Unsupported) => Outcome::Unsupported,
        Ok(StructureAttempt::Unsafe(reason)) => Outcome::Unsafe(*reason),
        Err(payload) => Outcome::Panic(payload.downcast_ref::<String>().cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_else(|| "non-string panic".into())),
    }
}

fn compare(function: &Function, locals: &[RcLocal], admitted: bool) -> Outcome {
    let protected = locals.iter().take(2).cloned().collect();
    let source_debug = format!("{function:?}");
    let source_blocks = function.blocks().map(|(node, block)| (node, block_view(block))).collect::<Vec<_>>();
    let source_metadata = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
    let source_owners = owners(function, locals);
    let base = ast::current_local_id();
    let expected = {
        let _restore = Restore(REFERENCE_TERMINAL_BLOCK.with(|flag| flag.replace(true)));
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
            lift_attempt_borrowed_with_ignored_locals(function, &protected)))
    };
    let end = ast::current_local_id();
    let expected_view = outcome(&expected);
    let expected_owners = owners(function, locals);
    drop(expected);
    assert_eq!(owners(function, locals), source_owners);
    ast::set_local_id_base(base);
    TERMINAL_BLOCK_ADMISSIONS.with(|count| count.set(0));
    let actual = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        lift_attempt_borrowed_with_ignored_locals(function, &protected)));
    let actual_view = outcome(&actual);
    assert_eq!(actual_view, expected_view);
    assert_eq!(owners(function, locals), expected_owners, "retain the same owners while output is alive");
    assert_eq!(ast::current_local_id(), end);
    assert_eq!(TERMINAL_BLOCK_ADMISSIONS.with(std::cell::Cell::get), usize::from(admitted));
    drop(actual);
    assert_eq!(owners(function, locals), source_owners);
    assert_eq!(format!("{function:?}"), source_debug);
    assert_eq!(function.blocks().map(|(node, block)| (node, block_view(block))).collect::<Vec<_>>(), source_blocks);
    assert_eq!(locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>(), source_metadata);
    actual_view
}

fn number(value: f64) -> RValue { Literal::Number(value).into() }
fn global(name: &str) -> RValue { Global::from(name).into() }
fn closure(body: Block, locals: &[RcLocal]) -> RValue {
    Closure { node_origin: Default::default(),
        function: ByAddress(Arc::new(Mutex::new(ast::Function { body, ..Default::default() }))),
        upvalues: vec![Upvalue::Copy(locals[0].clone()), Upvalue::Ref(locals[1].clone())],
    }.into()
}
fn function(block: Block, holes: usize) -> Function {
    let mut function = Function::new(27);
    let removed = (0..holes).map(|_| function.new_block()).collect::<Vec<_>>();
    let entry = function.new_block();
    for node in removed { function.graph_mut().remove_node(node); }
    function.set_entry(entry);
    *function.block_mut(entry).unwrap() = block;
    function
}
fn stamp(block: &mut Block) {
    let mut next = 0;
    let mut stamp = |origin: &mut ast::node_origins::Origin| {
        *origin = ast::node_origins::Origin::input(ast::node_origins::Input {
            function: "terminal-fast-path".into(), block: 17, statement: next, value: Some(next % 3),
        });
        let data = origin.0.as_mut().unwrap();
        data.inlined = next % 2 == 0;
        data.cloned = next % 3 == 0;
        data.synthesized = (next % 4 == 0).then_some("test-producer");
        data.incomplete = next % 5 == 0;
        next += 1;
    };
    for statement in block.iter_mut() {
        if let Some(origin) = ast::node_origins::statement_mut(statement) { stamp(origin); }
        statement.post_traverse_rvalues(&mut |value| -> Option<()> {
            if let Some(origin) = ast::node_origins::value_mut(value) { stamp(origin); }
            None
        });
    }
}

#[test]
fn terminal_clone_matches_builder_for_full_origins_ids_captures_and_sparse_slots() {
    let locals: Vec<_> = (0..8).map(|index| RcLocal::new(Local::new(Some(format!("local{index}"))))).collect();
    locals[0].0.lock().add_source_binding(ast::SourceBinding {
        origin: ast::BindingOrigin::DebugLocal { prototype: 4, register: 0, start_pc: 1, end_pc: 22 }, name: "source".into(),
    });
    for holes in [0, 1, 32] {
        for count in [0, 1, 8, 64] {
            let mut block = Block::default();
            for index in 0..count {
                let value = match index % 4 {
                    0 => Binary::new(locals[2].clone().into(), number(index as f64), ast::BinaryOperation::Add).into(),
                    1 => ast::Table::new(vec![(Some(Literal::String(b"field".to_vec()).into()), locals[3].clone().into())]).into(),
                    2 => ast::IfExpression::new(global("condition"), number(1.0), number(2.0)).into(),
                    _ => closure(Block(vec![Return::new(vec![locals[0].clone().into()]).into()]), &locals),
                };
                block.push(Assign::new(vec![locals[4].clone().into(), locals[4].clone().into()], vec![value, locals[5].clone().into()]).into());
                block.push(Call::new(global("observe"), vec![locals[4].clone().into()]).into());
                block.push(MethodCall::new(locals[6].clone().into(), "visit".into(), vec![number(index as f64)]).into());
                block.push(SetList::new(locals[7].clone(), 1, vec![locals[4].clone().into()], None).into());
                block.push(ast::Comment::new("retained comment".into()).into());
                block.push(ast::Empty {}.into());
            }
            if count > 0 { block.push(Return::new(vec![locals[4].clone().into()]).into()); }
            stamp(&mut block);
            let mut function = function(block, holes);
            function.parameters = vec![locals[0].clone(), locals[1].clone()];
            function.local_capture_bindings.insert(locals[1].clone());
            let entry = function.entry().unwrap();
            function.set_block_pc_range(entry, 0, count + 1);
            assert!(matches!(compare(&function, &locals, true), Outcome::Structured(_)));
        }
    }
}

#[test]
fn terminal_clone_preserves_capture_obligations_and_hidden_control_precedence() {
    let locals: Vec<_> = (0..4).map(|_| RcLocal::default()).collect();
    let required = ast::ForId { prep_pc: 7, step_pc: 13 };
    for proved in [false, true] {
        let mut function = function(Block(vec![Return::new(vec![number(1.0)]).into()]), 0);
        function.iteration_capture_obligations.insert(locals[0].clone(), FxHashSet::from_iter([required]));
        if proved { function.iteration_capture_proofs.insert(locals[0].clone(), FxHashSet::from_iter([required])); }
        let result = compare(&function, &locals, proved);
        assert_eq!(matches!(result, Outcome::Unsafe(UnsafeStructureReason::CapturedLoopResultRef)), !proved);
    }
    for indexed_lhs in [false, true] {
        for marker in [false, true] {
            for close in [false, true] {
                let mut body = Block::default();
                if marker { body.push(NumForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into()); }
                if close { body.push(Close { locals: vec![locals[1].clone()] }.into()); }
                body.push(Return::default().into());
                let value = closure(body, &locals);
                let statement = if indexed_lhs {
                    Assign::new(vec![ast::Index::new(global("target"), value).into()], vec![number(1.0)]).into()
                } else { Call::new(global("capture"), vec![value]).into() };
                let function = function(Block(vec![statement]), 0);
                let result = compare(&function, &locals, !marker && !close);
                if marker { assert_eq!(result, Outcome::Unsafe(UnsafeStructureReason::UnmodeledControl)); }
                else if close { assert_eq!(result, Outcome::Unsafe(UnsafeStructureReason::UnmodeledClose)); }
            }
        }
    }
    let mut function = function(Block(vec![GenericForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into()]), 0);
    function.iteration_capture_obligations.insert(locals[0].clone(), FxHashSet::from_iter([required]));
    assert_eq!(compare(&function, &locals, false), Outcome::Unsafe(UnsafeStructureReason::ForOriginMissing));
}

#[test]
fn terminal_clone_falls_back_for_edges_internal_control_and_malformed_entries() {
    let locals: Vec<_> = (0..4).map(|_| RcLocal::default()).collect();
    let variants = vec![
        Block(vec![Return::default().into(), ast::Comment::new("after return".into()).into()]),
        Block(vec![Return::default().into(), ast::Empty {}.into()]),
        Block(vec![ast::Break {}.into()]),
        Block(vec![ast::Continue {}.into()]),
        Block(vec![If::new(global("test"), Block::default(), Block::default()).into()]),
        Block(vec![Close { locals: vec![locals[0].clone()] }.into()]),
        Block(vec![NumForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into()]),
    ];
    for block in variants { compare(&function(block, 0), &locals, false); }
    for branch in [BranchType::Unconditional, BranchType::Then, BranchType::Else] {
        let mut function = function(Block(vec![Return::default().into()]), 4);
        let entry = function.entry().unwrap();
        let mut edge = BlockEdge::new(branch);
        edge.arguments.push((locals[0].clone(), locals[1].clone().into()));
        function.set_edges(entry, vec![(entry, edge)]);
        compare(&function, &locals, false);
    }
    let mut no_entry = Function::new(3);
    no_entry.new_block();
    assert_eq!(compare(&no_entry, &locals, false), Outcome::Unsupported);
    let mut stale_entry = function(Block::default(), 0);
    let entry = stale_entry.entry().unwrap();
    stale_entry.new_block();
    stale_entry.graph_mut().remove_node(entry);
    compare(&stale_entry, &locals, false);
    let mut unreachable = function(Block(vec![Return::default().into()]), 0);
    unreachable.new_block();
    assert!(matches!(compare(&unreachable, &locals, false), Outcome::Structured(_)));
}

#[test]
fn terminal_clone_rechecks_published_closure_bodies_on_every_call() {
    let locals: Vec<_> = (0..3).map(|_| RcLocal::default()).collect();
    let value = closure(Block::default(), &locals);
    let child = value.as_closure().unwrap().function.clone();
    let function = function(Block(vec![Call::new(global("publish"), vec![value]).into()]), 0);
    compare(&function, &locals, true);
    child.lock().body.push(Close { locals: vec![locals[0].clone()] }.into());
    assert_eq!(compare(&function, &locals, false), Outcome::Unsafe(UnsafeStructureReason::UnmodeledClose));
    child.lock().body = Block(vec![NumForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into()]);
    assert_eq!(compare(&function, &locals, false), Outcome::Unsafe(UnsafeStructureReason::UnmodeledControl));
    child.lock().body = Block(vec![Return::default().into()]);
    compare(&function, &locals, true);
}
