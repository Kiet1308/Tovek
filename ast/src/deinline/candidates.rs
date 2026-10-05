//! Compile the statement matcher's existing head refusals into a scoped index.
//!
//! This module can only avoid attempts that the exact matcher would reject
//! before spending search fuel. It never proves a rewrite. Ranks refer to the
//! caller's already-prioritized list, so iterating set bits preserves both
//! candidate order and the complete ambiguity set. The owner rebuilds the list
//! when lexical scope grows and drops it at the end of the block scan.

use std::mem::{Discriminant, discriminant};

use rustc_hash::FxHashMap;

use super::{TKind, Target, ValueAnchor};
use crate::{Assign, Statement};

type StatementKind = Discriminant<Statement>;

// Tiny scopes retain the allocation-free scan. The module-wide target ceiling
// is 256, but exceeding it here must fall back to a complete list, never truncate
// potential rivals if that upstream limit changes.
const MIN_INDEXED_TARGETS: usize = 16;
const MAX_INDEXED_TARGETS: usize = 256;
const RANK_WORDS: usize = MAX_INDEXED_TARGETS / u64::BITS as usize;

#[derive(Clone, Copy, Default)]
struct Ranks([u64; RANK_WORDS]);

impl Ranks {
    fn insert(&mut self, rank: usize) {
        self.0[rank / u64::BITS as usize] |= 1u64 << (rank % u64::BITS as usize);
    }

    fn include(&mut self, other: Self) {
        for (word, other) in self.0.iter_mut().zip(other.0) {
            *word |= other;
        }
    }
}

#[derive(Default)]
struct KindIndex {
    all: Ranks,
    unnamed: Ranks,
    named: FxHashMap<u64, Ranks>,
}

impl KindIndex {
    fn matching(&self, name: Option<u64>) -> Ranks {
        let Some(name) = name else {
            // A site with no fixed name says nothing about a target's name.
            return self.all;
        };
        let mut ranks = self.unnamed;
        if let Some(named) = self.named.get(&name) {
            ranks.include(*named);
        }
        ranks
    }
}

#[derive(Default)]
struct HeadIndex {
    any_kind: Ranks,
    by_kind: FxHashMap<StatementKind, KindIndex>,
}

impl HeadIndex {
    fn new(ordered: &[usize], targets: &[Target]) -> Self {
        let mut index = Self::default();
        for (rank, &target) in ordered.iter().enumerate() {
            let target = &targets[target];
            let Some(first) = target.pat.first() else {
                // The exact attempt refuses an empty pattern before fuel too.
                continue;
            };
            let head_may_vanish = target.specializable && matches!(first, Statement::If(_));
            let use_kind = !head_may_vanish
                && (target.kind == TKind::Void
                    || (target.kind == TKind::Value
                        && target.value_anchor == ValueAnchor::AtPrefix));
            if !use_kind {
                // Value-result declarations and a specialized leading branch
                // need the family's full matcher; their head is not a filter.
                index.any_kind.insert(rank);
                continue;
            }
            let kind = index.by_kind.entry(target.pat0_kind).or_default();
            kind.all.insert(rank);
            match target.pat0_anchor_key {
                Some(name) => kind.named.entry(name).or_default().insert(rank),
                None => kind.unnamed.insert(rank),
            }
        }
        index
    }

    fn matching(&self, kind: Option<StatementKind>, name: Option<u64>, is_if: bool) -> Ranks {
        let mut ranks = self.any_kind;
        if let Some(kind) = kind {
            if let Some(index) = self.by_kind.get(&kind) {
                ranks.include(index.matching(name));
            }
            if is_if {
                // Canonicalization may fuse a site's select-if into the
                // assignment that begins a helper pattern.
                let assign = discriminant(&Statement::Assign(Assign::new(Vec::new(), Vec::new())));
                if let Some(index) = self.by_kind.get(&assign) {
                    ranks.include(index.matching(name));
                }
            }
        }
        ranks
    }
}

/// A complete lexical candidate list, with an optional non-owning head index.
/// Only numeric ranks and head hashes are indexed; no AST or local is retained.
pub(super) struct Candidates {
    ordered: Vec<usize>,
    index: Option<HeadIndex>,
}

impl Candidates {
    pub(super) fn new(ordered: Vec<usize>, targets: &[Target]) -> Self {
        let index = (!cfg!(feature = "reference-deinline-candidates")
            && (MIN_INDEXED_TARGETS..=MAX_INDEXED_TARGETS).contains(&ordered.len()))
        .then(|| HeadIndex::new(&ordered, targets))
        // An all-wildcard scope cannot avoid a single head attempt. It needs
        // neither retained metadata nor a mask iterator at every position.
        .filter(|index| !index.by_kind.is_empty());
        if index.is_some() {
            // Construction counters aggregate by phase, outside the position /
            // target hot loops. Normal execution creates no diagnostic maps.
            crate::telemetry::count("candidate_index_builds", 1);
            crate::telemetry::count("candidate_index_targets", ordered.len() as u64);
        }
        Self { ordered, index }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    pub(super) fn iter(&self) -> std::slice::Iter<'_, usize> {
        self.ordered.iter()
    }

    pub(super) fn matching(
        &self,
        kind: Option<StatementKind>,
        name: Option<u64>,
        is_if: bool,
    ) -> Matching<'_> {
        Matching(match &self.index {
            Some(index) => MatchingState::Indexed {
                ordered: &self.ordered,
                ranks: index.matching(kind, name, is_if),
                word: 0,
            },
            None => MatchingState::Linear(self.ordered.iter()),
        })
    }
}

pub(super) struct Matching<'a>(MatchingState<'a>);

enum MatchingState<'a> {
    Linear(std::slice::Iter<'a, usize>),
    Indexed {
        ordered: &'a [usize],
        ranks: Ranks,
        word: usize,
    },
}

impl Iterator for Matching<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            MatchingState::Linear(iter) => iter.next().copied(),
            MatchingState::Indexed {
                ordered,
                ranks,
                word,
            } => {
                while *word < RANK_WORDS {
                    let bits = &mut ranks.0[*word];
                    if *bits == 0 {
                        *word += 1;
                        continue;
                    }
                    let rank = *word * u64::BITS as usize + bits.trailing_zeros() as usize;
                    *bits &= *bits - 1;
                    return Some(ordered[rank]);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Call, If, Literal, Return};

    fn statement(variant: usize) -> Statement {
        match variant {
            0 => Statement::Assign(Assign::new(Vec::new(), Vec::new())),
            1 => Statement::Call(Call::new(Literal::Nil.into(), Vec::new())),
            2 => Statement::If(If::new(
                Literal::Nil.into(),
                Block::default(),
                Block::default(),
            )),
            3 => Statement::Return(Return::default()),
            _ => Statement::Empty(crate::Empty {}),
        }
    }

    fn target(
        variant: usize,
        value: bool,
        prefix: bool,
        specializable: bool,
        name: Option<u64>,
    ) -> Target {
        let first = statement(variant);
        Target {
            f_local: Default::default(),
            func_ptr: std::ptr::null(),
            kind: if value { TKind::Value } else { TKind::Void },
            pat0_kind: discriminant(&first),
            pat0_anchor_key: name,
            pat: vec![first],
            pat_raw_len: 1,
            pat_spine_len: 1,
            pat_nodes: 1,
            focused: true,
            value_anchor: if prefix {
                ValueAnchor::AtPrefix
            } else {
                ValueAnchor::AtResultDecl
            },
            prefix_len: usize::from(prefix),
            params: Default::default(),
            locals: Default::default(),
            param_order: Vec::new(),
            written_params: Vec::new(),
            unread: Default::default(),
            first_reads: Vec::new(),
            first_register_reads: Vec::new(),
            free_cells: Vec::new(),
            specializable,
            truth_params: Vec::new(),
            optional_params: Vec::new(),
            specializations: Default::default(),
            falls_off: false,
            cps_loop_return: false,
            loop_exit_at: None,
            returns: Vec::new(),
            captures: Default::default(),
            search: Default::default(),
        }
    }

    // The pre-index predicate, kept independently from HeadIndex. Do not
    // replace it with a call into the implementation: this is the exhaustive
    // enumeration oracle for the optimization, including the exact fallbacks.
    fn old_head_allows(
        target: &Target,
        kind: Option<StatementKind>,
        name: Option<u64>,
        is_if: bool,
    ) -> bool {
        if target.pat.is_empty() {
            return false;
        }
        let head_may_vanish = target.specializable && matches!(target.pat[0], Statement::If(_));
        let use_disc = !head_may_vanish
            && (target.kind == TKind::Void
                || (target.kind == TKind::Value && target.value_anchor == ValueAnchor::AtPrefix));
        let assign_kind = discriminant(&statement(0));
        let fused_head = is_if && target.pat0_kind == assign_kind;
        if use_disc && kind != Some(target.pat0_kind) && !fused_head {
            return false;
        }
        if use_disc {
            if let (Some(site), Some(pattern)) = (name, target.pat0_anchor_key) {
                if site != pattern {
                    return false;
                }
            }
        }
        true
    }

    fn indexed(ordered: Vec<usize>, targets: &[Target]) -> Candidates {
        Candidates {
            index: Some(HeadIndex::new(&ordered, targets)),
            ordered,
        }
    }

    #[test]
    fn indexed_order_matches_independent_head_oracle() {
        let mut targets = Vec::new();
        for variant in 0..5 {
            for value in [false, true] {
                for prefix in [false, true] {
                    for specializable in [false, true] {
                        for name in [None, Some(0), Some(1), Some(u64::MAX)] {
                            targets.push(target(variant, value, prefix, specializable, name));
                        }
                    }
                }
            }
        }
        let mut empty = target(0, false, false, false, None);
        empty.pat.clear();
        targets.push(empty);
        let mut orders = vec![(0..targets.len()).collect::<Vec<_>>()];
        orders.push(orders[0].iter().copied().rev().collect());
        orders.push(
            (0..targets.len())
                .step_by(2)
                .chain((1..targets.len()).step_by(2))
                .collect(),
        );
        for order in orders {
            let candidates = indexed(order.clone(), &targets);
            for variant in 0..=5 {
                let kind = (variant < 5).then(|| discriminant(&statement(variant)));
                let is_if = variant == 2;
                for name in [None, Some(0), Some(1), Some(2), Some(u64::MAX)] {
                    let expected: Vec<_> = order
                        .iter()
                        .copied()
                        .filter(|&i| old_head_allows(&targets[i], kind, name, is_if))
                        .collect();
                    assert_eq!(
                        candidates.matching(kind, name, is_if).collect::<Vec<_>>(),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn rank_boundaries_and_named_buckets_preserve_priority() {
        let targets: Vec<_> = (0..MAX_INDEXED_TARGETS)
            .map(|i| target(1, false, false, false, Some(i as u64)))
            .collect();
        let order: Vec<_> = (0..targets.len()).rev().collect();
        let candidates = indexed(order.clone(), &targets);
        let call = Some(discriminant(&statement(1)));
        for rank in [0, 1, 63, 64, 65, 127, 128, 191, 192, 254, 255] {
            let id = order[rank];
            assert_eq!(
                candidates
                    .matching(call, Some(id as u64), false)
                    .collect::<Vec<_>>(),
                vec![id]
            );
        }
        assert_eq!(
            candidates.matching(call, None, false).collect::<Vec<_>>(),
            order
        );
        assert!(candidates.matching(call, Some(256), false).next().is_none());
    }

    #[test]
    fn tiny_and_oversized_scopes_keep_every_candidate() {
        let targets: Vec<_> = (0..=MAX_INDEXED_TARGETS)
            .map(|i| target(1, false, false, false, Some(i as u64)))
            .collect();
        for len in [0, 1, MIN_INDEXED_TARGETS - 1, MAX_INDEXED_TARGETS + 1] {
            let order: Vec<_> = (0..len).rev().collect();
            let candidates = Candidates::new(order.clone(), &targets);
            assert!(candidates.index.is_none());
            assert_eq!(candidates.is_empty(), len == 0);
            assert_eq!(candidates.iter().copied().collect::<Vec<_>>(), order);
            assert_eq!(
                candidates
                    .matching(None, Some(u64::MAX), false)
                    .collect::<Vec<_>>(),
                order
            );
        }
    }

    #[test]
    fn production_constructor_indexes_threshold_and_capacity_boundaries() {
        let targets: Vec<_> = (0..MAX_INDEXED_TARGETS)
            .map(|i| target(i % 4, false, false, false, Some((i % 3) as u64)))
            .collect();
        for len in [MIN_INDEXED_TARGETS, MAX_INDEXED_TARGETS] {
            let order: Vec<_> = (0..len).rev().collect();
            let candidates = Candidates::new(order.clone(), &targets);
            assert_eq!(
                candidates.index.is_some(),
                !cfg!(feature = "reference-deinline-candidates")
            );
            for variant in 0..4 {
                let kind = Some(discriminant(&statement(variant)));
                let is_if = variant == 2;
                for name in [None, Some(0), Some(2), Some(u64::MAX)] {
                    let expected: Vec<_> = order
                        .iter()
                        .copied()
                        .filter(|&i| old_head_allows(&targets[i], kind, name, is_if))
                        .collect();
                    let offered: Vec<_> = candidates.matching(kind, name, is_if).collect();
                    #[cfg(not(feature = "reference-deinline-candidates"))]
                    assert_eq!(offered, expected);
                    #[cfg(feature = "reference-deinline-candidates")]
                    assert_eq!(offered, order);
                    // In the ablation build the unchanged attempt gate performs
                    // these refusals; both builds must attempt the same survivors.
                    let survivors: Vec<_> = offered
                        .into_iter()
                        .filter(|&i| old_head_allows(&targets[i], kind, name, is_if))
                        .collect();
                    assert_eq!(survivors, expected);
                }
            }
        }
    }

    #[test]
    fn all_wildcard_scopes_stay_linear() {
        let targets: Vec<_> = (0..MIN_INDEXED_TARGETS)
            .map(|i| target(2, i % 2 == 0, false, i % 2 != 0, Some(i as u64)))
            .collect();
        let order: Vec<_> = (0..targets.len()).collect();
        let candidates = Candidates::new(order.clone(), &targets);
        assert!(candidates.index.is_none());
        assert_eq!(
            candidates.matching(None, None, false).collect::<Vec<_>>(),
            order
        );
    }

    #[test]
    fn written_parameter_prefix_uses_compiled_site_head() {
        let mut written = target(1, false, false, false, Some(77));
        // Collection changes the site head to a leading argument-copy Assign;
        // neither the pattern's original kind nor its method name filters it.
        written.pat0_kind = discriminant(&statement(0));
        written.pat0_anchor_key = None;
        written.written_params.push(Default::default());
        let targets = vec![written];
        let candidates = indexed(vec![0], &targets);
        for variant in 0..5 {
            let kind = Some(discriminant(&statement(variant)));
            let is_if = variant == 2;
            let expected = old_head_allows(&targets[0], kind, Some(99), is_if);
            assert_eq!(
                candidates.matching(kind, Some(99), is_if).next().is_some(),
                expected
            );
        }
    }

    #[test]
    fn scope_growth_keeps_old_lists_isolated_and_rivals_complete() {
        let targets: Vec<_> = (0..20)
            .map(|i| target(1, false, false, false, Some((i % 2) as u64)))
            .collect();
        let outer = indexed((0..16).rev().collect(), &targets);
        let child = indexed((0..20).rev().collect(), &targets);
        let rivals = indexed(vec![18, 19, 16, 17], &targets);
        let call = Some(discriminant(&statement(1)));
        assert_eq!(
            outer.matching(call, Some(0), false).collect::<Vec<_>>(),
            vec![14, 12, 10, 8, 6, 4, 2, 0]
        );
        assert_eq!(
            child.matching(call, Some(0), false).collect::<Vec<_>>(),
            vec![18, 16, 14, 12, 10, 8, 6, 4, 2, 0]
        );
        assert_eq!(
            rivals.matching(call, Some(0), false).collect::<Vec<_>>(),
            vec![18, 16]
        );
        assert_eq!(
            outer.matching(call, Some(0), false).collect::<Vec<_>>(),
            vec![14, 12, 10, 8, 6, 4, 2, 0]
        );
    }
}
