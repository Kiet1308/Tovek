//! Write-only facts for condition normalization. All consumers ask whether a
//! local is written exactly once in a function and its descendant closures.
//! One DFS gives each function a write-position interval; per-local positions
//! answer that question without rescanning descendants at every closure depth.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{Block, LocalRw, RValue, RcLocal, Statement, Traverse};

#[derive(Clone, Copy)]
pub(super) struct Interval {
    start: usize,
    end: usize,
}

enum Positions {
    One(usize),
    Many(Vec<usize>),
}

impl Positions {
    fn add(&mut self, position: usize) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, position]),
            Self::Many(positions) => positions.push(position),
        }
    }

    fn single(&self, interval: Interval) -> bool {
        match self {
            Self::One(position) => interval.start <= *position && *position < interval.end,
            Self::Many(positions) => {
                let first = positions.partition_point(|position| *position < interval.start);
                positions.get(first).is_some_and(|position| *position < interval.end)
                    && positions.get(first + 1).is_none_or(|position| *position >= interval.end)
            }
        }
    }
}

pub(super) struct WriteIndex {
    scopes: FxHashMap<usize, Interval>,
    locals: FxHashMap<u64, Positions>,
    statements: u64,
    legacy_statements: u64,
    writes: usize,
    limited: bool,
}

#[derive(Clone, Copy)]
struct Limits {
    blocks: usize,
    positions: usize,
    positions_per_local: usize,
    position_slack: usize,
}

const INDEX_LIMITS: Limits = Limits {
    blocks: 65_536,
    positions: 262_144,
    positions_per_local: 8,
    position_slack: 1_024,
};

impl WriteIndex {
    pub(super) fn new(block: &Block) -> Option<Self> {
        #[cfg(test)]
        if REFERENCE_USAGE.with(std::cell::Cell::get) { return None; }
        Self::with_limits(block, INDEX_LIMITS)
    }

    fn with_limits(block: &Block, limits: Limits) -> Option<Self> {
        let mut index = Self {
            scopes: Default::default(), locals: Default::default(), statements: 0,
            legacy_statements: 0, writes: 0, limited: false,
        };
        let complete = index.block(block, &mut FxHashSet::default(), true, 1, limits);
        crate::telemetry::count("normalize_write_index_attempts", 1);
        crate::telemetry::count("normalize_write_index_statements", index.statements);
        crate::telemetry::count("normalize_write_index_writes", index.writes as u64);
        crate::telemetry::count(if complete { "normalize_write_index_accepted" }
            else { "normalize_write_index_refused" }, 1);
        if complete {
            crate::telemetry::count("normalize_write_census_repeated_statement_visits_avoided",
                index.legacy_statements - index.statements);
        } else if index.limited {
            crate::telemetry::count("normalize_write_index_budget_refused", 1);
        }
        complete.then_some(index)
    }

    fn record(&mut self, local: &RcLocal) {
        let position = self.writes;
        self.writes += 1;
        self.locals.entry(local.stable_id()).and_modify(|positions| positions.add(position))
            .or_insert(Positions::One(position));
    }

    fn block(&mut self, block: &Block, seen: &mut FxHashSet<usize>, function: bool,
        depth: u64, limits: Limits) -> bool {
        let identity = block as *const Block as usize;
        // The normalizer revisits shared bodies in occurrence order. Keep its
        // established per-function snapshot path for DAGs and cyclic graphs.
        if !seen.insert(identity) { return false; }
        if seen.len() > limits.blocks {
            self.limited = true;
            return false;
        }
        let start = self.writes;
        for statement in &block.0 {
            self.statements += 1;
            self.legacy_statements += depth;
            // The legacy usage collector descends only through direct RHS
            // roots, whereas normalization also enters indexed-LHS closures.
            // Do not accidentally add those bodies to ancestor write counts.
            let mut lhs_closure = false;
            statement.visit_lvalues(&mut |left| {
                left.traverse_rvalues_ref(&mut |value| lhs_closure |= matches!(value, RValue::Closure(_)));
                !lhs_closure
            });
            if lhs_closure { return false; }
            if !statement.visit_local_writes(&mut |local| {
                self.record(local);
                // A repeatedly assigned single local must not make this index
                // arbitrarily larger than the old distinct-local map. Both
                // relative and absolute budgets choose only the algorithm;
                // refusal keeps the exact unbounded write-only snapshot path.
                self.limited = self.writes > limits.positions
                    || self.writes > self.locals.len() * limits.positions_per_local + limits.position_slack;
                !self.limited
            }) { return false; }
            let mut complete = true;
            crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
                if complete {
                    complete = closure.function.try_lock()
                        .is_some_and(|function| self.block(&function.body, seen, true, depth + 1, limits));
                }
            });
            if !complete { return false; }
            let mut child = |block: &parking_lot::Mutex<Block>| {
                block.try_lock().is_some_and(|block| self.block(&block, seen, false, depth, limits))
            };
            if !visit_blocks(statement, &mut child) { return false; }
        }
        if function { self.scopes.insert(identity, Interval { start, end: self.writes }); }
        true
    }
}

pub(super) enum FunctionWrites<'a> {
    Indexed(&'a WriteIndex, Interval),
    Direct(FxHashMap<u64, u8>),
    #[cfg(test)]
    Reference(FxHashMap<RcLocal, crate::inline_temps::Usage>),
}

impl<'a> FunctionWrites<'a> {
    pub(super) fn new(block: &Block, index: Option<&'a WriteIndex>) -> Self {
        #[cfg(test)]
        if REFERENCE_USAGE.with(std::cell::Cell::get) {
            return Self::Reference(crate::inline_temps::collect_usage(block));
        }
        if let Some(index) = index {
            crate::telemetry::count("normalize_write_indexed_functions", 1);
            return Self::Indexed(index, index.scopes[&(block as *const Block as usize)]);
        }
        let mut counts = FxHashMap::default();
        let mut statements = 0;
        collect_writes(block, &mut counts, &mut statements);
        crate::telemetry::count("normalize_write_direct_statements", statements);
        crate::telemetry::count("normalize_write_direct_functions", 1);
        Self::Direct(counts)
    }

    pub(super) fn for_function(&self, block: &Block) -> Self {
        // Normalization never changes statement destinations, structured block
        // ownership or closure occurrences. Its discarded children are Boolean
        // literals; every closure-bearing operand is moved intact. Thus the
        // initial interval counts remain exact while descendant expressions
        // are normalized. Aliases and selector mismatches use fresh snapshots.
        Self::new(block, match self { Self::Indexed(index, _) => Some(index), _ => None })
    }

    pub(super) fn single(&self, local: &RcLocal) -> bool {
        match self {
            Self::Indexed(index, interval) => index.locals.get(&local.stable_id())
                .is_some_and(|positions| positions.single(*interval)),
            Self::Direct(counts) => counts.get(&local.stable_id()) == Some(&1),
            #[cfg(test)]
            Self::Reference(usage) => usage.get(local).is_some_and(|usage| usage.writes == 1),
        }
    }
}

fn visit_blocks(statement: &Statement, visit: &mut impl FnMut(&parking_lot::Mutex<Block>) -> bool) -> bool {
    match statement {
        Statement::If(node) => visit(&node.then_block) && visit(&node.else_block),
        Statement::While(node) => visit(&node.block),
        Statement::Repeat(node) => visit(&node.block),
        Statement::NumericFor(node) => visit(&node.block),
        Statement::GenericFor(node) => visit(&node.block),
        _ => true,
    }
}

fn collect_writes(block: &Block, counts: &mut FxHashMap<u64, u8>, statements: &mut u64) {
    for statement in &block.0 {
        *statements += 1;
        statement.visit_local_writes(&mut |local| {
            let count = counts.entry(local.stable_id()).or_default();
            // Only the ==1 predicate is observable; duplicate write slots and
            // repeated shared-body occurrences still contribute individually.
            *count = (*count + 1).min(2);
            true
        });
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            collect_writes(&closure.function.lock().body, counts, statements);
        });
        visit_blocks(statement, &mut |block| {
            collect_writes(&block.lock(), counts, statements);
            true
        });
    }
}

#[cfg(test)]
thread_local! {
    pub(super) static REFERENCE_USAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, BinaryOperation, Call, Closure, Function, GenericFor, Global,
        If, IfExpression, Index, LValue, Literal, Local, NumericFor, Return, Unary, UnaryOperation, Upvalue};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn number(value: f64) -> RValue { Literal::Number(value).into() }
    fn comparison(left: RValue, right: RValue) -> RValue {
        Unary::new(Binary::new(left, right, BinaryOperation::LessThan).into(), UnaryOperation::Not).into()
    }
    fn assign(local: &RcLocal, value: RValue) -> Statement {
        Assign::new(vec![local.clone().into()], vec![value]).into()
    }
    fn closure(body: Block, locals: &[RcLocal]) -> RValue {
        Closure {
            node_origin: Default::default(),
            function: ByAddress(Arc::new(Mutex::new(Function { body, ..Default::default() }))),
            upvalues: vec![Upvalue::Ref(locals[0].clone()), Upvalue::Copy(locals[2].clone())],
        }.into()
    }

    // Mode 0 is a tree. The remaining modes exercise repeated closure bodies,
    // shared structured blocks and the legacy indexed-LHS closure omission.
    fn fixture(locals: &[RcLocal], mode: usize, seed: usize) -> Block {
        let mut nested = Block(vec![assign(&locals[2], number(5.0)),
            Return::new(vec![comparison(locals[2].clone().into(), number(9.0))]).into()]);
        for level in 0..(seed % 4 + 1) {
            let value = closure(nested, locals);
            nested = Block(vec![assign(&locals[3 + level], number(level as f64)),
                Call::new(Global::from("child").into(), vec![value]).into(),
                Return::new(vec![comparison(locals[3 + level].clone().into(), number(9.0))]).into()]);
        }
        if seed % 2 == 0 { nested.push(assign(&locals[0], number(f64::NAN))); }
        let child = closure(nested, locals);
        let mut block = Block(vec![assign(&locals[0], number(1.0)),
            Assign::new(vec![locals[1].clone().into(), locals[1].clone().into()],
                vec![number(2.0), number(f64::NAN)]).into(),
            // A later write in a descendant must prevent propagation even
            // before that closure is reached by the mutation walk.
            Return::new(vec![comparison(locals[0].clone().into(), number(3.0)),
                comparison(locals[1].clone().into(), number(4.0))]).into(),
        ]);
        match mode {
            1 => {
                block.push(Call::new(Global::from("left").into(), vec![child.clone()]).into());
                block.push(Call::new(Global::from("right").into(), vec![child]).into());
            }
            2 => {
                let mut branch = If::new(comparison(locals[0].clone().into(), number(2.0)),
                    Block(vec![Call::new(Global::from("shared").into(), vec![child]).into()]), Block::default());
                branch.else_block = branch.then_block.clone();
                block.push(branch.into());
            }
            3 => block.push(Assign::new(vec![LValue::Index(Index::new(Global::from("targets").into(), child))],
                vec![number(1.0)]).into()),
            4 => block.push(Return::new(vec![Binary::new(Binary::new(
                Unary::new(Global::from("choose").into(), UnaryOperation::Not).into(),
                child, BinaryOperation::And).into(),
                closure(Block(vec![assign(&locals[9], number(7.0))]), locals), BinaryOperation::Or).into()]).into()),
            _ => block.push(Call::new(Global::from("tree").into(), vec![child]).into()),
        }
        let mut loop_body = Block(vec![Return::new(vec![comparison(locals[7].clone().into(), number(3.0))]).into()]);
        if seed % 3 == 0 { loop_body.push(assign(&locals[7], number(f64::NAN))); }
        block.push(NumericFor::new(number(1.0), number(2.0), number(1.0), locals[7].clone(), loop_body).into());
        block.push(GenericFor::new(vec![locals[8].clone(), locals[8].clone()], vec![Global::from("next").into()],
            Block(vec![Return::new(vec![IfExpression::new(
                Binary::new(locals[8].clone().into(), number(0.0), BinaryOperation::Equal).into(),
                Literal::Boolean(false).into(), Literal::Boolean(true).into()).into()]).into()])).into());
        block.push(Return::new(vec![IfExpression::new(
            comparison(locals[0].clone().into(), number(9.0)), Literal::Boolean(true).into(),
            Literal::Boolean(false).into()).into()]).into());
        block
    }

    fn visit_functions(block: &Block, visit: &mut impl FnMut(&Block)) {
        for statement in &block.0 {
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Closure(closure) = value { visit(&closure.function.lock().body); }
            });
            visit_blocks(statement, &mut |block| {
                visit_functions(&block.lock(), visit);
                true
            });
        }
    }

    fn assert_counts(block: &Block, index: Option<&WriteIndex>, locals: &[RcLocal]) {
        let expected = crate::inline_temps::collect_usage(block);
        let actual = FunctionWrites::new(block, index);
        for local in locals {
            assert_eq!(actual.single(local), expected.get(local).is_some_and(|usage| usage.writes == 1),
                "local {}", local.stable_id());
        }
        visit_functions(block, &mut |child| assert_counts(child, index, locals));
    }

    #[test]
    fn indexed_and_fallback_counts_match_legacy_without_retaining_local_owners() {
        for seed in 0..24 {
            let locals: Vec<_> = (0..12).map(|index| RcLocal::new(Local::new(Some(format!("v{index}"))))).collect();
            for mode in 0..5 {
                let block = fixture(&locals, mode, seed);
                let owners: Vec<_> = locals.iter().map(|local| Arc::count(&local.0.0)).collect();
                let index = WriteIndex::new(&block);
                assert_eq!(index.is_some(), matches!(mode, 0 | 4), "seed {seed}, mode {mode}");
                assert_counts(&block, index.as_ref(), &locals);
                assert_eq!(locals.iter().map(|local| Arc::count(&local.0.0)).collect::<Vec<_>>(), owners);
            }
        }
    }

    #[test]
    fn interval_boundaries_and_bounded_admission_preserve_single_write_queries() {
        let mut positions = Positions::One(0);
        for position in [2, 5, 6] { positions.add(position); }
        for start in 0..9 {
            for end in start..9 {
                assert_eq!(positions.single(Interval { start, end }),
                    [0, 2, 5, 6].into_iter().filter(|position| start <= *position && *position < end).count() == 1);
            }
        }
        let locals: Vec<_> = (0..12).map(|_| RcLocal::default()).collect();
        let block = fixture(&locals, 0, 2);
        for limits in [
            Limits { blocks: 1, ..INDEX_LIMITS },
            Limits { positions: 1, ..INDEX_LIMITS },
            Limits { positions_per_local: 0, position_slack: 1, ..INDEX_LIMITS },
        ] {
            assert!(WriteIndex::with_limits(&block, limits).is_none());
            assert_counts(&block, None, &locals);
        }
        let mut block = Block((0..2048).map(|value| assign(&locals[0], number(value as f64))).collect());
        block.push(assign(&locals[1], number(1.0)));
        assert!(WriteIndex::new(&block).is_none(), "high occurrence/distinct-local ratio uses bounded fallback");
        assert_counts(&block, None, &locals);
    }

    type OriginView = Option<(Vec<crate::node_origins::Input>, bool, bool, Option<&'static str>, bool)>;
    fn origin_view(origin: Option<&crate::node_origins::Origin>) -> OriginView {
        origin.and_then(|origin| origin.0.as_ref()).map(|data| (
            data.inputs.iter().map(|input| (**input).clone()).collect(), data.inlined,
            data.cloned, data.synthesized, data.incomplete,
        ))
    }
    fn origins(block: &Block, output: &mut Vec<OriginView>) {
        for statement in &block.0 {
            output.push(origin_view(crate::node_origins::statement(statement)));
            statement.traverse_rvalues_ref(&mut |value| output.push(origin_view(crate::node_origins::value(value))));
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Closure(closure) = value { origins(&closure.function.lock().body, output); }
            });
            visit_blocks(statement, &mut |block| { origins(&block.lock(), output); true });
        }
    }
    fn stamp(block: &mut Block, counter: &mut usize) {
        for statement in &mut block.0 {
            if let Some(origin) = crate::node_origins::statement_mut(statement) {
                *origin = crate::node_origins::Origin::input(crate::node_origins::Input {
                    function: "write-index-oracle".into(), block: 0, statement: *counter, value: None,
                });
                *counter += 1;
            }
            statement.post_traverse_rvalues(&mut |value| -> Option<()> {
                if let Some(origin) = crate::node_origins::value_mut(value) {
                    *origin = crate::node_origins::Origin::input(crate::node_origins::Input {
                        function: "write-index-oracle".into(), block: 0, statement: *counter, value: Some(0),
                    });
                    *counter += 1;
                }
                if let RValue::Closure(closure) = value { stamp(&mut closure.function.lock().body, counter); }
                None
            });
            visit_blocks(statement, &mut |block| { stamp(&mut block.lock(), counter); true });
        }
    }
    fn closure_owners(block: &Block, output: &mut Vec<(usize, usize)>) {
        for statement in &block.0 {
            statement.traverse_rvalues_ref(&mut |value| {
                if let RValue::Closure(closure) = value {
                    output.push((Arc::as_ptr(&closure.function.0) as usize, Arc::strong_count(&closure.function.0)));
                    closure_owners(&closure.function.lock().body, output);
                }
            });
            visit_blocks(statement, &mut |block| { closure_owners(&block.lock(), output); true });
        }
    }

    #[test]
    fn normalization_matches_legacy_usage_in_all_styles_nan_modes_and_aliases() {
        struct Restore(bool);
        impl Drop for Restore { fn drop(&mut self) { REFERENCE_USAGE.with(|flag| flag.set(self.0)); } }
        for seed in 0..16 {
            let locals: Vec<_> = (0..12).map(|index| RcLocal::new(Local::new(Some(format!("v{index}"))))).collect();
            locals[0].0.lock().add_source_binding(crate::SourceBinding {
                origin: crate::BindingOrigin::DebugLocal { prototype: 3, register: 0, start_pc: 0, end_pc: 10 },
                name: "source".into(),
            });
            for mode in 0..5 {
                for no_nan in [false, true] {
                    for expressions in [false, true] {
                        let mut expected = fixture(&locals, mode, seed);
                        let mut actual = fixture(&locals, mode, seed);
                        stamp(&mut expected, &mut 0); stamp(&mut actual, &mut 0);
                        let metadata: Vec<_> = locals.iter().map(|local| local.0.lock().clone()).collect();
                        let ids = crate::current_local_id();
                        let mut owners = Vec::new(); closure_owners(&actual, &mut owners);
                        owners.sort_unstable();
                        {
                            let _restore = Restore(REFERENCE_USAGE.with(|flag| flag.replace(true)));
                            super::super::normalize_with_style(&mut expected, no_nan, expressions);
                        }
                        super::super::normalize_with_style(&mut actual, no_nan, expressions);
                        assert_eq!(actual.to_string(), expected.to_string(),
                            "seed={seed}, mode={mode}, nan={no_nan}, expr={expressions}");
                        let mut actual_origins = Vec::new(); let mut expected_origins = Vec::new();
                        origins(&actual, &mut actual_origins); origins(&expected, &mut expected_origins);
                        assert_eq!(actual_origins, expected_origins);
                        assert_eq!(locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>(), metadata);
                        assert_eq!(crate::current_local_id(), ids);
                        let mut after_owners = Vec::new(); closure_owners(&actual, &mut after_owners);
                        after_owners.sort_unstable();
                        assert_eq!(after_owners, owners, "normalization keeps every closure occurrence and owner");
                    }
                }
            }
        }
    }

    #[test]
    fn descendant_write_index_visits_each_statement_once_at_increasing_depth() {
        for depth in [8, 16, 32, 64] {
            let locals: Vec<_> = (0..depth + 3).map(|_| RcLocal::default()).collect();
            let mut block = Block::default();
            for index in 0..depth {
                block = Block(vec![assign(&locals[index], number(index as f64)),
                    Return::new(vec![closure(block, &locals)]).into()]);
            }
            let index = WriteIndex::new(&block).unwrap();
            assert_eq!(index.statements, (depth * 2) as u64);
            assert_eq!(index.writes, depth);
            assert_eq!(index.scopes.len(), depth + 1);
            let mut legacy_statements = 0;
            fn old_scope_work(block: &Block, count: &mut u64) {
                collect_writes(block, &mut FxHashMap::default(), count);
                visit_functions(block, &mut |child| old_scope_work(child, count));
            }
            old_scope_work(&block, &mut legacy_statements);
            assert_eq!(legacy_statements, (depth * (depth + 1)) as u64);
            assert_eq!(index.legacy_statements, legacy_statements);
            assert_counts(&block, Some(&index), &locals);
        }
    }
}
