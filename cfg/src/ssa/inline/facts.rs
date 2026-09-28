//! Read-only facts for one block during a single `inline_rvalues` invocation.
//! The block's statement positions and both group maps are fixed in this
//! interval. Callers must invalidate every statement that they mutate. The
//! cache is discarded before cleanup can remove, move, or fold statements.
//! Only integer group IDs and booleans are retained, never AST/local owners.
use ast::{LocalRw, RcLocal, Statement};
use ast::FxIndexMap as IndexMap;
use rustc_hash::FxHashMap;

const MIN_CACHED_STATEMENTS: usize = 4;
const MAX_CACHED_SLOTS: usize = 16_384;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct StatementFacts {
    pub read_groups: Vec<usize>,
    pub write_groups: Vec<usize>,
    pub writes_upvalue: bool,
    pub observable: bool,
    pub single_rhs_observable: Option<bool>,
}

impl StatementFacts {
    fn new(
        statement: &Statement,
        local_to_group: &FxHashMap<RcLocal, usize>,
        upvalue_to_group: &IndexMap<RcLocal, RcLocal>,
        observability_reuses: &mut u64,
    ) -> Self {
        let single_rhs = statement.as_assign().filter(|assign| assign.right.len() == 1);
        let single_rhs_effect = single_rhs.map(|assign| ast::is_observable(&assign.right[0]));
        // Local destinations have no effects. For this common SSA assignment
        // shape, the statement and its one RHS have identical observability.
        // Keep captured reads as a separate, stricter movement constraint.
        let observable = if single_rhs.is_some_and(|assign| assign.left.iter().all(|left| left.as_local().is_some())) {
            *observability_reuses += 1;
            single_rhs_effect.unwrap()
        } else {
            ast::statement_is_observable(statement)
        };
        let mut facts = Self {
            read_groups: Vec::new(),
            write_groups: Vec::new(),
            writes_upvalue: false,
            observable,
            single_rhs_observable: single_rhs
                .map(|assign| single_rhs_effect.unwrap()
                    || assign.right[0].any_local_read(&mut |local| upvalue_to_group.contains_key(local))),
        };
        statement.visit_local_reads(&mut |local| {
            if let Some(&group) = local_to_group.get(local) { facts.read_groups.push(group); }
            true
        });
        statement.visit_local_writes(&mut |local| {
            if let Some(&group) = local_to_group.get(local) { facts.write_groups.push(group); }
            facts.writes_upvalue |= upvalue_to_group.contains_key(local);
            true
        });
        #[cfg(test)]
        assert_eq!(facts, Self::new_reference(statement, local_to_group, upvalue_to_group));
        facts
    }

    #[cfg(test)]
    fn new_reference(
        statement: &Statement,
        local_to_group: &FxHashMap<RcLocal, usize>,
        upvalue_to_group: &IndexMap<RcLocal, RcLocal>,
    ) -> Self {
        let reads = statement.values_read();
        let writes = statement.values_written();
        Self {
            read_groups: reads
                .iter()
                .filter_map(|local| local_to_group.get(*local).copied())
                .collect(),
            write_groups: writes
                .iter()
                .filter_map(|local| local_to_group.get(*local).copied())
                .collect(),
            writes_upvalue: writes
                .iter()
                .any(|local| upvalue_to_group.contains_key(*local)),
            observable: ast::statement_is_observable(statement),
            single_rhs_observable: statement
                .as_assign()
                .filter(|assign| assign.right.len() == 1)
                .map(|assign| {
                    let value = &assign.right[0];
                    ast::is_observable(value)
                        || value
                            .values_read()
                            .iter()
                            .any(|local| upvalue_to_group.contains_key(*local))
                }),
        }
    }
}

#[derive(Default, Clone, Copy)]
pub(super) struct Statistics {
    pub hits: u64,
    pub misses: u64,
    pub uncached: u64,
    pub invalidations: u64,
    pub evictions: u64,
    pub slots: u64,
    pub observability_reuses: u64,
}

impl Statistics {
    pub fn add(&mut self, other: Self) {
        self.hits += other.hits;
        self.misses += other.misses;
        self.uncached += other.uncached;
        self.invalidations += other.invalidations;
        self.evictions += other.evictions;
        self.slots += other.slots;
        self.observability_reuses += other.observability_reuses;
    }

    pub fn record(self) {
        if ast::telemetry::enabled() {
            ast::telemetry::count("ssa_fact_cache_hits", self.hits);
            ast::telemetry::count("ssa_fact_cache_misses", self.misses);
            ast::telemetry::count("ssa_fact_cache_uncached", self.uncached);
            ast::telemetry::count("ssa_fact_cache_invalidations", self.invalidations);
            ast::telemetry::count("ssa_fact_cache_evictions", self.evictions);
            ast::telemetry::count("ssa_fact_cache_slots", self.slots);
            ast::telemetry::count("ssa_fact_observability_reuses", self.observability_reuses);
        }
    }
}

pub(super) struct Cache<'a> {
    local_to_group: &'a FxHashMap<RcLocal, usize>,
    upvalue_to_group: &'a IndexMap<RcLocal, RcLocal>,
    slots: Vec<Option<(usize, StatementFacts)>>,
    scratch: Option<StatementFacts>,
    statistics: Statistics,
}

impl<'a> Cache<'a> {
    pub fn new(
        statements: usize,
        local_to_group: &'a FxHashMap<RcLocal, usize>,
        upvalue_to_group: &'a IndexMap<RcLocal, RcLocal>,
    ) -> Self {
        Self::with_limit(
            statements,
            MAX_CACHED_SLOTS,
            local_to_group,
            upvalue_to_group,
        )
    }

    fn with_limit(
        statements: usize,
        limit: usize,
        local_to_group: &'a FxHashMap<RcLocal, usize>,
        upvalue_to_group: &'a IndexMap<RcLocal, RcLocal>,
    ) -> Self {
        let count = if statements >= MIN_CACHED_STATEMENTS {
            statements.min(limit)
        } else {
            0
        };
        Self {
            local_to_group,
            upvalue_to_group,
            slots: (0..count).map(|_| None).collect(),
            scratch: None,
            statistics: Statistics {
                slots: count as u64,
                ..Statistics::default()
            },
        }
    }

    /// Large blocks retain the same bounded cache budget as smaller ones.
    /// Tags distinguish positions sharing a slot; eviction changes cost only.
    /// The common within-budget block avoids the modulo operation entirely.
    fn slot_index(&self, index: usize) -> Option<usize> {
        let count = self.slots.len();
        if count == 0 { None }
        else if index < count { Some(index) }
        else { Some(index % count) }
    }

    pub fn get(&mut self, index: usize, statement: &Statement) -> &StatementFacts {
        if let Some(slot_index) = self.slot_index(index) {
            let slot = &mut self.slots[slot_index];
            if slot.as_ref().is_some_and(|(cached_index, _)| *cached_index == index) {
                self.statistics.hits += 1;
                // Exercise the invalidation contract on every real cache hit
                // in debug/CI runs. Release builds do not recompute the facts.
                debug_assert_eq!(
                    &slot.as_ref().unwrap().1,
                    &StatementFacts::new(statement, self.local_to_group, self.upvalue_to_group, &mut 0),
                    "SSA inline statement facts were not invalidated",
                );
            } else {
                self.statistics.evictions += u64::from(slot.is_some());
                self.statistics.misses += 1;
                *slot = Some((index, StatementFacts::new(
                    statement,
                    self.local_to_group,
                    self.upvalue_to_group,
                    &mut self.statistics.observability_reuses,
                )));
            }
            &slot.as_ref().unwrap().1
        } else {
            self.statistics.uncached += 1;
            self.scratch = Some(StatementFacts::new(
                statement,
                self.local_to_group,
                self.upvalue_to_group,
                &mut self.statistics.observability_reuses,
            ));
            self.scratch.as_ref().unwrap()
        }
    }

    pub fn invalidate(&mut self, index: usize) {
        if let Some(slot_index) = self.slot_index(index) {
            let slot = &mut self.slots[slot_index];
            if slot.as_ref().is_some_and(|(cached_index, _)| *cached_index == index) {
                *slot = None;
                self.statistics.invalidations += 1;
            }
        }
    }

    pub fn statistics(&self) -> Statistics {
        self.statistics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast::{Assign, Call, Global, LValue, Literal, Local, RValue};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_owned())))
    }

    #[test]
    fn shared_rhs_observability_matches_full_statement_checks() {
        let [output, captured] = std::array::from_fn(|_| RcLocal::default());
        let groups = FxHashMap::default();
        let captures = IndexMap::from_iter([(captured.clone(), captured.clone())]);
        let values: Vec<RValue> = vec![
            Literal::Boolean(false).into(), captured.clone().into(),
            Global::from("read_environment").into(),
            Call::new(Global::from("effect").into(), Vec::new()).into(),
            ast::Unary::new(captured.clone().into(), ast::UnaryOperation::Not).into(),
            ast::Binary::new(captured.clone().into(), Literal::Boolean(false).into(), ast::BinaryOperation::And).into(),
            ast::Index::new(Literal::Nil.into(), Literal::String(b"key".to_vec()).into()).into(),
            ast::Table::new(vec![(Some(Literal::Nil.into()), Literal::Number(1.0).into())]).into(),
        ];
        for value in values {
            for left in [
                Vec::new(), vec![output.clone().into()],
                vec![output.clone().into(), output.clone().into()],
                vec![Global::from("write_environment").into()],
                vec![ast::Index::new(captured.clone().into(), Literal::Nil.into()).into()],
            ] {
                for right in [vec![value.clone()], vec![value.clone(), value.clone()]] {
                    let statement = Assign::new(left.clone(), right).into();
                    let mut reuses = 0;
                    let actual = StatementFacts::new(&statement, &groups, &captures, &mut reuses);
                    assert_eq!(actual, StatementFacts::new_reference(&statement, &groups, &captures));
                    let assign = statement.as_assign().unwrap();
                    assert_eq!(reuses, u64::from(assign.right.len() == 1 && assign.left.iter().all(|left| left.as_local().is_some())));
                }
            }
        }
    }

    #[test]
    fn invalidation_refreshes_reads_writes_cells_and_effects() {
        let a = local("a");
        let b = local("b");
        let c = local("c");
        let groups = FxHashMap::from_iter([(a.clone(), 1), (b.clone(), 2), (c.clone(), 3)]);
        let captures = IndexMap::from_iter([(c.clone(), c.clone())]);
        let mut cache = Cache::new(4, &groups, &captures);
        let mut statement = Assign::new(vec![LValue::Local(a.clone())], vec![RValue::Local(
            b.clone(),
        )])
        .into();
        let facts = cache.get(0, &statement);
        assert_eq!(facts.read_groups, [2]);
        assert_eq!(facts.write_groups, [1]);
        assert!(!facts.writes_upvalue);
        assert_eq!(facts.single_rhs_observable, Some(false));
        assert!(!facts.observable);
        cache.get(0, &statement);
        assert_eq!(cache.statistics().hits, 1);

        statement = Assign::new(vec![LValue::Local(c.clone())], vec![
            Call::new(Global::from("effect").into(), vec![a.into()]).into(),
        ])
        .into();
        cache.invalidate(0);
        let facts = cache.get(0, &statement);
        assert_eq!(facts.read_groups, [1]);
        assert_eq!(facts.write_groups, [3]);
        assert!(facts.writes_upvalue);
        assert!(facts.observable);
        assert_eq!(facts.single_rhs_observable, Some(true));

        statement = Assign::new(vec![LValue::Local(b)], vec![c.into()]).into();
        cache.invalidate(0);
        assert_eq!(cache.get(0, &statement).single_rhs_observable, Some(true));
        statement = ast::Empty {}.into();
        cache.invalidate(0);
        let facts = cache.get(0, &statement);
        assert!(facts.read_groups.is_empty() && facts.write_groups.is_empty());
        assert!(!facts.observable && !facts.writes_upvalue);
        assert!(facts.single_rhs_observable.is_none());
        assert_eq!(cache.statistics().misses, 4);
        assert_eq!(cache.statistics().invalidations, 3);
    }

    #[test]
    fn bounded_cache_tags_evictions_and_invalidations_on_large_blocks() {
        let groups = FxHashMap::default();
        let captures = IndexMap::default();
        let mut cache = Cache::with_limit(5, 4, &groups, &captures);
        let mut statement: Statement = ast::Return::new(vec![Literal::Nil.into()]).into();
        assert_eq!(cache.statistics().slots, 4);
        assert!(cache.get(0, &statement).single_rhs_observable.is_none());
        statement =
            Assign::new(vec![local("a").into()], vec![Global::from("lookup").into()]).into();
        // Position 4 aliases position 0, but must compute its own facts.
        assert_eq!(cache.get(4, &statement).single_rhs_observable, Some(true));
        assert_eq!(cache.statistics().evictions, 1);
        cache.invalidate(0); // must not invalidate position 4's replacement
        assert_eq!(cache.get(4, &statement).single_rhs_observable, Some(true));
        assert_eq!(cache.statistics().hits, 1);
        cache.invalidate(4);
        statement = ast::Return::new(vec![Literal::Nil.into()]).into();
        assert!(cache.get(4, &statement).single_rhs_observable.is_none());
        assert_eq!(cache.statistics().invalidations, 1);
        assert_eq!(cache.statistics().uncached, 0);
        assert_eq!(cache.statistics().misses, 3);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "SSA inline statement facts were not invalidated")]
    fn debug_guard_detects_a_missing_invalidation() {
        let groups = FxHashMap::default();
        let captures = IndexMap::default();
        let mut cache = Cache::new(4, &groups, &captures);
        let mut statement: Statement = ast::Empty {}.into();
        cache.get(0, &statement);
        statement =
            Assign::new(vec![local("a").into()], vec![Global::from("lookup").into()]).into();
        cache.get(0, &statement);
    }
}
