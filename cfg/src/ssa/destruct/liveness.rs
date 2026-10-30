use std::cell::{OnceCell, RefCell};

use ast::{LocalRw, RcLocal};
use rustc_hash::FxHashMap;

use petgraph::stable_graph::NodeIndex;

use crate::function::Function;

/// SSA liveness with block parameters, per local on its first query:
///
/// * LiveOut(B) = arg_out_uses(B) ⋃ ( ⋃_{S ∊ successor(B)} [live_in(S) ∖ params(S)] )
/// * LiveIn(B) = [uses(B) ⋃ live_out(B)] ∖ defs(B)
///
/// `uses` are all reads of the block, `arg_out_uses` the reads of the
/// arguments passed to its successors and `params` the destinations those
/// arguments define. A local's live blocks are found by walking back from its
/// reads to its definitions, so the work and memory are the live ranges of the
/// locals interference actually asks about. Precomputing every local is
/// quadratic in a long function: each incoming upvalue read is an entry
/// definition, live from the entry to its read.
#[derive(Debug, Default)]
pub struct Liveness {
    ids: FxHashMap<u64, u32>,
    slots: FxHashMap<NodeIndex, usize>,
    predecessors: Vec<Vec<usize>>,
    /// `(local, slot)` facts sorted by local: reads, writes, parameters and
    /// the reads of the arguments a block passes to its successors.
    uses: Vec<(u32, u32)>,
    defs: Vec<(u32, u32)>,
    params: Vec<(u32, u32)>,
    arg_out: Vec<(u32, u32)>,
    /// Per local, the ascending slots it is live into and out of.
    rows: Vec<OnceCell<(Box<[u32]>, Box<[u32]>)>>,
    scratch: RefCell<Scratch>,
    #[cfg(test)]
    locals: Vec<RcLocal>,
}

/// Per slot, the last local (plus one) that defines it, is a parameter of it,
/// or was found live into or out of it: no clearing between locals.
#[derive(Debug, Default)]
struct Scratch {
    defined: Vec<u32>,
    parameter: Vec<u32>,
    entered: Vec<u32>,
    exited: Vec<u32>,
    work: Vec<usize>,
}

/// The slots of `local`'s facts.
fn facts(facts: &[(u32, u32)], local: u32) -> impl Iterator<Item = usize> + '_ {
    facts[facts.partition_point(|&(id, _)| id < local)..].iter()
        .take_while(move |&&(id, _)| id == local)
        .map(|&(_, slot)| slot as usize)
}

impl Liveness {
    /// No block was analysed (the terminal single-block fast path).
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn live_in(&self, node: NodeIndex, local: &RcLocal) -> bool {
        self.contains(node, local, |(live_in, _)| live_in)
    }

    pub fn live_out(&self, node: NodeIndex, local: &RcLocal) -> bool {
        self.contains(node, local, |(_, live_out)| live_out)
    }

    fn contains(
        &self,
        node: NodeIndex,
        local: &RcLocal,
        row: impl FnOnce(&(Box<[u32]>, Box<[u32]>)) -> &[u32],
    ) -> bool {
        let (Some(&slot), Some(&id)) = (self.slots.get(&node), self.ids.get(&local.stable_id())) else {
            return false;
        };
        row(self.rows[id as usize].get_or_init(|| self.walk(id))).binary_search(&(slot as u32)).is_ok()
    }

    /// The blocks `local` is live into and out of: back from its reads to
    /// its definitions, not past a block taking it as a parameter.
    fn walk(&self, local: u32) -> (Box<[u32]>, Box<[u32]>) {
        let mut scratch = self.scratch.borrow_mut();
        let Scratch { defined, parameter, entered, exited, work } = &mut *scratch;
        let mark = local + 1;
        for slot in facts(&self.defs, local) {
            defined[slot] = mark;
        }
        for slot in facts(&self.params, local) {
            parameter[slot] = mark;
        }
        let (mut live_in, mut live_out) = (Vec::new(), Vec::new());
        for slot in facts(&self.arg_out, local) {
            if exited[slot] != mark {
                exited[slot] = mark;
                live_out.push(slot as u32);
            }
            work.push(slot);
        }
        work.extend(facts(&self.uses, local));
        while let Some(slot) = work.pop() {
            if defined[slot] == mark || entered[slot] == mark {
                continue;
            }
            entered[slot] = mark;
            live_in.push(slot as u32);
            if parameter[slot] == mark {
                continue;
            }
            for &predecessor in &self.predecessors[slot] {
                if exited[predecessor] != mark {
                    exited[predecessor] = mark;
                    live_out.push(predecessor as u32);
                }
                work.push(predecessor);
            }
        }
        live_in.sort_unstable();
        live_out.sort_unstable();
        (live_in.into(), live_out.into())
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
        let (mut uses, mut defs, mut params, mut arg_out) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (slot, &node) in nodes.iter().enumerate() {
            let slot = slot as u32;
            for statement in function.block(node).unwrap().iter() {
                statement.visit_local_reads(&mut |local| {
                    uses.push((id(local), slot));
                    true
                });
                statement.visit_local_writes(&mut |local| {
                    defs.push((id(local), slot));
                    true
                });
            }
            for (predecessor, edge) in function.edges_to_block(node) {
                params.extend(edge.arguments.iter().map(|(parameter, _)| (id(parameter), slot)));
                let predecessor = slots[&predecessor] as u32;
                for (_, argument) in &edge.arguments {
                    argument.visit_local_reads(&mut |local| {
                        arg_out.push((id(local), predecessor));
                        true
                    });
                }
            }
        }
        for facts in [&mut uses, &mut defs, &mut params, &mut arg_out] {
            facts.sort_unstable();
            facts.dedup();
        }
        let predecessors = nodes.iter()
            .map(|&node| function.predecessor_blocks(node).map(|previous| slots[&previous]).collect())
            .collect();
        let scratch = Scratch {
            defined: vec![0; nodes.len()],
            parameter: vec![0; nodes.len()],
            entered: vec![0; nodes.len()],
            exited: vec![0; nodes.len()],
            work: Vec::new(),
        };
        Self {
            rows: (0..ids.len()).map(|_| OnceCell::new()).collect(),
            ids,
            slots,
            predecessors,
            uses,
            defs,
            params,
            arg_out,
            scratch: RefCell::new(scratch),
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
    use crate::block::{BlockEdge, BranchType};

    /// Random graphs with loops, parallel edges and edge arguments: the walk
    /// per local gives the least fixed point of the equations above.
    #[test]
    fn walks_match_the_fixed_point() {
        for seed in 0..300u64 {
            let mut state = seed;
            let mut next = |bound: usize| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (state >> 33) as usize % bound
            };
            let mut function = Function::new(0);
            let blocks = (0..2 + next(7)).map(|_| function.new_block()).collect::<Vec<_>>();
            function.set_entry(blocks[0]);
            let locals = (0..2 + next(8)).map(|_| RcLocal::default()).collect::<Vec<_>>();
            let pick = |next: &mut dyn FnMut(usize) -> usize| locals[next(locals.len())].clone();
            for &block in &blocks {
                for _ in 0..next(4) {
                    let read: ast::RValue = if next(2) == 0 {
                        pick(&mut next).into()
                    } else {
                        ast::Binary::new(pick(&mut next).into(), pick(&mut next).into(), ast::BinaryOperation::Add).into()
                    };
                    function.block_mut(block).unwrap().push(ast::Assign::new(vec![pick(&mut next).into()], vec![read]).into());
                }
                let edges = (0..next(3)).map(|index| {
                    let arguments = (0..next(3)).map(|_| (pick(&mut next), pick(&mut next).into())).collect();
                    let branch_type = match index { 0 => BranchType::Unconditional, 1 => BranchType::Then, _ => BranchType::Else };
                    (blocks[next(blocks.len())], BlockEdge { branch_type, arguments })
                }).collect();
                function.set_edges(block, edges);
            }

            let nodes = function.graph().node_indices().collect::<Vec<_>>();
            let reads = |statements: &[ast::Statement]| statements.iter().flat_map(|s| s.values_read()).cloned().collect::<FxHashSet<_>>();
            let writes = |statements: &[ast::Statement]| statements.iter().flat_map(|s| s.values_written()).cloned().collect::<FxHashSet<_>>();
            let params = |node| function.edges_to_block(node).flat_map(|(_, edge)| edge.arguments.iter().map(|(p, _)| p.clone())).collect::<FxHashSet<_>>();
            let mut live_in = nodes.iter().map(|&node| (node, FxHashSet::default())).collect::<FxHashMap<_, _>>();
            let mut live_out = live_in.clone();
            loop {
                let mut changed = false;
                for &node in &nodes {
                    let mut out = FxHashSet::default();
                    for edge in function.graph().edges(node) {
                        let successor = petgraph::visit::EdgeRef::target(&edge);
                        for argument in &edge.weight().arguments {
                            out.extend(argument.1.values_read().into_iter().cloned());
                        }
                        let parameters = params(successor);
                        out.extend(live_in[&successor].iter().filter(|local| !parameters.contains(*local)).cloned());
                    }
                    let block = function.block(node).unwrap();
                    let defined = writes(&block.0);
                    let mut into = reads(&block.0);
                    into.extend(out.iter().cloned());
                    into.retain(|local| !defined.contains(local));
                    changed |= live_out[&node] != out || live_in[&node] != into;
                    live_out.insert(node, out);
                    live_in.insert(node, into);
                }
                if !changed {
                    break;
                }
            }
            let liveness = Liveness::calculate(&function);
            for &node in &nodes {
                for local in &locals {
                    assert_eq!(liveness.live_in(node, local), live_in[&node].contains(local), "seed {seed}");
                    assert_eq!(liveness.live_out(node, local), live_out[&node].contains(local), "seed {seed}");
                }
            }
        }
    }

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
