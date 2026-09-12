//! Bounded may-effect summaries. These are observations of the current tree,
//! never proof from a type annotation and never valid across arbitrary mutation.
use crate::{BinaryOperation, LocalRw, RValue, RcLocal, Traverse, UnaryOperation};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects(u16);

impl Effects {
    pub const MAY_THROW: Self = Self(1 << 0);
    pub const TABLE_READ: Self = Self(1 << 1);
    pub const TABLE_WRITE: Self = Self(1 << 2);
    pub const ALLOCATION: Self = Self(1 << 3);
    pub const CALL: Self = Self(1 << 4);
    pub const YIELD: Self = Self(1 << 5);
    pub const CAPTURE_READ: Self = Self(1 << 6);
    pub const CAPTURE_WRITE: Self = Self(1 << 7);
    pub const GLOBAL_READ: Self = Self(1 << 8);
    pub const GLOBAL_WRITE: Self = Self(1 << 9);
    pub const UNKNOWN: Self = Self(1 << 10);
    pub const CONDITIONAL: Self = Self(1 << 11);
    /// An unknown function/metamethod can access all externally visible state.
    pub const DYNAMIC_CALL: Self = Self((1 << 11) - 1);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Safe to discard an unused value under the project's ordinary-runtime
    /// contract (excluding resource exhaustion, native finalizers and debug hooks).
    /// Allocation is still reported separately; captured reads can be pure but
    /// cannot cross a callback that may change the cell.
    pub fn is_total_pure(self) -> bool {
        self.0 & !(Self::ALLOCATION.0 | Self::CAPTURE_READ.0 | Self::CONDITIONAL.0) == 0
    }

    pub fn has_order_dependency(self) -> bool {
        !self.is_total_pure() || self.contains(Self::CAPTURE_READ)
    }
}

/// Only effects at this node, before adding children. Closure bodies are not
/// executed by construction. A caller supplies proven cell identities for the
/// current SSA epoch; an empty predicate is not evidence that cells do not exist.
pub fn intrinsic(value: &RValue, is_capture: &impl Fn(&RcLocal) -> bool) -> Effects {
    use crate::{Literal, Select};
    match value {
        RValue::Local(local) if is_capture(local) => Effects::CAPTURE_READ,
        // These AST constants are emitted through math/vector environment
        // lookups. Motion must account for the source that will execute.
        RValue::Literal(Literal::Number(n))
            if !n.is_finite() || n.abs().to_bits() == std::f64::consts::PI.to_bits() => Effects::DYNAMIC_CALL,
        RValue::Literal(Literal::Vector(..) | Literal::VectorD(..)) => Effects::DYNAMIC_CALL,
        RValue::Local(_)
        | RValue::Literal(_)
        | RValue::VarArg(_)
        | RValue::Select(Select::VarArg(_)) => Effects::default(),
        RValue::Global(_) => Effects::DYNAMIC_CALL.union(Effects::GLOBAL_READ),
        RValue::Index(_) => Effects::DYNAMIC_CALL.union(Effects::TABLE_READ),
        RValue::Call(_)
        | RValue::MethodCall(_)
        | RValue::Select(Select::Call(_) | Select::MethodCall(_)) => Effects::DYNAMIC_CALL,
        RValue::Closure(closure) => {
            let mut effects = Effects::ALLOCATION;
            if closure.values_read().into_iter().any(is_capture) {
                effects = effects.union(Effects::CAPTURE_READ);
            }
            effects
        }
        RValue::Table(table) => {
            let invalid_key = table.0.iter().any(|(key, _)| {
                key.as_ref()
                    .is_some_and(|key| !crate::is_total_table_key(key))
            });
            if invalid_key {
                Effects::ALLOCATION.union(Effects::MAY_THROW)
            } else {
                Effects::ALLOCATION
            }
        }
        RValue::Unary(unary) if unary.operation == UnaryOperation::Not => Effects::default(),
        RValue::Unary(_) => Effects::DYNAMIC_CALL,
        RValue::Binary(binary) => match binary.operation {
            BinaryOperation::And | BinaryOperation::Or => Effects::CONDITIONAL,
            BinaryOperation::Equal | BinaryOperation::NotEqual
                if [&*binary.left, &*binary.right].into_iter().any(|value| {
                    matches!(
                        value,
                        RValue::Literal(
                            Literal::Nil
                                | Literal::Boolean(_)
                                | Literal::Number(_)
                                | Literal::String(_)
                        )
                    )
                }) =>
            {
                Effects::default()
            }
            _ => Effects::DYNAMIC_CALL,
        },
        RValue::IfExpression(_) => Effects::CONDITIONAL,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub effects: Effects,
    pub nodes: usize,
    pub exhausted: bool,
}

/// Merge possible effects, including skipped arms conservatively. A budget
/// failure sets UNKNOWN and every dynamic-call flag; it never implies purity.
/// The walk retains no AST/local owners and never enters closure bodies.
pub fn summarize(value: &RValue, is_capture: &impl Fn(&RcLocal) -> bool) -> Summary {
    fn walk(value: &RValue, capture: &impl Fn(&RcLocal) -> bool, depth: usize, out: &mut Summary) {
        let width = match value {
            RValue::Table(table) => table.0.len().saturating_mul(2),
            RValue::Call(call) | RValue::Select(crate::Select::Call(call)) => {
                call.arguments.len().saturating_add(1)
            }
            RValue::MethodCall(call) | RValue::Select(crate::Select::MethodCall(call)) => {
                call.arguments.len().saturating_add(1)
            }
            RValue::Closure(closure) => closure.upvalues.len(),
            _ => 0,
        };
        if out.nodes >= 8192 || depth >= 128 || width > 8192usize.saturating_sub(out.nodes) {
            out.exhausted = true;
            out.effects = out.effects.union(Effects::DYNAMIC_CALL);
            return;
        }
        out.nodes += 1;
        out.effects = out.effects.union(intrinsic(value, capture));
        for child in value.rvalues() {
            if out.exhausted {
                break;
            }
            walk(child, capture, depth + 1, out);
        }
    }
    let mut result = Summary {
        effects: Effects::default(),
        nodes: 0,
        exhausted: false,
    };
    walk(value, is_capture, 0, &mut result);
    result
}

/// A destination's captured read only conflicts with a candidate that can
/// write captured state. Two reads commute. Stop at the first possible call,
/// avoiding a complete summary for the common call/index/operator candidate.
pub fn may_write_capture(value: &RValue) -> bool {
    fn walk(value: &RValue, remaining: &mut usize, depth: usize) -> bool {
        if *remaining == 0 || depth >= 128 {
            return true;
        }
        *remaining -= 1;
        let children_width = match value {
            RValue::Table(table) => table.0.len().saturating_mul(2),
            // Capturing a cell/value does not write it or execute the body.
            RValue::Closure(_) => return false,
            _ => 0,
        };
        if children_width > *remaining {
            return true;
        }
        if intrinsic(value, &|_| false).contains(Effects::CAPTURE_WRITE) {
            return true;
        }
        value
            .rvalues()
            .into_iter()
            .any(|child| walk(child, remaining, depth + 1))
    }
    walk(value, &mut 8192, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Binary, Call, Index, Literal, Local, Table};

    #[test]
    fn cells_and_allocations_are_distinct_from_calls_and_errors() {
        let cell = RcLocal::new(Local::new(Some("cell".into())));
        let capture = |local: &RcLocal| local == &cell;
        let read = summarize(&RValue::Local(cell.clone()), &capture).effects;
        assert!(read.is_total_pure());
        assert!(read.has_order_dependency());
        assert!(!may_write_capture(&RValue::Local(cell.clone())));
        let table = summarize(&RValue::Table(Table::default()), &capture).effects;
        assert!(table.contains(Effects::ALLOCATION));
        assert!(table.is_total_pure());
        let indexed = Index::new(
            RValue::Local(cell.clone()),
            Literal::String(b"Value".to_vec()).into(),
        )
        .into();
        let effects = summarize(&indexed, &capture).effects;
        assert!(effects.contains(Effects::TABLE_READ));
        assert!(effects.contains(Effects::CALL));
        assert!(effects.contains(Effects::CAPTURE_WRITE));
        assert!(effects.contains(Effects::YIELD));
        assert!(may_write_capture(&indexed));
        let dynamic_key = RValue::Table(Table(vec![(
            Some(RValue::Local(cell.clone())),
            Literal::Nil.into(),
        )]));
        let effects = summarize(&dynamic_key, &capture).effects;
        assert!(effects.contains(Effects::MAY_THROW));
        assert!(!effects.contains(Effects::CALL));
        assert!(!may_write_capture(&dynamic_key));
    }

    #[test]
    fn primitive_comparison_does_not_hide_child_effects_or_trust_hints() {
        let value = Binary::new(
            Call::new(Literal::Nil.into(), vec![]).into(),
            Literal::Nil.into(),
            BinaryOperation::Equal,
        )
        .into();
        assert!(summarize(&value, &|_| false)
            .effects
            .contains(Effects::CALL));
        let value = Binary::new(
            Literal::Number(1.0).into(),
            Literal::Nil.into(),
            BinaryOperation::Equal,
        )
        .into();
        assert!(summarize(&value, &|_| false).effects.is_total_pure());
    }

    #[test]
    fn budget_exhaustion_keeps_unknown_barrier() {
        let mut value = Literal::Nil.into();
        for _ in 0..140 {
            value = crate::Unary::new(value, UnaryOperation::Not).into();
        }
        let summary = summarize(&value, &|_| false);
        assert!(summary.exhausted);
        assert!(summary.effects.contains(Effects::UNKNOWN));
        assert!(!summary.effects.is_total_pure());
    }
}
