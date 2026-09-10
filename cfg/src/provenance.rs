//! Optional, bounded lineage records. No record here is a close/ownership
//! certificate and no transform may treat storage ancestry as value equality.
use std::collections::{BTreeMap, BTreeSet};

use ast::{LocalRw, RcLocal, SourceBinding, Statement};
use petgraph::{stable_graph::NodeIndex, visit::EdgeRef};

use crate::function::Function;

pub const RECORD_LIMIT: usize = 50_000;

#[derive(Debug, Clone)]
pub struct Register {
    pub id: u64,
    pub slot: usize,
    pub kind: &'static str,
    pub source_bindings: Vec<SourceBinding>,
}

#[derive(Debug, Clone)]
pub struct LiftedStatement {
    pub block: usize,
    pub index: usize,
    /// Instruction PCs, excluding auxiliary payload slots. A deferred open
    /// result and NAMECALL/CAPTURE clusters can contribute several PCs.
    pub pcs: Vec<usize>,
    pub lines: Vec<u32>,
    pub kind: &'static str,
    pub read_registers: Vec<u64>,
    pub written_registers: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct Definition {
    pub id: u64,
    pub kind: &'static str,
    pub register: u64,
    pub block: usize,
    pub statement: Option<usize>,
    pub write_index: Option<usize>,
    pub dependencies: Vec<u64>,
    pub source_bindings: Vec<SourceBinding>,
}

#[derive(Debug, Clone)]
pub struct MapEvent {
    pub phase: &'static str,
    pub from: u64,
    pub to: u64,
}

#[derive(Debug, Clone)]
pub struct SelectResult {
    pub phase: &'static str,
    pub branch: usize,
    pub join: usize,
    pub binding: u64,
    pub condition_bindings: Vec<u64>,
    pub then_predecessor: usize,
    pub else_predecessor: usize,
    pub then_value: u64,
    pub else_value: u64,
}

#[derive(Debug, Clone)]
pub struct FunctionTrace {
    pub prototype: usize,
    pub instruction_count: usize,
    pub function_id: String,
    pub phase: &'static str,
    pub registers: BTreeMap<u64, Register>,
    pub lifted: BTreeMap<(usize, usize), LiftedStatement>,
    pub definitions: BTreeMap<u64, Definition>,
    pub maps: Vec<MapEvent>,
    pub selects: Vec<SelectResult>,
    pub pre_destruct_bindings: Vec<u64>,
    pub post_destruct_bindings: Vec<u64>,
    pub dropped_records: usize,
}

impl FunctionTrace {
    pub fn new(prototype: usize, function_id: String) -> Self {
        Self {
            prototype,
            instruction_count: 0,
            function_id,
            phase: "lifting",
            registers: BTreeMap::new(),
            lifted: BTreeMap::new(),
            definitions: BTreeMap::new(),
            maps: Vec::new(),
            selects: Vec::new(),
            pre_destruct_bindings: Vec::new(),
            post_destruct_bindings: Vec::new(),
            dropped_records: 0,
        }
    }

    fn room(&mut self) -> bool {
        if self.registers.len()
            + self.lifted.len()
            + self.definitions.len()
            + self.maps.len()
            + self.selects.len()
            >= RECORD_LIMIT
        {
            self.dropped_records += 1;
            false
        } else {
            true
        }
    }

    pub fn register(&mut self, local: &RcLocal, slot: usize, kind: &'static str) {
        if self.registers.contains_key(&local.stable_id()) {
            return;
        }
        if !self.room() {
            local.mark_lineage_incomplete();
            return;
        }
        if kind != "register" {
            local.record_definition_lineage();
        }
        self.registers.insert(
            local.stable_id(),
            Register {
                id: local.stable_id(),
                slot,
                kind,
                source_bindings: local.0.lock().2.clone(),
            },
        );
    }

    pub fn statement(&mut self, statement: LiftedStatement) {
        if self.room() {
            self.lifted
                .insert((statement.block, statement.index), statement);
        }
    }

    pub fn definition(
        &mut self,
        local: &RcLocal,
        register: &RcLocal,
        node: NodeIndex,
        site: Option<(usize, usize)>,
        dependencies: Vec<u64>,
    ) {
        if !self.room() {
            local.mark_lineage_incomplete();
            return;
        }
        local.record_definition_lineage();
        let source_bindings = local.0.lock().2.clone();
        let kind = if site.is_none() {
            "ssa_block_parameter"
        } else if source_bindings
            .iter()
            .any(|b| matches!(b.origin, ast::BindingOrigin::DebugLocal { .. }))
        {
            "recorded_source_definition"
        } else {
            "unclassified_ssa_definition"
        };
        self.definitions.insert(
            local.stable_id(),
            Definition {
                id: local.stable_id(),
                kind,
                register: register.stable_id(),
                block: node.index(),
                statement: site.map(|s| s.0),
                write_index: site.map(|s| s.1),
                dependencies,
                source_bindings,
            },
        );
    }

    pub fn local_map(&mut self, phase: &'static str, from: &RcLocal, to: &RcLocal) {
        if from != to && self.room() {
            self.maps.push(MapEvent {
                phase,
                from: from.stable_id(),
                to: to.stable_id(),
            });
        }
    }
}

pub fn statement_kind(statement: &Statement) -> &'static str {
    match statement {
        Statement::Assign(_) => "assignment",
        Statement::Call(_) => "call",
        Statement::MethodCall(_) => "method_call",
        Statement::If(_) => "branch",
        Statement::Return(_) => "return",
        Statement::Close(_) => "close",
        Statement::SetList(_) => "setlist",
        Statement::NumForInit(_) => "numeric_for_prep",
        Statement::NumForNext(_) => "numeric_for_step",
        Statement::GenericForInit(_) => "generic_for_prep",
        Statement::GenericForNext(_) => "generic_for_step",
        Statement::Comment(_) => "annotation",
        _ => "other",
    }
}

pub fn binding_ids(function: &Function) -> Vec<u64> {
    function
        .parameters
        .iter()
        .map(RcLocal::stable_id)
        .chain(function.blocks().flat_map(|(_, block)| {
            block
                .iter()
                .flat_map(|s| s.values())
                .map(RcLocal::stable_id)
        }))
        .chain(function.graph().edge_weights().flat_map(|edge| {
            edge.arguments.iter().flat_map(|(p, v)| {
                std::iter::once(p.stable_id())
                    .chain(v.values_read().into_iter().map(RcLocal::stable_id))
            })
        }))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Recognize a two-arm SSA join without changing its representation. Each arm
/// is empty (a direct edge) or one private block with a sole edge to the join.
/// This proves which branch supplies each scalar phi input, not purity,
/// totality, source spelling, or permission to evaluate either arm eagerly.
pub fn record_selects(function: &mut Function, phase: &'static str) {
    if function.provenance.is_none() {
        return;
    }
    let mut results = Vec::new();
    let mut dropped_results = 0;
    for branch in function.graph().node_indices() {
        let Some((yes, no)) = function.conditional_edges(branch) else {
            continue;
        };
        let successors = [yes.target(), no.target()];
        let join_of = |arm: NodeIndex| function.unconditional_edge(arm).map(|e| e.target());
        let mut joins = BTreeSet::from([successors[0].index(), successors[1].index()]);
        joins.extend(
            successors
                .iter()
                .filter_map(|&arm| join_of(arm))
                .map(|n| n.index()),
        );
        for join in joins.into_iter().map(NodeIndex::new) {
            if join == branch || function.predecessor_blocks(join).count() != 2 {
                continue;
            }
            let arm_predecessor = |arm: NodeIndex| {
                if arm == join {
                    return Some(branch);
                }
                if arm == branch || join_of(arm) != Some(join) {
                    return None;
                }
                let predecessors = function.predecessor_blocks(arm).collect::<Vec<_>>();
                (predecessors.as_slice() == [branch]).then_some(arm)
            };
            let (Some(then_pred), Some(else_pred)) = (
                arm_predecessor(successors[0]),
                arm_predecessor(successors[1]),
            ) else {
                continue;
            };
            if then_pred == else_pred {
                continue;
            }
            let edge_args = |pred| {
                function
                    .edges(pred)
                    .find(|edge| edge.target() == join)
                    .map(|edge| &edge.weight().arguments)
            };
            let (Some(then_args), Some(else_args)) = (edge_args(then_pred), edge_args(else_pred))
            else {
                continue;
            };
            let Some(Statement::If(condition)) = function.block(branch).and_then(|b| b.last())
            else {
                continue;
            };
            let conditions = condition
                .condition
                .values_read()
                .into_iter()
                .map(RcLocal::stable_id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            for (parameter, then_value) in then_args {
                let Some((_, else_value)) = else_args.iter().find(|(p, _)| p == parameter) else {
                    continue;
                };
                let (Some(then_value), Some(else_value)) =
                    (then_value.as_local(), else_value.as_local())
                else {
                    continue;
                };
                if then_value == else_value || then_value == parameter || else_value == parameter {
                    continue;
                }
                if results.len() >= RECORD_LIMIT {
                    dropped_results += 1;
                    continue;
                }
                results.push(SelectResult {
                    phase,
                    branch: branch.index(),
                    join: join.index(),
                    binding: parameter.stable_id(),
                    condition_bindings: conditions.clone(),
                    then_predecessor: then_pred.index(),
                    else_predecessor: else_pred.index(),
                    then_value: then_value.stable_id(),
                    else_value: else_value.stable_id(),
                });
            }
        }
    }
    let trace = function.provenance.as_mut().unwrap();
    trace.dropped_records += dropped_results;
    results.sort_by_key(|r| (r.branch, r.join, r.binding));
    for result in results {
        if trace.room() {
            trace.selects.push(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockEdge, BranchType};
    use ast::{Assign, Block, If, Literal, RValue, Return};

    fn local(name: &str) -> RcLocal {
        RcLocal::new(ast::Local::new(Some(name.into())))
    }
    fn edge(
        function: &mut Function,
        from: NodeIndex,
        to: NodeIndex,
        branch: BranchType,
        parameter: Option<&RcLocal>,
        value: Option<RValue>,
    ) {
        let mut edge = BlockEdge::new(branch);
        if let (Some(parameter), Some(value)) = (parameter, value) {
            edge.arguments.push((parameter.clone(), value));
        }
        function.graph_mut().add_edge(from, to, edge);
    }

    fn diamond(triangle: bool) -> (Function, RcLocal, RcLocal, RcLocal, RcLocal, [NodeIndex; 4]) {
        let mut function = Function::new(0);
        function.provenance = Some(Box::new(FunctionTrace::new(0, "root:p0".into())));
        let nodes = std::array::from_fn(|_| function.new_block());
        let [branch, yes, no, join] = nodes;
        function.set_entry(branch);
        let condition = local("condition");
        let primary = local("primary");
        let fallback = local("fallback");
        let selected = local("selected");
        function
            .block_mut(branch)
            .unwrap()
            .push(If::new(condition.clone().into(), Block::default(), Block::default()).into());
        edge(
            &mut function,
            branch,
            if triangle { join } else { yes },
            BranchType::Then,
            triangle.then_some(&selected),
            triangle.then_some(primary.clone().into()),
        );
        if !triangle {
            edge(
                &mut function,
                yes,
                join,
                BranchType::Unconditional,
                Some(&selected),
                Some(primary.clone().into()),
            );
        }
        edge(&mut function, branch, no, BranchType::Else, None, None);
        edge(
            &mut function,
            no,
            join,
            BranchType::Unconditional,
            Some(&selected),
            Some(fallback.clone().into()),
        );
        function
            .block_mut(join)
            .unwrap()
            .push(Return::new(vec![selected.clone().into()]).into());
        (function, condition, primary, fallback, selected, nodes)
    }

    #[test]
    fn diamond_and_triangle_keep_polarity_without_mutation() {
        for triangle in [false, true] {
            let (mut function, condition, primary, fallback, selected, _) = diamond(triangle);
            let before = binding_ids(&function);
            record_selects(&mut function, "test");
            assert_eq!(binding_ids(&function), before);
            let records = &function.provenance.as_ref().unwrap().selects;
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].condition_bindings, vec![condition.stable_id()]);
            assert_eq!(records[0].binding, selected.stable_id());
            assert_eq!(records[0].then_value, primary.stable_id());
            assert_eq!(records[0].else_value, fallback.stable_id());
        }
    }

    #[test]
    fn shared_arm_extra_predecessor_and_loop_phi_refuse() {
        for shape in 0..3 {
            let (mut function, _, _, _, selected, [branch, yes, _, join]) = diamond(false);
            if shape == 0 {
                let extra = function.new_block();
                edge(
                    &mut function,
                    extra,
                    yes,
                    BranchType::Unconditional,
                    None,
                    None,
                );
            } else if shape == 1 {
                let extra = function.new_block();
                edge(
                    &mut function,
                    extra,
                    join,
                    BranchType::Unconditional,
                    None,
                    None,
                );
            } else {
                let target = function.graph().find_edge(yes, join).unwrap();
                function
                    .graph_mut()
                    .edge_weight_mut(target)
                    .unwrap()
                    .arguments[0]
                    .1 = selected.into();
                edge(
                    &mut function,
                    join,
                    branch,
                    BranchType::Unconditional,
                    None,
                    None,
                );
            }
            record_selects(&mut function, "test");
            assert!(function.provenance.as_ref().unwrap().selects.is_empty());
        }
    }

    #[test]
    fn equal_or_nonlocal_phi_inputs_do_not_claim_a_select() {
        for nonlocal in [false, true] {
            let (mut function, _, primary, _, _, [_, _, no, join]) = diamond(false);
            let target = function.graph().find_edge(no, join).unwrap();
            function
                .graph_mut()
                .edge_weight_mut(target)
                .unwrap()
                .arguments[0]
                .1 = if nonlocal {
                Literal::Boolean(false).into()
            } else {
                primary.into()
            };
            record_selects(&mut function, "test");
            assert!(function.provenance.as_ref().unwrap().selects.is_empty());
        }
    }

    #[test]
    fn trace_does_not_promote_source_evidence() {
        let mut trace = FunctionTrace::new(0, "root:p0".into());
        let register = local("p");
        let version = local("v");
        trace.register(&register, 0, "parameter");
        trace.definition(
            &version,
            &register,
            NodeIndex::new(0),
            Some((0, 0)),
            vec![register.stable_id()],
        );
        assert!(!register.has_source_binding());
        assert!(!version.has_source_binding());
        assert!(register.source_bindings_compatible(&version));
    }

    #[test]
    fn register_reuse_keeps_separate_definition_and_source_interval_records() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);
        function.provenance = Some(Box::new(FunctionTrace::new(0, "root:p0".into())));
        let register = local("r");
        function
            .provenance
            .as_mut()
            .unwrap()
            .register(&register, 3, "register");
        for index in 0..2 {
            function.block_mut(entry).unwrap().push(
                Assign::new(
                    vec![register.clone().into()],
                    vec![Literal::Number(index as f64).into()],
                )
                .into(),
            );
            function.local_source_bindings.insert(
                (entry, index, 0),
                vec![SourceBinding {
                    name: if index == 0 { "first" } else { "second" }.into(),
                    origin: ast::BindingOrigin::DebugLocal {
                        prototype: 0,
                        register: 3,
                        start_pc: index * 2,
                        end_pc: index * 2 + 2,
                    },
                }],
            );
        }
        function
            .block_mut(entry)
            .unwrap()
            .push(Return::new(vec![register.clone().into()]).into());
        crate::ssa::construct(&mut function, &Vec::new());
        let definitions = function
            .provenance
            .as_ref()
            .unwrap()
            .definitions
            .values()
            .filter(|d| d.statement.is_some())
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 2);
        assert_ne!(definitions[0].id, definitions[1].id);
        assert_eq!(definitions[0].register, register.stable_id());
        assert_eq!(definitions[1].register, register.stable_id());
        assert_ne!(
            definitions[0].source_bindings,
            definitions[1].source_bindings
        );
    }

    #[test]
    fn lineage_union_is_bounded_order_independent_and_never_a_source_binding() {
        let a = local("a");
        let b = local("b");
        a.0.lock().3 = Some(Box::default());
        b.0.lock().3 = Some(Box::default());
        for id in 0..300 {
            a.0.lock().3.as_mut().unwrap().add(id);
        }
        for id in (0..300).rev() {
            b.0.lock().3.as_mut().unwrap().add(id);
        }
        assert_eq!(a.0.lock().3, b.0.lock().3);
        assert!(a.0.lock().3.as_ref().unwrap().incomplete);
        assert_eq!(
            a.0.lock().3.as_ref().unwrap().definitions.len(),
            ast::BindingLineage::LIMIT
        );
        assert!(!a.has_source_binding());
        let c = local("c");
        c.inherit_source_bindings(&a);
        assert_eq!(a.0.lock().3, c.0.lock().3);
        assert!(!c.has_source_binding());
    }

    #[test]
    fn exhausted_trace_marks_missing_lineage_incomplete() {
        let mut trace = FunctionTrace::new(0, "root:p0".into());
        trace.maps.resize(
            RECORD_LIMIT,
            MapEvent {
                phase: "test",
                from: 0,
                to: 1,
            },
        );
        let register = local("p");
        let version = local("v");
        trace.definition(&version, &register, NodeIndex::new(0), Some((0, 0)), vec![]);
        assert_eq!(trace.dropped_records, 1);
        assert!(trace.definitions.is_empty());
        assert!(version.0.lock().3.as_ref().unwrap().incomplete);
        assert!(!version.has_source_binding());
    }
}
