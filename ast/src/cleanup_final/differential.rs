//! Exact legacy comparison including ancestry, floating bits and owner counts.
use super::*;
use crate::{Binary, BinaryOperation as Op, Call, Closure, Function, Global, If,
    IfExpression, Return, Select, SetList, Unary, UnaryOperation, Upvalue, While};
use by_address::ByAddress;
use parking_lot::Mutex;
use triomphe::Arc;

fn local(name: &str) -> RcLocal { RcLocal::new(Local::new(Some(name.into()))) }
fn global(name: &str) -> RValue { Global::from(name).into() }
fn boolean(value: bool) -> RValue { Literal::Boolean(value).into() }
fn assign(local: &RcLocal, value: RValue) -> Statement {
    let mut result = Assign::new(vec![local.clone().into()], vec![value]);
    result.prefix = true;
    result.into()
}
fn next(seed: &mut u64) -> usize {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*seed >> 32) as usize
}
fn expression(seed: &mut u64, depth: usize, locals: &[RcLocal]) -> RValue {
    let choice = next(seed);
    if depth == 0 {
        return match choice % 10 {
            0 => Literal::Number(f64::from_bits(0x7ff8_0000_0000_0017)).into(),
            1 => Literal::Number(-0.0).into(),
            2 => Literal::Number(f64::INFINITY).into(),
            3 => Literal::String(vec![0, b'\n', 255]).into(),
            4 => boolean(choice & 16 != 0),
            5 => Literal::Nil.into(),
            6 => global("source"),
            _ => locals[choice % locals.len()].clone().into(),
        };
    }
    let left = expression(seed, depth - 1, locals);
    let right = expression(seed, depth - 1, locals);
    match choice % 10 {
        0..=2 => Binary::new(left, right, [Op::And, Op::Or, Op::Equal][choice % 3]).into(),
        3 => Binary::new(left, right, Op::Concat).into(),
        4 => Unary::new(left, UnaryOperation::Not).into(),
        5 => Index::new(left, right).into(),
        6 => Call::new(global("observe"), vec![left, right]).into(),
        7 => Select::Call(Call::new(left, vec![right])).into(),
        8 => Table::new(vec![(Some(left), right)]).into(),
        _ => IfExpression::new(left, right, boolean(false)).into(),
    }
}

struct Fixture {
    block: Block,
    locals: Vec<RcLocal>,
    functions: Vec<Arc<Mutex<Function>>>,
    blocks: Vec<Arc<Mutex<Block>>>,
}
impl Fixture {
    fn owners(&self) -> Vec<usize> {
        self.locals.iter().map(|local| Arc::strong_count(&local.0.0))
            .chain(self.functions.iter().map(Arc::strong_count))
            .chain(self.blocks.iter().map(Arc::strong_count)).collect()
    }
}
fn fixture(mut seed: u64) -> Fixture {
    crate::reset_local_ids();
    let locals = ["flag", "value", "result", "index", "table"].map(local).to_vec();
    let variant = seed as usize;
    let flag = &locals[0];
    let value = &locals[1];
    let result = &locals[2];
    let child = Arc::new(Mutex::new(Function {
        parameters: vec![value.clone()],
        body: Block(vec![
            assign(result, boolean(variant & 1 != 0)),
            If::new(result.clone().into(),
                Block(vec![Return::new(vec![expression(&mut seed, 2, &locals)]).into()]),
                Block(vec![Return::new(vec![value.clone().into()]).into()])).into(),
        ]),
        ..Function::default()
    }));
    let closure = |capture: bool| -> RValue { Closure {
        node_origin: Default::default(), function: ByAddress(child.clone()),
        upvalues: if capture { vec![Upvalue::Ref(flag.clone()), Upvalue::Copy(locals[4].clone())] } else { vec![] },
    }.into() };
    let branch = Block(vec![
        assign(value, expression(&mut seed, 2, &locals)),
        Call::new(global("sink"), vec![value.clone().into(), flag.clone().into()]).into(),
    ]);
    let conditional = If::new(
        match variant % 4 {
            0 => flag.clone().into(),
            1 => Binary::new(value.clone().into(), Literal::Number(7.0).into(), Op::Equal).into(),
            _ => expression(&mut seed, 2, &locals),
        }, branch, Block(vec![assign(value, boolean(false))]));
    let shared = conditional.then_block.clone();
    let mut block = Block(vec![
        assign(flag, if variant & 2 == 0 { boolean(true) } else { expression(&mut seed, 2, &locals) }),
        assign(result, expression(&mut seed, 3, &locals)),
        conditional.into(),
        // Indexed LHS closure and RHS closure share a function, but each occurrence
        // must still trigger the legacy child pass at its original position.
        Assign::new(vec![Index::new(closure(variant & 4 != 0), flag.clone().into()).into()], vec![closure(false)]).into(),
        SetList::new(locals[4].clone(), 1, vec![flag.clone().into(), expression(&mut seed, 2, &locals)],
            Some(Select::Call(Call::new(global("expand"), vec![flag.clone().into()])).into())).into(),
        While::new(flag.clone().into(), Block(vec![
            assign(flag, boolean(false)),
            crate::Repeat::new(flag.clone().into(), Block(vec![
                Call::new(global("sink"), vec![flag.clone().into()]).into(),
            ])).into(),
            Return::new(vec![]).into(),
        ])).into(),
        Return::new(vec![result.clone().into()]).into(),
    ]);
    if variant % 11 == 0 { block.0.insert(0, crate::Label::from("again").into()); }
    if variant % 13 == 0 {
        block.0.push(Return::new(vec![Table::new(vec![
            (Some(Literal::String(b"First".to_vec()).into()), closure(false)),
            (Some(Literal::String(b"Second".to_vec()).into()), closure(false)),
        ]).into()]).into());
    }
    annotate_block(&mut block, &mut 0);
    Fixture { block, locals, functions: vec![child], blocks: vec![shared] }
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
    closure_aliases: Vec<usize>,
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
fn cleanup_visitors_and_gates_match_legacy_tree_ownership_and_metadata() {
    for seed in 1..=192 {
        let mut actual = fixture(seed);
        cleanup_final(&mut actual.block, Some("cleanup-test.luau"));
        let actual_shape = shape(&actual.block);
        let actual_source = actual.block.to_string();
        let actual_snapshot = snapshot(&actual.block);
        let actual_owners = actual.owners();
        let mut expected = fixture(seed);
        reference::cleanup_final(&mut expected.block, Some("cleanup-test.luau"));
        assert_eq!(shape(&expected.block), actual_shape, "shape seed {seed}");
        assert_eq!(expected.block.to_string(), actual_source, "source seed {seed}");
        assert_eq!(snapshot(&expected.block), actual_snapshot, "metadata seed {seed}");
        assert_eq!(expected.owners(), actual_owners, "owners seed {seed}");
    }
}

#[test]
fn empty_state_skips_expression_size_even_when_if_requires_the_pass() {
    for width in [1, 64, 4096] {
        let make = || Block(vec![If::new(global("condition"), Block(vec![
            Call::new(global("sink"), (0..width).map(|_| {
                Binary::new(global("left"), global("right"), Op::Add).into()
            }).collect()).into(),
        ]), Block::default()).into()]);
        let mut actual = make();
        let mut expected = make();
        CONSTANT_VALUE_VISITS.with(|visits| visits.set(0));
        REFERENCE_VALUE_VISITS.with(|visits| visits.set(0));
        simplify_constants_in_tree(&mut actual, &FxHashSet::default());
        reference::simplify_constants_in_tree(&mut expected, &FxHashSet::default());
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(CONSTANT_VALUE_VISITS.with(|visits| visits.get()), 0);
        assert!(REFERENCE_VALUE_VISITS.with(|visits| visits.get()) >= 3 * width);
    }
}

#[test]
fn opportunity_gate_excludes_nested_literals_but_keeps_if_and_loop_seeds() {
    let flag = local("flag");
    let mut block = Block(vec![assign(&flag, Table::new(vec![(None, boolean(true))]).into()),
        Call::new(global("sink"), vec![boolean(true)]).into()]);
    assert_eq!(constant_opportunities(&block), Some(false));
    let mut parallel = Assign::new(vec![flag.clone().into()], vec![boolean(true)]);
    parallel.parallel = true;
    block.0.push(parallel.into());
    assert_eq!(constant_opportunities(&block), Some(false));
    block.0.push(While::new(boolean(true), Block(vec![assign(&flag, boolean(false))])).into());
    assert_eq!(constant_opportunities(&block), Some(true));
    block.0.push(crate::Label::from("again").into());
    assert_eq!(constant_opportunities(&block), None);
    let if_only = Block(vec![If::new(boolean(true), Block::default(), Block::default()).into()]);
    assert_eq!(constant_opportunities(&if_only), Some(true));
    let mut actual = if_only;
    simplify_constants_in_tree(&mut actual, &FxHashSet::default());
    assert!(actual.0.is_empty(), "If reduction must still run with no state");
}

#[test]
fn child_shared_block_mutation_is_seen_before_parent_opportunity_gate() {
    let make = || {
        let x = local("x");
        let y = local("y");
        let shared = Arc::new(Mutex::new(Block(vec![
            assign(&x, y.clone().into()),
            Call::new(global("sink"), vec![x.clone().into()]).into(),
        ])));
        let child_if = If { node_origin: Default::default(), condition: global("condition"),
            then_block: shared.clone(), else_block: Arc::new(Mutex::new(Block::default())) };
        let function = Arc::new(Mutex::new(Function {
            body: Block(vec![assign(&y, boolean(true)), child_if.into()]), ..Default::default()
        }));
        let closure: RValue = Closure { node_origin: Default::default(), function: ByAddress(function), upvalues: vec![] }.into();
        Block(vec![Call::new(global("accept"), vec![closure]).into(),
            While { condition: global("running"), block: shared }.into()])
    };
    let mut actual = make();
    assert_eq!(constant_opportunities(&actual), Some(false));
    simplify_constants_in_tree(&mut actual, &FxHashSet::default());
    let mut expected = make();
    reference::simplify_constants_in_tree(&mut expected, &FxHashSet::default());
    assert_eq!(actual.to_string(), expected.to_string());
    assert!(actual.to_string().contains("sink(true)"));
}

#[test]
fn equal_if_arms_release_counting_locks_between_occurrences() {
    let binding = local("binding");
    let shared = Arc::new(Mutex::new(Block(vec![
        assign(&binding, global("source")),
        Call::new(global("sink"), vec![binding.clone().into()]).into(),
    ])));
    let mut block = Block(vec![If {
        node_origin: Default::default(), condition: global("condition"),
        then_block: shared.clone(), else_block: shared.clone(),
    }.into()]);
    assert_eq!(count_declared_locals(&block), 2, "count occurrences, not unique blocks");
    cleanup_final(&mut block, None);
    assert_eq!(count_declared_locals(&block), 2);
    assert_eq!(Arc::strong_count(&shared), 3);
}
