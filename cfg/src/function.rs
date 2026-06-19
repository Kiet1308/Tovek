use ast::{LocalRw, RcLocal};
use contracts::requires;

use petgraph::{
    stable_graph::{EdgeReference, Neighbors, NodeIndex, StableDiGraph},
    visit::{EdgeRef, IntoEdgesDirected},
    Direction,
};

use crate::block::{BlockEdge, BranchType};

#[derive(Debug, Clone, Default)]
pub struct Function {
    pub id: usize,
    pub name: Option<String>,
    pub parameters: Vec<RcLocal>,
    pub is_variadic: bool,
    graph: StableDiGraph<ast::Block, BlockEdge>,
    entry: Option<NodeIndex>,
}

impl Function {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            name: None,
            parameters: Vec::new(),
            is_variadic: false,
            graph: StableDiGraph::new(),
            entry: None,
        }
    }

    pub fn name_mut(&mut self) -> &mut Option<String> {
        &mut self.name
    }

    pub fn entry(&self) -> &Option<NodeIndex> {
        &self.entry
    }

    #[requires(self.has_block(new_entry))]
    pub fn set_entry(&mut self, new_entry: NodeIndex) {
        self.entry = Some(new_entry);
    }

    pub fn graph(&self) -> &StableDiGraph<ast::Block, BlockEdge> {
        &self.graph
    }

    pub fn graph_mut(&mut self) -> &mut StableDiGraph<ast::Block, BlockEdge> {
        &mut self.graph
    }

    pub fn has_block(&self, block: NodeIndex) -> bool {
        self.graph.contains_node(block)
    }

    pub fn block(&self, block: NodeIndex) -> Option<&ast::Block> {
        self.graph.node_weight(block)
    }

    pub fn block_mut(&mut self, block: NodeIndex) -> Option<&mut ast::Block> {
        self.graph.node_weight_mut(block)
    }

    pub fn blocks(&self) -> impl Iterator<Item = (NodeIndex, &ast::Block)> {
        self.graph
            .node_indices()
            .map(|i| (i, self.graph.node_weight(i).unwrap()))
    }

    pub fn blocks_mut(&mut self) -> impl Iterator<Item = &mut ast::Block> {
        self.graph.node_weights_mut()
    }

    pub fn successor_blocks(&self, block: NodeIndex) -> Neighbors<BlockEdge> {
        self.graph.neighbors_directed(block, Direction::Outgoing)
    }

    pub fn predecessor_blocks(&self, block: NodeIndex) -> Neighbors<BlockEdge> {
        self.graph.neighbors_directed(block, Direction::Incoming)
    }

    pub fn edges_to_block(&self, node: NodeIndex) -> impl Iterator<Item = (NodeIndex, &BlockEdge)> {
        let mut edges = self.predecessor_blocks(node).detach();
        std::iter::from_fn(move || edges.next_edge(&self.graph)).filter_map(move |e| {
            let (source, target) = self.graph.edge_endpoints(e).unwrap();
            if target == node {
                Some((source, self.graph.edge_weight(e).unwrap()))
            } else {
                None
            }
        })
    }

    pub fn edges(&self, node: NodeIndex) -> impl Iterator<Item = EdgeReference<BlockEdge>> {
        self.graph.edges_directed(node, Direction::Outgoing)
    }

    pub fn remove_edges(&mut self, node: NodeIndex) -> Vec<(NodeIndex, BlockEdge)> {
        let mut edges = Vec::new();
        for (target, edge) in self
            .edges(node)
            .map(|e| (e.target(), e.id()))
            .collect::<Vec<_>>()
        {
            edges.push((target, self.graph.remove_edge(edge).unwrap()));
        }
        edges
    }

    // returns previous edges
    pub fn set_edges(
        &mut self,
        node: NodeIndex,
        new_edges: Vec<(NodeIndex, BlockEdge)>,
    ) -> Vec<(NodeIndex, BlockEdge)> {
        let prev_edges = self.remove_edges(node);
        for (target, edge) in new_edges {
            self.graph.add_edge(node, target, edge);
        }
        prev_edges
    }

    pub fn conditional_edges(
        &self,
        node: NodeIndex,
    ) -> Option<(EdgeReference<BlockEdge>, EdgeReference<BlockEdge>)> {
        let edges = self
            .graph
            .edges_directed(node, Direction::Outgoing)
            .collect::<Vec<_>>();
        if let [e0, e1] = edges[..] {
            let mut res = (e0, e1);
            if res.1.weight().branch_type == BranchType::Then {
                std::mem::swap(&mut res.0, &mut res.1);
            }
            assert!(res.0.weight().branch_type == BranchType::Then);
            assert!(res.1.weight().branch_type == BranchType::Else);
            Some(res)
        } else {
            None
        }
    }

    pub fn unconditional_edge(&self, node: NodeIndex) -> Option<EdgeReference<BlockEdge>> {
        let edges = self
            .graph
            .edges_directed(node, Direction::Outgoing)
            .collect::<Vec<_>>();
        if let [e] = edges[..] {
            Some(e)
        } else {
            None
        }
    }

    // TODO: disable_contracts for production builds
    #[requires(self.has_block(node))]
    pub fn values_read(&self, node: NodeIndex) -> impl Iterator<Item = &RcLocal> {
        self.block(node)
            .unwrap()
            .0
            .iter()
            .flat_map(|s| s.values_read())
            .chain(self.edges(node).flat_map(|e| {
                e.weight()
                    .arguments
                    .iter()
                    .flat_map(|(_, a)| a.values_read())
            }))
    }

    pub fn new_block(&mut self) -> NodeIndex {
        self.graph.add_node(ast::Block::default())
    }

    pub fn remove_block(&mut self, block: NodeIndex) -> Option<ast::Block> {
        self.graph.remove_node(block)
    }
}
