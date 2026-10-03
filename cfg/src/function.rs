use ast::{LocalRw, RcLocal};
use contracts::requires;
use rustc_hash::{FxHashMap, FxHashSet};

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
    /// Bytecode-type naming hints for the locals WRITTEN by lifted statements,
    /// keyed by `(block, statement index, index into values_written())`.  Filled
    /// by the lifter from the compiler's typed-register ranges and consumed
    /// (moved onto the fresh SSA versions) by `ssa::construct`, which renames
    /// every definition exactly once before any statement is inserted or
    /// removed — so the positional keys are only valid until then.
    pub local_type_hints: FxHashMap<(NodeIndex, usize, usize), String>,
    /// Debug interval/name evidence, consumed at definition renaming exactly as
    /// `local_type_hints`. Keys are invalid after SSA construction.
    pub local_source_bindings: FxHashMap<(NodeIndex, usize, usize), Vec<ast::SourceBinding>>,
    /// Source binding visible at a block entry, keyed by original register-local
    /// identity. Only SSA input/phi creation consumes these facts.
    pub entry_source_bindings: FxHashMap<(NodeIndex, RcLocal), ast::SourceBinding>,
    /// For each by-reference captured SSA local, loops whose original close
    /// paths prove that capture uses a fresh iteration cell. An empty set is
    /// significant: coalescing with an unproven capture invalidates a proof.
    /// Populated before SSA erases CLOSEUPVALS; local maps intersect certificates.
    pub iteration_capture_proofs: FxHashMap<RcLocal, FxHashSet<ast::ForId>>,
    /// Captures originating from a generic-for result register, even when SSA
    /// separates a body assignment from the marker result into another local.
    /// Every listed loop needs a matching close-path certificate before source
    /// structuring may introduce an iteration-local declaration for that cell.
    pub iteration_capture_obligations: FxHashMap<RcLocal, FxHashSet<ast::ForId>>,
    /// Captured cells defined in this function, populated by the lifter from
    /// SSA's passed-upvalue groups after destruction. Incoming upvalues are
    /// excluded. A source loop may keep one of these outer bindings while
    /// allocating a private iteration local, if no earlier/inside capture can
    /// observe the delayed export. Empty for callers without this provenance.
    pub local_capture_bindings: FxHashSet<RcLocal>,
    /// Optional diagnostic history, containing IDs rather than RcLocal owners.
    pub provenance: Option<Box<crate::provenance::FunctionTrace>>,
    /// Ids minted while lifting this function (its register locals) and the
    /// first id of the segment it mints versions from; see [`ast::dense`].
    pub lifted_ids: std::ops::Range<u64>,
    pub minted_ids: u64,
    /// The chunk's globals, shared by all its functions: which library
    /// fetches run no script code (see [`ast::ChunkGlobals`]).
    pub globals: std::sync::Arc<ast::ChunkGlobals>,
}

impl Function {
    /// Dense slots for the locals of this function (see [`ast::dense`]).
    pub fn local_index(&self) -> ast::dense::LocalIndex {
        ast::dense::LocalIndex::new(self.lifted_ids.clone(), self.minted_ids)
    }

    pub fn new(id: usize) -> Self {
        Self {
            id,
            name: None,
            parameters: Vec::new(),
            is_variadic: false,
            graph: StableDiGraph::new(),
            entry: None,
            block_pc_ranges: std::collections::HashMap::new(),
            local_type_hints: FxHashMap::default(),
            local_source_bindings: FxHashMap::default(),
            entry_source_bindings: FxHashMap::default(),
            iteration_capture_proofs: FxHashMap::default(),
            iteration_capture_obligations: FxHashMap::default(),
            local_capture_bindings: FxHashSet::default(),
            provenance: None,
            lifted_ids: 0..0,
            minted_ids: 0,
            globals: Default::default(),
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
        // Map preserves stable indices, adjacency order and free-slot lists.
        // Clone each AST weight deeply once; cloning the whole graph first
        // would build and immediately discard a second copy of every expression.
        Self {
            id: self.id,
            name: self.name.clone(),
            parameters: self.parameters.clone(),
            is_variadic: self.is_variadic,
            graph: self.graph.map(
                |_, block| ast::simplify_gotos::deep_clone_block(block),
                |_, edge| edge.clone(),
            ),
            entry: self.entry,
            block_pc_ranges: self.block_pc_ranges.clone(),
            local_type_hints: self.local_type_hints.clone(),
            local_source_bindings: self.local_source_bindings.clone(),
            entry_source_bindings: self.entry_source_bindings.clone(),
            iteration_capture_proofs: self.iteration_capture_proofs.clone(),
            iteration_capture_obligations: self.iteration_capture_obligations.clone(),
            local_capture_bindings: self.local_capture_bindings.clone(),
            provenance: self.provenance.clone(),
            lifted_ids: self.lifted_ids.clone(),
            minted_ids: self.minted_ids,
            globals: self.globals.clone(),
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

    /// Build an immutable inverse index for a batch of provenance queries.
    /// Duplicate starts retain exactly the same first entry as `block_at_pc`,
    /// including hand-built CFGs with overlapping PC metadata.
    pub fn block_start_pc_index(&self) -> FxHashMap<usize, NodeIndex> {
        let mut index = FxHashMap::with_capacity_and_hasher(
            self.block_pc_ranges.len(), Default::default());
        for (&node, range) in &self.block_pc_ranges {
            index.entry(range.start).or_insert(node);
        }
        index
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
        let mut edges = self
            .graph
            .edges_directed(node, Direction::Outgoing);
        let (Some(e0), Some(e1), None) = (edges.next(), edges.next(), edges.next()) else {
            return None;
        };
        match (&e0.weight().branch_type, &e1.weight().branch_type) {
            (BranchType::Then, BranchType::Else) => Some((e0, e1)),
            (BranchType::Else, BranchType::Then) => Some((e1, e0)),
            _ => None,
        }
    }

    pub fn unconditional_edge(&self, node: NodeIndex) -> Option<EdgeReference<BlockEdge>> {
        let mut edges = self
            .graph
            .edges_directed(node, Direction::Outgoing);
        match (edges.next(), edges.next()) {
            (Some(edge), None) => Some(edge),
            _ => None,
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
        self.block_pc_ranges.remove(&block);
        self.graph.remove_node(block)
    }
}

#[cfg(test)]
mod tests {
    use super::Function;
    use crate::block::{BlockEdge, BranchType};
    use ast::{Block, Comment, If, Literal, Statement};
    use petgraph::visit::EdgeRef;

    #[test]
    fn single_copy_deep_clone_matches_reference_origins_owners_and_sparse_graph() {
        use ast::{RValue, Traverse};
        fn origin(origin: &ast::node_origins::Origin) -> String {
            origin.0.as_ref().map_or_else(|| "none".into(), |data| format!(
                "{:?}/{}/{}/{:?}/{}", data.inputs, data.inlined, data.cloned,
                data.synthesized, data.incomplete,
            ))
        }
        fn origins(block: &Block) -> Vec<String> {
            let mut result = Vec::new();
            for statement in block.iter() {
                if let Some(tag) = ast::node_origins::statement(statement) { result.push(origin(tag)); }
                statement.traverse_rvalues_ref(&mut |value| {
                    if let Some(tag) = ast::node_origins::value(value) { result.push(origin(tag)); }
                });
                if let Statement::If(branch) = statement {
                    result.extend(origins(&branch.then_block.lock()));
                    result.extend(origins(&branch.else_block.lock()));
                }
            }
            result
        }
        for seed in 0..32usize {
            let mut original = Function::new(seed);
            original.name = Some(format!("fixture_{seed}"));
            original.is_variadic = seed % 2 == 0;
            let local = ast::RcLocal::default();
            original.parameters.push(local.clone());
            let nodes = (0..9).map(|_| original.new_block()).collect::<Vec<_>>();
            original.set_entry(nodes[0]);
            let closure = ast::Closure { node_origin: Default::default(), function: Default::default(),
                upvalues: vec![ast::Upvalue::Copy(local.clone()), ast::Upvalue::Ref(local.clone())] };
            let closure_owner = closure.function.clone();
            let tag = || ast::node_origins::Origin::input(ast::node_origins::Input {
                function: "p0".into(), block: seed, statement: 3, value: Some(1),
            });
            let mut call = ast::Call::new(ast::Global::from("consume").into(), vec![closure.into(),
                ast::Binary::new(local.clone().into(), ast::Literal::Number(-0.0).into(),
                    ast::BinaryOperation::Add).into()]);
            call.node_origin = tag();
            if let RValue::Binary(binary) = &mut call.arguments[1] { binary.node_origin = tag(); }
            let mut branch = If::new(local.clone().into(), Block(vec![call.into()]), Block::default());
            branch.node_origin = tag();
            original.block_mut(nodes[0]).unwrap().push(branch.into());
            let mut edge_ids = Vec::new();
            for (index, pair) in nodes.windows(2).enumerate() {
                edge_ids.push(original.graph.add_edge(pair[0], pair[1], BlockEdge {
                    branch_type: if index % 2 == 0 { BranchType::Then } else { BranchType::Else },
                    arguments: vec![(local.clone(), local.clone().into())],
                }));
                original.set_block_pc_range(pair[0], index * 3, index * 3 + 2);
            }
            original.graph.remove_edge(edge_ids[seed % edge_ids.len()]);
            original.remove_block(nodes[2 + seed % 5]);
            original.local_type_hints.insert((nodes[0], 0, 0), "number".into());
            original.local_capture_bindings.insert(local.clone());
            original.iteration_capture_proofs.insert(local.clone(), Default::default());
            original.iteration_capture_obligations.insert(local.clone(), Default::default());
            let original_origins = origins(original.block(nodes[0]).unwrap());
            let original_owners = triomphe::Arc::strong_count(&closure_owner.0);
            let next_local = ast::current_local_id();
            let mut expected = original.clone();
            for block in expected.graph.node_weights_mut() {
                *block = ast::simplify_gotos::deep_clone_block(block);
            }
            let expected_owners = triomphe::Arc::strong_count(&closure_owner.0) - original_owners;
            let mut actual = original.deep_clone();
            assert_eq!(triomphe::Arc::strong_count(&closure_owner.0), original_owners + 2 * expected_owners);
            assert_eq!(ast::current_local_id(), next_local);
            assert_eq!(actual.id, expected.id);
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.parameters, expected.parameters);
            assert_eq!(actual.is_variadic, expected.is_variadic);
            assert_eq!(actual.entry, expected.entry);
            assert_eq!(actual.block_pc_ranges, expected.block_pc_ranges);
            assert_eq!(actual.local_type_hints, expected.local_type_hints);
            assert_eq!(actual.local_source_bindings, expected.local_source_bindings);
            assert_eq!(actual.entry_source_bindings, expected.entry_source_bindings);
            assert_eq!(actual.iteration_capture_proofs, expected.iteration_capture_proofs);
            assert_eq!(actual.iteration_capture_obligations, expected.iteration_capture_obligations);
            assert_eq!(actual.local_capture_bindings, expected.local_capture_bindings);
            for ((node, block), (other, expected_block)) in actual.blocks().zip(expected.blocks()) {
                assert_eq!(node, other);
                // If::PartialEq deliberately returns false even for equal
                // trees. Debug exposes shape/closure identity; origins below
                // are compared separately because semantic Debug hides them.
                assert_eq!(format!("{block:?}"), format!("{expected_block:?}"));
                assert_eq!(origins(block), origins(expected_block));
                let edges = |function: &Function| function.edges(node).map(|edge|
                    (edge.id(), edge.target(), edge.weight().branch_type.clone(), edge.weight().arguments.clone()))
                    .collect::<Vec<_>>();
                assert_eq!(edges(&actual), edges(&expected));
            }
            // Probe allocation order after holes: preserving only live topology
            // is insufficient if a later pass reuses different node/edge slots.
            for _ in 0..12 {
                let a = actual.new_block();
                let b = expected.new_block();
                assert_eq!(a, b);
                assert_eq!(actual.graph.add_edge(nodes[0], a, BlockEdge::default()),
                           expected.graph.add_edge(nodes[0], b, BlockEdge::default()));
            }
            if let Statement::If(branch) = &actual.block(nodes[0]).unwrap()[0] {
                branch.then_block.lock().clear();
            }
            assert_eq!(origins(original.block(nodes[0]).unwrap()), original_origins);
            drop(actual);
            drop(expected);
            assert_eq!(triomphe::Arc::strong_count(&closure_owner.0), original_owners);
        }
    }

    #[test]
    fn edge_shape_queries_match_full_collection_with_parallel_edges_and_tags() {
        for count in 0..=5usize {
            for tags in 0..3usize.pow(count as u32) {
                let mut function = Function::new(0);
                let source = function.new_block();
                let target = function.new_block();
                let mut code = tags;
                for _ in 0..count {
                    let branch_type = match code % 3 {
                        0 => BranchType::Unconditional,
                        1 => BranchType::Then,
                        _ => BranchType::Else,
                    };
                    code /= 3;
                    function.graph_mut().add_edge(source, target, BlockEdge::new(branch_type));
                }
                let edges: Vec<_> = function.edges(source).collect();
                let unconditional = match &edges[..] {
                    [edge] => Some(edge.id()),
                    _ => None,
                };
                let conditional = match &edges[..] {
                    [a, b] => match (&a.weight().branch_type, &b.weight().branch_type) {
                        (BranchType::Then, BranchType::Else) => Some((a.id(), b.id())),
                        (BranchType::Else, BranchType::Then) => Some((b.id(), a.id())),
                        _ => None,
                    },
                    _ => None,
                };
                assert_eq!(function.unconditional_edge(source).map(|edge| edge.id()), unconditional);
                assert_eq!(function.conditional_edges(source).map(|(a, b)| (a.id(), b.id())), conditional);
            }
        }
    }

    #[test]
    fn pc_index_matches_scan_with_duplicate_starts_and_range_updates() {
        let mut function = Function::new(0);
        let nodes: Vec<_> = (0..12).map(|_| function.new_block()).collect();
        for (index, &node) in nodes.iter().enumerate() {
            function.set_block_pc_range(node, index / 2, index + 20);
        }
        function.set_block_pc_range(nodes[0], 9, 30);
        function.remove_block(nodes[3]);
        let index = function.block_start_pc_index();
        for pc in 0..32 {
            assert_eq!(index.get(&pc).copied(), function.block_at_pc(pc));
        }
    }

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
