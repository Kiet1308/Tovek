//! Retain the lifetime evidence that would otherwise disappear with `Close`.
//!
//! This analysis runs on the original statement order after SSA renaming and
//! before inlining/CFG simplification. Close operands still name VM registers;
//! `old_locals` relates every SSA use to those registers. Certificates describe
//! capture sites, not just result definitions, so local coalescing cannot turn
//! an unrelated, unclosed capture into a proven iteration cell.

use ast::{ForId, ForOrigin, LocalRw, RcLocal, Statement, Upvalue};
use petgraph::{stable_graph::NodeIndex, visit::EdgeRef};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::function::Function;

type CaptureSite = (NodeIndex, usize, RcLocal);

fn ref_captures(statement: &Statement) -> impl Iterator<Item = &RcLocal> {
    // NEWCLOSURE is still a separate assignment at this point. Its child
    // function is a different CFG; only the parent's capture operands belong
    // to this lifetime analysis.
    statement
        .as_assign()
        .into_iter()
        .flat_map(|assign| assign.right.iter())
        .filter_map(|value| value.as_closure())
        .flat_map(|closure| closure.upvalues.iter())
        .filter_map(|upvalue| match upvalue {
            Upvalue::Ref(local) => Some(local),
            Upvalue::Copy(_) => None,
        })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CellState {
    Uncaptured,
    Open,
    Closed,
}

fn prove_result(
    function: &Function,
    old_locals: &FxHashMap<RcLocal, RcLocal>,
    header: NodeIndex,
    origin: ForOrigin,
    register: &RcLocal,
) -> Option<FxHashSet<CaptureSite>> {
    let (body_edge, _) = function.conditional_edges(header)?;
    let body_entry = body_edge.target();
    if origin.body_pc >= origin.step_pc
        || function.block_pc_range(body_entry)?.start != origin.body_pc
        || function.block_pc_range(header)?.start != origin.step_pc
    {
        return None;
    }
    let mut work = vec![(body_entry, CellState::Uncaptured)];
    let mut visited = FxHashSet::default();
    let mut captures = FxHashSet::default();
    while let Some((node, mut state)) = work.pop() {
        if !visited.insert((node, state)) {
            continue;
        }
        let range = function.block_pc_range(node)?;
        // Leaving the original bytecode body ends the source iteration, on a
        // backedge/continue as well as break. The VM must already have closed
        // every cell captured on that path. RETURN is checked below because it
        // closes all open cells itself.
        if node == header || range.start < origin.body_pc || range.start >= origin.step_pc {
            if state == CellState::Open {
                return None;
            }
            continue;
        }
        if range.end >= origin.step_pc {
            return None;
        }
        let mut returned = false;
        for (index, statement) in function.block(node)?.iter().enumerate() {
            if let Statement::Close(close) = statement {
                if state == CellState::Open && close.locals.contains(register) {
                    state = CellState::Closed;
                }
                continue;
            }
            if state == CellState::Closed
                && statement
                    .values()
                    .iter()
                    .any(|local| old_locals.get(*local) == Some(register))
            {
                // Closing early and then using/writing/recapturing the VM
                // register cannot be represented by one source loop binding.
                return None;
            }
            for local in ref_captures(statement) {
                if old_locals.get(local) == Some(register) {
                    captures.insert((node, index, local.clone()));
                    state = CellState::Open;
                }
            }
            if matches!(statement, Statement::Return(_)) {
                returned = true;
                break;
            }
            // The lifter has not built nested statement blocks yet. Refuse a
            // different input dialect rather than silently miss nested uses.
            if matches!(statement, Statement::If(branch)
                if !branch.then_block.lock().is_empty() || !branch.else_block.lock().is_empty())
                || matches!(
                    statement,
                    Statement::While(_)
                        | Statement::Repeat(_)
                        | Statement::GenericFor(_)
                        | Statement::NumericFor(_)
                )
            {
                return None;
            }
        }
        if returned {
            continue;
        }
        let successors = function.successor_blocks(node).collect::<Vec<_>>();
        if successors.is_empty() {
            return None;
        }
        // SSA edge arguments have no original VM operation. All original
        // reads/writes/captures, including CLOSEUPVALS, were checked above.
        work.extend(successors.into_iter().map(|target| (target, state)));
    }
    Some(captures)
}

pub(super) fn record(function: &mut Function, old_locals: &FxHashMap<RcLocal, RcLocal>) {
    function.iteration_capture_proofs.clear();
    function.iteration_capture_obligations.clear();
    let mut sites: FxHashMap<CaptureSite, FxHashSet<ForId>> = function
        .blocks()
        .flat_map(|(node, block)| {
            block
                .iter()
                .enumerate()
                .flat_map(move |(index, statement)| {
                    ref_captures(statement)
                        .map(move |local| ((node, index, local.clone()), FxHashSet::default()))
                })
        })
        .collect();
    if sites.is_empty() {
        return;
    }
    let mut obligations: FxHashMap<RcLocal, FxHashSet<ForId>> = FxHashMap::default();
    for (header, block) in function.blocks() {
        let Some(next) = block.last().and_then(|s| s.as_generic_for_next()) else {
            continue;
        };
        let Some(origin) = next.origin() else {
            continue;
        };
        for result in next.res_locals.iter().filter_map(|value| value.as_local()) {
            let Some(register) = old_locals.get(result) else {
                continue;
            };
            let result_captures = sites
                .keys()
                .filter(|(node, _, local)| {
                    old_locals.get(local) == Some(register)
                        && function.block_pc_range(*node).is_some_and(|range| {
                            range.start >= origin.body_pc && range.end < origin.step_pc
                        })
                })
                .collect::<Vec<_>>();
            if result_captures.is_empty() {
                continue;
            }
            for (_, _, local) in result_captures {
                obligations
                    .entry(local.clone())
                    .or_default()
                    .insert(origin.id());
            }
            if let Some(captures) = prove_result(function, old_locals, header, origin, register) {
                for site in captures {
                    sites.get_mut(&site).unwrap().insert(origin.id());
                }
            }
        }
    }
    function.iteration_capture_obligations = obligations;
    for ((_, _, local), proofs) in sites {
        merge(&mut function.iteration_capture_proofs, local, proofs);
    }
}

fn merge(map: &mut FxHashMap<RcLocal, FxHashSet<ForId>>, local: RcLocal, proofs: FxHashSet<ForId>) {
    map.entry(local)
        .and_modify(|existing| existing.retain(|id| proofs.contains(id)))
        .or_insert(proofs);
}

pub(super) fn apply_local_map(function: &mut Function, local_map: &FxHashMap<RcLocal, RcLocal>) {
    if local_map.is_empty() || function.iteration_capture_proofs.is_empty() {
        return;
    }
    for (mut local, proofs) in std::mem::take(&mut function.iteration_capture_proofs) {
        let mut seen = FxHashSet::default();
        while let Some(next) = local_map.get(&local) {
            if !seen.insert(local.clone()) {
                // Other SSA consumers require acyclic maps. Do not retain a
                // lifetime certificate if that invariant was violated.
                function.iteration_capture_proofs.clear();
                return;
            }
            local = next.clone();
        }
        merge(&mut function.iteration_capture_proofs, local, proofs);
    }
    for (mut local, obligations) in std::mem::take(&mut function.iteration_capture_obligations) {
        let mut seen = FxHashSet::default();
        while let Some(next) = local_map.get(&local) {
            if !seen.insert(local.clone()) {
                function.iteration_capture_proofs.clear();
                break;
            }
            local = next.clone();
        }
        function
            .iteration_capture_obligations
            .entry(local)
            .or_default()
            .extend(obligations);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockEdge, BranchType};
    use ast::{
        Assign, Close, Closure, ForPrepKind, GenericForInit, GenericForNext, Global, If, LValue,
        Literal, RValue, VmProfileId,
    };

    fn fixture() -> (Function, RcLocal, [NodeIndex; 5], ForOrigin) {
        let mut function = Function::new(0);
        let nodes = [
            function.new_block(),
            function.new_block(),
            function.new_block(),
            function.new_block(),
            function.new_block(),
        ];
        let [init, body, tail, header, exit] = nodes;
        function.set_entry(init);
        for (node, pc) in nodes.into_iter().zip([0, 10, 20, 30, 40]) {
            function.set_block_pc_range(node, pc, pc);
        }
        let origin = ForOrigin {
            prep_pc: 0,
            body_pc: 10,
            step_pc: 30,
            follow_pc: 40,
            prep_kind: ForPrepKind::Generic,
            base_register: 0,
            result_count: 1,
            aux: 1,
            bytecode_version: 6,
            vm_profile: VmProfileId::Luau,
            explicit_nil_args: false,
        };
        let (generator, state, control, result) = (
            RcLocal::default(),
            RcLocal::default(),
            RcLocal::default(),
            RcLocal::default(),
        );
        let mut prep = GenericForInit::new_with_origin(
            generator.clone(),
            state.clone(),
            control.clone(),
            origin,
        );
        prep.0.right = vec![RValue::Global(Global::from("items"))];
        function.block_mut(init).unwrap().push(prep.into());
        function.block_mut(header).unwrap().push(
            GenericForNext::new_with_origin(
                vec![result.clone()],
                generator.into(),
                state,
                control,
                origin,
            )
            .into(),
        );
        function.block_mut(body).unwrap().push(
            Assign::new(vec![LValue::Local(RcLocal::default())], vec![
                Closure {
                    node_origin: Default::default(),
                    function: Default::default(),
                    upvalues: vec![Upvalue::Ref(result.clone())],
                }
                .into(),
            ])
            .into(),
        );
        function.block_mut(tail).unwrap().push(
            Close {
                locals: vec![result.clone()],
            }
            .into(),
        );
        function
            .block_mut(exit)
            .unwrap()
            .push(ast::Return::new(vec![]).into());
        function.set_edges(init, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(header, vec![
            (body, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        function.set_edges(body, vec![(
            tail,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        function.set_edges(tail, vec![(
            header,
            BlockEdge::new(BranchType::Unconditional),
        )]);
        (function, result, nodes, origin)
    }

    fn certify(function: &mut Function, result: &RcLocal, origin: ForOrigin) -> bool {
        let locals = function
            .blocks()
            .flat_map(|(_, block)| block.iter())
            .flat_map(|statement| statement.values())
            .cloned()
            .map(|local| (local.clone(), local))
            .collect();
        record(function, &locals);
        function
            .iteration_capture_proofs
            .get(result)
            .is_some_and(|ids| ids.contains(&origin.id()))
    }

    #[test]
    fn closes_every_backedge_and_break() {
        let (mut function, result, [_, _, tail, header, exit], origin) = fixture();
        function.block_mut(tail).unwrap().push(
            If::new(
                Global::from("again").into(),
                Default::default(),
                Default::default(),
            )
            .into(),
        );
        function.set_edges(tail, vec![
            (header, BlockEdge::new(BranchType::Then)),
            (exit, BlockEdge::new(BranchType::Else)),
        ]);
        assert!(certify(&mut function, &result, origin));
    }

    #[test]
    fn refuses_missing_wrong_or_premature_close() {
        for mode in 0..4 {
            let (mut function, result, [_, body, tail, _, _], origin) = fixture();
            match mode {
                0 => function.block_mut(tail).unwrap().clear(),
                1 => {
                    *function.block_mut(tail).unwrap() = ast::Block::from(vec![
                        Close {
                            locals: vec![RcLocal::default()],
                        }
                        .into(),
                    ])
                }
                2 => {
                    function.block_mut(tail).unwrap().clear();
                    function.block_mut(body).unwrap().insert(
                        0,
                        Close {
                            locals: vec![result.clone()],
                        }
                        .into(),
                    );
                }
                _ => function.block_mut(tail).unwrap().push(
                    Assign::new(vec![LValue::Local(result.clone())], vec![
                        Literal::Number(2.0).into(),
                    ])
                    .into(),
                ),
            }
            assert!(!certify(&mut function, &result, origin), "mode {mode}");
        }
    }

    #[test]
    fn refuses_continue_and_break_bypassing_close() {
        for is_break in [false, true] {
            let (mut function, result, [_, body, tail, header, exit], origin) = fixture();
            function.block_mut(body).unwrap().push(
                If::new(
                    Global::from("skip").into(),
                    Default::default(),
                    Default::default(),
                )
                .into(),
            );
            function.set_edges(body, vec![
                (tail, BlockEdge::new(BranchType::Then)),
                (
                    if is_break { exit } else { header },
                    BlockEdge::new(BranchType::Else),
                ),
            ]);
            assert!(!certify(&mut function, &result, origin));
        }
    }

    #[test]
    fn return_closes_the_captured_cell() {
        let (mut function, result, [_, body, _, _, _], origin) = fixture();
        function
            .block_mut(body)
            .unwrap()
            .push(ast::Return::new(vec![]).into());
        function.set_edges(body, vec![]);
        assert!(certify(&mut function, &result, origin));
    }

    #[test]
    fn local_maps_preserve_and_intersect_capture_certificates() {
        let (mut function, result, _, origin) = fixture();
        assert!(certify(&mut function, &result, origin));
        let renamed = RcLocal::default();
        super::apply_local_map(
            &mut function,
            &[(result, renamed.clone())].into_iter().collect(),
        );
        assert!(function.iteration_capture_proofs[&renamed].contains(&origin.id()));
        let unproven = RcLocal::default();
        function
            .iteration_capture_proofs
            .insert(unproven.clone(), FxHashSet::default());
        super::apply_local_map(
            &mut function,
            &[(unproven, renamed.clone())].into_iter().collect(),
        );
        assert!(function.iteration_capture_proofs[&renamed].is_empty());
    }

    #[test]
    fn certificate_survives_ssa_construction_and_destruction() {
        let (mut function, _, [_, _, _, header, _], origin) = fixture();
        let (count, _, incoming, passed) = crate::ssa::construct(&mut function, &vec![]);
        assert!(incoming.is_empty());
        assert!(
            !function
                .blocks()
                .any(|(_, block)| block.iter().any(|s| matches!(s, Statement::Close(_))))
        );
        let groups = passed
            .into_iter()
            .flat_map(|group| {
                let root = RcLocal::default();
                group.into_iter().map(move |local| (local, root.clone()))
            })
            .collect();
        crate::ssa::Destructor::new(&mut function, groups, FxHashSet::default(), count).destruct();
        let next = function
            .block(header)
            .unwrap()
            .last()
            .unwrap()
            .as_generic_for_next()
            .unwrap();
        let result = next.res_locals[0].as_local().unwrap();
        assert!(function.iteration_capture_proofs[result].contains(&origin.id()));
    }
}
