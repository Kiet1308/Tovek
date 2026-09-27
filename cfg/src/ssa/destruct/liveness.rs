use ast::{LocalRw, RcLocal};
use rustc_hash::{FxHashMap, FxHashSet};

use petgraph::{stable_graph::NodeIndex, visit::Walker};

use crate::function::Function;

#[derive(Debug, Default)]
struct BlockLiveness<'a> {
    // the locals that are used in this block
    uses: FxHashSet<&'a RcLocal>,
    // the locals that are defined in this block
    defs: FxHashSet<&'a RcLocal>,
    // the locals that are used by arguments passed from this block to its successor
    arg_out_uses: FxHashSet<&'a RcLocal>,
    // the locals that are defined by the parameters passed to this block by its predecessor
    params: FxHashSet<&'a RcLocal>,
    live_sets: LiveSets,
}

#[derive(Debug, Default)]
pub struct LiveSets {
    // the set LiveIn(B) = params(B) ⋃ ( [uses(B) ⋃ live_out(B)] ∖ defs(B))
    pub live_in: FxHashSet<RcLocal>,
    // the set LiveOut(B) = ( ⋃_{S ∊ successor(B)} [live_in(S)∖params(S)] ) ⋃ arg_out_uses(B)
    pub live_out: FxHashSet<RcLocal>,
}

#[derive(Debug)]
pub struct Liveness<'a> {
    block_liveness: FxHashMap<NodeIndex, BlockLiveness<'a>>,
}

impl<'a> Liveness<'a> {
    fn explore_all_paths(
        liveness: &mut Liveness,
        function: &'a Function,
        node: NodeIndex,
        variable: &'a RcLocal,
        stack: &mut Vec<NodeIndex>,
    ) {
        debug_assert!(stack.is_empty());
        stack.push(node);
        while let Some(node) = stack.pop() {
            let block_liveness = liveness.block_liveness.get_mut(&node).unwrap();
            if block_liveness.defs.contains(variable)
                // block already visited
                || block_liveness.live_sets.live_in.contains(variable)
            {
                continue;
            }
            block_liveness.live_sets.live_in.insert(variable.clone());
            if block_liveness.params.contains(variable) {
                continue;
            }
            for pred in function.predecessor_blocks(node) {
                liveness
                    .block_liveness
                    .get_mut(&pred)
                    .unwrap()
                    .live_sets
                    .live_out
                    .insert(variable.clone());
                stack.push(pred);
            }
        }
    }

    pub fn calculate(function: &'a Function) -> FxHashMap<NodeIndex, LiveSets> {
        let mut liveness = Liveness {
            block_liveness: FxHashMap::with_capacity_and_hasher(
                function.graph().node_count(),
                Default::default(),
            ),
        };
        for (node, block) in function.blocks() {
            let block_liveness = liveness.block_liveness.entry(node).or_default();
            for instruction in block.iter() {
                instruction.visit_local_reads(&mut |local| {
                    block_liveness.uses.insert(local);
                    true
                });
                instruction.visit_local_writes(&mut |local| {
                    block_liveness.defs.insert(local);
                    true
                });
            }
            for (pred, edge) in function.edges_to_block(node) {
                liveness
                    .block_liveness
                    .get_mut(&node)
                    .unwrap()
                    .params
                    .extend(edge.arguments.iter().map(|(k, _)| k));
                let block_liveness = liveness.block_liveness.entry(pred).or_default();
                for (_, argument) in &edge.arguments {
                    argument.visit_local_reads(&mut |local| {
                        block_liveness.arg_out_uses.insert(local);
                        true
                    });
                }
            }
        }
        let mut stack = Vec::new();
        for node in function.graph().node_indices() {
            let block_liveness = liveness.block_liveness.get_mut(&node).unwrap();
            block_liveness.live_sets.live_in.reserve(
                block_liveness.params.len()
                    + block_liveness
                        .uses
                        .len()
                        .saturating_sub(block_liveness.defs.len()),
            );
            let arg_out_uses = std::mem::take(&mut block_liveness.arg_out_uses);
            block_liveness
                .live_sets
                .live_out
                .reserve(arg_out_uses.len());
            for variable in arg_out_uses {
                let block_liveness = liveness.block_liveness.get_mut(&node).unwrap();
                block_liveness.live_sets.live_out.insert(variable.clone());
                Self::explore_all_paths(&mut liveness, function, node, variable, &mut stack);
            }
            let block_liveness = liveness.block_liveness.get_mut(&node).unwrap();
            for variable in std::mem::take(&mut block_liveness.uses) {
                Self::explore_all_paths(&mut liveness, function, node, variable, &mut stack);
            }
        }
        liveness
            .block_liveness
            .into_iter()
            .map(|(n, l)| (n, l.live_sets))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockEdge;

    #[test]
    fn visitor_liveness_keeps_phi_transport_and_generic_control_live() {
        for backedge in [false, true] {
            let mut function = Function::new(0);
            let entry = function.new_block();
            let step = function.new_block();
            let exhausted = function.new_block();
            let body = function.new_block();
            function.set_entry(entry);
            let [generator, state, control, result, phi] = std::array::from_fn(|_| RcLocal::default());
            function.parameters = vec![generator.clone(), state.clone(), control.clone()];
            function.graph_mut().add_edge(entry, step, BlockEdge::default());
            function.graph_mut().add_edge(step, exhausted, BlockEdge::default());
            let argument = ast::Binary::new(result.clone().into(), result.clone().into(),
                ast::BinaryOperation::Add).into();
            let edge = BlockEdge { arguments: vec![(phi.clone(), argument)], ..Default::default() };
            // Parallel edges and duplicate reads in an argument must not lose
            // the transport's use at its source or its definition at its target.
            function.graph_mut().add_edge(step, body, edge.clone());
            function.graph_mut().add_edge(step, body, edge);
            if backedge { function.graph_mut().add_edge(step, step, BlockEdge::default()); }
            function.block_mut(step).unwrap().push(ast::GenericForNext::new(
                vec![result.clone(), result.clone()], generator.clone().into(),
                state.clone(), control.clone(),
            ).into());
            function.block_mut(exhausted).unwrap().push(ast::Return::new(vec![control.clone().into()]).into());
            function.block_mut(body).unwrap().push(ast::Return::new(vec![phi.clone().into(), control.clone().into()]).into());

            let live = Liveness::calculate(&function);
            let inputs: FxHashSet<_> = [generator, state, control.clone()].into_iter().collect();
            assert_eq!(live[&entry].live_in, inputs);
            assert_eq!(live[&entry].live_out, inputs);
            assert_eq!(live[&step].live_in, inputs);
            let mut outputs: FxHashSet<_> = [result, control.clone()].into_iter().collect();
            if backedge { outputs.extend(inputs); }
            assert_eq!(live[&step].live_out, outputs);
            assert_eq!(live[&exhausted].live_in, [control.clone()].into_iter().collect());
            assert_eq!(live[&body].live_in, [phi, control].into_iter().collect());
            assert!(live[&exhausted].live_out.is_empty());
            assert!(live[&body].live_out.is_empty());
        }
    }
}
