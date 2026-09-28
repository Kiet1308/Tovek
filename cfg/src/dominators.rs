//! Immediate dominators of a function CFG in dense arrays.
//!
//! Cooper, Harvey and Kennedy's iterative algorithm over reverse postorder.
//! Immediate dominators are unique, so the answers equal those of
//! `petgraph::algo::dominators::simple_fast`, without its keyed hash map.

use petgraph::{
    stable_graph::{NodeIndex, StableDiGraph},
    visit::NodeIndexable,
    Direction,
};

const NONE: u32 = u32::MAX;

pub struct Dominators {
    root: NodeIndex,
    /// Immediate dominator per node index; the root is its own; `NONE` for
    /// nodes that are unreachable from the root (or holes).
    idom: Vec<u32>,
}

impl Dominators {
    pub fn new<N, E>(graph: &StableDiGraph<N, E>, root: NodeIndex) -> Self {
        let bound = graph.node_bound();
        // Postorder numbers of the nodes reachable from the root.
        let mut post = vec![NONE; bound];
        let mut order = Vec::new();
        let mut visited = vec![false; bound];
        let mut stack = vec![(root, graph.neighbors_directed(root, Direction::Outgoing).detach())];
        visited[root.index()] = true;
        while let Some((node, successors)) = stack.last_mut() {
            let node = *node;
            match successors.next_node(graph) {
                Some(next) if !visited[next.index()] => {
                    visited[next.index()] = true;
                    stack.push((next, graph.neighbors_directed(next, Direction::Outgoing).detach()));
                }
                Some(_) => {}
                None => {
                    post[node.index()] = order.len() as u32;
                    order.push(node);
                    stack.pop();
                }
            }
        }
        let mut idom = vec![NONE; bound];
        idom[root.index()] = root.index() as u32;
        let intersect = |idom: &[u32], mut a: u32, mut b: u32| {
            while a != b {
                while post[a as usize] < post[b as usize] { a = idom[a as usize]; }
                while post[b as usize] < post[a as usize] { b = idom[b as usize]; }
            }
            a
        };
        let mut changed = true;
        while changed {
            changed = false;
            for &node in order.iter().rev() {
                if node == root { continue; }
                let mut new_idom = NONE;
                for predecessor in graph.neighbors_directed(node, Direction::Incoming) {
                    let predecessor = predecessor.index() as u32;
                    if idom[predecessor as usize] == NONE { continue; }
                    new_idom = if new_idom == NONE { predecessor } else { intersect(&idom, predecessor, new_idom) };
                }
                if new_idom != NONE && idom[node.index()] != new_idom {
                    idom[node.index()] = new_idom;
                    changed = true;
                }
            }
        }
        Self { root, idom }
    }

    pub fn root(&self) -> NodeIndex {
        self.root
    }

    /// `None` for the root and for nodes the root does not reach.
    pub fn immediate_dominator(&self, node: NodeIndex) -> Option<NodeIndex> {
        if node == self.root { return None; }
        match self.idom.get(node.index()) {
            Some(&parent) if parent != NONE => Some(NodeIndex::new(parent as usize)),
            _ => None,
        }
    }

    /// `node` and then each of its dominators up to the root; `None` when the
    /// root does not reach `node`.
    pub fn dominators(&self, node: NodeIndex) -> Option<impl Iterator<Item = NodeIndex> + '_> {
        (self.idom.get(node.index()).is_some_and(|&parent| parent != NONE)).then(|| {
            let mut next = Some(node);
            std::iter::from_fn(move || {
                let current = next?;
                next = self.immediate_dominator(current);
                Some(current)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_petgraph_on_random_graphs() {
        for seed in 1..=300u64 {
            let mut state = seed;
            let mut random = |limit: usize| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((state >> 33) as usize) % limit
            };
            let mut graph = StableDiGraph::<(), ()>::new();
            let count = 1 + random(12);
            let nodes: Vec<_> = (0..count).map(|_| graph.add_node(())).collect();
            for _ in 0..random(3 * count + 1) {
                graph.add_edge(nodes[random(count)], nodes[random(count)], ());
            }
            if count > 3 && random(2) == 0 {
                graph.remove_node(nodes[1 + random(count - 1)]);
            }
            let root = nodes[0];
            let expected = petgraph::algo::dominators::simple_fast(&graph, root);
            let actual = Dominators::new(&graph, root);
            for node in graph.node_indices() {
                assert_eq!(actual.immediate_dominator(node), expected.immediate_dominator(node), "seed {seed}");
                assert_eq!(
                    actual.dominators(node).map(|d| d.collect::<Vec<_>>()),
                    expected.dominators(node).map(|d| d.collect::<Vec<_>>()),
                    "seed {seed}",
                );
            }
        }
    }
}
