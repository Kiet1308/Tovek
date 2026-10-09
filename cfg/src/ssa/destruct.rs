use std::{cell::{OnceCell, RefCell}, collections::BTreeMap, ops::Deref, rc::Rc};

use ast::{LocalRw, RcLocal, Traverse};
use ast::FxIndexMap as IndexMap;
use itertools::Itertools;
use petgraph::{
    stable_graph::NodeIndex,
    visit::{Dfs, DfsPostOrder, EdgeRef, NodeIndexable},
    Direction,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    block::{BlockEdge, BranchType},
    function::Function,
};

mod liveness;
mod bindings;

use bindings::BindingSummary;
use self::liveness::Liveness;

#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Copy, Debug)]
enum ParamOrStatIndex {
    Param(usize),
    Stat(usize),
}

/// A local's final read in each block. `build_def_use` visits blocks once in
/// increasing dominator preorder, so repeated reads update the final entry
/// and each new block appends. No per-local block hash table is necessary.
#[derive(Default, Debug)]
struct LastUses(Vec<(usize, usize)>);

impl LastUses {
    fn record(&mut self, block_order: usize, statement: usize) {
        if let Some((last_block, last_statement)) = self.0.last_mut() {
            if *last_block == block_order {
                *last_statement = statement;
                return;
            }
            debug_assert!(*last_block < block_order);
        }
        self.0.push((block_order, statement));
    }

    fn get(&self, block_order: usize) -> Option<usize> {
        self.0.binary_search_by_key(&block_order, |&(block, _)| block)
            .ok().map(|index| self.0[index].1)
    }
}

#[derive(PartialEq, Eq)]
enum RedOrBlue {
    Red,
    Blue,
}

/// Whether two congruence classes have members defined at the same point.
fn share_definition_point(a: &CongruenceClass, b: &CongruenceClass) -> bool {
    let (small, large) = if a.members.len() <= b.members.len() { (a, b) } else { (b, a) };
    small.members.keys().any(|key| large.members.contains_key(key))
}

#[derive(Default)]
struct CongruenceClass {
    members: BTreeMap<(usize, ParamOrStatIndex), RcLocal>,
    bindings: OnceCell<BindingSummary>,
}

impl CongruenceClass {
    fn insert(&mut self, key: (usize, ParamOrStatIndex), local: RcLocal) {
        match self.members.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                if let Some(summary) = self.bindings.get_mut() {
                    summary.add_local(&local);
                    ast::telemetry::count("destruct_binding_summary_incremental_members", 1);
                }
                entry.insert(local);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.insert(local);
                // A same-key replacement can remove the only restrictive
                // member. Aggregate union cannot subtract its old facts.
                self.bindings.take();
                ast::telemetry::count("destruct_binding_summary_replacement_invalidations", 1);
            }
        }
    }

    fn extend(&mut self, other: Self) {
        let was_empty = self.members.is_empty();
        let incoming = other.bindings.into_inner();
        let mut overlap = false;
        let mut incremental_members = 0;
        // Detect replacement during the mandatory inserts, with no extra
        // disjointness scan of either class. Preserve right-hand replacement
        // semantics and the existing ordered member keys.
        for (key, local) in other.members {
            match self.members.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    if incoming.is_none() && let Some(summary) = self.bindings.get_mut() {
                        summary.add_local(&local);
                        incremental_members += 1;
                    }
                    entry.insert(local);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(local);
                    overlap = true;
                    self.bindings.take();
                }
            }
        }
        if incremental_members != 0 {
            ast::telemetry::count("destruct_binding_summary_incremental_members", incremental_members);
        }
        if overlap {
            ast::telemetry::count("destruct_binding_summary_replacement_invalidations", 1);
        } else if let Some(incoming) = incoming {
            if let Some(summary) = self.bindings.get_mut() {
                summary.merge(incoming);
                ast::telemetry::count("destruct_binding_summary_merges", 1);
            } else if was_empty {
                // Transfer a donor's existing cache; never eagerly build an
                // uncached receiver merely because a donor was queried.
                self.bindings.set(incoming).ok().unwrap();
                ast::telemetry::count("destruct_binding_summary_cache_transfers", 1);
            }
        }
    }

    fn bindings(&self) -> &BindingSummary {
        self.bindings.get_or_init(|| {
            ast::telemetry::count("destruct_binding_summary_builds", 1);
            ast::telemetry::count("destruct_binding_summary_full_rebuilds", 1);
            ast::telemetry::count("destruct_binding_summary_members", self.members.len() as u64);
            BindingSummary::from_locals(self.members.values())
        })
    }
}

impl Deref for CongruenceClass {
    type Target = BTreeMap<(usize, ParamOrStatIndex), RcLocal>;
    fn deref(&self) -> &Self::Target { &self.members }
}

impl PartialEq for CongruenceClass {
    fn eq(&self, other: &Self) -> bool { self.members == other.members }
}
impl Eq for CongruenceClass {}

#[cfg(test)]
mod class_cache_tests {
    use super::*;

    #[test]
    fn membership_changes_invalidate_binding_summary() {
        let parameter = RcLocal::default();
        parameter.0.lock().4.parameter = true;
        let separate = RcLocal::default();
        separate.0.lock().4.separate_from_parameter = true;
        let neutral = RcLocal::default();
        let key = (0, ParamOrStatIndex::Stat(0));
        let mut class = CongruenceClass::default();
        class.insert(key, parameter.clone());
        assert!(class.bindings().compatible(class.bindings()));
        // A replacement can change constraints without changing class length.
        class.insert(key, separate.clone());
        assert!(!class.bindings().compatible(&BindingSummary::from_locals([&parameter].into_iter())));
        class.insert(key, neutral);
        assert!(class.bindings().compatible(&BindingSummary::from_locals([&parameter].into_iter())));
        let mut other = CongruenceClass::default();
        other.insert((1, ParamOrStatIndex::Stat(0)), separate);
        class.extend(other);
        assert!(!class.bindings().compatible(&BindingSummary::from_locals([&parameter].into_iter())));
        class.insert((2, ParamOrStatIndex::Stat(0)), parameter);
        assert!(!class.bindings().compatible(class.bindings()));
    }

    #[test]
    fn cached_class_unions_reuse_summaries_without_rescanning_members() {
        for count in [64, 256, 1024] {
            let locals: Vec<_> = (0..count).map(|_| RcLocal::default()).collect();
            let mut class = CongruenceClass::default();
            class.bindings();
            bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.set(0));
            for (index, local) in locals.iter().enumerate() {
                let mut donor = CongruenceClass::default();
                donor.insert((index, ParamOrStatIndex::Stat(0)), local.clone());
                donor.bindings();
                class.extend(donor);
                assert!(class.bindings.get().is_some());
                class.bindings();
            }
            assert_eq!(bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get()), count);
            assert_eq!(*class.bindings(), BindingSummary::from_locals(class.values()));
        }
    }

    #[test]
    fn initial_summary_builds_remain_lazy_and_cached_insertions_are_incremental() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        let c = RcLocal::default();
        a.0.lock().4.parameter = true;
        c.0.lock().4.separate_from_parameter = true;
        bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.set(0));
        let mut class = CongruenceClass::default();
        class.insert((0, ParamOrStatIndex::Param(0)), a);
        class.insert((0, ParamOrStatIndex::Stat(0)), b);
        let mut donor = CongruenceClass::default();
        donor.insert((1, ParamOrStatIndex::Stat(0)), c);
        donor.bindings();
        class.extend(donor);
        assert!(class.bindings.get().is_none());
        assert_eq!(bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get()), 1);
        class.bindings();
        assert_eq!(bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get()), 4);
        class.insert((2, ParamOrStatIndex::Stat(0)), RcLocal::default());
        class.bindings();
        assert_eq!(bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get()), 5);
        let mut uncached = CongruenceClass::default();
        uncached.insert((3, ParamOrStatIndex::Stat(0)), RcLocal::default());
        class.extend(uncached);
        assert_eq!(bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get()), 6);
        assert_eq!(*class.bindings(), BindingSummary::from_locals(class.values()));

        let mut empty = CongruenceClass::default();
        let before = bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get());
        empty.extend(class);
        assert!(empty.bindings.get().is_some());
        empty.bindings();
        assert_eq!(bindings::SUMMARY_LOCAL_VISITS.with(|visits| visits.get()), before);
    }

    #[test]
    fn overlapping_member_keys_replace_and_invalidate_even_with_cached_donors() {
        let parameter = RcLocal::default();
        parameter.0.lock().4.parameter = true;
        let separate = RcLocal::default();
        separate.0.lock().4.separate_from_parameter = true;
        let neutral = RcLocal::default();
        let key = (0, ParamOrStatIndex::Stat(0));
        for cached_donor in [false, true] {
            let mut class = CongruenceClass::default();
            class.insert(key, parameter.clone());
            class.bindings();
            let mut donor = CongruenceClass::default();
            // Include disjoint entries on either side of the overlapping key.
            donor.insert((0, ParamOrStatIndex::Param(0)), separate.clone());
            donor.insert(key, neutral.clone());
            donor.insert((1, ParamOrStatIndex::Stat(0)), neutral.clone());
            if cached_donor { donor.bindings(); }
            let expected: BTreeMap<_, _> = donor.members.iter().map(|(key, local)| (*key, local.clone())).collect();
            class.extend(donor);
            assert_eq!(class.members, expected);
            assert!(class.bindings.get().is_none(), "replacement must discard stale role facts");
            assert_eq!(*class.bindings(), BindingSummary::from_locals(class.values()));
            assert!(class.bindings().compatible(&BindingSummary::from_locals([&separate].into_iter())));
            // Replacing with the same identity must still take the invalidation
            // path; no hidden assumption about metadata changes is introduced.
            class.insert(key, neutral.clone());
            assert!(class.bindings.get().is_none());
        }
    }

    #[test]
    fn cached_mutation_sequence_matches_ordered_members_and_pairwise_evidence() {
        let pool: Vec<_> = (0..12).map(|index| {
            let local = RcLocal::default();
            let mut metadata = local.0.lock();
            metadata.4.parameter = index & 1 != 0;
            metadata.4.separate_from_parameter = index & 2 != 0;
            let origin = if index < 4 { ast::BindingOrigin::Function { prototype: index } }
                else { ast::BindingOrigin::DebugLocal { prototype: 0, register: 0, start_pc: index / 4, end_pc: 3 } };
            metadata.add_source_binding(ast::SourceBinding { origin, name: "sameSpelling".into() });
            drop(metadata);
            local
        }).collect();
        let mut class = CongruenceClass::default();
        let mut expected = BTreeMap::new();
        let mut state = 1u64;
        let mut next = || { state = state.wrapping_mul(6364136223846793005).wrapping_add(1); (state >> 32) as usize };
        for step in 0..256 {
            let mut donor = CongruenceClass::default();
            let width = if step < pool.len() { 1 } else { 1 + next() % 4 };
            for slot in 0..width {
                let key = if step < pool.len() { (0, ParamOrStatIndex::Stat(0)) }
                    else { (next() % 3, if slot % 2 == 0 { ParamOrStatIndex::Param(next() % 5) }
                        else { ParamOrStatIndex::Stat(next() % 5) }) };
                let local = pool[if step < pool.len() { step } else { next() % pool.len() }].clone();
                expected.insert(key, local.clone());
                donor.insert(key, local);
            }
            if step % 2 == 0 { donor.bindings(); }
            class.extend(donor);
            assert_eq!(class.members, expected, "step {step}");
            assert_eq!(*class.bindings(), BindingSummary::from_locals(expected.values()), "step {step}");
            for probe in &pool {
                let compatible = expected.values().all(|local| local.source_bindings_compatible(probe));
                assert_eq!(class.bindings().compatible(&BindingSummary::from_locals([probe].into_iter())), compatible);
            }
        }
    }
}

// Benoit Boissinot, Alain Darte, Fabrice Rastello, Benoît Dupont de Dinechin, Christophe Guillon.
// Revisiting Out-of-SSA Translation for Correctness, Code Quality, and Efficiency. [Research Report]
// 2008, pp.14. inria-00349925v3
// https://hal.inria.fr/inria-00349925/file/RR.pdf
// Slides: https://compilers.cs.uni-saarland.de/ssasem/talks/Alain.Darte.pdf
// https://github.com/LLVM-but-worse/maple-ir/blob/f8711230b7c63ce5fd916f86563912ec36f1217e/org.mapleir.ir/src/main/java/org/mapleir/ir/algorithms/BoissinotDestructor.java
pub struct Destructor<'a> {
    function: &'a mut Function,
    upvalue_to_group: IndexMap<RcLocal, RcLocal>,
    upvalues_in: FxHashSet<RcLocal>,
    values: FxHashMap<RcLocal, Rc<RefCell<FxHashSet<RcLocal>>>>,
    // map( local -> rc_map( local -> (pre-order block index, param index) ) )
    // TODO: hash map?
    congruence_classes: FxHashMap<RcLocal, Rc<RefCell<CongruenceClass>>>,
    equal_ancestor_in: FxHashMap<RcLocal, RcLocal>,
    equal_ancestor_out: FxHashMap<RcLocal, RcLocal>,
    local_defs: FxHashMap<RcLocal, (usize, NodeIndex, ParamOrStatIndex)>,
    local_last_use: FxHashMap<RcLocal, LastUses>,
    /// Dominator-tree children per node index, in CFG node order.
    dominator_children: Vec<Vec<NodeIndex>>,
    // Half-open DFS intervals provide ancestor queries in O(1), with O(V)
    // storage instead of copying every ancestor on a deep dominator chain.
    /// Dominator-tree preorder interval per node index.
    dominators: Vec<(usize, usize)>,
    liveness: Liveness,
    undesirable_blocks: FxHashSet<NodeIndex>,
    terminal_block: Option<NodeIndex>,
    /// Bytecode register (lifter local) of each SSA version, when known.
    register_groups: Option<&'a FxHashMap<RcLocal, usize>>,
    /// Roots of the captured cells no closure writes.
    unwritten_cells: Option<&'a FxHashSet<RcLocal>>,
    /// Registers of the phi transports this destructor creates.
    transport_groups: FxHashMap<RcLocal, usize>,
    /// Locals a phi transport carries (`temp = local` on an edge).
    transported: FxHashSet<RcLocal>,
    /// Locals closures of this function capture, collected on first use.
    captured: Option<FxHashSet<RcLocal>>,
    /// Roots of the cells whose value may change after a copy of it is
    /// taken ([`Self::find_unstable_cells`]).
    unstable_cells: FxHashSet<RcLocal>,
    /// `(value's stable id, dominator order, statement)` for every by-value
    /// capture made by a statement that also writes a local. A closure reads
    /// its captures whenever it runs, so after that statement has written its
    /// targets. Recorded in [`Self::build_def_use`]: coalescing removes
    /// emptied statements, so positions looked up later would be stale.
    value_captures: FxHashSet<(u64, usize, usize)>,
}

/// Terminal SSA still needs copy/capture coalescing and sequentialization,
/// but every definition belongs to this one block and no value is live-out.
/// Keep malformed or already-structured control on the legacy analysis path.
fn terminal_destruction_block(function: &Function) -> Option<NodeIndex> {
    #[cfg(test)]
    if REFERENCE_TERMINAL_DESTRUCTION.with(std::cell::Cell::get) { return None; }
    if function.graph().node_count() != 1 || function.graph().edge_count() != 0 { return None; }
    let entry = function.entry().as_ref().copied()?;
    let block = function.block(entry)?;
    ast::telemetry::count("ssa_destruct_terminal_eligible", 1);
    block.iter().enumerate().all(|(index, statement)| match statement {
        ast::Statement::Assign(assign) => !assign.parallel || (assign.right.len() >= assign.left.len()
            && assign.left.iter().all(|left| left.as_local().is_some())),
        ast::Statement::Call(_) | ast::Statement::MethodCall(_) | ast::Statement::SetList(_)
            | ast::Statement::Comment(_) | ast::Statement::Empty(_) => true,
        ast::Statement::Return(_) => index + 1 == block.len(),
        _ => false,
    }).then_some(entry)
}

#[cfg(test)]
thread_local! {
    static REFERENCE_TERMINAL_DESTRUCTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod terminal_tests;

#[cfg(test)]
mod capture_tests;

/// Visit the by-value captures of the closures in `statement`, at any depth
/// of its values (index targets included; nested function bodies aside).
/// This runs on every assignment of a function that makes closures, and most
/// hold none: an assignment's values are matched out by kind, so a leaf costs
/// no call, where `Traverse` makes one per value.
fn visit_value_captures(statement: &ast::Statement, visit: &mut impl FnMut(&RcLocal)) {
    let ast::Statement::Assign(assign) = statement else {
        statement.traverse_rvalues_ref(&mut |value| {
            if let ast::RValue::Closure(closure) = value {
                closure_captures(closure, visit);
            }
        });
        return;
    };
    #[cfg(any(test, debug_assertions))]
    let mut found = Vec::new();
    let mut visit = |local: &RcLocal| {
        #[cfg(any(test, debug_assertions))]
        found.push(local.stable_id());
        visit(local);
    };
    for left in &assign.left {
        if let ast::LValue::Index(index) = left {
            value_captures(&index.left, &mut visit);
            value_captures(&index.right, &mut visit);
        }
    }
    assign.right.iter().for_each(|value| value_captures(value, &mut visit));
    #[cfg(any(test, debug_assertions))]
    {
        let mut expected = Vec::new();
        statement.traverse_rvalues_ref(&mut |value| {
            if let ast::RValue::Closure(closure) = value {
                closure_captures(closure, &mut |local| expected.push(local.stable_id()));
            }
        });
        assert_eq!(found, expected, "the by-value captures of {statement}");
    }
}

fn closure_captures(closure: &ast::Closure, visit: &mut impl FnMut(&RcLocal)) {
    for upvalue in &closure.upvalues {
        if let ast::Upvalue::Copy(local) = upvalue {
            visit(local);
        }
    }
}

fn value_captures(value: &ast::RValue, visit: &mut impl FnMut(&RcLocal)) {
    use ast::{RValue, Select};
    match value {
        RValue::Closure(closure) => closure_captures(closure, visit),
        RValue::Call(call) | RValue::Select(Select::Call(call)) => {
            value_captures(&call.value, visit);
            call.arguments.iter().for_each(|argument| value_captures(argument, visit));
        }
        RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
            value_captures(&call.value, visit);
            call.arguments.iter().for_each(|argument| value_captures(argument, visit));
        }
        RValue::Table(table) => {
            for (key, value) in &table.0 {
                if let Some(key) = key {
                    value_captures(key, visit);
                }
                value_captures(value, visit);
            }
        }
        RValue::Index(index) => {
            value_captures(&index.left, visit);
            value_captures(&index.right, visit);
        }
        RValue::Unary(unary) => value_captures(&unary.value, visit),
        RValue::Binary(binary) => {
            value_captures(&binary.left, visit);
            value_captures(&binary.right, visit);
        }
        RValue::IfExpression(expression) => {
            value_captures(&expression.condition, visit);
            value_captures(&expression.then_value, visit);
            value_captures(&expression.else_value, visit);
        }
        RValue::Local(_) | RValue::Global(_) | RValue::Literal(_) | RValue::VarArg(_) | RValue::Select(Select::VarArg(_)) => {}
    }
}

impl<'a> Destructor<'a> {
    pub fn new(
        function: &'a mut Function,
        upvalue_to_group: IndexMap<RcLocal, RcLocal>,
        upvalues_in: FxHashSet<RcLocal>,
        local_count: usize,
    ) -> Self {
        let terminal_block = terminal_destruction_block(function);
        Self {
            function,
            upvalue_to_group,
            upvalues_in,
            values: FxHashMap::with_capacity_and_hasher(local_count, Default::default()),
            congruence_classes: FxHashMap::with_capacity_and_hasher(
                local_count,
                Default::default(),
            ),
            equal_ancestor_in: FxHashMap::default(),
            equal_ancestor_out: FxHashMap::default(),
            local_defs: FxHashMap::with_capacity_and_hasher(local_count, Default::default()),
            local_last_use: FxHashMap::default(),
            dominator_children: Vec::new(),
            dominators: Vec::new(),
            liveness: Liveness::default(),
            undesirable_blocks: FxHashSet::default(),
            terminal_block,
            register_groups: None,
            unwritten_cells: None,
            transport_groups: FxHashMap::default(),
            transported: FxHashSet::default(),
            captured: None,
            unstable_cells: FxHashSet::default(),
            value_captures: FxHashSet::default(),
        }
    }

    /// Coalesce each bytecode register's own phi web before any copy between
    /// registers. `register_groups` maps SSA versions to the lifter local
    /// (register) they came from; versions it does not know keep the legacy
    /// order. See [`Self::coalesce_copies`].
    pub fn with_register_groups(mut self, register_groups: &'a FxHashMap<RcLocal, usize>) -> Self {
        self.register_groups = Some(register_groups);
        self
    }

    pub fn with_unwritten_cells(mut self, unwritten_cells: &'a FxHashSet<RcLocal>) -> Self {
        self.unwritten_cells = Some(unwritten_cells);
        self
    }

    /// Joins `left` to the cell of `right` for the copy `left = right` out of
    /// a cell no closure writes (`local instance = attachment`), when `left`
    /// is defined only there, flows into no phi (whose other values would
    /// then be written to the cell), is captured by no closure (which, in
    /// source, would see the cell's later writes), and is dead at every
    /// later write of it:
    /// every such write is a definition here, so `left` then always reads
    /// the value it copied. Value equality does not decide: all versions of a
    /// cell are one variable, so equal versions may still be overwritten. A
    /// copy into a cell would give it its value early, where a closure may
    /// still read the old one.
    fn captured_locals(&mut self) -> &FxHashSet<RcLocal> {
        let function = &*self.function;
        self.captured.get_or_insert_with(|| {
            let mut captured = FxHashSet::default();
            for (_, block) in function.blocks() {
                for statement in block.iter() {
                    statement.traverse_rvalues_ref(&mut |value| {
                        if let ast::RValue::Closure(closure) = value {
                            captured.extend(closure.upvalues.iter().map(|upvalue| {
                                let (ast::Upvalue::Copy(local) | ast::Upvalue::Ref(local)) = upvalue;
                                local.clone()
                            }));
                        }
                    });
                }
            }
            captured
        })
    }

    fn coalesce_unwritten_cell_copy(&mut self, left: &RcLocal, right: &RcLocal) -> bool {
        if self.upvalue_to_group.contains_key(left)
            || self.transported.contains(left)
            || !self.upvalue_to_group.get(right)
                .is_some_and(|cell| self.unwritten_cells.is_some_and(|cells| cells.contains(cell)))
            || self.captured_locals().contains(left)
        {
            return false;
        }
        // A copy keeping a source local of its own (`local second = value`)
        // stays that local, as any copy does.
        if !left.source_bindings_compatible(right) {
            return false;
        }
        let copy = self.get_congruence_class(left.clone()).clone();
        let cell = self.get_congruence_class(right.clone()).clone();
        if copy.borrow().len() != 1
            || Rc::ptr_eq(&copy, &cell)
            || !cell.borrow().bindings().compatible(copy.borrow().bindings())
        {
            return false;
        }
        let written_while_live = cell.borrow().values().any(|version| {
            version != left && self.dominates(left, version) && self.intersect(version, left)
        });
        if written_while_live {
            return false;
        }
        self.merge_congruence_classes(&cell, &copy);
        // The class joined without an interference walk, so `left` has no
        // equal ancestor recorded. Later walks only test a candidate against
        // the closest dominating member and that member's equal ancestors:
        // through `left` they must still reach the version it copies, which
        // may outlive it (`tmp = b; b = a; a = tmp` with `a` copied before).
        self.equal_ancestor_in.insert(left.clone(), right.clone());
        // `left` is the cell now: no later copy may join another definition
        // to it, which would become a write closures observe.
        let root = self.upvalue_to_group[right].clone();
        self.upvalue_to_group.insert(left.clone(), root);
        true
    }

    /// Cells whose value may change after a copy of it is taken: written by
    /// a closure, or here by more than one statement. All versions of a cell
    /// are one variable and a read names an earlier version (`return a` with
    /// `a` the cell's first version after `a, b = b, a` wrote it), so SSA
    /// values and liveness do not show such a change. Found before
    /// `lift_params` adds transports, while phis are still edge arguments.
    fn find_unstable_cells(&self) -> FxHashSet<RcLocal> {
        let mut definitions = FxHashMap::<&RcLocal, usize>::default();
        // A version entering as a parameter or a phi is a definition too
        // (`local b = if c then 1 else -1`, then `b = a`): one per version,
        // however many edges supply it.
        let mut entering = FxHashSet::default();
        let phis = self.function.graph().edge_weights().flat_map(|edge| edge.arguments.iter().map(|(param, _)| param));
        for version in self.function.parameters.iter().chain(phis) {
            if let Some(cell) = self.upvalue_to_group.get(version)
                && entering.insert(version)
            {
                *definitions.entry(cell).or_default() += 1;
            }
        }
        for (_, block) in self.function.blocks() {
            for statement in block.iter() {
                statement.visit_local_writes(&mut |local| {
                    if let Some(cell) = self.upvalue_to_group.get(local) {
                        *definitions.entry(cell).or_default() += 1;
                    }
                    true
                });
            }
        }
        self.upvalue_to_group.values()
            .filter(|cell| definitions.get(cell).is_some_and(|&count| count > 1)
                || !self.unwritten_cells.is_some_and(|cells| cells.contains(*cell)))
            .cloned()
            .collect()
    }

    fn in_unstable_cell(&self, local: &RcLocal) -> bool {
        self.upvalue_to_group.get(local).is_some_and(|cell| self.unstable_cells.contains(cell))
    }

    fn register_group(&self, local: &RcLocal) -> Option<usize> {
        self.register_groups?.get(local).or_else(|| self.transport_groups.get(local)).copied()
    }

    pub fn destruct(mut self) {
        if let Some(node) = self.terminal_block {
            ast::telemetry::count("ssa_destruct_terminal_admitted", 1);
            ast::telemetry::count("ssa_destruct_terminal_statements", self.function.block(node).unwrap().len() as u64);
        }
        self.unstable_cells = self.find_unstable_cells();
        let phase = ast::telemetry::Span::new("SSA_LIFT_PARAMS");
        if self.terminal_block.is_none() {
            self.lift_params();
            self.sort_params();
        }
        drop(phase);

        let phase = ast::telemetry::Span::new("SSA_LIVENESS");
        if self.terminal_block.is_none() {
            self.liveness = Liveness::calculate(self.function);
        }
        drop(phase);
        // this is for debugging :)
        //crate::dot::render_to(self.function, &mut std::io::stdout()).unwrap();

        let phase = ast::telemetry::Span::new("SSA_DEF_USE");
        self.build_def_use();
        drop(phase);

        let phase = ast::telemetry::Span::new("SSA_VALUE_INTERFERENCE");
        self.compute_value_interference();
        drop(phase);

        let phase = ast::telemetry::Span::new("SSA_COALESCE_MANDATORY");
        self.coalesce_upvalues();
        if self.terminal_block.is_none() { self.coalesce_params(); }
        drop(phase);
        let phase = ast::telemetry::Span::new("SSA_COALESCE_COPIES");
        self.coalesce_copies();
        self.coalesce_dead_self_updates();
        drop(phase);
        #[cfg(any(test, debug_assertions))]
        self.assert_captures_keep_their_values();

        let phase = ast::telemetry::Span::new("SSA_APPLY_LOCAL_MAP");
        super::construct::apply_local_map(self.function, self.build_local_map());
        drop(phase);

        //crate::dot::render_to(self.function, &mut std::io::stdout()).unwrap();

        self.sink_for_step_copies();
        let _phase = ast::telemetry::Span::new("SSA_SEQUENTIALIZE");
        self.sequentialize();
    }

    /// A `for` step opens its block: no source runs code between the end of
    /// an iteration (or the preparation) and the step. Phi copies coalescing
    /// left at the head of a step block run at the end of every edge into it
    /// instead, before a preparation marker where an edge transfer may go,
    /// in a block of their own on an edge out of a branch.
    fn sink_for_step_copies(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            if !self.is_for_next(node) {
                continue;
            }
            let block = self.function.block(node).unwrap();
            let step = block.len() - 1;
            let copies_only = block.0[..step].iter().all(|statement| match statement {
                ast::Statement::Empty(_) => true,
                ast::Statement::Assign(assign) => {
                    assign.left.iter().all(|left| left.as_local().is_some())
                        && assign.right.iter().all(|right| right.as_local().is_some())
                }
                _ => false,
            });
            if step == 0 || !copies_only {
                continue;
            }
            // A copy the preparation of a loop entering here must run first
            // (it touches a cell `__iter` code may see) would follow that
            // preparation, which no source spells: the copies stay.
            let head = &self.function.block(node).unwrap().0[..step];
            let after_preparation = self.function.graph()
                .neighbors_directed(node, petgraph::Direction::Incoming)
                .filter(|&pred| self.function.edges(pred).count() == 1)
                .any(|pred| {
                    let block = self.function.block(pred).unwrap();
                    matches!(block.last(), Some(ast::Statement::GenericForInit(_) | ast::Statement::NumForInit(_)))
                        && head.iter().filter_map(ast::Statement::as_assign).any(|copy| {
                            Self::split_edge_transfer_around_for_prep(block, copy.clone(), &self.upvalue_to_group)
                                .1
                                .is_some()
                        })
                });
            if after_preparation {
                continue;
            }
            let copies = self.function.block_mut(node).unwrap().0.drain(..step)
                .filter_map(|statement| match statement {
                    ast::Statement::Assign(assign) => Some(assign),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let incoming = self.function.graph()
                .edges_directed(node, petgraph::Direction::Incoming)
                .map(|edge| edge.id())
                .collect::<Vec<_>>();
            for edge in incoming {
                let (pred, _) = self.function.graph().edge_endpoints(edge).unwrap();
                let target = if self.function.edges(pred).count() == 1 {
                    pred
                } else {
                    let weight = self.function.graph_mut().remove_edge(edge).unwrap();
                    let split = self.function.new_block();
                    self.function.set_edges(split, vec![(
                        node,
                        BlockEdge { branch_type: BranchType::Unconditional, arguments: weight.arguments },
                    )]);
                    self.function.graph_mut().add_edge(pred, split, BlockEdge::new(weight.branch_type));
                    split
                };
                for copy in &copies {
                    let block = self.function.block_mut(target).unwrap();
                    let (before_prep, after_prep) =
                        Self::split_edge_transfer_around_for_prep(block, copy.clone(), &self.upvalue_to_group);
                    if let Some(before_prep) = before_prep {
                        let marker_index = block.len() - 1;
                        block.insert(marker_index, before_prep.into());
                    }
                    if let Some(after_prep) = after_prep {
                        block.push(after_prep.into());
                    }
                }
            }
        }
    }

    fn coalesce_upvalues(&mut self) {
        for (upvalue, group) in self
            .upvalue_to_group
            .iter()
            .map(|(u, g)| (u.clone(), g.clone()))
            .collect::<Vec<_>>()
        {
            let con_class = self.get_congruence_class(group.clone()).clone();
            let (upval_dom_index, _, upval_stat_index) = self.local_defs[&upvalue];
            con_class
                .borrow_mut()
                .insert((upval_dom_index, upval_stat_index), upvalue.clone());
            self.congruence_classes.insert(upvalue.clone(), con_class);
        }
    }

    fn sequentialize(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            let mut replace_map = Vec::new();
            for (stat_index, stat) in self
                .function
                .block_mut(node)
                .unwrap()
                .0
                .iter_mut()
                .enumerate()
            {
                if let ast::Statement::Assign(assign) = stat {
                    if assign.parallel {
                        if assign.left.len() == 1 {
                            if assign.right[0]
                                .as_local()
                                .is_some_and(|r| r == assign.left[0].as_local().unwrap())
                            {
                                // redundant assign, we can remove it
                                replace_map.push((stat_index, Vec::new()))
                            } else {
                                assign.parallel = false;
                            }
                        } else {
                            let mut ready = Vec::new();
                            let mut to_do = Vec::new();
                            let mut loc = FxHashMap::default();
                            let mut pred = FxHashMap::default();

                            // The set of parallel destinations. A non-local RHS that
                            // READS one of these must see its PRE-copy value, so it is
                            // pre-evaluated into a temp at the FRONT (before any copy
                            // clobbers it); otherwise the local-to-local copies below
                            // run first and the expression reads the wrong value
                            // (C3 — `x,y = y, x+y` lowered to `x=y; y=x+y` computed
                            // `(new x)+y` and turned Fibonacci into powers of two).
                            let dst_set: FxHashSet<RcLocal> = assign
                                .left
                                .iter()
                                .filter_map(|l| l.as_local().cloned())
                                .collect();
                            let mut result_head = Vec::new();
                            let mut result_end = Vec::new();
                            for i in 0..assign.left.len() {
                                // TODO: unneccessary clones, take assign.left and assign.right
                                let dst = assign.left[i].as_local().unwrap();
                                match &assign.right[i] {
                                    ast::RValue::Local(src) => {
                                        loc.insert(src.clone(), src.clone());
                                        pred.insert(dst.clone(), src.clone());
                                        to_do.push(dst.clone());
                                    }
                                    rvalue => {
                                        // The inliner only places side-effect-free,
                                        // non-upvalue rvalues into a parallel copy, so
                                        // pre-evaluating one cannot reorder effects.
                                        //
                                        // Interference is reading ANOTHER destination
                                        // (one a copy will clobber), not its OWN: a
                                        // self-update `x = x + c` reads `x` before it
                                        // writes `x` in the same statement, so it stays
                                        // a plain (compound) assign — only a coupled RHS
                                        // like Fibonacci's `y = x + y` (reads the other
                                        // destination `x`) needs the pre-spill.
                                        let reads_other_dst = rvalue
                                            .values_read()
                                            .iter()
                                            .any(|r| *r != dst && dst_set.contains(*r));
                                        if reads_other_dst {
                                            let tmp = RcLocal::default();
                                            result_head.push(ast::Assign::new(
                                                vec![tmp.clone().into()],
                                                vec![rvalue.clone()],
                                            ));
                                            result_end.push(ast::Assign::new(
                                                vec![dst.clone().into()],
                                                vec![tmp.into()],
                                            ));
                                        } else {
                                            result_end.push(ast::Assign::new(
                                                vec![dst.clone().into()],
                                                vec![rvalue.clone()],
                                            ));
                                        }
                                    }
                                }
                            }

                            for i in 0..assign.left.len() {
                                let dst = assign.left[i].as_local().unwrap();
                                if !loc.contains_key(dst) && assign.right[i].as_local().is_some() {
                                    ready.push(dst.clone());
                                }
                            }

                            let mut spill = None;
                            // Head spills (pre-evaluated destination-reading rvalues)
                            // run BEFORE the local-to-local copy resolution; the tail
                            // (destination writes from temps / non-interfering rvalues)
                            // runs after.
                            let mut result = result_head;
                            while let Some(local_b) = to_do.pop() {
                                while let Some(local_b) = ready.pop() {
                                    let local_a = pred[&local_b].clone();
                                    let local_c = loc[&local_a].clone();
                                    result.push(ast::Assign::new(
                                        vec![local_b.clone().into()],
                                        vec![local_c.clone().into()],
                                    ));
                                    if local_a == local_c && pred.contains_key(&local_a) {
                                        ready.push(local_a.clone());
                                    }
                                    loc.insert(local_a, local_b);
                                }

                                if local_b != loc[&pred[&local_b]] {
                                    let spill = spill.get_or_insert_with(RcLocal::default);
                                    result.push(ast::Assign::new(
                                        vec![spill.clone().into()],
                                        vec![local_b.clone().into()],
                                    ));
                                    loc.insert(local_b.clone(), spill.clone());
                                    ready.push(local_b);
                                }
                            }
                            result.extend(result_end);

                            replace_map.push((stat_index, result))
                        }
                    }
                }
            }

            let block = self.function.block_mut(node).unwrap();
            for (stat_index, assigns) in replace_map.into_iter().rev() {
                block.splice(
                    stat_index..stat_index + 1,
                    // TODO: pad with ast::Empty and then use retain
                    assigns.into_iter().map(|a| a.into()),
                );
            }
        }
    }

    fn build_local_map(&self) -> FxHashMap<RcLocal, RcLocal> {
        let mut map = FxHashMap::default();
        for (local, con_class) in &self.congruence_classes {
            let con_class = con_class.borrow();
            let new_local = con_class.iter().next().unwrap().1;
            // TODO: see apply_local_map TODO,
            // we dont want to handle this here
            if local != new_local {
                map.insert(local.clone(), new_local.clone());
            }
        }
        map
    }

    /// Invariant I1, checked before the local map is applied: a closure made
    /// by a statement (at any depth of it) reads its by-value captures after
    /// the statement wrote its targets, so a capture may share a variable
    /// with another target of its statement only when both hold one value.
    /// Printed, `x = function() ... x ... end` always means the closure
    /// captures itself, which `materialize_value_captures` relies on.
    #[cfg(any(test, debug_assertions))]
    fn assert_captures_keep_their_values(&self) {
        let class = |local: &RcLocal| self.congruence_classes.get(local).map(Rc::as_ptr);
        let value = |local: &RcLocal| self.values.get(local).map(Rc::as_ptr);
        for (_, block) in self.function.blocks() {
            for statement in block.iter() {
                let mut captures = Vec::new();
                statement.traverse_rvalues_ref(&mut |rvalue| {
                    if let ast::RValue::Closure(closure) = rvalue {
                        captures.extend(closure.upvalues.iter().filter_map(|upvalue| match upvalue {
                            ast::Upvalue::Copy(local) => Some(local.clone()),
                            ast::Upvalue::Ref(_) => None,
                        }));
                    }
                });
                if captures.is_empty() {
                    continue;
                }
                statement.visit_local_writes(&mut |target| {
                    for capture in &captures {
                        let shared = capture != target && class(capture).is_some() && class(capture) == class(target);
                        let same_value = value(capture).is_some() && value(capture) == value(target);
                        assert!(
                            !shared || same_value,
                            "closure capture {capture} shares a variable with {target}, written by its own statement {statement}"
                        );
                    }
                    true
                });
            }
        }
    }

    // TODO: combine with compute value interference
    fn build_def_use(&mut self) {
        #[cfg(test)]
        let mut last_use_reference = FxHashMap::<RcLocal, FxHashMap<NodeIndex, (usize, ParamOrStatIndex)>>::default();
        if self.terminal_block.is_none() {
            let dominators = crate::dominators::Dominators::new(self.function.graph(), self.function.entry().unwrap());
            let bound = self.function.graph().node_bound();
            self.dominator_children = vec![Vec::new(); bound];
            for node in self.function.graph().node_indices() {
                if let Some(dominator) = dominators.immediate_dominator(node) {
                    self.dominator_children[dominator.index()].push(node);
                }
            }
            self.dominators = vec![(0, 0); bound];
            let mut clock = 0;
            let mut walk = vec![(self.function.entry().unwrap(), false)];
            while let Some((node, exiting)) = walk.pop() {
                if exiting {
                    self.dominators[node.index()].1 = clock;
                } else {
                    self.dominators[node.index()] = (clock, 0);
                    clock += 1;
                    walk.push((node, true));
                    walk.extend(self.dominator_children[node.index()].iter().map(|&child| (child, false)));
                }
            }
        }

        let mut dominator_index = 0;
        // Interference's ancestor stack requires dominator-tree preorder
        // (children pushed in order, visited last-first).
        // CFG DFS can visit an exit sibling before a dominated loop branch.
        let mut preorder = vec![self.function.entry().unwrap()];
        while let Some(node) = preorder.pop() {
            if let Some(children) = self.dominator_children.get(node.index()) {
                preorder.extend(children.iter().copied());
            }
            if node == self.function.entry().unwrap() {
                assert!(dominator_index == 0);
                assert!(!self
                    .function
                    .edges_to_block(node)
                    .any(|(_, e)| !e.arguments.is_empty()));
                for (i, local) in self
                    .upvalues_in
                    .iter()
                    .chain(self.upvalue_to_group.iter().flat_map(|(u, g)| [u, g]))
                    .chain(self.function.parameters.iter())
                    .enumerate()
                {
                    if !self.local_defs.contains_key(local) {
                        self.local_defs.insert(
                            local.clone(),
                            (dominator_index, node, ParamOrStatIndex::Param(i)),
                        );
                    }
                }
            }

            if let Some((_, edge)) = self.function.edges_to_block(node).next() {
                for (param_index, (param, _)) in edge.arguments.iter().enumerate() {
                    self.local_defs.insert(
                        param.clone(),
                        (dominator_index, node, ParamOrStatIndex::Param(param_index)),
                    );
                }
            }
            for (stat_index, stat) in self.function.block(node).unwrap().0.iter().enumerate() {
                let mut writes_local = false;
                stat.visit_local_writes(&mut |local| {
                    writes_local = true;
                    self.local_defs.insert(
                        local.clone(),
                        (dominator_index, node, ParamOrStatIndex::Stat(stat_index)),
                    );
                    true
                });
                // A closure at any depth counts: `x = keep(function() ... end)`
                // stores the closure before it can run, as `x = function() ...
                // end` does.
                if writes_local && self.function.may_hold_closures {
                    visit_value_captures(stat, &mut |local| {
                        self.value_captures.insert((local.stable_id(), dominator_index, stat_index));
                    });
                }

                stat.visit_local_reads(&mut |local| {
                    self.local_last_use
                        .entry(local.clone())
                        .or_default()
                        .record(dominator_index, stat_index);
                    #[cfg(test)]
                    last_use_reference.entry(local.clone()).or_default()
                        .insert(node, (dominator_index, ParamOrStatIndex::Stat(stat_index)));
                    true
                });
            }
            dominator_index += 1;
        }
        #[cfg(test)]
        {
            assert_eq!(self.local_last_use.len(), last_use_reference.len());
            for (local, expected) in last_use_reference {
                let actual = &self.local_last_use[&local];
                assert_eq!(actual.0.len(), expected.len());
                for (_, (order, position)) in expected {
                    assert_eq!(actual.get(order).map(ParamOrStatIndex::Stat), Some(position));
                }
            }
        }
        if ast::telemetry::enabled() {
            ast::telemetry::count("destruct_last_use_locals", self.local_last_use.len() as u64);
            let mut entries = 0;
            let mut single_block = 0;
            for uses in self.local_last_use.values() {
                entries += uses.0.len();
                single_block += usize::from(uses.0.len() == 1);
            }
            ast::telemetry::count("destruct_last_use_block_entries", entries as u64);
            ast::telemetry::count("destruct_last_use_single_block_locals", single_block as u64);
        }
    }

    // a dominates b?
    // same as dominates if a == b
    fn check_pre_dom_order(&self, a: &RcLocal, b: &RcLocal) -> bool {
        let (a_dom_index, _, a_stat_index) = self.local_defs[a];
        let (b_dom_index, _, b_stat_index) = self.local_defs[b];
        (a_dom_index, a_stat_index) < (b_dom_index, b_stat_index)
    }

    // initialize congruence classes based on block params and remove block params
    fn coalesce_params(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            for edge in self
                .function
                .graph()
                .edges_directed(node, Direction::Incoming)
                .map(|e| e.id())
                .collect::<Vec<_>>()
            {
                let args = std::mem::take(
                    &mut self
                        .function
                        .graph_mut()
                        .edge_weight_mut(edge)
                        .unwrap()
                        .arguments,
                );

                for (param, arg) in args {
                    let arg = arg.into_local().unwrap();
                    let congruence_class = self.get_congruence_class(param).clone();
                    // A transport carrying a cell's value into its own
                    // register's phi already joined the cell: the phi joins
                    // it too, so no member ever sits in two classes.
                    if let Some(cell) = self.congruence_classes.get(&arg).cloned() {
                        if !Rc::ptr_eq(&cell, &congruence_class) {
                            self.merge_congruence_classes(&cell, &congruence_class);
                        }
                        continue;
                    }

                    let (dominator_index, _, stat_index) = self.local_defs[&arg];
                    congruence_class
                        .borrow_mut()
                        .insert((dominator_index, stat_index), arg.clone());
                    self.congruence_classes.insert(arg, congruence_class);
                }
            }
        }
    }

    fn get_congruence_class(&mut self, local: RcLocal) -> &Rc<RefCell<CongruenceClass>> {
        self.congruence_classes
            .entry(local.clone())
            .or_insert_with(|| {
                let mut congruence_class = CongruenceClass::default();
                let (dominator_index, _, stat_index) = self.local_defs[&local];
                congruence_class.insert((dominator_index, stat_index), local);
                Rc::new(RefCell::new(congruence_class))
            })
    }

    fn is_for_next(&self, node: NodeIndex) -> bool {
        self.function
            .block(node)
            .unwrap()
            .last()
            .map(|s| {
                matches!(
                    s,
                    ast::Statement::GenericForNext(_) | ast::Statement::NumForNext(_)
                )
            })
            .unwrap_or(false)
    }

    fn coalesce_copies_for_block(&mut self, node: NodeIndex, same_register_only: bool) {
        for stat_index in 0..self.function.block_mut(node).unwrap().0.len() {
            let should_remove = if let ast::Statement::Assign(assign) =
                &self.function.block(node).unwrap()[stat_index]
                // Most assignments copy no local; skip them without allocating.
                && assign.left.iter().zip(&assign.right)
                    .any(|(left, right)| left.as_local().is_some() && right.as_local().is_some())
            {
                let mut to_remove = Vec::new();
                let left = assign
                    .left
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| Some((i, l.as_local()?.clone())))
                    .collect::<Vec<_>>();
                for (i, left, right) in left
                    .into_iter()
                    .filter_map(|(i, l)| Some((i, l, assign.right.get(i)?.as_local()?.clone())))
                    .collect::<Vec<_>>()
                {
                    // upvalues in and parameters cannot be coalesced
                    debug_assert!(
                        !(self.function.parameters.contains(&left)
                            && self.upvalues_in.contains(&right))
                            || self.function.parameters.contains(&right)
                                && self.upvalues_in.contains(&left)
                    );

                    if self.upvalue_to_group.contains_key(&left)
                        || self.upvalue_to_group.contains_key(&right)
                    {
                        if self.coalesce_unwritten_cell_copy(&left, &right) {
                            to_remove.push(i);
                        }
                        continue;
                    }
                    if same_register_only
                        && !self.register_group(&left)
                            .is_some_and(|group| self.register_group(&right) == Some(group))
                    {
                        continue;
                    }

                    if self.try_coalesce_copy_by_value(left.clone(), right.clone())
                        || self.try_coalesce_copy_by_sharing(&left, &right)
                    {
                        to_remove.push(i);
                    }
                }
                let assign = self.function.block_mut(node).unwrap()[stat_index]
                    .as_assign_mut()
                    .unwrap();
                for i in to_remove.into_iter().rev() {
                    assign.left.remove(i);
                    assign.right.remove(i);
                }
                assign.left.is_empty()
            } else {
                false
            };

            if should_remove {
                let block = self.function.block_mut(node).unwrap();
                block[stat_index] = ast::Empty {}.into();
            }
        }

        // we check block.ast.len() elsewhere and do `i - ` elsewhere so we need to get rid of empty statements
        // TODO: fix here and elsewhere, see inline.rs
        let block = self.function.block_mut(node).unwrap();
        block.retain(|s| s.as_empty().is_none());
    }

    /// Greedy copy coalescing; every merge is still checked for interference,
    /// so the order changes only which copies survive, never correctness.
    ///
    /// With register groups, the first sweep merges only copies between
    /// versions of one bytecode register: the transports of that register's
    /// phi web. A copy between two registers (`last = now`) then stays an
    /// assignment instead of pulling the whole web into `now`, which would
    /// leave compensating copies on every other path and loop edge, and a
    /// reassigned source variable keeps one name (`x = min ... x = max`).
    fn coalesce_copies(&mut self) {
        let sweeps: &[bool] = if self.register_groups.is_some() { &[true, false] } else { &[false] };
        for &same_register_only in sweeps {
            let mut dominator_dfs = Dfs::new(self.function.graph(), self.function.entry().unwrap());
            while let Some(node) = dominator_dfs.next(self.function.graph()) {
                if self.undesirable_blocks.contains(&node) {
                    self.coalesce_copies_for_block(node, same_register_only);
                }
            }

            let mut dominator_dfs = Dfs::new(self.function.graph(), self.function.entry().unwrap());
            while let Some(node) = dominator_dfs.next(self.function.graph()) {
                self.coalesce_copies_for_block(node, same_register_only);
            }
        }
    }

    /// A dead self-update `v = v + 1` (no statement reads its result and no
    /// phi rescues it, e.g. the last increment on one branch) is otherwise a
    /// singleton class and prints as `local _ = v + 1`, hiding the source
    /// `v += 1` (C13 self-update). When its value reads exactly one version of
    /// its own bytecode register, join that version's class if the ordinary
    /// interference and binding checks allow. The same-register read keeps
    /// every other `local _ =` (a discarded field read, call or unrelated
    /// closure reusing the register) out of reach.
    fn coalesce_dead_self_updates(&mut self) {
        if self.register_groups.is_none() {
            return;
        }
        let mut self_updates = Vec::new();
        for (_, block) in self.function.blocks() {
            for statement in block.iter() {
                let ast::Statement::Assign(assign) = statement else { continue };
                if assign.parallel || assign.left.len() != 1 {
                    continue;
                }
                let Some(dst) = assign.left[0].as_local() else { continue };
                if self.local_last_use.contains_key(dst)
                    || self.upvalues_in.contains(dst)
                    || self.upvalue_to_group.contains_key(dst)
                {
                    continue;
                }
                let Some(group) = self.register_group(dst) else { continue };
                // One version of the register, possibly read twice (`v + v`);
                // two distinct versions are ambiguous.
                let mut operand: Option<&RcLocal> = None;
                let single = statement.visit_local_reads(&mut |read| {
                    if self.register_group(read) != Some(group) {
                        return true;
                    }
                    match operand {
                        Some(seen) => seen == read,
                        None => {
                            operand = Some(read);
                            true
                        }
                    }
                });
                if let Some(operand) = operand.filter(|_| single) {
                    self_updates.push((dst.clone(), operand.clone()));
                }
            }
        }
        self_updates.sort_by_key(|(dst, _)| self.local_defs.get(dst).map(|&(order, _, index)| (order, index)));
        for (dst, operand) in self_updates {
            if !dst.source_bindings_compatible(&operand) {
                continue;
            }
            let dead_class = self.get_congruence_class(dst).clone();
            let live_class = self.get_congruence_class(operand).clone();
            // A class holding a cell (its own versions, or a copy of it
            // `coalesce_unwritten_cell_copy` joined) is the captured variable:
            // the dead write would become a store closures observe
            // (`local y = x; y += 1` must not print as `x += 1`).
            if Rc::ptr_eq(&dead_class, &live_class)
                || dead_class.borrow().len() != 1
                || !live_class.borrow().bindings().compatible(dead_class.borrow().bindings())
                || live_class.borrow().values().any(|version| self.upvalue_to_group.contains_key(version))
            {
                continue;
            }
            if !self.check_interfere(&live_class, &dead_class) {
                self.merge_congruence_classes(&live_class, &dead_class);
            }
        }
    }

    fn try_coalesce_copy_by_value(&mut self, left: RcLocal, right: RcLocal) -> bool {
        if !left.source_bindings_compatible(&right) { return false; }
        let left_con_class = self.get_congruence_class(left).clone();
        let right_con_class = self.get_congruence_class(right).clone();

        // Exact equivalent of the former pairwise cross-product, including
        // internally conflicting mandatory classes. Never accept a same-class
        // copy before checking these constraints. Cache invalidates on every
        // membership change; source metadata stays immutable during coalescing.
        if !left_con_class.borrow().bindings().compatible(right_con_class.borrow().bindings()) {
            return false;
        }

        // Congruence classes form a partition: two live classes share members
        // exactly when they share this owner. Avoid comparing growing maps.
        if Rc::ptr_eq(&left_con_class, &right_con_class) {
            true
        } else if share_definition_point(&left_con_class.borrow(), &right_con_class.borrow()) {
            // Two values one statement defines (`local a, b, c = f()`) are
            // alive together from that point; the dominance-order test never
            // compares them, and one class could not even hold both.
            false
        } else if left_con_class.borrow().len() == 1 && right_con_class.borrow().len() == 1 {
            if self.check_interfere_single(&left_con_class, &right_con_class) {
                false
            } else {
                self.merge_congruence_classes(&left_con_class, &right_con_class);
                true
            }
        } else if !self.check_interfere(&left_con_class, &right_con_class) {
            self.merge_congruence_classes(&left_con_class, &right_con_class);
            true
        } else {
            false
        }
    }

    // Process the copy local_a = local_b (destination first).
    fn try_coalesce_copy_by_sharing(&mut self, local_a: &RcLocal, local_b: &RcLocal) -> bool {
        if !local_a.source_bindings_compatible(local_b) { return false; }
        let con_class_x = self.get_congruence_class(local_a.clone()).clone();
        let con_class_y = self.get_congruence_class(local_b.clone()).clone();

        let values = self
            .get_value_class(local_a.clone())
            .borrow()
            .iter()
            .cloned()
            .collect_vec();
        for local_c in values {
            if &local_c == local_b
                || &local_c == local_a
                || !self.check_pre_dom_order(&local_c, local_a)
                || !self.intersect(local_a, &local_c)
            {
                continue;
            }

            let con_class_z = self.get_congruence_class(local_c.clone()).clone();
            if Rc::ptr_eq(&con_class_x, &con_class_z) && !Rc::ptr_eq(&con_class_x, &con_class_y) {
                return true;
            }
            if !Rc::ptr_eq(&con_class_y, &con_class_x)
                && !Rc::ptr_eq(&con_class_y, &con_class_z)
                && !Rc::ptr_eq(&con_class_x, &con_class_z)
                && self.try_coalesce_copy_by_value(local_a.clone(), local_c)
            {
                return true;
            }
        }

        false
    }

    fn check_interfere_single(
        &mut self,
        red: &Rc<RefCell<CongruenceClass>>,
        blue: &Rc<RefCell<CongruenceClass>>,
    ) -> bool {
        let mut local_a = red.borrow().values().next().unwrap().clone();
        let mut local_b = blue.borrow().values().next().unwrap().clone();
        // assumes one of the blocks dominates the other
        // as check_pre_dom_order depends on this
        if self.check_pre_dom_order(&local_a, &local_b) {
            std::mem::swap(&mut local_a, &mut local_b);
        }
        if self.intersect(&local_a, &local_b)
            // TODO: get many mut
            && self.get_value_class(local_a.clone()).clone() != self.get_value_class(local_b.clone()).clone()
        {
            true
        } else {
            self.equal_ancestor_in.insert(local_a, local_b.clone());
            false
        }
    }

    fn intersect(&self, local_a: &RcLocal, local_b: &RcLocal) -> bool {
        assert!(local_a != local_b);
        assert!(!self.dominates(local_a, local_b));

        let (def_dom_index, block_a, def_stat_index) = self.local_defs[local_a];
        let (_, block_b, _) = self.local_defs[local_b];
        // An edgeless single-block function has empty live_out, and both
        // definitions are in that block. The legacy live_in test can never
        // decide this query; keep the exact last-use/definition comparison.
        if self.terminal_block.is_none() && self.liveness.live_out(block_a, local_b) {
            true
        } else if self.terminal_block.is_none()
            && !self.liveness.live_in(block_a, local_b) && block_a != block_b {
            false
        } else if let Some(last_use) = self
            .local_last_use
            .get(local_b)
            .and_then(|uses| uses.get(def_dom_index))
        {
            let last_use_position = ParamOrStatIndex::Stat(last_use);
            last_use_position > def_stat_index
                // `a = function() ... b ... end`, or a parallel copy holding
                // it: the closure reads `b` after `a` is written, so `b` keeps
                // a variable of its own. (The self capture `a = function() ...
                // a ... end` is the closure's own value and never asked.)
                || last_use_position == def_stat_index
                    && self.value_captures.contains(&(local_b.stable_id(), def_dom_index, last_use))
        } else {
            false
        }
    }

    fn dominates(&self, local_a: &RcLocal, local_b: &RcLocal) -> bool {
        let (a_dom_index, block_a, a_stat_index) = self.local_defs[local_a];
        let (b_dom_index, block_b, b_stat_index) = self.local_defs[local_b];
        if block_a == block_b {
            // same as check_pre_dom_order
            (a_dom_index, a_stat_index) < (b_dom_index, b_stat_index)
        } else {
            let (start, end) = self.dominators[block_a.index()];
            let (point, _) = self.dominators[block_b.index()];
            start <= point && point < end
        }
    }

    fn check_interfere(
        &mut self,
        red: &Rc<RefCell<CongruenceClass>>,
        blue: &Rc<RefCell<CongruenceClass>>,
    ) -> bool {
        let mut dom = Vec::<(&RcLocal, RedOrBlue)>::new();

        let red = red.borrow();
        let blue = blue.borrow();
        let mut red_iter = red.iter().peekable();
        let mut blue_iter = blue.iter().peekable();
        let mut red_count = 0;
        let mut blue_count = 0;

        self.equal_ancestor_out.remove(red_iter.peek().unwrap().1);
        self.equal_ancestor_out.remove(blue_iter.peek().unwrap().1);
        // Keep the old per-iteration counter only as a test oracle. Release
        // profiling derives exactly the consumed items from iterator lengths.
        #[cfg(test)]
        let mut reference_visits = 0;
        loop {
            #[cfg(test)]
            { reference_visits += 1; }
            let (curr, curr_class) = if blue_iter.peek().is_none()
                || (red_iter.peek().is_some()
                    && self.check_pre_dom_order(
                        red_iter.peek().unwrap().1,
                        blue_iter.peek().unwrap().1,
                    )) {
                red_count += 1;
                (red_iter.next().unwrap().1, RedOrBlue::Red)
            } else {
                blue_count += 1;
                (blue_iter.next().unwrap().1, RedOrBlue::Blue)
            };

            while !dom.is_empty() && !self.dominates(dom.last().unwrap().0, curr) {
                match dom.pop().unwrap().1 {
                    RedOrBlue::Red => red_count -= 1,
                    RedOrBlue::Blue => blue_count -= 1,
                }
            }

            if !dom.is_empty()
                && self.interference(
                    curr,
                    dom.last().unwrap().0,
                    curr_class == dom.last().unwrap().1,
                )
            {
                #[cfg(test)]
                assert_eq!(red.len() - red_iter.len() + blue.len() - blue_iter.len(), reference_visits);
                if ast::telemetry::enabled() {
                    let visits = red.len() - red_iter.len() + blue.len() - blue_iter.len();
                    ast::telemetry::count("destruct_interference_visits", visits as u64);
                }
                return true;
            }

            dom.push((curr, curr_class));

            if (red_iter.peek().is_some() && blue_count > 0)
                || (blue_iter.peek().is_some() && red_count > 0)
                || (red_iter.peek().is_some() && blue_iter.peek().is_some())
            {
                continue;
            }

            break;
        }

        #[cfg(test)]
        assert_eq!(red.len() - red_iter.len() + blue.len() - blue_iter.len(), reference_visits);
        if ast::telemetry::enabled() {
            let visits = red.len() - red_iter.len() + blue.len() - blue_iter.len();
            ast::telemetry::count("destruct_interference_visits", visits as u64);
        }
        false
    }

    fn interference(&mut self, local_a: &RcLocal, local_b: &RcLocal, same_con_class: bool) -> bool {
        self.equal_ancestor_out.remove(local_a);
        let local_b = if same_con_class {
            self.equal_ancestor_out.get(local_b)
        } else {
            Some(local_b)
        };

        if let Some(local_b) = local_b.cloned() {
            assert!(!self.dominates(local_a, &local_b));

            let mut tmp = Some(&local_b);
            while let Some(curr_tmp) = tmp
                && !self.intersect(local_a, curr_tmp)
            {
                tmp = self.equal_ancestor_in.get(curr_tmp);
            }
            let tmp = tmp.cloned();

            let local_b = local_b.clone();
            // TODO: get many mut
            if self.get_value_class(local_a.clone()).clone()
                != self.get_value_class(local_b).clone()
            {
                tmp.is_some()
            } else {
                if let Some(tmp) = tmp {
                    self.equal_ancestor_out.insert(local_a.clone(), tmp);
                } else {
                    self.equal_ancestor_out.remove(local_a);
                }
                false
            }
        } else {
            false
        }
    }

    fn merge_congruence_classes(
        &mut self,
        con_class_a: &Rc<RefCell<CongruenceClass>>,
        con_class_b: &Rc<RefCell<CongruenceClass>>,
    ) {
        // TODO: move out of con_class_b with con_class_b.unwrap()
        let con_class_b = std::mem::take(&mut *con_class_b.borrow_mut());
        ast::telemetry::count("destruct_class_merges", 1);
        ast::telemetry::count("destruct_moved_members", con_class_b.len() as u64);
        for local in con_class_b.values() {
            self.congruence_classes
                .insert(local.clone(), con_class_a.clone());
        }
        con_class_a.borrow_mut().extend(con_class_b);

        let merged_size = con_class_a.borrow().len();
        ast::telemetry::count("destruct_ancestor_refresh_members", merged_size as u64);
        if ast::telemetry::enabled() {
            let bucket = match merged_size {
                0..=15 => "destruct_merged_class_under_16",
                16..=63 => "destruct_merged_class_16_63",
                64..=255 => "destruct_merged_class_64_255",
                _ => "destruct_merged_class_ge_256",
            };
            ast::telemetry::count(bucket, 1);
        }
        let mut out_present = 0;
        let mut updates = 0;
        for local in con_class_a.borrow().values() {
            // Keep inspecting the complete class: failed interference attempts
            // can leave relevant out-state on any member. Only omit writes
            // that would reinstall the exact ancestor already held in `in`.
            let Some(local_out) = self.equal_ancestor_out.get(local) else { continue; };
            out_present += 1;
            if self.equal_ancestor_in.get(local).is_some_and(|local_in|
                !self.check_pre_dom_order(local_in, local_out)) { continue; }
            self.equal_ancestor_in.insert(local.clone(), local_out.clone());
            updates += 1;
        }
        ast::telemetry::count("destruct_ancestor_out_present", out_present);
        ast::telemetry::count("destruct_ancestor_updates", updates);
    }

    fn compute_value_interference(&mut self) {
        // TODO: STYLE: rename to dom_dfs_post_order, along with other dominator_dfs
        let mut dominator_dfs_post_order =
            DfsPostOrder::new(self.function.graph(), self.function.entry().unwrap());

        while let Some(node) = dominator_dfs_post_order.next(self.function.graph()) {
            let params = if let Some((_, edge)) = self.function.edges_to_block(node).next() {
                edge.arguments
                    .iter()
                    .map(|(p, _)| p.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            for param in params {
                self.get_value_class(param);
            }
            for stat_index in 0..self.function.block_mut(node).unwrap().0.len() {
                if let ast::Statement::Assign(assign) =
                    &self.function.block(node).unwrap()[stat_index]
                {
                    let left = assign
                        .left
                        .iter()
                        .enumerate()
                        .filter_map(|(i, l)| Some((i, l.as_local()?.clone())));
                    for (left, right) in left
                        .into_iter()
                        .map(|(i, l)| (l, assign.right.get(i).and_then(|r| r.as_local().cloned())))
                        .collect::<Vec<_>>()
                    {
                        // A version of a cell that may change names no
                        // stable value: a copy of it holds what the cell held
                        // then, one into it what the cell holds until its next
                        // write ([`Self::find_unstable_cells`]).
                        if let Some(right) = right
                            && !self.in_unstable_cell(&left)
                            && !self.in_unstable_cell(&right)
                        {
                            let value_class = self.get_value_class(right.clone()).clone();
                            value_class.borrow_mut().insert(left.clone());
                            let prev_val_class =
                                self.values.insert(left.clone(), value_class.clone());
                            if let Some(prev_val_class) = prev_val_class {
                                // merge value classes
                                let prev_val_class = prev_val_class.take();
                                for local in &prev_val_class {
                                    self.values.insert(local.clone(), value_class.clone());
                                }
                                value_class.borrow_mut().extend(prev_val_class);
                            }
                            //assert!(prev_val_class.is_none() || prev_val_class.unwrap().borrow().is_empty(), "function not in ssa form");
                        }
                    }
                }
            }
        }
    }

    fn get_value_class(&mut self, local: RcLocal) -> &Rc<RefCell<FxHashSet<RcLocal>>> {
        self.values.entry(local.clone()).or_insert_with(|| {
            let mut value_class = FxHashSet::default();
            value_class.insert(local);
            Rc::new(RefCell::new(value_class))
        })
    }

    fn sort_params(&mut self) {
        for edge in self.function.graph_mut().edge_weights_mut() {
            edge.arguments.sort_by(|(p0, _), (p1, _)| p0.cmp(p1));
        }
    }

    fn lift_params(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            self.lift_block_params(node);
        }
    }

    // Note that the phi-functions do not have a circular dependency and are ordered accordingly (we have to do this before),
    // i.e., no variable that is defined by a Phi-function is used in a 'later' phi-function.
    fn lift_block_params(&mut self, node: NodeIndex) {
        let mut param_map = FxHashMap::default();
        if let Some((_, BlockEdge { arguments, .. })) = self.function.edges_to_block(node).next() {
            for param in arguments.iter().map(|(p, _)| p) {
                let temp_param = RcLocal::default();
                temp_param.inherit_source_bindings(param);
                if let Some(group) = self.register_group(param) {
                    self.transport_groups.insert(temp_param.clone(), group);
                }
                if let Some(group) = self.upvalue_to_group.get(param) {
                    self.upvalue_to_group
                        .insert(temp_param.clone(), group.clone());
                }
                param_map.insert(param.clone(), temp_param);
            }
        }

        if let Some(trace) = &mut self.function.provenance {
            let mut mappings = param_map.iter().collect::<Vec<_>>();
            mappings.sort_by_key(|(from, _)| from.stable_id());
            for (from, to) in mappings { trace.local_map("phi_parameter_transport", from, to); }
        }

        if !param_map.is_empty() {
            self.function.block_mut(node).unwrap().insert(
                0,
                ast::Assign {
                    node_origin: Default::default(),
                    left: param_map.keys().map(|k| k.clone().into()).collect(),
                    right: param_map.values().map(|v| v.clone().into()).collect(),
                    prefix: false,
                    parallel: true, compound: false,
                }
                .into(),
            );
        }

        let mut visited = FxHashSet::default();
        let mut preds = self.function.predecessor_blocks(node).detach();
        while let Some((_, pred)) = preds.next(self.function.graph()) {
            // if there are multiple edges from b0 -> b1, b0 will occur more than once.
            if visited.contains(&pred) {
                continue;
            }
            visited.insert(pred);

            let edges = self.function.edges(pred).collect::<Vec<_>>();
            let is_unconditional = edges.len() == 1;
            if is_unconditional {
                assert!(edges[0].weight().branch_type == BranchType::Unconditional);
            }

            let edges_to_node = edges
                .iter()
                .filter(|e| e.target() == node)
                .map(|e| e.id())
                .collect::<Vec<_>>();

            for &edge in &edges_to_node {
                let trace_enabled = self.function.provenance.is_some();
                let mut transport_origins = Vec::new();
                let mut omitted_transports = 0;
                let args = self
                    .function
                    .graph_mut()
                    .edge_weight_mut(edge)
                    .unwrap()
                    .arguments
                    .iter_mut();

                let mut parallel_assign = ast::Assign {
                    node_origin: Default::default(),
                    left: Vec::with_capacity(args.len()),
                    right: Vec::with_capacity(args.len()),
                    prefix: false,
                    parallel: true, compound: false,
                };

                for (param, arg) in args {
                    let temp_local = RcLocal::default();
                    temp_local.inherit_source_bindings(param);
                    if let Some(group) = self.register_groups.and_then(|groups| groups.get(param)) {
                        self.transport_groups.insert(temp_local.clone(), *group);
                    }
                    if trace_enabled {
                        if transport_origins.len() < crate::provenance::RECORD_LIMIT {
                            transport_origins.push((param.stable_id(), temp_local.stable_id()));
                        } else { omitted_transports += 1; }
                        if let Some(source) = arg.as_local() {
                            if transport_origins.len() < crate::provenance::RECORD_LIMIT {
                                transport_origins.push((source.stable_id(), temp_local.stable_id()));
                            } else { omitted_transports += 1; }
                        }
                    }

                    // A cell's value carried into another version of the same
                    // register stays that variable (one source local, captured
                    // on some paths only). Carried into another register's
                    // phi (`b = a`, `a` captured) it is a copy: as a member of
                    // the cell, `b` would become `a`.
                    if let ast::RValue::Local(arg) = arg
                        && let Some(group) = self.upvalue_to_group.get(arg)
                        && self.register_groups.is_some_and(|groups| {
                            groups.get(param).is_some_and(|register| groups.get(arg) == Some(register))
                        })
                    {
                        self.upvalue_to_group
                            .insert(temp_local.clone(), group.clone());
                    }

                    if let ast::RValue::Local(arg) = arg {
                        self.transported.insert(arg.clone());
                    }
                    parallel_assign.left.push(temp_local.clone().into());
                    parallel_assign
                        .right
                        .push(std::mem::replace(arg, temp_local.into()));
                    *param = param_map[param].clone();
                }

                if let Some(trace) = &mut self.function.provenance {
                    trace.dropped_records += omitted_transports;
                    for (from, to) in transport_origins {
                        trace.local_map_ids("phi_edge_transport", from, to);
                    }
                }

                if !parallel_assign.left.is_empty() {
                    let mut assign_block = pred;
                    // always insert a new block if the pred is conditional
                    // this is because it generates output that makes more sense
                    /*
                    local a, b = 1, 2
                    while p do
                        local t = a
                        a = b
                        b = t
                    end
                    return a, b
                    -- if we insert into the conditional block, we get weird output
                    local v1 = 1
                    local v2 = 2
                    repeat
                        local v3 = v1
                        v1 = v2
                        v2 = v3
                    until not p
                    return v2, v1
                    */
                    if !is_unconditional {
                        assign_block = self.function.new_block();
                        if self.is_for_next(self.function.graph().edge_endpoints(edge).unwrap().0) {
                            self.undesirable_blocks.insert(assign_block);
                        }
                        let edge = self.function.graph_mut().remove_edge(edge).unwrap();
                        self.function.set_edges(
                            assign_block,
                            vec![(
                                node,
                                BlockEdge {
                                    branch_type: BranchType::Unconditional,
                                    arguments: edge.arguments,
                                },
                            )],
                        );

                        self.function.graph_mut().add_edge(
                            pred,
                            assign_block,
                            BlockEdge::new(edge.branch_type),
                        );
                        visited.insert(assign_block);
                    }

                    let block = self.function.block_mut(assign_block).unwrap();
                    let (before_prep, after_prep) =
                        Self::split_edge_transfer_around_for_prep(
                            block,
                            parallel_assign,
                            &self.upvalue_to_group,
                        );
                    if let Some(before_prep) = before_prep {
                        let marker_index = block.len() - 1;
                        block.insert(marker_index, before_prep.into());
                    }
                    if let Some(after_prep) = after_prep {
                        block.push(after_prep.into());
                    }
                }
            }
        }
    }

    /// Decide where an edge transfer (lowered phi copy) belongs inside `block`.
    ///
    /// A block whose last statement is a numeric/generic `for` preparation
    /// marker (`FORNPREP`/`FORGPREP`) is a block whose real terminator is that
    /// marker: the lifter always ends the block at the prep instruction.  The
    /// values transferred along the init edge were all produced *before* the
    /// preparation in the original bytecode (either by a plain register copy
    /// or by a definition the SSA inliner folded into the edge argument), so
    /// materializing the copy before the marker restores the bytecode order
    /// instead of leaving a spurious "post-prep suffix" that no source-level
    /// `for` can express.
    ///
    /// A transfer element stays after the marker only when it reads something
    /// the marker itself defines (a value that only exists after preparation,
    /// e.g. the loop-carried counter/control phi) or writes what the marker
    /// reads, a version of the same cell included.  A cell's other copies run
    /// before it: the bytecode wrote and read the cell there, so code the
    /// preparation runs (`__iter`/`__call`, a call among the operands) sees
    /// the same values.  Splitting the parallel copy is sound because every
    /// destination is a fresh temporary that no element reads.
    ///
    /// Returns `(before_marker, after_marker)`; for a block that does not end
    /// in a prep marker everything is returned in `after_marker`.
    fn split_edge_transfer_around_for_prep(
        block: &ast::Block,
        transfer: ast::Assign,
        upvalue_to_group: &IndexMap<RcLocal, RcLocal>,
    ) -> (Option<ast::Assign>, Option<ast::Assign>) {
        let Some(marker) = block.last() else {
            return (None, Some(transfer));
        };
        if !matches!(
            marker,
            ast::Statement::GenericForInit(_) | ast::Statement::NumForInit(_)
        ) {
            return (None, Some(transfer));
        }
        let marker_outputs = marker.values_written();
        let marker_inputs = marker.values_read();
        // Every value on the edge was made before the preparation, a cell's
        // included, so its copy runs there too: the code a preparation may
        // run (`__iter`, a call among its operands) sees the cells as the
        // bytecode left them. Only a cell the marker itself reads keeps its
        // write after it.
        let marker_cells = marker_inputs.iter().filter_map(|read| upvalue_to_group.get(*read)).collect::<Vec<_>>();
        let mut before = ast::Assign {
            node_origin: Default::default(),
            left: Vec::new(),
            right: Vec::new(),
            prefix: false,
            parallel: true, compound: false,
        };
        let mut after = before.clone();
        for (left, right) in transfer.left.into_iter().zip(transfer.right) {
            let reads_marker_output = right.values_read().into_iter().any(|read| marker_outputs.contains(&read));
            let writes_marker_input = left.values_written().into_iter().any(|written| {
                marker_inputs.contains(&written)
                    || upvalue_to_group.get(written).is_some_and(|cell| marker_cells.contains(&cell))
            });
            let target = if reads_marker_output || writes_marker_input {
                &mut after
            } else {
                &mut before
            };
            target.left.push(left);
            target.right.push(right);
        }
        let non_empty = |assign: ast::Assign| (!assign.left.is_empty()).then_some(assign);
        (non_empty(before), non_empty(after))
    }
}

#[cfg(test)]
mod copy_sharing_regressions {
    use super::*;

    #[test]
    fn compact_last_uses_match_hash_maps_for_sparse_blocks_and_repeated_reads() {
        for seed in 0..64u64 {
            let mut state = seed;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 32) as usize
            };
            let mut actual: [LastUses; 12] = std::array::from_fn(|_| LastUses::default());
            let mut expected: [FxHashMap<usize, usize>; 12] = std::array::from_fn(|_| FxHashMap::default());
            for order in 0..128 {
                if next() % 3 == 0 { continue; }
                for statement in 0..1 + next() % 8 {
                    let local = next() % actual.len();
                    for _ in 0..1 + next() % 3 {
                        actual[local].record(order, statement);
                        expected[local].insert(order, statement);
                    }
                }
            }
            for (actual, expected) in actual.iter().zip(&expected) {
                assert_eq!(actual.0.len(), expected.len());
                for order in 0..=128 {
                    assert_eq!(actual.get(order), expected.get(&order).copied(), "seed={seed}, block={order}");
                }
            }
        }
    }

    #[test]
    fn ancestor_refresh_matches_reinsertion_with_preexisting_refusal_state() {
        for seed in 0..64usize {
            let mut function = Function::new(0);
            let entry = function.new_block();
            function.set_entry(entry);
            let members: Vec<_> = (0..6).map(|_| RcLocal::default()).collect();
            let ancestors: Vec<_> = (0..4).map(|_| RcLocal::default()).collect();
            let mut destructor = Destructor::new(&mut function, IndexMap::default(), FxHashSet::default(), 10);
            for (index, local) in ancestors.iter().chain(&members).enumerate() {
                destructor.local_defs.insert(local.clone(), (0, entry, ParamOrStatIndex::Stat(index)));
            }
            // Distinct identities can share a parallel-definition position;
            // equal positions must retain `in`, not select the new `out`.
            destructor.local_defs.insert(ancestors[1].clone(), (0, entry, ParamOrStatIndex::Stat(0)));
            let mut classes = [CongruenceClass::default(), CongruenceClass::default()];
            for (index, local) in members.iter().enumerate() {
                classes[index % 2].insert((0, ParamOrStatIndex::Stat(index + 4)), local.clone());
                let before = (seed + index * 3) % 5;
                let after = (seed / 5 + index * 2) % 5;
                if before < ancestors.len() {
                    destructor.equal_ancestor_in.insert(local.clone(), ancestors[before].clone());
                }
                if after < ancestors.len() {
                    // Include old receiver members as well as donor members,
                    // as happens after an earlier refused interference query.
                    destructor.equal_ancestor_out.insert(local.clone(), ancestors[after].clone());
                }
            }
            let mut expected = destructor.equal_ancestor_in.clone();
            for local in &members {
                let selected = match (expected.get(local), destructor.equal_ancestor_out.get(local)) {
                    (None, Some(local)) | (Some(local), None) => Some(local),
                    (Some(before), Some(after)) => Some(if destructor.check_pre_dom_order(before, after) { after } else { before }),
                    _ => None,
                }.cloned();
                if let Some(selected) = selected { expected.insert(local.clone(), selected); }
            }
            let out_before = destructor.equal_ancestor_out.clone();
            let [left, right] = classes.map(|class| Rc::new(RefCell::new(class)));
            destructor.merge_congruence_classes(&left, &right);
            assert_eq!(destructor.equal_ancestor_in, expected, "seed {seed}");
            assert_eq!(destructor.equal_ancestor_out, out_before);
            assert_eq!(left.borrow().len(), members.len());
        }
    }

    #[test]
    fn interference_visit_oracle_covers_early_conflict_and_complete_merge_walk() {
        for equal_values in [false, true] {
            let mut function = Function::new(0);
            let entry = function.new_block();
            function.set_entry(entry);
            let locals: Vec<_> = (0..8).map(|_| RcLocal::default()).collect();
            for (index, local) in locals.iter().enumerate() {
                let value = if equal_values && index > 0 {
                    ast::RValue::Local(locals[index - 1].clone())
                } else {
                    ast::Literal::Number(index as f64).into()
                };
                function.block_mut(entry).unwrap().push(
                    ast::Assign::new(vec![local.clone().into()], vec![value]).into());
            }
            // All distinct values remain live, causing an early cross-class
            // conflict. The equal-value variant permits the complete walk.
            function.block_mut(entry).unwrap().push(
                ast::Return::new(locals.iter().cloned().map(Into::into).collect()).into());
            let mut destructor = Destructor::new(&mut function, IndexMap::default(), FxHashSet::default(), locals.len());
            destructor.liveness = Liveness::calculate(destructor.function);
            destructor.build_def_use();
            destructor.compute_value_interference();
            let mut classes = [CongruenceClass::default(), CongruenceClass::default()];
            for (index, local) in locals.iter().enumerate() {
                let (order, _, position) = destructor.local_defs[local];
                classes[index % 2].insert((order, position), local.clone());
            }
            let [red, blue] = classes.map(|class| Rc::new(RefCell::new(class)));
            // The actual routine asserts its new exit count against the old
            // incrementing counter under cfg(test), for both return paths.
            assert_eq!(destructor.check_interfere(&red, &blue), !equal_values);
        }
    }

    #[test]
    fn singleton_copy_classes_merge_only_when_values_can_share_storage() {
        for equal_values in [false, true] {
            let (a, b) = (RcLocal::default(), RcLocal::default());
            let mut function = Function::new(0);
            let entry = function.new_block();
            function.set_entry(entry);
            let second = if equal_values {
                ast::RValue::Local(a.clone())
            } else {
                ast::Literal::Number(2.0).into()
            };
            function.block_mut(entry).unwrap().extend([
                ast::Assign::new(vec![a.clone().into()], vec![ast::Literal::Number(1.0).into()]).into(),
                ast::Assign::new(vec![b.clone().into()], vec![second]).into(),
                ast::Return::new(vec![a.clone().into(), b.clone().into()]).into(),
            ]);
            let mut destructor = Destructor::new(&mut function, IndexMap::default(), FxHashSet::default(), 2);
            destructor.liveness = Liveness::calculate(destructor.function);
            destructor.build_def_use();
            destructor.compute_value_interference();
            assert_eq!(destructor.try_coalesce_copy_by_value(b.clone(), a.clone()), equal_values);
            let first = destructor.get_congruence_class(a).clone();
            let second = destructor.get_congruence_class(b).clone();
            assert_eq!(Rc::ptr_eq(&first, &second), equal_values);
            assert_eq!(first.borrow().len(), if equal_values { 2 } else { 1 });
        }
    }

    #[test]
    fn deep_dominator_tree_uses_linear_storage() {
        let mut function = Function::new(0);
        let nodes: Vec<_> = (0..4096).map(|_| function.new_block()).collect();
        function.set_entry(nodes[0]);
        for pair in nodes.windows(2) {
            function.set_edges(pair[0], vec![(pair[1], BlockEdge::default())]);
        }
        let mut destructor =
            Destructor::new(&mut function, IndexMap::default(), FxHashSet::default(), 0);
        destructor.build_def_use();
        assert_eq!(destructor.dominators.len(), nodes.len());
        for pair in nodes.windows(2) {
            let (start, end) = destructor.dominators[pair[0].index()];
            let (child, _) = destructor.dominators[pair[1].index()];
            assert!(start < child && child < end);
        }
    }
    #[derive(Clone, Debug, PartialEq)]
    enum Value {
        Number(f64),
        Boolean(bool),
    }
    fn eval(f: &Function, input: &[Value]) -> Vec<Value> {
        fn value(v: &ast::RValue, m: &FxHashMap<RcLocal, Value>) -> Value {
            match v {
                ast::RValue::Local(x) => m
                    .get(x)
                    .expect("read of unassigned local after SSA destruction")
                    .clone(),
                ast::RValue::Literal(ast::Literal::Number(n)) => Value::Number(*n),
                _ => panic!("unexpected expression"),
            }
        }
        let mut m: FxHashMap<_, _> = f
            .parameters
            .iter()
            .cloned()
            .zip(input.iter().cloned())
            .collect();
        let mut node = f.entry().unwrap();
        for _ in 0..32 {
            let mut branch = BranchType::Unconditional;
            for stmt in f.block(node).unwrap().iter() {
                match stmt {
                    ast::Statement::Assign(a) => {
                        let vals = a.right.iter().map(|v| value(v, &m)).collect::<Vec<_>>();
                        for (l, v) in a.left.iter().zip(vals) {
                            m.insert(l.as_local().unwrap().clone(), v);
                        }
                    }
                    ast::Statement::If(i) => {
                        branch = if value(&i.condition, &m) != Value::Boolean(false) {
                            BranchType::Then
                        } else {
                            BranchType::Else
                        }
                    }
                    ast::Statement::Return(r) => {
                        return r.values.iter().map(|v| value(v, &m)).collect();
                    }
                    ast::Statement::Empty(_) => {}
                    _ => panic!("unexpected statement"),
                }
            }
            let e = f
                .edges(node)
                .find(|e| e.weight().branch_type == branch)
                .unwrap();
            let vals = e
                .weight()
                .arguments
                .iter()
                .map(|(k, v)| (k.clone(), value(v, &m)))
                .collect::<Vec<_>>();
            m.extend(vals);
            node = e.target();
        }
        panic!("too many steps")
    }
    #[test]
    fn review_full_destructor_phi_copies() {
        let mut failures = 0;
        for copies in 0..5 {
            for keep in 0..8 {
                let z = RcLocal::default();
                let flag = RcLocal::default();
                let p = RcLocal::default();
                let q = RcLocal::default();
                let mut f = Function::new(0);
                f.parameters = vec![z.clone(), flag.clone()];
                let entry = f.new_block();
                let left = f.new_block();
                let right = f.new_block();
                let join = f.new_block();
                f.set_entry(entry);
                let mut chain = vec![z.clone()];
                for _ in 0..copies {
                    let x = RcLocal::default();
                    f.block_mut(entry).unwrap().push(
                        ast::Assign::new(
                            vec![x.clone().into()],
                            vec![chain.last().unwrap().clone().into()],
                        )
                        .into(),
                    );
                    chain.push(x);
                }
                f.block_mut(entry).unwrap().push(
                    ast::If::new(flag.clone().into(), Default::default(), Default::default())
                        .into(),
                );
                f.block_mut(right).unwrap().push(
                    ast::Assign::new(
                        vec![q.clone().into()],
                        vec![ast::Literal::Number(42.0).into()],
                    )
                    .into(),
                );
                let mut out = vec![p.clone().into()];
                if keep & 1 != 0 {
                    out.push(z.clone().into());
                }
                if keep & 2 != 0 {
                    out.push(chain.last().unwrap().clone().into());
                }
                if keep & 4 != 0 {
                    let t = RcLocal::default();
                    f.block_mut(left).unwrap().push(
                        ast::Assign::new(
                            vec![t.clone().into()],
                            vec![chain.last().unwrap().clone().into()],
                        )
                        .into(),
                    );
                    chain.push(t);
                }
                f.block_mut(join)
                    .unwrap()
                    .push(ast::Return::new(out).into());
                f.set_edges(
                    entry,
                    vec![
                        (left, BlockEdge::new(BranchType::Then)),
                        (right, BlockEdge::new(BranchType::Else)),
                    ],
                );
                f.set_edges(
                    left,
                    vec![(
                        join,
                        BlockEdge {
                            branch_type: BranchType::Unconditional,
                            arguments: vec![(p.clone(), chain.last().unwrap().clone().into())],
                        },
                    )],
                );
                f.set_edges(
                    right,
                    vec![(
                        join,
                        BlockEdge {
                            branch_type: BranchType::Unconditional,
                            arguments: vec![(p.clone(), q.into())],
                        },
                    )],
                );
                let before = [
                    eval(&f, &[Value::Number(11.0), Value::Boolean(false)]),
                    eval(&f, &[Value::Number(11.0), Value::Boolean(true)]),
                ];
                Destructor::new(&mut f, IndexMap::default(), FxHashSet::default(), 32).destruct();
                let after = [
                    eval(&f, &[Value::Number(11.0), Value::Boolean(false)]),
                    eval(&f, &[Value::Number(11.0), Value::Boolean(true)]),
                ];
                if before != after {
                    failures += 1;
                    println!(
                        "REVIEW full SSA copies={copies} keep={keep} before={before:?} after={after:?}"
                    );
                }
            }
        }
        println!("REVIEW full SSA variants=40 failures={failures}");
        assert_eq!(failures, 0);
    }
}
