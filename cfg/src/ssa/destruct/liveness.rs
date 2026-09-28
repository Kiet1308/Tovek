use ast::{LocalRw, RcLocal};
use rustc_hash::FxHashMap;

use petgraph::stable_graph::NodeIndex;

use crate::function::Function;

/// SSA liveness with block parameters, as dense bit rows per block:
///
/// * LiveOut(B) = arg_out_uses(B) â‹ƒ ( â‹ƒ_{S âˆŠ successor(B)} [live_in(S) âˆ– params(S)] )
/// * LiveIn(B) = [uses(B) â‹ƒ live_out(B)] âˆ– defs(B)
///
/// `uses` are all reads of the block, `arg_out_uses` the reads of the
/// arguments passed to its successors and `params` the destinations those
/// arguments define. The least fixed point is unique, so this equals the
/// former per-variable path exploration.
#[derive(Debug, Default)]
pub struct Liveness {
    ids: FxHashMap<u64, u32>,
    slots: FxHashMap<NodeIndex, usize>,
    words: usize,
    live_in: Vec<u64>,
    live_out: Vec<u64>,
    #[cfg(test)]
    locals: Vec<RcLocal>,
}

fn set(bits: &mut [u64], id: u32) {
    bits[id as usize / 64] |= 1 << (id % 64);
}

impl Liveness {
    /// No block was analysed (the terminal single-block fast path).
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn live_in(&self, node: NodeIndex, local: &RcLocal) -> bool {
        self.contains(&self.live_in, node, local)
    }

    pub fn live_out(&self, node: NodeIndex, local: &RcLocal) -> bool {
        self.contains(&self.live_out, node, local)
    }

    fn contains(&self, rows: &[u64], node: NodeIndex, local: &RcLocal) -> bool {
        let (Some(&slot), Some(&id)) = (self.slots.get(&node), self.ids.get(&local.stable_id())) else {
            return false;
        };
        rows[slot * self.words + id as usize / 64] & (1 << (id % 64)) != 0
    }

    pub fn calculate(function: &Function) -> Self {
        let nodes = function.graph().node_indices().collect::<Vec<_>>();
        let slots = nodes.iter().enumerate().map(|(slot, &node)| (node, slot)).collect::<FxHashMap<_, _>>();
        let mut ids = FxHashMap::<u64, u32>::default();
        #[cfg(test)]
        let mut locals = Vec::new();
        let mut id = |local: &RcLocal| {
            let next = ids.len() as u32;
            *ids.entry(local.stable_id()).or_insert_with(|| {
                #[cfg(test)]
                locals.push(local.clone());
                next
            })
        };
        // (uses, defs, params, arg_out_uses) as id lists per block.
        let mut facts = vec![(Vec::new(), Vec::new(), Vec::new(), Vec::new()); nodes.len()];
        for (slot, &node) in nodes.iter().enumerate() {
            for statement in function.block(node).unwrap().iter() {
                statement.visit_local_reads(&mut |local| {
                    facts[slot].0.push(id(local));
                    true
                });
                statement.visit_local_writes(&mut |local| {
                    facts[slot].1.push(id(local));
                    true
                });
            }
            for (predecessor, edge) in function.edges_to_block(node) {
                facts[slot].2.extend(edge.arguments.iter().map(|(parameter, _)| id(parameter)));
                let predecessor = slots[&predecessor];
                for (_, argument) in &edge.arguments {
                    argument.visit_local_reads(&mut |local| {
                        facts[predecessor].3.push(id(local));
                        true
                    });
                }
            }
        }
        let words = ids.len().div_ceil(64).max(1);
        let rows = nodes.len() * words;
        let (mut uses, mut keep, mut params, mut arg_out) =
            (vec![0u64; rows], vec![!0u64; rows], vec![0u64; rows], vec![0u64; rows]);
        for (slot, (block_uses, block_defs, block_params, block_arg_out)) in facts.iter().enumerate() {
            let row = slot * words..(slot + 1) * words;
            for &read in block_uses { set(&mut uses[row.clone()], read); }
            for &written in block_defs { keep[row.start + written as usize / 64] &= !(1 << (written % 64)); }
            for &parameter in block_params { set(&mut params[row.clone()], parameter); }
            for &read in block_arg_out { set(&mut arg_out[row.clone()], read); }
        }
        let successors = nodes.iter()
            .map(|&node| function.successor_blocks(node).map(|next| slots[&next]).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let predecessors = nodes.iter()
            .map(|&node| function.predecessor_blocks(node).map(|previous| slots[&previous]).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let mut live_in = vec![0u64; rows];
        let mut live_out = vec![0u64; rows];
        let mut work = (0..nodes.len()).rev().collect::<Vec<_>>();
        let mut queued = vec![true; nodes.len()];
        let mut next_out = vec![0u64; words];
        while let Some(slot) = work.pop() {
            queued[slot] = false;
            let row = slot * words..(slot + 1) * words;
            next_out.copy_from_slice(&arg_out[row.clone()]);
            for &successor in &successors[slot] {
                let successor = successor * words..(successor + 1) * words;
                for word in 0..words {
                    next_out[word] |= live_in[successor.start + word] & !params[successor.start + word];
                }
            }
            let mut changed = false;
            for word in 0..words {
                let at = row.start + word;
                let value = (uses[at] | next_out[word]) & keep[at];
                changed |= live_in[at] != value;
                live_in[at] = value;
                live_out[at] = next_out[word];
            }
            if changed {
                for &predecessor in &predecessors[slot] {
                    if !queued[predecessor] {
                        queued[predecessor] = true;
                        work.push(predecessor);
                    }
                }
            }
        }
        Self {
            ids,
            slots,
            words,
            live_in,
            live_out,
            #[cfg(test)]
            locals,
        }
    }

    #[cfg(test)]
    fn live_in_set(&self, node: NodeIndex) -> rustc_hash::FxHashSet<RcLocal> {
        self.locals.iter().filter(|local| self.live_in(node, local)).cloned().collect()
    }

    #[cfg(test)]
    fn live_out_set(&self, node: NodeIndex) -> rustc_hash::FxHashSet<RcLocal> {
        self.locals.iter().filter(|local| self.live_out(node, local)).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet;
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
            assert_eq!(live.live_in_set(entry), inputs);
            assert_eq!(live.live_out_set(entry), inputs);
            assert_eq!(live.live_in_set(step), inputs);
            let mut outputs: FxHashSet<_> = [result, control.clone()].into_iter().collect();
            if backedge { outputs.extend(inputs); }
            assert_eq!(live.live_out_set(step), outputs);
            assert_eq!(live.live_in_set(exhausted), [control.clone()].into_iter().collect());
            assert_eq!(live.live_in_set(body), [phi, control].into_iter().collect());
            assert!(live.live_out_set(exhausted).is_empty());
            assert!(live.live_out_set(body).is_empty());
        }
    }
}
