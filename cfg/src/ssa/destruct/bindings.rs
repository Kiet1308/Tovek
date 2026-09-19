//! Exact aggregate of RcLocal::source_bindings_compatible for a class.
//! Metadata is immutable between lift_params and apply_local_map. Empty debug
//! evidence is a wildcard; distinct nonempty sequences conflict. Role conflicts
//! exclude identity pairs, including a local carrying both role flags.
use ast::{BindingOrigin, RcLocal, SourceBinding};

#[derive(Default)]
enum Identities {
    #[default]
    Empty,
    One(RcLocal),
    Many,
}

impl Identities {
    fn add(&mut self, local: &RcLocal) {
        match self {
            Self::Empty => *self = Self::One(local.clone()),
            Self::One(previous) if previous != local => *self = Self::Many,
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

#[derive(Default)]
enum DebugBindings {
    #[default]
    Empty,
    Uniform(Vec<SourceBinding>),
    Mixed,
}

impl DebugBindings {
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

#[derive(Default)]
pub(super) struct BindingSummary {
    parameters: Identities,
    separate: Identities,
    debug: DebugBindings,
}

impl BindingSummary {
    pub(super) fn from_locals<'a>(locals: impl Iterator<Item = &'a RcLocal>) -> Self {
        let mut result = Self::default();
        for local in locals {
            let metadata = local.0.lock();
            if metadata.4.parameter { result.parameters.add(local); }
            if metadata.4.separate_from_parameter { result.separate.add(local); }
            result.debug.add(&metadata.2);
        }
        result
    }

    pub(super) fn compatible(&self, other: &Self) -> bool {
        !self.parameters.has_distinct_pair(&other.separate)
            && !other.parameters.has_distinct_pair(&self.separate)
            && self.debug.compatible(&other.debug)
    }
}

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
}
