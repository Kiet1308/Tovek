use ast::{LocalRw, RcLocal};
use ast::FxIndexSet as IndexSet;
use petgraph::{
    prelude::DiGraph,
    stable_graph::NodeIndex,
    visit::{DfsPostOrder, Walker},
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::function::Function;

// https://github.com/fkie-cad/dewolf/blob/7afe5b46e79a7b56e9904e63f29d54bd8f7302d9/decompiler/pipeline/ssa/phi_dependency_graph.py
#[derive(Debug)]
pub struct ParamDependencyGraph {
    pub graph: DiGraph<RcLocal, ()>,
    pub local_to_node: FxHashMap<RcLocal, NodeIndex>,
}

impl ParamDependencyGraph {
    pub fn new(function: &Function, node: NodeIndex) -> Self {
        Self::with_param_names(function, node, RcLocal::clone)
    }

    /// Nodes are keyed by `name(param)`: the pass building this graph may
    /// already have renamed a param (to the captured cell its value merges
    /// into), and its own lookups use that name.
    pub fn with_param_names(
        function: &Function,
        node: NodeIndex,
        name: impl Fn(&RcLocal) -> RcLocal,
    ) -> Self {
        let mut this = Self {
            graph: DiGraph::new(),
            local_to_node: FxHashMap::default(),
        };
        let resolve = |local: &RcLocal| name(local);
        let mut own_params = FxHashMap::default();

        let edges = function.edges_to_block(node);
        for (edge_index, edge) in edges.enumerate() {
            if edge_index == 0 {
                for (param, _) in &edge.1.arguments {
                    let key = resolve(param);
                    own_params.insert(param.clone(), key.clone());
                    this.add_node(key);
                }
            }
            // TODO: support non-local block arguments
            for (param, arg) in &edge.1.arguments {
                let param = own_params.get(param).cloned().unwrap_or_else(|| resolve(param));
                arg.visit_local_reads(&mut |read| {
                    let read = own_params.get(read).unwrap_or(read);
                    if let Some(&defining_param_node) = this.local_to_node.get(read) {
                        let param_node = this.local_to_node[&param];
                        if param_node != defining_param_node {
                            this.graph.add_edge(param_node, defining_param_node, ());
                        }
                    }
                    true
                });
            }
        }

        this
    }

    /// `DiGraph` fills the hole with its last node, so the local that pointed
    /// at the last index now points at `node`.
    pub fn remove_node(&mut self, node: NodeIndex) -> Option<RcLocal> {
        let local = self.graph.remove_node(node)?;
        if self.local_to_node.get(&local) == Some(&node) {
            self.local_to_node.remove(&local);
        }
        let last = NodeIndex::new(self.graph.node_count());
        if let Some(moved) = self.graph.node_weight(node)
            && let Some(index) = self.local_to_node.get_mut(moved)
            && *index == last
        {
            *index = node;
        }
        Some(local)
    }

    pub fn add_node(&mut self, local: RcLocal) -> NodeIndex {
        let node = self.graph.add_node(local.clone());
        self.local_to_node.insert(local, node);
        node
    }

    // This function computes a directed feedback vertex (directed fvs) set of a given graph.
    // Since this problem is NP-hard, we only compute an approximate solution.
    pub fn compute_directed_fvs(&self) -> IndexSet<NodeIndex> {
        let mut directed_fvs = IndexSet::default();
        let mut dfs_post_order = DfsPostOrder::empty(&self.graph);
        dfs_post_order.stack.extend(self.graph.node_indices());
        let mut topological_order = dfs_post_order.iter(&self.graph).collect::<Vec<_>>();
        topological_order.reverse();

        let mut smaller_order = FxHashSet::default();
        for node in topological_order {
            if self
                .graph
                .neighbors(node)
                .any(|n| smaller_order.contains(&n))
            {
                directed_fvs.insert(node);
            } else {
                smaller_order.insert(node);
            }
        }

        directed_fvs
    }
}

// TODO: fix
// #[cfg(test)]
// mod tests {
//     use petgraph::dot::Dot;

//     use crate::block::{BasicBlockEdge, Terminator};

//     use super::*;

//     #[test]
//     fn test_dependency_graph() {
//         let mut function = Function::default();

//         let local_y1 = ast::RcLocal::new(ast::Local::new("y1".to_string().into()));
//         let local_y2 = ast::RcLocal::new(ast::Local::new("y2".to_string().into()));
//         let local_z1 = ast::RcLocal::new(ast::Local::new("z1".to_string().into()));
//         let local_z2 = ast::RcLocal::new(ast::Local::new("z2".to_string().into()));

//         let entry_node = function.new_block();
//         let block1_node = function.new_block();
//         let block2_node = function.new_block();
//         function.set_entry(entry_node);

//         let arguments = vec![(local_y2.clone(), local_y1), (local_z2.clone(), local_z1)];
//         function.set_block_terminator(
//             entry_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block1_node,
//                 arguments,
//             })),
//         );

//         function.set_block_terminator(
//             block1_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block2_node,
//                 arguments: Vec::new(),
//             })),
//         );

//         let arguments = vec![
//             (local_y2.clone(), local_z2.clone()),
//             (local_z2.clone(), local_y2.clone()),
//         ];
//         function.set_block_terminator(
//             block2_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block1_node,
//                 arguments,
//             })),
//         );

//         let dependency_graph = ParamDependencyGraph::new(&mut function, block1_node);
//         println!("{:?}", Dot::new(&dependency_graph.graph));

//         assert!(
//             dependency_graph
//                 .graph
//                 .neighbors(dependency_graph.local_to_node[&local_z2])
//                 .next()
//                 .unwrap()
//                 == dependency_graph.local_to_node[&local_y2]
//         );
//         assert!(
//             dependency_graph
//                 .graph
//                 .neighbors(dependency_graph.local_to_node[&local_y2])
//                 .next()
//                 .unwrap()
//                 == dependency_graph.local_to_node[&local_z2]
//         );
//     }

//     #[test]
//     fn test_directed_fvs() {
//         let mut function = Function::default();

//         let local_y1 = ast::RcLocal::new(ast::Local::new("y1".to_string().into()));
//         let local_y2 = ast::RcLocal::new(ast::Local::new("y2".to_string().into()));
//         let local_z1 = ast::RcLocal::new(ast::Local::new("z1".to_string().into()));
//         let local_z2 = ast::RcLocal::new(ast::Local::new("z2".to_string().into()));

//         let entry_node = function.new_block();
//         let block1_node = function.new_block();
//         let block2_node = function.new_block();
//         function.set_entry(entry_node);

//         let arguments = vec![(local_y2.clone(), local_y1), (local_z2.clone(), local_z1)];
//         function.set_block_terminator(
//             entry_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block1_node,
//                 arguments,
//             })),
//         );

//         function.set_block_terminator(
//             block1_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block2_node,
//                 arguments: Vec::new(),
//             })),
//         );

//         let arguments = vec![
//             (local_y2.clone(), local_z2.clone()),
//             (local_z2.clone(), local_y2.clone()),
//         ];
//         function.set_block_terminator(
//             block2_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block1_node,
//                 arguments,
//             })),
//         );

//         let dependency_graph = ParamDependencyGraph::new(&mut function, block1_node);
//         println!("{:?}", Dot::new(&dependency_graph.graph));

//         let directed_fvs = dependency_graph.compute_directed_fvs();
//         println!("{:?}", directed_fvs);
//         assert!(
//             directed_fvs.contains(&dependency_graph.local_to_node[&local_y2])
//                 || directed_fvs.contains(&dependency_graph.local_to_node[&local_z2])
//         );
//     }

//     #[test]
//     fn test_multiple_directed_fvs() {
//         let mut function = Function::default();

//         let local_y1 = ast::RcLocal::new(ast::Local::new("y1".to_string().into()));
//         let local_y2 = ast::RcLocal::new(ast::Local::new("y2".to_string().into()));
//         let local_z1 = ast::RcLocal::new(ast::Local::new("z1".to_string().into()));
//         let local_z2 = ast::RcLocal::new(ast::Local::new("z2".to_string().into()));
//         let local_a1 = ast::RcLocal::new(ast::Local::new("a1".to_string().into()));
//         let local_a2 = ast::RcLocal::new(ast::Local::new("a2".to_string().into()));
//         let local_b1 = ast::RcLocal::new(ast::Local::new("b1".to_string().into()));
//         let local_b2 = ast::RcLocal::new(ast::Local::new("b2".to_string().into()));

//         let entry_node = function.new_block();
//         let block1_node = function.new_block();
//         let block2_node = function.new_block();
//         function.set_entry(entry_node);

//         let arguments = vec![
//             (local_y2.clone(), local_y1),
//             (local_z2.clone(), local_z1),
//             (local_a2.clone(), local_a1),
//             (local_b2.clone(), local_b1),
//         ];

//         function.set_block_terminator(
//             entry_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block1_node,
//                 arguments,
//             })),
//         );

//         function.set_block_terminator(
//             block1_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block2_node,
//                 arguments: Vec::new(),
//             })),
//         );

//         let arguments = vec![
//             (local_y2.clone(), local_z2.clone()),
//             (local_z2.clone(), local_y2.clone()),
//             (local_a2.clone(), local_b2.clone()),
//             (local_b2.clone(), local_a2.clone()),
//         ];
//         function.set_block_terminator(
//             block2_node,
//             Some(Terminator::Jump(BasicBlockEdge {
//                 node: block1_node,
//                 arguments,
//             })),
//         );

//         let dependency_graph = ParamDependencyGraph::new(&mut function, block1_node);
//         println!("{:?}", Dot::new(&dependency_graph.graph));

//         let directed_fvs = dependency_graph.compute_directed_fvs();
//         println!("{:?}", directed_fvs);
//         assert!(
//             directed_fvs.contains(&dependency_graph.local_to_node[&local_y2])
//                 || directed_fvs.contains(&dependency_graph.local_to_node[&local_z2])
//         );
//         assert!(
//             directed_fvs.contains(&dependency_graph.local_to_node[&local_a2])
//                 || directed_fvs.contains(&dependency_graph.local_to_node[&local_b2])
//         );
//     }
// }
