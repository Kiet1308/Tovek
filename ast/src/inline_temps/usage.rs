//! Occurrence counts for one monotone temp-inlining traversal. The scope tree
//! stays fixed: an initializer moves within its block and only its binder's
//! single read/write disappears. No AST owners or facts survive this phase.

use rustc_hash::FxHashMap;
use crate::{Block, LocalRw, RcLocal, RValue, Statement, Traverse};

#[derive(Clone, Copy, Debug)]
pub(super) struct Scope {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug)]
struct Occurrence {
    scope: usize,
    reads: usize,
    writes: usize,
}

enum Occurrences {
    // Most locals occur in only one block. Keep this case allocation-free.
    One(Occurrence),
    Many { entries: Vec<Occurrence>, next: Vec<usize> },
}

impl Occurrences {
    fn add(&mut self, occurrence: Occurrence) {
        match self {
            Self::One(first) if first.scope == occurrence.scope => {
                first.reads += occurrence.reads;
                first.writes += occurrence.writes;
            }
            Self::One(first) => {
                *self = Self::Many { entries: vec![*first, occurrence], next: Vec::new() };
            }
            Self::Many { entries, .. } => {
                if let Some(last) = entries.last_mut().filter(|last| last.scope == occurrence.scope) {
                    last.reads += occurrence.reads;
                    last.writes += occurrence.writes;
                } else { entries.push(occurrence); }
            }
        }
    }

    fn finish(&mut self) {
        let Self::Many { entries, next } = self else { return; };
        // A parent can have direct reads both before and after a child block.
        entries.sort_unstable_by_key(|entry| entry.scope);
        let mut write = 0;
        for read in 0..entries.len() {
            if write > 0 && entries[write - 1].scope == entries[read].scope {
                entries[write - 1].reads += entries[read].reads;
                entries[write - 1].writes += entries[read].writes;
            } else {
                entries[write] = entries[read];
                write += 1;
            }
        }
        entries.truncate(write);
        next.extend(0..=write);
    }

    fn single_use(&mut self, scope: Scope) -> bool {
        match self {
            Self::One(entry) => scope.start <= entry.scope && entry.scope < scope.end
                && entry.reads == 1 && entry.writes == 1,
            Self::Many { entries, next } => {
                let mut index = live(next, entries.partition_point(|entry| entry.scope < scope.start));
                let (mut reads, mut writes) = (0, 0);
                while let Some(entry) = entries.get(index).filter(|entry| entry.scope < scope.end) {
                    reads += entry.reads;
                    writes += entry.writes;
                    // Every live entry contains a read or write, so at most
                    // three entries can be examined before returning.
                    if reads > 1 || writes > 1 { return false; }
                    index = live(next, index + 1);
                }
                reads == 1 && writes == 1
            }
        }
    }

    fn remove(&mut self, scope: Scope) {
        match self {
            Self::One(entry) => {
                debug_assert_eq!((entry.scope, entry.reads, entry.writes), (scope.start, 1, 1));
                entry.reads = 0;
                entry.writes = 0;
            }
            Self::Many { entries, next } => {
                let index = entries.binary_search_by_key(&scope.start, |entry| entry.scope).unwrap();
                debug_assert_eq!((entries[index].reads, entries[index].writes), (1, 1));
                debug_assert_eq!(next[index], index);
                next[index] = live(next, index + 1);
            }
        }
    }
}

/// Successor deletion with path compression skips emptied descendant scopes.
/// It never revisits a growing run of dead occurrences at every ancestor.
fn live(next: &mut [usize], mut index: usize) -> usize {
    let mut root = index;
    while next[root] != root {
        #[cfg(test)]
        SUCCESSOR_STEPS.with(|steps| steps.set(steps.get() + 1));
        root = next[root];
    }
    while next[index] != index {
        let previous = next[index];
        next[index] = root;
        index = previous;
    }
    root
}

#[cfg(test)]
thread_local! { static SUCCESSOR_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(super) struct SubtreeUsage {
    scopes: FxHashMap<usize, Scope>,
    locals: FxHashMap<u64, Occurrences>,
}

impl SubtreeUsage {
    pub(super) fn new(block: &Block) -> Option<Self> {
        let mut result = Self { scopes: Default::default(), locals: Default::default() };
        if !result.block(block) { return None; }
        for occurrences in result.locals.values_mut() { occurrences.finish(); }
        Some(result)
    }

    pub(super) fn scope(&self, block: &Block) -> Scope {
        self.scopes[&(block as *const Block as usize)]
    }

    pub(super) fn single_use(&mut self, scope: Scope, local: &RcLocal) -> bool {
        self.locals.get_mut(&local.stable_id()).is_some_and(|entries| entries.single_use(scope))
    }

    pub(super) fn remove(&mut self, scope: Scope, local: &RcLocal) {
        self.locals.get_mut(&local.stable_id()).unwrap().remove(scope);
    }

    fn record(&mut self, local: &RcLocal, scope: usize, write: bool) {
        let occurrence = Occurrence { scope, reads: usize::from(!write), writes: usize::from(write) };
        self.locals.entry(local.stable_id()).and_modify(|entries| entries.add(occurrence))
            .or_insert(Occurrences::One(occurrence));
    }

    fn block(&mut self, block: &Block) -> bool {
        let identity = block as *const Block as usize;
        let start = self.scopes.len();
        // Shared bodies can change through an earlier sibling occurrence.
        // Decline the index before mutating anything; retain the exact legacy
        // traversal/counting path for such graphs.
        if self.scopes.insert(identity, Scope { start, end: start }).is_some() { return false; }
        for statement in &block.0 {
            let mut lhs_closure = false;
            statement.visit_lvalues(&mut |left| {
                left.traverse_rvalues_ref(&mut |value| lhs_closure |= matches!(value, RValue::Closure(_)));
                !lhs_closure
            });
            // Mutation traverses indexed-LHS closures; the legacy usage census
            // counts their capture operands but intentionally not their bodies.
            if lhs_closure { return false; }
            statement.visit_local_reads(&mut |local| { self.record(local, start, false); true });
            statement.visit_local_writes(&mut |local| { self.record(local, start, true); true });
            let mut complete = true;
            super::collect_closures_in_statement(statement, &mut |closure| {
                if complete {
                    complete = closure.function.try_lock().is_some_and(|function| self.block(&function.body));
                }
            });
            if !complete { return false; }
            let mut child = |block: &parking_lot::Mutex<Block>| {
                block.try_lock().is_some_and(|body| self.block(&body))
            };
            let complete = match statement {
                Statement::If(node) => child(&node.then_block) && child(&node.else_block),
                Statement::While(node) => child(&node.block),
                Statement::Repeat(node) => child(&node.block),
                Statement::NumericFor(node) => child(&node.block),
                Statement::GenericFor(node) => child(&node.block),
                _ => true,
            };
            if !complete { return false; }
        }
        let end = self.scopes.len();
        self.scopes.get_mut(&identity).unwrap().end = end;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successor_deletions_do_not_rescan_dead_descendant_runs() {
        for count in [64, 256, 1024, 4096] {
            let mut entries = Occurrences::One(Occurrence { scope: 0, reads: 1, writes: 1 });
            for scope in 1..count { entries.add(Occurrence { scope, reads: 1, writes: 1 }); }
            entries.finish();
            SUCCESSOR_STEPS.with(|steps| steps.set(0));
            for start in (1..count).rev() {
                assert!(entries.single_use(Scope { start, end: count }));
                entries.remove(Scope { start, end: count });
                assert!(!entries.single_use(Scope { start, end: count }));
            }
            for _ in 0..count { assert!(entries.single_use(Scope { start: 0, end: count })); }
            assert!(SUCCESSOR_STEPS.with(|steps| steps.get()) < count * 5);
        }
    }

    #[test]
    fn bounded_queries_preserve_split_reads_writes_and_parent_occurrences() {
        let mut entries = Occurrences::One(Occurrence { scope: 1, reads: 0, writes: 1 });
        entries.add(Occurrence { scope: 3, reads: 1, writes: 0 });
        entries.add(Occurrence { scope: 1, reads: 1, writes: 0 });
        entries.add(Occurrence { scope: 5, reads: 0, writes: 1 });
        entries.finish();
        assert!(entries.single_use(Scope { start: 1, end: 3 }));
        assert!(!entries.single_use(Scope { start: 1, end: 4 }));
        entries.remove(Scope { start: 1, end: 3 });
        assert!(entries.single_use(Scope { start: 0, end: 6 }));
        assert!(!entries.single_use(Scope { start: 4, end: 6 }));
    }
}
