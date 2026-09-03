use ast::{LocalRw, RcLocal};
use contracts::requires;

use petgraph::{
    Direction,
    stable_graph::{EdgeReference, Neighbors, NodeIndex, StableDiGraph},
    visit::{EdgeRef, IntoEdgesDirected},
};

use crate::block::{BlockEdge, BranchType};

/// Optional bytecode PC envelope for a lifted CFG block.  Hand-built CFGs may
/// leave this unset; production Luau lifting records it so provenance-seeded
/// region proofs can distinguish a loop body from an outer continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPcRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Function {
    pub id: usize,
    pub name: Option<String>,
    pub parameters: Vec<RcLocal>,
    pub is_variadic: bool,
    graph: StableDiGraph<ast::Block, BlockEdge>,
    entry: Option<NodeIndex>,
    block_pc_ranges: std::collections::HashMap<NodeIndex, BlockPcRange>,
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
            block_pc_ranges: std::collections::HashMap::new(),
        }
    }

    /// Clone this CFG while detaching every mutable structured block nested in
    /// its AST node weights.
    ///
    /// `Function::clone` is intentionally shallow for AST `Arc<Mutex<Block>>`
    /// fields.  That is normally cheap and correct, but speculative source-like
    /// and fallback structurers must never share those containers: either pass
    /// may consume or rewrite a nested branch after the other pass has already
    /// built an output tree.  Locals and closure-function identities remain
    /// shared, exactly as they do for ordinary AST cloning.
    pub fn deep_clone(&self) -> Self {
        let mut cloned = self.clone();
        for block in cloned.graph.node_weights_mut() {
            *block = ast::simplify_gotos::deep_clone_block(block);
        }
        cloned
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

    pub fn set_block_pc_range(&mut self, block: NodeIndex, start: usize, end: usize) {
        if self.has_block(block) {
            self.block_pc_ranges
                .insert(block, BlockPcRange { start, end });
        }
    }

    pub fn block_pc_range(&self, block: NodeIndex) -> Option<BlockPcRange> {
        self.block_pc_ranges.get(&block).copied()
    }

    pub fn block_at_pc(&self, pc: usize) -> Option<NodeIndex> {
        self.block_pc_ranges.iter().find_map(|(node, range)| {
            (range.start == pc).then_some(*node)
        })
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
        let [e0, e1] = edges[..] else {
            return None;
        };
        match (&e0.weight().branch_type, &e1.weight().branch_type) {
            (BranchType::Then, BranchType::Else) => Some((e0, e1)),
            (BranchType::Else, BranchType::Then) => Some((e1, e0)),
            _ => None,
        }
    }

    pub fn unconditional_edge(&self, node: NodeIndex) -> Option<EdgeReference<BlockEdge>> {
        let edges = self
            .graph
            .edges_directed(node, Direction::Outgoing)
            .collect::<Vec<_>>();
        if let [e] = edges[..] { Some(e) } else { None }
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
        self.block_pc_ranges.remove(&block);
        self.graph.remove_node(block)
    }
}

#[cfg(test)]
mod tests {
    use super::Function;
    use ast::{Block, Comment, If, Literal, Statement};

    #[test]
    fn deep_clone_detaches_nested_structured_blocks() {
        let mut original = Function::new(0);
        let node = original.new_block();
        original.block_mut(node).unwrap().push(
            If::new(
                Literal::Boolean(true).into(),
                Block::default(),
                Block::default(),
            )
            .into(),
        );

        let mut cloned = original.deep_clone();
        let statement = cloned.block_mut(node).unwrap().first_mut().unwrap();
        let Statement::If(if_statement) = statement else {
            panic!("test block must contain an If");
        };
        if_statement
            .then_block
            .lock()
            .push(Comment::new("clone-only".to_string()).into());

        let original_statement = original.block(node).unwrap().first().unwrap();
        let Statement::If(original_if) = original_statement else {
            panic!("test block must contain an If");
        };
        assert!(original_if.then_block.lock().is_empty());
    }
}
