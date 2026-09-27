//! Exact phase-local preparation and complete naming comparisons.
use super::*;
use crate::{Assign, Closure, Function, Global, If, NumericFor, Return, Upvalue};
use by_address::ByAddress;
use parking_lot::Mutex;
thread_local! {
    static NAME_OWNERS: std::cell::RefCell<Option<Vec<(u64, usize)>>> = const { std::cell::RefCell::new(None) };
    pub(super) static FUSED_EXPRESSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static FUSED_STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static REFERENCE_EXPRESSIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static REFERENCE_STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static FUSED_USAGE_VALUES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static REFERENCE_USAGE_VALUES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(super) fn note_name_owner(local: &RcLocal) {
    NAME_OWNERS.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() { trace.push((local.stable_id(), Arc::count(&local.0.0))); }
    });
}
fn local() -> RcLocal { RcLocal::default() }
fn global(name: &str) -> RValue { Global::from(name).into() }
fn string(name: &str) -> RValue { Literal::String(name.as_bytes().to_vec()).into() }
fn number(value: f64) -> RValue { Literal::Number(value).into() }
fn assign(local: &RcLocal, value: RValue) -> Statement {
    let mut assign = Assign::new(vec![local.clone().into()], vec![value]); assign.prefix = true; assign.into()
}
fn closure(function: &Arc<Mutex<Function>>, upvalues: Vec<Upvalue>) -> RValue {
    Closure { node_origin: Default::default(), function: ByAddress(function.clone()), upvalues }.into()
}
fn compare_preparation(actual: &NamingPreparation, expected: &NamingPreparation) {
    let counts = |facts: &NamingPreparation| facts.counts.iter()
        .map(|(ptr, value)| (*ptr, (value.reads, value.writes, value.captured)))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual.create_element_aliases, expected.create_element_aliases);
    assert_eq!(actual.collapse_candidates, expected.collapse_candidates);
    assert_eq!(actual.class_signal_locals, expected.class_signal_locals);
    assert_eq!(actual.field_aliases, expected.field_aliases);
    assert_eq!(counts(actual), counts(expected));
    assert_eq!(actual.identities, expected.identities);
    assert_eq!(actual.definitions, expected.definitions);
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
    let locals: Vec<_> = (0..8).map(|_| local()).collect();
    let [helper, parameter, alias, object, counter, diamond, nested, typed] = locals.as_slice() else { unreachable!() };
    if seed & 1 != 0 { typed.0.lock().add_source_binding(crate::SourceBinding {
        name: "SourceValue".into(), origin: crate::BindingOrigin::DebugLocal {
            prototype: 0, register: 1, start_pc: 0, end_pc: 100,
        },
    }); }
    let child = Arc::new(Mutex::new(Function {
        name: (seed % 3 == 0).then_some("readValue".into()), parameters: vec![parameter.clone(), local()],
        parameter_name_hints: vec![Some("number".into()), Some("callback".into())],
        body: Block(vec![
            assign(alias, Index::new(global("UI"), string("createElement")).into()),
            Assign::new(vec![Index::new(object.clone().into(), string("Width")).into()], vec![parameter.clone().into()]).into(),
            Return::new(vec![Call::new(alias.clone().into(), vec![string("Frame"),
                Table::new(vec![(Some(string("Value")), parameter.clone().into())]).into()]).into()]).into(),
        ]), ..Default::default()
    }));
    let mut empty_decl = Assign::new(vec![diamond.clone().into()], vec![]); empty_decl.prefix = true;
    let mut block = Block(vec![
        assign(object, Table::default().into()),
        assign(counter, number(0.0)),
        assign(typed, Index::new(global("configuration"), string("Width")).into()),
        assign(&local(), number(5.0)), // no retained test owner: must still become `_`.
        assign(helper, closure(&child, vec![Upvalue::Copy(object.clone())])),
        assign(alias, Index::new(global("UI"), string("Children")).into()),
        empty_decl.into(),
        If::new(global("condition"),
            Block(vec![Assign::new(vec![diamond.clone().into()], vec![Literal::Boolean(true).into()]).into()]),
            Block(vec![Assign::new(vec![diamond.clone().into()], vec![Literal::Boolean(false).into()]).into()])).into(),
        Call::new(global("consume"), vec![diamond.clone().into()]).into(),
        NumericFor::new(number(1.0), number(3.0), number(1.0), counter.clone(), Block(vec![
            Assign::new(vec![Index::new(object.clone().into(), counter.clone().into()).into()], vec![typed.clone().into()]).into(),
            Call::new(helper.clone().into(), vec![typed.clone().into()]).into(),
        ])).into(),
        Return::new(vec![number(-0.0), number(f64::from_bits(0x7ff8_0000_0000_1234)), object.clone().into()]).into(),
    ]);
    match seed % 6 {
        0 => block.0.insert(0, Assign::new(vec![Index::new(closure(&child, vec![]), string("x")).into()], vec![number(1.0)]).into()),
        1 => block.0.insert(0, assign(nested, closure(&child, vec![Upvalue::Ref(parameter.clone())]))),
        2 => block.0.insert(1, Assign::new(vec![helper.clone().into()], vec![global("replacement")]).into()),
        3 => block.0.insert(1, Call::new(helper.clone().into(), vec![string("unknown")]).into()),
        4 => block.0.insert(1, crate::GenericFor::new(vec![nested.clone(), alias.clone()], vec![global("records")],
            Block(vec![Call::new(global("consume"), vec![nested.clone().into(), alias.clone().into()]).into()])).into()),
        _ => block.0.insert(1, Assign::new(vec![helper.clone().into(), nested.clone().into()],
            vec![Select::Call(Call::new(global("getValues"), vec![])).into()]).into()),
    }
    annotate_block(&mut block, &mut 0);
    Fixture { block, locals, functions: vec![child] }
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



#[test]
fn fused_facts_and_usage_match_independent_censuses_without_owners() {
    for seed in 0..48 {
        let mut fixture = fixture(seed);
        let owners = fixture.owners();
        let actual = NamingPreparation::for_naming(&fixture.block, true);
        let expected = reference::prepare(&fixture.block, true);
        compare_preparation(&actual, &expected);
        assert_eq!(fixture.owners(), owners);
        let mut actual_usage = FxHashMap::default();
        gather_usage(&mut fixture.block, false, &actual.create_element_aliases, &actual.field_aliases, &mut actual_usage);
        let mut expected_usage = FxHashMap::default();
        reference::gather_usage(&mut fixture.block, false, &expected.create_element_aliases, &mut expected_usage);
        assert_eq!(actual_usage, expected_usage, "usage seed {seed}");
        assert_eq!(fixture.owners(), owners);
    }
}

#[test]
fn complete_naming_matches_legacy_preparation_with_ids_evidence_and_origins() {
    for seed in 0..48 {
        for rename in [false, true] {
            for collect_evidence in [false, true] {
                let options = NameLocalOptions { dont_reuse_var: seed & 2 != 0 };
                let mut actual = fixture(seed);
                NAME_OWNERS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
                let report = name_locals_impl::<false>(&mut actual.block, rename, Some("UI/Widget.luau"), options, collect_evidence);
                let actual_name_owners = NAME_OWNERS.with(|trace| trace.borrow_mut().take().unwrap());
                let actual_report = format!("{report:?}");
                let actual_shape = shape(&actual.block);
                let actual_source = actual.block.to_string();
                let actual_snapshot = snapshot(&actual.block);
                let actual_owners = actual.owners();
                let next_id = local().stable_id();
                let mut expected = fixture(seed);
                NAME_OWNERS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
                let report = name_locals_impl::<true>(&mut expected.block, rename, Some("UI/Widget.luau"), options, collect_evidence);
                assert_eq!(NAME_OWNERS.with(|trace| trace.borrow_mut().take().unwrap()), actual_name_owners,
                    "owner counts at every name_one entry, seed {seed}");
                assert_eq!(format!("{report:?}"), actual_report, "evidence seed {seed}, rename {rename}");
                assert_eq!(shape(&expected.block), actual_shape, "shape seed {seed}, rename {rename}");
                assert_eq!(expected.block.to_string(), actual_source);
                assert_eq!(snapshot(&expected.block), actual_snapshot);
                assert_eq!(expected.owners(), actual_owners);
                assert_eq!(local().stable_id(), next_id, "local mint sequence");
            }
        }
    }
}

#[test]
fn closure_domain_masks_keep_lhs_marker_alias_order_and_duplicate_definitions() {
    let alias = local();
    let binder = local();
    let captured = local();
    let leaf = Arc::new(Mutex::new(Function::default()));
    let make = |key: &str| Arc::new(Mutex::new(Function {
        body: Block(vec![assign(&alias, Index::new(global("source"), string(key)).into()),
            assign(&binder, closure(&leaf, vec![]))]), ..Default::default()
    }));
    let shared = make("Shared");
    let only_lhs = Block(vec![Assign::new(vec![Index::new(closure(&shared, vec![Upvalue::Ref(captured.clone())]), string("x")).into()],
        vec![number(0.0)]).into()]);
    let actual = NamingPreparation::for_naming(&only_lhs, true);
    compare_preparation(&actual, &reference::prepare(&only_lhs, true));
    assert!(!actual.field_aliases.contains_key(&local_ptr(&alias)));
    assert!(!actual.counts.contains_key(&local_ptr(&alias)));
    assert!(actual.definitions.contains_key(&local_ptr(&binder)));
    let capture = &actual.counts[&local_ptr(&captured)];
    assert_eq!((capture.reads, capture.captured), (1, false));
    let both = Block(vec![Assign::new(vec![Index::new(closure(&shared, vec![]), string("x")).into()], vec![closure(&shared, vec![])]).into()]);
    let actual = NamingPreparation::for_naming(&both, true);
    compare_preparation(&actual, &reference::prepare(&both, true));
    assert!(!actual.definitions.contains_key(&local_ptr(&binder)), "the two definition occurrences invalidate their binder");
    assert_eq!(actual.counts[&local_ptr(&alias)].writes, 1, "indexed-LHS body omitted only from usage");
    let marker = Block(vec![crate::NumForNext {
        counter: (local().into(), closure(&make("Counter"), vec![])),
        limit: closure(&make("Limit"), vec![]), step: closure(&make("Step"), vec![]),
    }.into()]);
    let actual = NamingPreparation::for_naming(&marker, true);
    compare_preparation(&actual, &reference::prepare(&marker, true));
    assert_eq!(actual.field_aliases[&local_ptr(&alias)], "Step");
    assert!(actual.definitions.is_empty());
}

#[test]
fn fused_naming_removes_whole_tree_discovery_and_second_usage_expression_walk() {
    for width in [1, 64, 4096] {
        let read = local();
        let mut block = Block(vec![Call::new(global("consume"), (0..width).map(|_| {
            Binary::new(read.clone().into(), number(1.0), BinaryOperation::Add).into()
        }).collect()).into()]);
        for counter in [&FUSED_EXPRESSIONS, &FUSED_STATEMENTS, &REFERENCE_EXPRESSIONS, &REFERENCE_STATEMENTS,
            &FUSED_USAGE_VALUES, &REFERENCE_USAGE_VALUES] { counter.with(|count| count.set(0)); }
        let actual = NamingPreparation::for_naming(&block, false);
        let expected = reference::prepare(&block, false);
        compare_preparation(&actual, &expected);
        assert_eq!(FUSED_STATEMENTS.with(|count| count.get()), 1);
        assert_eq!(REFERENCE_STATEMENTS.with(|count| count.get()), 3,
            "lower bound: old preparation + field aliases + definitions (usage census is additional)");
        assert_eq!(REFERENCE_EXPRESSIONS.with(|count| count.get()), 2 * FUSED_EXPRESSIONS.with(|count| count.get()),
            "lower bound: two old counted expression scans versus one fused scan");
        let (mut actual_usage, mut expected_usage) = (FxHashMap::default(), FxHashMap::default());
        gather_usage(&mut block, false, &actual.create_element_aliases, &actual.field_aliases, &mut actual_usage);
        reference::gather_usage(&mut block, false, &expected.create_element_aliases, &mut expected_usage);
        assert_eq!(actual_usage, expected_usage);
        assert_eq!(REFERENCE_USAGE_VALUES.with(|count| count.get()), 2 * FUSED_USAGE_VALUES.with(|count| count.get()));
    }
}
