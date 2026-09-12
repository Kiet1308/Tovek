//! Diagnostic node ancestry that survives moves and real AST copies.
//! No local owners, source spelling, effect facts or lifetime certificates.
use std::{fmt, sync::Arc};
use crate::{RValue, Statement, Select};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Input {
    pub function: Arc<str>,
    pub block: usize,
    pub statement: usize,
    pub value: Option<usize>,
}

/// Equality deliberately ignores this channel. A retained node may have been
/// rewritten: an origin is ancestry, never a claim of unchanged value/effect.
pub const INPUT_LIMIT: usize = 16;
/// Exhaustive lifecycle policy for the production pass families. These are
/// metadata contracts; none authorizes a transformation or transfers a proof.
pub const PASS_CONTRACTS: &[(&str, &str)] = &[
    ("lifting_and_ssa_construction", "seed immutable statement/value occurrences and register/debug intervals before positional keys expire"),
    ("ssa_copy_propagation", "preserve node tags; union binding ancestry; retain distinct source identities"),
    ("ssa_inline", "mark only installed value as inlined; record numeric producer/consumer events; reductions union parent/child ancestry"),
    ("ssa_phi_destruction", "preserve operand tags; record parameter and edge transport maps; fresh transfer statements have no invented input origin"),
    ("ssa_close_and_upvalue_maps", "diagnostic ancestry may union; close certificates independently intersect; names never transfer ownership"),
    ("structuring_and_fallback", "moved values retain tags; actual AST copies mark cloned; fresh control syntax remains unknown unless explicitly attributed"),
    ("deep_clone", "copy occurrence ancestry and mark AST copying; discarded speculative copies leave no output record"),
    ("reduce_and_reduce_condition", "union retained parent/child origins up to budget; detached scalar leaves invalidate occurrence identity"),
    ("factor_common_tails", "retained representatives keep their tags; deleted alternatives are not invented as equivalent input producers"),
    ("statement_expression_arithmetic_deinline", "explicit call construction records synthesis producer; preserved/copied arguments keep their own input ancestry"),
    ("synthesize_terminal_helpers", "explicit reconstructed calls have synthesis origin; new helper bodies do not inherit caller-PC claims"),
    ("inline_temps_and_ui_reconstruction", "installed root records inline; moved child nodes retain tags; reconstructed containers without tags remain unknown"),
    ("coalesce_locals_and_copy_cleanup", "preserve syntax tags and union storage ancestry; source-binding compatibility remains a separate gate"),
    ("conditional_expressions_and_table_rebuild", "retained child tags survive; newly assembled parent syntax has no fabricated input identity"),
    ("recover_methods_and_rehoist_constants", "retained operands keep ancestry; new wrappers start unknown; names do not create origin links"),
    ("materialize_value_captures_and_call_receivers", "fresh declarations start unknown; copied values retain tags; existing capture/evaluation gates remain authoritative"),
    ("eliminate_nil_and_recover_connection", "deleted nodes disappear; retained nodes keep ancestry; replacement locals receive no proof from names"),
    ("rebalance_and_cleanup_final", "moves/copies preserve ancestry; new expression structure is unknown; no structural similarity becomes origin proof"),
    ("canonicalize_and_normalize_conditions", "retained children keep ancestry; replaced polarity/operator roots require merge or remain unknown"),
    ("guard_continue_cleanup_returns_flatten_guards", "retained/moved nodes keep ancestry; fresh control flow remains unknown; eliminated paths are not emitted"),
    ("branch_constructors_and_lower_conditionals", "fresh locals require committed producer ledger; retained/copied values carry ancestry; no inherited ownership certificate"),
    ("name_locals_and_refine_names", "source identities and node origins are independent of spelling; storage splits retain only explicitly copied ancestry"),
    ("formatter", "record actual emitted spans; metadata snapshots never mark cloning; previews add no occurrences; opaque or unattributed regions stay unknown"),
];
#[derive(Clone, Default)]
pub struct Data {
    pub inputs: Vec<Arc<Input>>,
    pub inlined: bool,
    pub cloned: bool,
    pub synthesized: Option<&'static str>,
    pub incomplete: bool,
}
#[derive(Default)]
pub struct Origin(pub Option<Box<Data>>);
impl Origin {
    /// Copy metadata into a report without claiming a new AST occurrence.
    pub fn snapshot(&self) -> Self { Self(self.0.clone()) }
    pub fn input(input: Input) -> Self {
        Self(Some(Box::new(Data { inputs: vec![Arc::new(input)], ..Default::default() })))
    }
    pub fn synthesized(pass: &'static str) -> Self {
        Self(Some(Box::new(Data { synthesized: Some(pass), ..Default::default() })))
    }
    pub fn merge(&mut self, other: &Self) {
        let Some(other_data) = &other.0 else { return; };
        let Some(data) = &mut self.0 else { *self = other.snapshot(); return; };
        let other = other_data;
        data.inlined |= other.inlined;
        data.cloned |= other.cloned;
        data.incomplete |= other.incomplete;
        data.inputs.extend(other.inputs.iter().cloned());
        data.inputs.sort();
        data.inputs.dedup();
        data.incomplete |= data.inputs.len() > INPUT_LIMIT;
        data.inputs.truncate(INPUT_LIMIT);
        // Conflicting synthesis producers remain unknown, never relabeled.
        if data.synthesized != other.synthesized { data.synthesized = None; }
    }
}
impl Clone for Origin {
    fn clone(&self) -> Self {
        let mut copy = self.snapshot();
        if let Some(data) = &mut copy.0 { data.cloned = true; }
        copy
    }
}
impl PartialEq for Origin { fn eq(&self, _: &Self) -> bool { true } }
impl Eq for Origin {}
impl fmt::Debug for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Origin") }
}

// Keep semantic debug fingerprints exactly as they were before instrumentation.
macro_rules! semantic_debug {
    ($name:ident; $($field:ident),+ $(,)?) => {
        impl std::fmt::Debug for crate::$name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name))$(.field(stringify!($field), &self.$field))+.finish()
            }
        }
    };
}
pub(crate) use semantic_debug;

pub fn value(value: &RValue) -> Option<&Origin> {
    match value {
        RValue::Call(v) => Some(&v.node_origin),
        RValue::MethodCall(v) => Some(&v.node_origin),
        RValue::Binary(v) => Some(&v.node_origin),
        RValue::Unary(v) => Some(&v.node_origin),
        RValue::Index(v) => Some(&v.node_origin),
        RValue::IfExpression(v) => Some(&v.node_origin),
        RValue::Closure(v) => Some(&v.node_origin),
        RValue::Table(v) => Some(&v.1),
        RValue::Select(Select::Call(v)) => Some(&v.node_origin),
        RValue::Select(Select::MethodCall(v)) => Some(&v.node_origin),
        _ => None,
    }
}
pub fn value_mut(value: &mut RValue) -> Option<&mut Origin> {
    match value {
        RValue::Call(v) => Some(&mut v.node_origin),
        RValue::MethodCall(v) => Some(&mut v.node_origin),
        RValue::Binary(v) => Some(&mut v.node_origin),
        RValue::Unary(v) => Some(&mut v.node_origin),
        RValue::Index(v) => Some(&mut v.node_origin),
        RValue::IfExpression(v) => Some(&mut v.node_origin),
        RValue::Closure(v) => Some(&mut v.node_origin),
        RValue::Table(v) => Some(&mut v.1),
        RValue::Select(Select::Call(v)) => Some(&mut v.node_origin),
        RValue::Select(Select::MethodCall(v)) => Some(&mut v.node_origin),
        _ => None,
    }
}
pub fn statement(item: &Statement) -> Option<&Origin> {
    match item {
        Statement::Assign(v) => Some(&v.node_origin),
        Statement::Return(v) => Some(&v.node_origin),
        Statement::If(v) => Some(&v.node_origin),
        Statement::SetList(v) => Some(&v.node_origin),
        Statement::Call(v) => Some(&v.node_origin),
        Statement::MethodCall(v) => Some(&v.node_origin),
        _ => None,
    }
}
pub fn statement_mut(item: &mut Statement) -> Option<&mut Origin> {
    match item {
        Statement::Assign(v) => Some(&mut v.node_origin),
        Statement::Return(v) => Some(&mut v.node_origin),
        Statement::If(v) => Some(&mut v.node_origin),
        Statement::SetList(v) => Some(&mut v.node_origin),
        Statement::Call(v) => Some(&mut v.node_origin),
        Statement::MethodCall(v) => Some(&mut v.node_origin),
        _ => None,
    }
}

/// Mark only the expression actually installed by a committed substitution.
/// Descendant tags travel with it; failed probes never touch this flag.
pub fn inlined(value: &mut RValue) {
    if let Some(Origin(Some(data))) = value_mut(value) { data.inlined = true; }
}

/// A rewritten parent and an already attributed child can both contribute.
/// Scalar leaves cannot carry a tag; their precise occurrence is invalidated.
pub fn inherit(result: &mut RValue, parent: Option<&Origin>) {
    if let (Some(target), Some(source)) = (value_mut(result), parent) { target.merge(source); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Binary, BinaryOperation, Block, Call, Global, Literal, RcLocal, Reduce, Return};
    fn input(value: usize) -> Origin {
        Origin::input(Input { function: "root:p0".into(), block: 2, statement: 3, value: Some(value) })
    }
    #[test]
    fn metadata_does_not_change_equality_debug_or_local_ownership() {
        let local = RcLocal::default();
        let mut value: RValue = Call::new(local.clone().into(), vec![]).into();
        let baseline = value.clone();
        let owners = triomphe::Arc::strong_count(&local.0.0);
        *value_mut(&mut value).unwrap() = input(0);
        inlined(&mut value);
        assert_eq!(value, baseline);
        assert_eq!(format!("{value:?}"), format!("{baseline:?}"));
        assert_eq!(value.to_string(), baseline.to_string());
        assert_eq!(triomphe::Arc::strong_count(&local.0.0), owners);
        assert!(super::value(&baseline).unwrap().0.is_none());
    }
    #[test]
    fn deep_clone_and_inline_are_reported_without_marking_emission_as_a_clone() {
        let mut call = Call::new(Global::from("effect").into(), vec![]);
        call.node_origin = input(0);
        let mut result: RValue = call.into();
        inlined(&mut result);
        let body = Block(vec![Return::new(vec![result]).into()]);
        let copy = crate::simplify_gotos::deep_clone_block(&body);
        let (_, _, map) = crate::formatter::format_with_emission_map_options(
            &copy, Default::default(), true, false).unwrap();
        let node = map.regions.iter().find(|r| r.kind == "call").unwrap().origin.0.as_ref().unwrap();
        assert!(node.inlined && node.cloned);
        let (_, _, original) = crate::formatter::format_with_emission_map_options(
            &body, Default::default(), true, false).unwrap();
        assert!(!original.regions.iter().find(|r| r.kind == "call").unwrap().origin.0.as_ref().unwrap().cloned);
    }
    #[test]
    fn reduction_merges_actual_parent_and_child_ancestry_and_invalidates_detached_literals() {
        let mut call = Call::new(Global::from("effect").into(), vec![]);
        call.node_origin = input(1);
        let mut binary = Binary::new(Literal::Boolean(true).into(), call.into(), BinaryOperation::And);
        binary.node_origin = input(0);
        let reduced = RValue::Binary(binary).reduce();
        let data = value(&reduced).unwrap().0.as_ref().unwrap();
        assert_eq!(data.inputs.iter().map(|r| r.value).collect::<Vec<_>>(), vec![Some(0), Some(1)]);
        let mut binary = Binary::new(Literal::Boolean(false).into(), reduced, BinaryOperation::And);
        binary.node_origin = input(2);
        let reduced = RValue::Binary(binary).reduce();
        assert!(matches!(reduced, RValue::Literal(Literal::Boolean(false))));
        assert!(value(&reduced).is_none());
    }
    #[test]
    fn bounded_union_is_deterministic_and_never_transfers_a_close_certificate() {
        let mut left = Origin::default();
        let mut right = Origin::default();
        for id in 0..INPUT_LIMIT + 3 { left.merge(&input(id)); }
        for id in (0..INPUT_LIMIT + 3).rev() { right.merge(&input(id)); }
        assert_eq!(left.0.as_ref().unwrap().inputs, right.0.as_ref().unwrap().inputs);
        assert!(left.0.as_ref().unwrap().incomplete && right.0.as_ref().unwrap().incomplete);
        let fresh = RcLocal::default();
        let mut assign = Assign::new(vec![fresh.clone().into()], vec![Literal::Nil.into()]);
        assign.node_origin = left;
        assert!(!fresh.has_source_binding());
        assert!(fresh.0.lock().3.is_none());
    }
    #[test]
    fn named_closures_have_one_real_region_even_without_a_child_identity() {
        let mut closure = crate::Closure {
            node_origin: input(0),
            function: by_address::ByAddress(triomphe::Arc::new(parking_lot::Mutex::new(crate::Function::default()))),
            upvalues: vec![],
        };
        let captured = RcLocal::new(crate::Local::new(Some("cell".into())));
        closure.upvalues.push(crate::Upvalue::Ref(captured.clone()));
        let name = RcLocal::new(crate::Local::new(Some("helper".into())));
        let mut assignment = Assign::new(vec![name.into()], vec![closure.into()]);
        assignment.prefix = true;
        let (source, _, map) = crate::formatter::format_with_emission_map_options(
            &Block(vec![assignment.into()]), Default::default(), true, false).unwrap();
        let regions = map.regions.iter().filter(|r| r.kind == "closure").collect::<Vec<_>>();
        assert_eq!(regions.len(), 1);
        let region = regions[0];
        assert!(source[region.span.start.byte_offset..region.span.end.byte_offset].contains("function helper"));
        assert_eq!(region.bindings, vec![captured.stable_id()]);
        assert!(!region.origin.0.as_ref().unwrap().cloned);
    }
}
