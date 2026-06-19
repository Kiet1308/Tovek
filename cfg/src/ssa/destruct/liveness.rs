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
    ) {
        let mut stack = vec![node];
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
                block_liveness
                    .uses
                    .extend(instruction.values_read().into_iter());
                block_liveness
                    .defs
                    .extend(instruction.values_written().into_iter());
            }
            for (pred, edge) in function.edges_to_block(node) {
                liveness
                    .block_liveness
                    .get_mut(&node)
                    .unwrap()
                    .params
                    .extend(edge.arguments.iter().map(|(k, _)| k));
                let block_liveness = liveness.block_liveness.entry(pred).or_default();
                block_liveness
                    .arg_out_uses
                    .extend(edge.arguments.iter().flat_map(|(_, v)| v.values_read()));
            }
        }
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
                Self::explore_all_paths(&mut liveness, function, node, variable);
            }
            let block_liveness = liveness.block_liveness.get_mut(&node).unwrap();
            for variable in block_liveness.uses.clone() {
                Self::explore_all_paths(&mut liveness, function, node, variable);
            }
        }
        liveness
            .block_liveness
            .into_iter()
            .map(|(n, l)| (n, l.live_sets))
            .collect()
    }
}
