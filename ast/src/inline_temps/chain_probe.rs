//! Deterministic copy/transfer counts and independent source/guard oracles.
//! The hooks are compiled out outside tests and disabled for other test threads.
//! `copied_rvalue_nodes` counts the RValue occurrences in each committed Clone,
//! not bytes, heap allocations, closure-body nodes, or every analysis traversal.

use std::cell::RefCell;
use crate::{Assign, Block, Literal, Local, MethodCall, RValue, RcLocal, Return, Select, Table, Traverse};

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    rounds: usize,
    attempts: usize,
    attempted_initializer_nodes: usize,
    destination_visits: usize,
    copied_rvalue_nodes: usize,
    largest_copy_depth: usize,
    copy_sizes: Vec<usize>,
    transfer_sizes: Vec<usize>,
    largest_transfer_depth: usize,
}

thread_local! {
    static ACTIVE: RefCell<Option<Counts>> = const { RefCell::new(None) };
}

// No AST/local owners are retained. Traverse deliberately stops at Closure:
// Clone shares the function Arc instead of recursively copying its body.
fn shape(value: &RValue) -> (usize, usize) {
    let mut nodes = 1;
    let mut depth = 1;
    value.visit_rvalues(&mut |child| {
        let (child_nodes, child_depth) = shape(child);
        nodes += child_nodes;
        depth = depth.max(child_depth + 1);
        true
    });
    (nodes, depth)
}

pub(super) fn record_round() {
    ACTIVE.with(|active| { if let Some(counts) = active.borrow_mut().as_mut() { counts.rounds += 1; } });
}

pub(super) fn record_attempt(value: &RValue) {
    ACTIVE.with(|active| {
        if let Some(counts) = active.borrow_mut().as_mut() {
            counts.attempts += 1;
            counts.attempted_initializer_nodes += shape(value).0;
        }
    });
}

pub(super) fn record_destination_visit() {
    ACTIVE.with(|active| { if let Some(counts) = active.borrow_mut().as_mut() { counts.destination_visits += 1; } });
}

pub(super) fn record_copy(value: &RValue) {
    ACTIVE.with(|active| {
        if let Some(counts) = active.borrow_mut().as_mut() {
            let (nodes, depth) = shape(value);
            counts.copied_rvalue_nodes += nodes;
            counts.largest_copy_depth = counts.largest_copy_depth.max(depth);
            counts.copy_sizes.push(nodes);
        }
    });
}

pub(super) fn record_transfer(value: &RValue) {
    ACTIVE.with(|active| {
        if let Some(counts) = active.borrow_mut().as_mut() {
            // Sizing is an observer walk in tests, not work done by the move.
            let (nodes, depth) = shape(value);
            counts.transfer_sizes.push(nodes);
            counts.largest_transfer_depth = counts.largest_transfer_depth.max(depth);
        }
    });
}

fn counted(run: impl FnOnce()) -> Counts {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { ACTIVE.with(|active| { active.borrow_mut().take(); }); }
    }
    ACTIVE.with(|active| {
        assert!(active.borrow().is_none(), "nested probe");
        *active.borrow_mut() = Some(Counts::default());
    });
    let _reset = Reset;
    run();
    ACTIVE.with(|active| active.borrow_mut().take().unwrap())
}

#[derive(Clone, Copy, Debug)]
enum Chain { Constructor, ScalarMethod }

fn origin(statement: usize, value: Option<usize>) -> crate::node_origins::Origin {
    let mut origin = crate::node_origins::Origin::input(crate::node_origins::Input {
        function: "chain-probe:p0".into(), block: 0, statement, value,
    });
    let data = origin.0.as_mut().unwrap();
    data.synthesized = Some("chain_probe_fixture");
    data.incomplete = statement % 7 == 0;
    origin
}

fn fixture(kind: Chain, locals: &[RcLocal], receiver: &RcLocal) -> Block {
    let mut statements = Vec::with_capacity(locals.len() + 1);
    for (index, local) in locals.iter().enumerate() {
        let mut value = match kind {
            Chain::Constructor => Table::new(vec![(None, if index == 0 {
                Literal::Number(-0.0).into()
            } else { locals[index - 1].clone().into() })]).into(),
            // Select preserves the scalar initializer contract. A raw method
            // call returned in multret position has different eligibility.
            Chain::ScalarMethod => RValue::Select(Select::MethodCall(MethodCall::new(
                if index == 0 { receiver.clone().into() } else { locals[index - 1].clone().into() },
                "Step".to_owned(), vec![Literal::Number(index as f64).into()],
            ))),
        };
        *crate::node_origins::value_mut(&mut value).unwrap() = origin(index, Some(0));
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.node_origin = origin(index, None);
        statements.push(assign.into());
    }
    let mut tail = Return::new(vec![locals.last().unwrap().clone().into()]);
    tail.node_origin = origin(locals.len(), None);
    statements.push(tail.into());
    Block(statements)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OriginSnapshot {
    inputs: Vec<crate::node_origins::Input>,
    inlined: bool,
    cloned: bool,
    synthesized: Option<&'static str>,
    incomplete: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot { source: String, origins: Vec<Option<OriginSnapshot>>, number_bits: Vec<u64>, local_ids: Vec<u64> }

fn snapshot(block: &Block) -> Snapshot {
    use crate::LocalRw;
    fn origin(origin: &crate::node_origins::Origin) -> Option<OriginSnapshot> {
        // Origin equality intentionally ignores this channel; compare every
        // field explicitly without Clone (which would mark copied ancestry).
        origin.0.as_ref().map(|data| OriginSnapshot {
            inputs: data.inputs.iter().map(|input| (**input).clone()).collect(),
            inlined: data.inlined, cloned: data.cloned,
            synthesized: data.synthesized, incomplete: data.incomplete,
        })
    }
    fn walk(value: &RValue, out: &mut Snapshot) {
        if let Some(tag) = crate::node_origins::value(value) { out.origins.push(origin(tag)); }
        if let RValue::Literal(Literal::Number(number)) = value { out.number_bits.push(number.to_bits()); }
        value.visit_rvalues(&mut |child| { walk(child, out); true });
    }
    let mut out = Snapshot { source: block.to_string(), origins: vec![], number_bits: vec![], local_ids: vec![] };
    for statement in &block.0 {
        if let Some(tag) = crate::node_origins::statement(statement) { out.origins.push(origin(tag)); }
        statement.visit_local_reads(&mut |local| { out.local_ids.push(local.stable_id()); true });
        statement.visit_local_writes(&mut |local| { out.local_ids.push(local.stable_id()); true });
        statement.visit_rvalues(&mut |value| { walk(value, &mut out); true });
    }
    out
}

fn assert_chain_lifecycle(actual: &Snapshot, legacy: &Snapshot) {
    assert_eq!(actual.source, legacy.source);
    assert_eq!(actual.number_bits, legacy.number_bits);
    assert_eq!(actual.local_ids, legacy.local_ids);
    assert_eq!(actual.origins.len(), legacy.origins.len());
    for (actual, legacy) in actual.origins.iter().zip(&legacy.origins) {
        // These fixtures start with cloned=false everywhere. Every moved
        // constructor/method root was independently marked inlined; the
        // legacy path additionally copied exactly those occurrences.
        let actual = actual.as_ref().unwrap();
        let legacy = legacy.as_ref().unwrap();
        assert!(!actual.cloned, "a move fabricated copy history");
        assert_eq!(legacy.cloned, legacy.inlined);
        let mut specified_legacy = actual.clone();
        specified_legacy.cloned = actual.inlined;
        assert_eq!(&specified_legacy, legacy);
    }
}

#[test]
fn count_growing_initializer_chains() {
    for kind in [Chain::Constructor, Chain::ScalarMethod] {
        for length in [8, 16, 32, 64, 128, 256] {
            let locals: Vec<_> = (0..length).map(|index| RcLocal::new(Local::new(Some(format!("v{index}"))))).collect();
            let receiver = RcLocal::new(Local::new(Some("object".into())));
            let mut observed = fixture(kind, &locals, &receiver);
            let initial_nodes: usize = observed.0.iter().flat_map(Traverse::rvalues).map(|value| shape(value).0).sum();
            let counts = counted(|| { super::rebuild_ui_expression_trees(&mut observed); });
            let observed_snapshot = snapshot(&observed);
            let owners: Vec<_> = locals.iter().chain([&receiver]).map(|local| triomphe::Arc::strong_count(&local.0.0)).collect();
            let remaining = observed.0.iter().filter(|statement| super::candidate_decl(statement).is_some()).count();
            let final_nodes: usize = observed.0.iter().flat_map(Traverse::rvalues).map(|value| shape(value).0).sum();
            drop(observed);

            // Rebuild fresh occurrences with the same identities, never Clone
            // the initial AST: that would pre-mark all metadata as copied.
            let mut unobserved = fixture(kind, &locals, &receiver);
            super::rebuild_ui_expression_trees(&mut unobserved);
            assert_eq!(snapshot(&unobserved), observed_snapshot, "observer changed {kind:?}/{length}");
            assert_eq!(locals.iter().chain([&receiver]).map(|local| triomphe::Arc::strong_count(&local.0.0)).collect::<Vec<_>>(), owners);
            drop(unobserved);

            // Independent old restart-from-zero scheduler, same eligibility,
            // budgets and commit code. No table stores exist in these fixtures.
            let mut reference = fixture(kind, &locals, &receiver);
            let facts = super::collect_motion_facts(&reference, true);
            while super::inline_once_full_rescan(&mut reference, &facts) {}
            drop(facts);
            assert_eq!(snapshot(&reference), observed_snapshot, "reference changed {kind:?}/{length}");
            assert_eq!(locals.iter().chain([&receiver]).map(|local| triomphe::Arc::strong_count(&local.0.0)).collect::<Vec<_>>(), owners);
            drop(reference);

            // Same production schedule with a separately retained old walker
            // and clone commit: origin-history differences are checked against
            // the explicit fixture lifecycle, not normalized indiscriminately.
            let mut legacy = fixture(kind, &locals, &receiver);
            let legacy_counts = counted(|| super::legacy_clone::run(|| {
                super::rebuild_ui_expression_trees(&mut legacy);
            }));
            assert_chain_lifecycle(&observed_snapshot, &snapshot(&legacy));
            assert_eq!(locals.iter().chain([&receiver]).map(|local| triomphe::Arc::strong_count(&local.0.0)).collect::<Vec<_>>(), owners);
            assert_eq!((counts.rounds, counts.attempts, counts.attempted_initializer_nodes, counts.destination_visits),
                (legacy_counts.rounds, legacy_counts.attempts, legacy_counts.attempted_initializer_nodes, legacy_counts.destination_visits));
            assert_eq!(counts.copied_rvalue_nodes, 0);
            assert!(counts.copy_sizes.is_empty());
            let installed_sizes = &counts.transfer_sizes;
            assert_eq!(installed_sizes, &legacy_counts.copy_sizes);

            assert_eq!(installed_sizes.len() + remaining, length);
            assert!(counts.attempts >= installed_sizes.len());
            // Pin the exact quadratic copy sequence in the admitted region.
            // At 128/256 keep all existing refusal/budget behavior and report it.
            if length <= 64 {
                let expected: Vec<_> = (1..=length).map(|k| match kind {
                    Chain::Constructor => k + 1,
                    Chain::ScalarMethod => 2 * k + 1,
                }).collect();
                assert_eq!(installed_sizes, &expected, "{kind:?}/{length}");
                assert_eq!(remaining, 0);
                assert_eq!(final_nodes, match kind { Chain::Constructor => length + 1, Chain::ScalarMethod => 2 * length + 1 });
            }
            println!("CHAIN_PROBE {{\"owned\":true,\"kind\":\"{kind:?}\",\"length\":{length},\"initial_nodes\":{initial_nodes},\"final_nodes\":{final_nodes},\"rounds\":{},\"attempts\":{},\"attempted_initializer_nodes\":{},\"accepted_root_copies\":{},\"copied_rvalue_nodes\":{},\"accepted_root_transfers\":{},\"legacy_copied_rvalue_nodes\":{},\"destination_visits\":{},\"largest_copy_depth\":{},\"largest_transfer_depth\":{},\"remaining_declarations\":{remaining}}}",
                counts.rounds, counts.attempts, counts.attempted_initializer_nodes, counts.copy_sizes.len(), counts.copied_rvalue_nodes, counts.transfer_sizes.len(), legacy_counts.copied_rvalue_nodes, counts.destination_visits, counts.largest_copy_depth, counts.largest_transfer_depth);
        }
    }
}

#[test]
fn clone_node_measurement_excludes_shared_closure_bodies_and_retains_no_owners() {
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;
    let captured = RcLocal::default();
    let function = Arc::new(Mutex::new(crate::Function {
        body: Block(vec![Return::new(vec![Table::new(vec![(None, Literal::Number(1.0).into())]).into()]).into()]),
        ..Default::default()
    }));
    let value: RValue = Table::new(vec![(None, crate::Closure {
        node_origin: origin(0, Some(0)), function: ByAddress(function.clone()),
        upvalues: vec![crate::Upvalue::Copy(captured.clone())],
    }.into())]).into();
    let owners = (Arc::strong_count(&function), Arc::strong_count(&captured.0.0));
    let counts = counted(|| { record_attempt(&value); record_copy(&value); });
    assert_eq!((counts.attempted_initializer_nodes, counts.copied_rvalue_nodes, counts.largest_copy_depth), (2, 2, 2));
    assert_eq!((Arc::strong_count(&function), Arc::strong_count(&captured.0.0)), owners);
}
