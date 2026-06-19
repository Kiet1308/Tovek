use std::fmt;

use petgraph::{
    stable_graph::{NodeIndex, StableDiGraph},
    visit::{Dfs, EdgeRef, Walker},
    Direction,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{block::BlockEdge, function::Function};

pub trait NodeChecker {
    fn check(&self, node: NodeIndex) -> bool;
}

#[derive(Debug)]
pub struct PatternNode {
    pub allow_external_neighbors: bool,
}

impl PatternNode {
    pub fn new(allow_external_neighbors: bool) -> Self {
        Self {
            allow_external_neighbors,
        }
    }
}

impl fmt::Display for PatternNode {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

pub type PatternEdge = BlockEdge;
pub type PatternGraph = StableDiGraph<PatternNode, PatternEdge>;

#[derive(Debug)]
pub struct Pattern {
    root: NodeIndex,
    graph: PatternGraph,
}

impl Pattern {
    fn new(root: NodeIndex, graph: PatternGraph) -> Self {
        // make sure all nodes in pattern are connected
        assert!(Dfs::new(&graph, root).iter(&graph).count() == graph.node_count());
        Self { root, graph }
    }
}

#[derive(Debug)]
pub struct PatternChecker<'a> {
    pattern: &'a Pattern,
    function: &'a Function,
    visited: FxHashSet<NodeIndex>,
    mapping: FxHashMap<NodeIndex, NodeIndex>,
}

impl<'a> PatternChecker<'a> {
    fn check_successors(&self, _node: NodeIndex) {}

    fn check_pattern_rec(&self, pattern_node: NodeIndex, function_node: NodeIndex) -> bool {
        let _function_successors = self
            .function
            .successor_blocks(function_node)
            .collect::<Vec<_>>();

        let _pattern_successors = self
            .pattern
            .graph
            .neighbors_directed(pattern_node, Direction::Outgoing)
            .collect::<Vec<_>>();

        for function_edge in self.function.edges(pattern_node) {
            let function_node = function_edge.target();
            if let Some(&pattern_successor_node) = self.mapping.get(&function_node) {
                if let Some(_pattern_edge) = self
                    .pattern
                    .graph
                    .find_edge(pattern_node, pattern_successor_node)
                {
                    todo!();
                } else {
                    return false;
                }
            } else {
                todo!();
            }
        }

        true
    }

    fn check_pattern(mut self, root: NodeIndex) -> bool {
        self.mapping.insert(self.pattern.root, root);
        self.check_pattern_rec(self.pattern.root, root)
    }
}

impl Pattern {
    fn node_matches(&self, _function: &Function, _node: NodeIndex) -> bool {
        let _mapping = FxHashMap::<NodeIndex, NodeIndex>::default();

        false
    }

    pub fn match_on(&self, _function: &Function, _node: NodeIndex) -> Option<Match> {
        None
    }
}

#[derive(Debug, Default)]
pub struct Match {
    // pattern node to function node
    pub mapping: FxHashMap<NodeIndex, NodeIndex>,
}
