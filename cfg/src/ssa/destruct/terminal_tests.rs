use super::*;
use ast::{Assign, Binary, BinaryOperation, Block, Call, Closure, Global, LValue, Literal, Local,
    RValue, Return, Statement, Traverse, Upvalue};
use triomphe::Arc;

struct Restore(bool);
impl Drop for Restore {
    fn drop(&mut self) { REFERENCE_TERMINAL_DESTRUCTION.with(|flag| flag.set(self.0)); }
}
type OriginView = Option<(Vec<ast::node_origins::Input>, bool, bool, Option<&'static str>, bool)>;

fn semantic_debug(function: &Function, fresh_ids: std::ops::Range<u64>) -> String {
    let mut value = format!("{function:?}");
    // Normalize only replayed fresh-local allocations, keyed by their exact
    // stable ID. Existing local and closure addresses remain part of the oracle.
    let mut cursor = 0;
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

fn snapshot(function: &Function, fresh_ids: std::ops::Range<u64>) -> (String, Vec<OriginView>, Vec<(u64, Local)>, Vec<(usize, Vec<(u64, bool)>)>) {
    let mut origins = Vec::new();
    let mut metadata = Vec::new();
    let mut closures = Vec::new();
    for parameter in &function.parameters { metadata.push((parameter.stable_id(), parameter.0.lock().clone())); }
    for (_, block) in function.blocks() {
        for statement in block.iter() {
            let mut record = |origin: Option<&ast::node_origins::Origin>| origins.push(origin.and_then(|origin| origin.0.as_ref()).map(|data| (
                data.inputs.iter().map(|input| (**input).clone()).collect(), data.inlined,
                data.cloned, data.synthesized, data.incomplete,
            )));
            record(ast::node_origins::statement(statement));
            statement.traverse_rvalues_ref(&mut |value| {
                record(ast::node_origins::value(value));
                if let RValue::Closure(closure) = value {
                    closures.push((Arc::as_ptr(&closure.function.0) as usize,
                        closure.upvalues.iter().map(|upvalue| match upvalue {
                            Upvalue::Copy(local) => (local.stable_id(), false),
                            Upvalue::Ref(local) => (local.stable_id(), true),
                        }).collect()));
                }
            });
            statement.visit_local_reads(&mut |local| { metadata.push((local.stable_id(), local.0.lock().clone())); true });
            statement.visit_local_writes(&mut |local| { metadata.push((local.stable_id(), local.0.lock().clone())); true });
        }
    }
    (semantic_debug(function, fresh_ids), origins, metadata, closures)
}

fn owner_counts(locals: &[RcLocal]) -> Vec<isize> {
    locals.iter().map(|local| Arc::count(&local.0.0) as isize).collect()
}

fn compare(function: &Function, locals: &[RcLocal], groups: &IndexMap<RcLocal, RcLocal>,
    incoming: &FxHashSet<RcLocal>, admitted: bool) {
    assert_eq!(terminal_destruction_block(function).is_some(), admitted);
    let metadata = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
    let mut expected = function.deep_clone();
    let mut actual = function.deep_clone();
    let source_snapshot = snapshot(function, 0..0);
    let base = ast::current_local_id();
    let before_owners = owner_counts(locals);
    let reference = {
        let _restore = Restore(REFERENCE_TERMINAL_DESTRUCTION.with(|flag| flag.replace(true)));
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
            Destructor::new(&mut expected, groups.clone(), incoming.clone(), locals.len() + 8).destruct()))
    };
    let expected_owners = owner_counts(locals).into_iter().zip(before_owners).map(|(after, before)| after - before).collect::<Vec<_>>();
    let end = ast::current_local_id();
    let expected_snapshot = snapshot(&expected, base..end);
    let expected_metadata = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
    drop(expected);
    for (local, saved) in locals.iter().zip(&metadata) { *local.0.lock() = saved.clone(); }
    assert_eq!(snapshot(function, 0..0), source_snapshot);
    ast::set_local_id_base(base);
    let before_owners = owner_counts(locals);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        Destructor::new(&mut actual, groups.clone(), incoming.clone(), locals.len() + 8).destruct()));
    let actual_owners = owner_counts(locals).into_iter().zip(before_owners).map(|(after, before)| after - before).collect::<Vec<_>>();
    fn status(result: &Result<(), Box<dyn std::any::Any + Send>>) -> Result<(), String> {
        result.as_ref().map(|_| ()).map_err(|error| error.downcast_ref::<String>().cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|value| (*value).into())).unwrap_or_default())
    }
    assert_eq!(status(&result), status(&reference));
    assert_eq!(snapshot(&actual, base..end), expected_snapshot);
    assert_eq!(locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>(), expected_metadata);
    assert_eq!(actual_owners, expected_owners, "destruction must retain/release the same local owners");
    assert_eq!(ast::current_local_id(), end);
}

fn terminal(block: Block, holes: usize) -> Function {
    let mut function = Function::new(12);
    let removed = (0..holes).map(|_| function.new_block()).collect::<Vec<_>>();
    let entry = function.new_block();
    for node in removed { function.graph_mut().remove_node(node); }
    function.set_entry(entry);
    *function.block_mut(entry).unwrap() = block;
    function
}
fn assign(left: &RcLocal, right: RValue) -> Statement {
    Assign::new(vec![left.clone().into()], vec![right]).into()
}

#[test]
fn terminal_destructor_matches_legacy_for_copies_captures_parallel_spills_and_origins() {
    for mode in 0..6 {
        for holes in [0, 1, 24] {
            let locals = (0..12).map(|index| RcLocal::new(Local::new(Some(format!("local{index}"))))).collect::<Vec<_>>();
            locals[0].0.lock().4.parameter = true;
            locals[5].0.lock().add_source_binding(ast::SourceBinding {
                origin: ast::BindingOrigin::DebugLocal { prototype: 0, register: 5, start_pc: 0, end_pc: 20 }, name: "source".into(),
            });
            let mut groups = IndexMap::default();
            let mut incoming = FxHashSet::default();
            let mut block = Block(vec![assign(&locals[3], Literal::Number(3.0).into()),
                assign(&locals[4], locals[3].clone().into()), assign(&locals[5], locals[0].clone().into()),
                assign(&locals[6], locals[5].clone().into()),
            ]);
            if mode == 1 {
                incoming.insert(locals[2].clone());
                groups.extend([(locals[3].clone(), locals[2].clone()), (locals[4].clone(), locals[2].clone())]);
            }
            if mode == 2 { block.push(assign(&locals[0], Literal::Number(5.0).into())); }
            if matches!(mode, 3 | 4) {
                block.push(assign(&locals[7], Literal::Number(1.0).into()));
                block.push(assign(&locals[8], Literal::Number(2.0).into()));
                // Keep these cells distinct through mandatory coalescing so
                // sequentialization exercises its real swap/coupled-RHS path.
                groups.extend([(locals[7].clone(), locals[7].clone()), (locals[8].clone(), locals[8].clone())]);
                let mut parallel = Assign::new(vec![locals[7].clone().into(), locals[8].clone().into()],
                    vec![locals[8].clone().into(), if mode == 3 { locals[7].clone().into() }
                        else { Binary::new(locals[7].clone().into(), locals[8].clone().into(), BinaryOperation::Add).into() }]);
                parallel.parallel = true;
                block.push(parallel.into());
            }
            if mode == 5 { block.push(Assign::new(vec![locals[9].clone().into(), locals[9].clone().into()],
                vec![Literal::Number(1.0).into(), Literal::Number(2.0).into()]).into()); }
            let closure = Closure { node_origin: Default::default(), function: Default::default(),
                upvalues: vec![Upvalue::Ref(locals[4].clone()), Upvalue::Copy(locals[6].clone())] };
            closure.function.lock().body = Block(vec![Return::new(vec![locals[4].clone().into()]).into()]);
            block.push(assign(&locals[10], closure.into()));
            block.push(Call::new(Global::from("observe").into(), vec![locals[0].clone().into(), locals[6].clone().into()]).into());
            block.push(Return::new(vec![locals[10].clone().into()]).into());
            for (index, statement) in block.iter_mut().enumerate() {
                if let Some(origin) = ast::node_origins::statement_mut(statement) {
                    *origin = ast::node_origins::Origin::input(ast::node_origins::Input {
                        function: "terminal-destructor".into(), block: holes, statement: index, value: None,
                    });
                    let data = origin.0.as_mut().unwrap();
                    data.inlined = index % 2 == 0;
                    data.cloned = index % 3 == 0;
                    data.synthesized = (index % 4 == 0).then_some("test-producer");
                    data.incomplete = index % 5 == 0;
                }
            }
            let mut function = terminal(block, holes);
            function.parameters = locals[..2].to_vec();
            compare(&function, &locals, &groups, &incoming, true);
        }
    }
}

#[test]
fn terminal_def_use_order_and_all_interference_queries_match_full_analysis() {
    let locals = (0..8).map(|_| RcLocal::default()).collect::<Vec<_>>();
    let mut function = terminal(Block(vec![assign(&locals[3], locals[0].clone().into()),
        assign(&locals[4], locals[3].clone().into()), assign(&locals[0], Literal::Number(8.0).into()),
        Return::new(vec![locals[4].clone().into(), locals[0].clone().into(), locals[2].clone().into()]).into(),
    ]), 33);
    function.parameters = locals[..2].to_vec();
    let groups = IndexMap::from_iter([(locals[2].clone(), locals[2].clone())]);
    let incoming = FxHashSet::from_iter([locals[2].clone()]);
    let mut old_function = function.deep_clone();
    let mut old = {
        let _restore = Restore(REFERENCE_TERMINAL_DESTRUCTION.with(|flag| flag.replace(true)));
        Destructor::new(&mut old_function, groups.clone(), incoming.clone(), 16)
    };
    old.liveness = Liveness::calculate(old.function);
    old.build_def_use();
    let mut actual = Destructor::new(&mut function, groups, incoming, 16);
    actual.build_def_use();
    assert!(actual.liveness.is_empty() && actual.dominators.is_empty());
    assert_eq!(actual.local_defs.iter().collect::<Vec<_>>(), old.local_defs.iter().collect::<Vec<_>>());
    assert_eq!(actual.local_last_use.len(), old.local_last_use.len());
    for (local, uses) in &actual.local_last_use { assert_eq!(uses.0, old.local_last_use[local].0); }
    for left in actual.local_defs.keys() {
        for right in actual.local_defs.keys() {
            assert_eq!(actual.dominates(left, right), old.dominates(left, right));
            if left != right && !actual.dominates(left, right) {
                assert_eq!(actual.intersect(left, right), old.intersect(left, right));
            }
        }
    }
}

#[test]
fn terminal_destructor_preserves_missing_defs_and_refuses_nonterminal_or_edged_cfgs() {
    let locals = (0..4).map(|_| RcLocal::default()).collect::<Vec<_>>();
    let groups = IndexMap::default();
    let incoming = FxHashSet::default();
    // Undefined reads alone historically survive; a missing definition needed
    // by copy coalescing historically panics. Preserve both behaviors.
    compare(&terminal(Block(vec![Return::new(vec![locals[0].clone().into()]).into()]), 0), &locals, &groups, &incoming, true);
    compare(&terminal(Block(vec![assign(&locals[1], locals[0].clone().into())]), 0), &locals, &groups, &incoming, true);
    for block in [
        Block(vec![Return::default().into(), ast::Empty {}.into()]),
        Block(vec![ast::NumForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into()]),
        Block(vec![ast::Close { locals: vec![locals[0].clone()] }.into()]),
        Block(vec![Assign { parallel: true, ..Assign::new(vec![LValue::Index(ast::Index::new(Global::from("t").into(), Literal::Number(1.0).into()))], vec![Literal::Nil.into()]) }.into()]),
    ] { compare(&terminal(block, 2), &locals, &groups, &incoming, false); }
    let mut function = terminal(Block(vec![Return::default().into()]), 0);
    let entry = function.entry().unwrap();
    for _ in 0..2 { function.graph_mut().add_edge(entry, entry, BlockEdge::new(BranchType::Then)); }
    compare(&function, &locals, &groups, &incoming, false);
    let mut stale = terminal(Block::default(), 0);
    let entry = stale.entry().unwrap();
    stale.new_block();
    stale.graph_mut().remove_node(entry);
    compare(&stale, &locals, &groups, &incoming, false);
    let mut missing = Function::new(0);
    missing.new_block();
    compare(&missing, &locals, &groups, &incoming, false);
}
