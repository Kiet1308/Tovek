use super::*;
use ast::{Assign, Binary, BinaryOperation, Block, Call, Closure, Global, If, Index,
    Literal, Local, RValue, RcLocal, Return, Table, Upvalue};
use cfg::block::{BlockEdge, BranchType};

type OriginView = Option<(Vec<ast::node_origins::Input>, bool, bool, Option<&'static str>, bool)>;
fn semantic_debug(function: &Function, fresh_ids: std::ops::Range<u64>) -> String {
    let mut value = format!("{function:?}");
    let mut cursor = 0;
    // Preserve existing local/closure heap identities. Only a replayed fresh
    // local's allocation address is normalized, using its deterministic ID.
    while let Some(offset) = value[cursor..].find(" @ 0x") {
        let start = cursor + offset;
        let end = start + 5 + value[start + 5..].bytes().take_while(u8::is_ascii_hexdigit).count();
        let local_id = value[end..].strip_prefix("), ").and_then(|tail| {
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            tail[digits..].starts_with(')').then(|| tail[..digits].parse::<u64>().ok()).flatten()
        });
        if let Some(id) = local_id.filter(|id| fresh_ids.contains(id)) {
            let replacement = format!(" @ <fresh-local:{id}>");
            value.replace_range(start..end, &replacement);
            cursor = start + replacement.len();
        } else { cursor = end; }
    }
    value
}
fn origins(block: &Block, out: &mut Vec<OriginView>) {
    let mut record = |origin: Option<&ast::node_origins::Origin>| {
        out.push(origin.and_then(|origin| origin.0.as_ref()).map(|data| (
            data.inputs.iter().map(|input| (**input).clone()).collect(), data.inlined,
            data.cloned, data.synthesized, data.incomplete,
        )));
    };
    for statement in block.iter() {
        record(ast::node_origins::statement(statement));
        statement.traverse_rvalues_ref(&mut |value| record(ast::node_origins::value(value)));
    }
}

fn snapshot(function: &Function, fresh_ids: std::ops::Range<u64>) -> (String, Vec<OriginView>, Vec<(usize, Vec<(u64, bool)>)>) {
    let mut tagged = Vec::new();
    let mut closures = Vec::new();
    for (_, block) in function.blocks() {
        origins(block, &mut tagged);
        for statement in block.iter() {
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Closure(closure) = value {
                    closures.push((Arc::as_ptr(&closure.function.0) as usize,
                        closure.upvalues.iter().map(|upvalue| match upvalue {
                            Upvalue::Copy(local) => (local.stable_id(), false),
                            Upvalue::Ref(local) => (local.stable_id(), true),
                        }).collect()));
                    origins(&closure.function.lock().body, &mut tagged);
                }
            });
        }
    }
    (semantic_debug(function, fresh_ids), tagged, closures)
}

fn compare(function: &Function, locals: &[RcLocal], groups: &IndexMap<RcLocal, RcLocal>,
    budget: usize, admitted: bool) -> bool {
    let local_groups = locals.iter().cloned().enumerate().map(|(index, local)| (local, index / 2)).collect();
    let protected: FxHashSet<RcLocal> = groups.keys().cloned().collect();
    let (mut expected_groups, mut actual_groups) = (groups.clone(), groups.clone());
    let (mut expected_protected, mut actual_protected) = (protected.clone(), protected);
    let readonly = groups.keys().take(1).map(RcLocal::stable_id).collect();
    let mut expected = function.deep_clone();
    let mut actual = function.deep_clone();
    let source_snapshot = snapshot(function, 0..0);
    let before = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
    let base = ast::current_local_id();
    let reference = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        cleanup_ssa::<false>(&mut expected, &local_groups, &mut expected_groups, &readonly, &Default::default(), &mut expected_protected, budget)));
    let end = ast::current_local_id();
    let expected_snapshot = snapshot(&expected, base..end);
    let metadata = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
    for (local, metadata) in locals.iter().zip(before) { *local.0.lock() = metadata; }
    assert_eq!(snapshot(function, 0..0), source_snapshot,
        "legacy cleanup may not mutate shared closure bodies or source CFG syntax");
    ast::set_local_id_base(base);
    SSA_CLEANUP_TERMINAL_ADMISSIONS.with(|count| count.set(0));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        cleanup_ssa::<true>(&mut actual, &local_groups, &mut actual_groups, &readonly, &Default::default(), &mut actual_protected, budget)));
    fn status(result: &Result<bool, Box<dyn std::any::Any + Send>>) -> Result<bool, String> {
        result.as_ref().map(|value| *value).map_err(|error| error.downcast_ref::<String>().cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|value| (*value).into())).unwrap_or_default())
    }
    assert_eq!(status(&result), status(&reference));
    assert_eq!(snapshot(&actual, base..end), expected_snapshot);
    assert_eq!(locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>(), metadata);
    assert_eq!(ast::current_local_id(), end);
    assert_eq!(SSA_CLEANUP_TERMINAL_ADMISSIONS.with(std::cell::Cell::get) > 0, admitted);
    result.unwrap_or(false)
}

fn terminal(block: Block, holes: usize) -> Function {
    let mut function = Function::new(41);
    let removed = (0..holes).map(|_| function.new_block()).collect::<Vec<_>>();
    let entry = function.new_block();
    for node in removed { function.graph_mut().remove_node(node); }
    function.set_entry(entry);
    *function.block_mut(entry).unwrap() = block;
    function
}

#[test]
fn cleanup_terminal_matches_full_round_for_inline_tables_captures_and_budget() {
    for seed in 0..48 {
        let locals = (0..10).map(|index| RcLocal::new(Local::new(Some(format!("v{index}"))))).collect::<Vec<_>>();
        if seed % 3 == 0 { locals[3].0.lock().add_source_binding(ast::SourceBinding {
            origin: ast::BindingOrigin::DebugLocal { prototype: 4, register: 3, start_pc: 0, end_pc: 20 }, name: "preserved".into(),
        }); }
        let mut block = Block(vec![
            Assign::new(vec![locals[2].clone().into()], vec![Binary::new(locals[0].clone().into(), Literal::Number(1.0).into(), BinaryOperation::Add).into()]).into(),
            Assign::new(vec![locals[3].clone().into()], vec![locals[2].clone().into()]).into(),
            Assign::new(vec![locals[4].clone().into()], vec![Table::new(Vec::new()).into()]).into(),
            Assign::new(vec![Index::new(locals[4].clone().into(), Literal::String(b"field".to_vec()).into()).into()], vec![locals[3].clone().into()]).into(),
            Call::new(Global::from("observe").into(), vec![locals[4].clone().into()]).into(),
            Assign::new(vec![locals[5].clone().into()], vec![Closure {
                node_origin: Default::default(),
                function: ByAddress(Arc::new(Mutex::new(ast::Function {
                    body: Block(vec![Return::new(vec![locals[1].clone().into()]).into()]), ..Default::default()
                }))),
                upvalues: vec![Upvalue::Ref(locals[1].clone()), Upvalue::Copy(locals[0].clone())],
            }.into()]).into(),
            Return::new(vec![locals[5].clone().into()]).into(),
        ]);
        for (index, statement) in block.iter_mut().enumerate() {
            if let Some(origin) = ast::node_origins::statement_mut(statement) {
                *origin = ast::node_origins::Origin::input(ast::node_origins::Input {
                    function: "cleanup-oracle".into(), block: 0, statement: index, value: None,
                });
                origin.0.as_mut().unwrap().inlined = index % 2 == 0;
            }
        }
        let mut function = terminal(block, seed % 17);
        function.parameters = locals[..2].to_vec();
        let groups = if seed % 2 == 0 { IndexMap::from_iter([(locals[1].clone(), locals[1].clone())]) }
            else { IndexMap::default() };
        assert!(!compare(&function, &locals, &groups, 0, false));
        assert!(compare(&function, &locals, &groups, 1, true));
        assert!(compare(&function, &locals, &groups, 64, true));
    }
}

#[test]
fn cleanup_terminal_preserves_legacy_refusal_and_can_admit_a_later_round() {
    let locals = (0..4).map(|_| RcLocal::default()).collect::<Vec<_>>();
    let groups = IndexMap::default();
    assert!(compare(&terminal(Block::default(), 11), &locals, &groups, 1, true));
    for block in [
        Block(vec![Return::default().into(), ast::Comment::new("after return".into()).into()]),
        Block(vec![If::new(ast::Unary::new(locals[0].clone().into(), ast::UnaryOperation::Not).into(), Block::default(), Block::default()).into()]),
        Block(vec![ast::Close { locals: vec![locals[0].clone()] }.into()]),
        Block(vec![ast::GenericForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into()]),
    ] { compare(&terminal(block, 3), &locals, &groups, 64, false); }

    let mut function = terminal(Block::default(), 5);
    let entry = function.entry().unwrap();
    let tail = function.new_block();
    function.block_mut(tail).unwrap().push(Return::new(vec![Literal::Number(7.0).into()]).into());
    function.set_edges(entry, vec![(tail, BlockEdge::default())]);
    assert!(!compare(&function, &locals, &groups, 1, false));
    assert!(compare(&function, &locals, &groups, 2, true));
    let mut branch = terminal(Block(vec![Return::default().into()]), 0);
    let entry = branch.entry().unwrap();
    branch.set_edges(entry, vec![(entry, BlockEdge::new(BranchType::Then))]);
    compare(&branch, &locals, &groups, 1, false);
    let mut missing = Function::new(0);
    missing.new_block();
    compare(&missing, &locals, &groups, 1, false);
}
