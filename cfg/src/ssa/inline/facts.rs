//! Read-only facts for one block during a single `inline_rvalues` invocation.
//! The block's statement positions and both group maps are fixed in this
//! interval. Callers must invalidate every statement that they mutate. The
//! cache is discarded before cleanup can remove, move, or fold statements.
//! Only integer group IDs and booleans are retained, never AST/local owners.
use ast::{LocalRw, RcLocal, Statement};
use indexmap::IndexMap;
use rustc_hash::FxHashMap;

const MIN_CACHED_STATEMENTS: usize = 4;
const MAX_CACHED_STATEMENTS: usize = 16_384;

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
    pub slots: u64,
}

impl Statistics {
    pub fn add(&mut self, other: Self) {
        self.hits += other.hits;
        self.misses += other.misses;
        self.uncached += other.uncached;
        self.invalidations += other.invalidations;
        self.slots += other.slots;
    }

    pub fn record(self) {
        if ast::telemetry::enabled() {
            ast::telemetry::count("ssa_fact_cache_hits", self.hits);
            ast::telemetry::count("ssa_fact_cache_misses", self.misses);
            ast::telemetry::count("ssa_fact_cache_uncached", self.uncached);
            ast::telemetry::count("ssa_fact_cache_invalidations", self.invalidations);
            ast::telemetry::count("ssa_fact_cache_slots", self.slots);
        }
    }
}

pub(super) struct Cache<'a> {
    local_to_group: &'a FxHashMap<RcLocal, usize>,
    upvalue_to_group: &'a IndexMap<RcLocal, RcLocal>,
    slots: Vec<Option<StatementFacts>>,
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
            MAX_CACHED_STATEMENTS,
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
        let count = if (MIN_CACHED_STATEMENTS..=limit).contains(&statements) {
            statements
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

    pub fn get(&mut self, index: usize, statement: &Statement) -> &StatementFacts {
        if let Some(slot) = self.slots.get_mut(index) {
            if slot.is_some() {
                self.statistics.hits += 1;
                // Exercise the invalidation contract on every real cache hit
                // in debug/CI runs. Release builds do not recompute the facts.
                debug_assert_eq!(
                    slot.as_ref().unwrap(),
                    &StatementFacts::new(statement, self.local_to_group, self.upvalue_to_group),
                    "SSA inline statement facts were not invalidated",
                );
            } else {
                self.statistics.misses += 1;
                *slot = Some(StatementFacts::new(
                    statement,
                    self.local_to_group,
                    self.upvalue_to_group,
                ));
            }
            slot.as_ref().unwrap()
        } else {
            self.statistics.uncached += 1;
            self.scratch = Some(StatementFacts::new(
                statement,
                self.local_to_group,
                self.upvalue_to_group,
            ));
            self.scratch.as_ref().unwrap()
        }
    }

    pub fn invalidate(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(index) {
            if slot.take().is_some() {
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
    fn size_limit_uses_fresh_facts_without_cached_slots() {
        let groups = FxHashMap::default();
        let captures = IndexMap::new();
        let mut cache = Cache::with_limit(5, 4, &groups, &captures);
        let mut statement: Statement = ast::Return::new(vec![Literal::Nil.into()]).into();
        assert_eq!(cache.statistics().slots, 0);
        assert!(cache.get(0, &statement).single_rhs_observable.is_none());
        statement =
            Assign::new(vec![local("a").into()], vec![Global::from("lookup").into()]).into();
        assert_eq!(cache.get(0, &statement).single_rhs_observable, Some(true));
        assert_eq!(cache.statistics().uncached, 2);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "SSA inline statement facts were not invalidated")]
    fn debug_guard_detects_a_missing_invalidation() {
        let groups = FxHashMap::default();
        let captures = IndexMap::new();
        let mut cache = Cache::new(4, &groups, &captures);
        let mut statement: Statement = ast::Empty {}.into();
        cache.get(0, &statement);
        statement =
            Assign::new(vec![local("a").into()], vec![Global::from("lookup").into()]).into();
        cache.get(0, &statement);
    }
}
