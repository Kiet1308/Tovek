//! Exact aggregate of RcLocal::source_bindings_compatible for a class.
//! Metadata is immutable between lift_params and apply_local_map. Empty debug
//! evidence is a wildcard; distinct nonempty sequences conflict. Role conflicts
//! exclude identity pairs, including a local carrying both role flags.
use ast::{BindingOrigin, RcLocal, SourceBinding};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Identities {
    #[default]
    Empty,
    One(u64),
    Many,
}

impl Identities {
    fn add(&mut self, local: &RcLocal) {
        self.merge(Self::One(local.stable_id()));
    }

    fn merge(&mut self, other: Self) {
        match self {
            Self::Empty => *self = other,
            Self::One(previous) if matches!(other, Self::Many)
                || matches!(other, Self::One(next) if *previous != next) => *self = Self::Many,
            _ => {}
        }
    }

    fn has_distinct_pair(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, _) | (_, Self::Empty) => false,
            (Self::One(a), Self::One(b)) => a != b,
            _ => true,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
enum DebugBindings {
    #[default]
    Empty,
    Uniform(Vec<SourceBinding>),
    Mixed,
}

impl DebugBindings {
    fn merge(&mut self, other: Self) {
        match self {
            Self::Empty => *self = other,
            Self::Uniform(previous) if matches!(&other, Self::Mixed)
                || matches!(&other, Self::Uniform(next) if previous.as_slice() != next.as_slice()) => *self = Self::Mixed,
            _ => {}
        }
    }

    fn add(&mut self, bindings: &[SourceBinding]) {
        if matches!(self, Self::Mixed) { return; }
        let mut recorded = bindings.iter().filter(|binding|
            matches!(binding.origin, BindingOrigin::DebugLocal { .. })).peekable();
        if recorded.peek().is_none() { return; }
        match self {
            Self::Empty => *self = Self::Uniform(recorded.cloned().collect()),
            Self::Uniform(previous) if !previous.iter().eq(recorded) => *self = Self::Mixed,
            _ => {}
        }
    }

    fn compatible(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, _) | (_, Self::Empty) => true,
            (Self::Uniform(a), Self::Uniform(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct BindingSummary {
    parameters: Identities,
    separate: Identities,
    debug: DebugBindings,
}

impl BindingSummary {
    pub(super) fn from_locals<'a>(locals: impl Iterator<Item = &'a RcLocal>) -> Self {
        let mut result = Self::default();
        for local in locals {
            result.add_local(local);
        }
        result
    }

    pub(super) fn add_local(&mut self, local: &RcLocal) {
        #[cfg(test)]
        SUMMARY_LOCAL_VISITS.with(|visits| visits.set(visits.get() + 1));
        let metadata = local.0.lock();
        if metadata.4.parameter { self.parameters.add(local); }
        if metadata.4.separate_from_parameter { self.separate.add(local); }
        self.debug.add(&metadata.2);
    }

    /// Exact union for immutable metadata. This joins aggregate states rather
    /// than rereading either class's members; repeated identities remain One.
    pub(super) fn merge(&mut self, other: Self) {
        self.parameters.merge(other.parameters);
        self.separate.merge(other.separate);
        self.debug.merge(other.debug);
    }

    pub(super) fn compatible(&self, other: &Self) -> bool {
        !self.parameters.has_distinct_pair(&other.separate)
            && !other.parameters.has_distinct_pair(&self.separate)
            && self.debug.compatible(&other.debug)
    }
}

#[cfg(test)]
thread_local! { pub(super) static SUMMARY_LOCAL_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;

    fn local(roles: u8, binding: usize) -> RcLocal {
        let local = RcLocal::default();
        {
            let mut meta = local.0.lock();
            meta.4.parameter = roles & 1 != 0;
            meta.4.separate_from_parameter = roles & 2 != 0;
            // Upvalue/function evidence is intentionally not a DebugLocal.
            meta.add_source_binding(SourceBinding {
                origin: BindingOrigin::DebugUpvalue { prototype: 1, slot: binding },
                name: "capture".into(),
            });
            if binding != 0 {
                meta.add_source_binding(SourceBinding {
                    origin: BindingOrigin::DebugLocal {
                        prototype: 1, register: 0, start_pc: binding, end_pc: binding + 1,
                    },
                    name: "sameSpelling".into(),
                });
            }
        }
        local
    }

    #[test]
    fn aggregate_matches_pairwise_contract_including_overlapping_classes() {
        let pool: Vec<_> = (0..4).flat_map(|role| (0..3).map(move |binding| local(role, binding))).collect();
        // Every subset of this mixed pool, paired with a deterministic sampling
        // of other subsets. Includes shared identities and mandatory classes
        // whose own members are incompatible.
        for mask in 0usize..(1 << pool.len()) {
            let left: Vec<_> = pool.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, l)| l).collect();
            let summary = BindingSummary::from_locals(left.iter().copied());
            for other_mask in [0, mask, !mask, mask.wrapping_mul(2654435761), 1, 1 << 9] {
                let right: Vec<_> = pool.iter().enumerate().filter(|(i, _)| other_mask & (1 << i) != 0).map(|(_, l)| l).collect();
                let expected = left.iter().all(|a| right.iter().all(|b| a.source_bindings_compatible(b)));
                assert_eq!(summary.compatible(&BindingSummary::from_locals(right.iter().copied())), expected,
                    "masks {mask:x} / {other_mask:x}");
            }
        }
    }

    #[test]
    fn multiple_debug_origins_and_order_remain_significant() {
        let a = local(0, 1);
        let b = local(0, 2);
        a.inherit_source_bindings(&b);
        let copy = RcLocal::default();
        copy.inherit_source_bindings(&a);
        let unknown = local(0, 0);
        for left in [&a, &b, &copy, &unknown] {
            for right in [&a, &b, &copy, &unknown] {
                assert_eq!(BindingSummary::from_locals([left].into_iter()).compatible(
                    &BindingSummary::from_locals([right].into_iter())), left.source_bindings_compatible(right));
            }
        }
    }

    #[test]
    fn incremental_union_matches_full_census_and_pairwise_contract() {
        let pool: Vec<_> = (0..4).flat_map(|role| (0..3).map(move |binding| local(role, binding))).collect();
        for seed in 0..512u64 {
            let mut state = seed + 1;
            let mut next = || { state = state.wrapping_mul(6364136223846793005).wrapping_add(1); (state >> 32) as usize };
            // Duplicate identities deliberately occur within and across groups.
            let left: Vec<_> = (0..next() % 9).map(|_| &pool[next() % pool.len()]).collect();
            let right: Vec<_> = (0..next() % 9).map(|_| &pool[next() % pool.len()]).collect();
            let third: Vec<_> = (0..next() % 9).map(|_| &pool[next() % pool.len()]).collect();
            let mut actual = BindingSummary::from_locals(left.iter().copied());
            actual.merge(BindingSummary::from_locals(right.iter().copied()));
            let mut all: Vec<_> = left.iter().chain(&right).copied().collect();
            assert_eq!(actual, BindingSummary::from_locals(all.iter().copied()), "seed {seed}");
            let mut reverse = BindingSummary::from_locals(right.iter().copied());
            reverse.merge(BindingSummary::from_locals(left.iter().copied()));
            assert_eq!(actual, reverse, "union order, seed {seed}");
            for probe in &pool {
                let expected = all.iter().all(|local| local.source_bindings_compatible(probe));
                assert_eq!(actual.compatible(&BindingSummary::from_locals([probe].into_iter())), expected);
            }
            for local in &third { actual.add_local(local); }
            all.extend(third);
            assert_eq!(actual, BindingSummary::from_locals(all.iter().copied()));
            let mut grouped = BindingSummary::from_locals(right.iter().copied());
            grouped.merge(BindingSummary::from_locals(all[left.len() + right.len()..].iter().copied()));
            let mut associative = BindingSummary::from_locals(left.iter().copied());
            associative.merge(grouped);
            assert_eq!(actual, associative, "associativity, seed {seed}");
        }
    }

    #[test]
    fn summary_union_retains_identity_exception_wildcards_and_debug_order() {
        let both = local(3, 1);
        let mut duplicate = BindingSummary::from_locals([&both].into_iter());
        duplicate.merge(BindingSummary::from_locals([&both].into_iter()));
        assert!(duplicate.compatible(&duplicate), "same identity carrying both roles is allowed");
        let unknown = local(0, 0);
        duplicate.merge(BindingSummary::from_locals([&unknown].into_iter()));
        assert!(duplicate.compatible(&duplicate), "non-debug evidence is a wildcard");
        let other_both = local(3, 1);
        duplicate.merge(BindingSummary::from_locals([&other_both].into_iter()));
        assert!(!duplicate.compatible(&duplicate), "distinct role-bearing identities conflict");

        let ordered = local(0, 1);
        let second = local(0, 2);
        ordered.inherit_source_bindings(&second);
        let reversed = RcLocal::default();
        reversed.0.lock().2 = ordered.0.lock().2.iter().rev().cloned().collect();
        let mut summary = BindingSummary::from_locals([&ordered].into_iter());
        summary.merge(BindingSummary::from_locals([&reversed].into_iter()));
        assert_eq!(summary, BindingSummary::from_locals([&ordered, &reversed].into_iter()));
        assert!(!summary.compatible(&BindingSummary::from_locals([&ordered].into_iter())));
        assert!(summary.compatible(&BindingSummary::from_locals([&unknown].into_iter())));
    }
}
