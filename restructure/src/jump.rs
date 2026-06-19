use ast::SideEffects;
use cfg::block::{BlockEdge, BranchType};
use itertools::Itertools;
use petgraph::{
    algo::dominators::Dominators,
    stable_graph::NodeIndex,
    visit::{EdgeRef, IntoEdgeReferences},
    Direction,
};

impl super::GraphStructurer {
    // TODO: STYLE: better name
    // TODO: this is the same as in structuring.rs but w/o block params
    // maybe we can use the same function?
    pub(crate) fn try_remove_unnecessary_condition(&mut self, node: NodeIndex) -> bool {
        let block = self.function.block(node).unwrap();
        if !block.is_empty()
            && block.last().unwrap().as_if().is_some()
            && let Some((then_edge, else_edge)) = self.function.conditional_edges(node)
            && then_edge.target() == else_edge.target()
        {
            let target = then_edge.target();
            let cond = self
                .function
                .block_mut(node)
                .unwrap()
                .pop()
                .unwrap()
                .into_if()
                .unwrap()
                .condition;

            let new_stat = match cond {
                ast::RValue::Call(call) => Some(call.into()),
                ast::RValue::MethodCall(method_call) => Some(method_call.into()),
                cond if cond.has_side_effects() => Some(
                    ast::Assign {
                        left: vec![ast::RcLocal::default().into()],
                        right: vec![cond],
                        prefix: true,
                        parallel: false,
                    }
                    .into(),
                ),
                _ => None,
            };
            self.function.block_mut(node).unwrap().extend(new_stat);
            self.function.set_edges(
                node,
                vec![(target, BlockEdge::new(BranchType::Unconditional))],
            );
            true
        } else {
            false
        }
    }

    pub(crate) fn match_jump(&mut self, node: NodeIndex, target: Option<NodeIndex>) -> bool {
        if let Some(target) = target {
            if node == target {
                return false;
            }
            if !self.is_for_next(node) {
                assert!(self.function.unconditional_edge(node).is_some());
                if Self::block_is_no_op(self.function.block(node).unwrap())
                    && self.function.entry() != &Some(node)
                    && !self.is_loop_header(node)
                {
                    for (source, edge) in self
                        .function
                        .graph()
                        .edges_directed(node, Direction::Incoming)
                        .map(|e| (e.source(), e.id()))
                        .collect::<Vec<_>>()
                    {
                        let edge = self.function.graph_mut().remove_edge(edge).unwrap();
                        self.function.graph_mut().add_edge(source, target, edge);
                        self.try_remove_unnecessary_condition(source);
                    }
                    self.function.remove_block(node);
                    true
                } else if self.function.predecessor_blocks(target).count() == 1
                    && !self.function.edges_to_block(node).any(|(t, _)| t == target)
                    && !self
                        .function
                        .edges_to_block(target)
                        .any(|(t, _)| t == target)
                {
                    if self.function.entry() != &Some(target)
                        && !self.is_loop_header(target)
                        && !self.is_for_next(target)
                    {
                        let edges = self.function.remove_edges(target);
                        let block = self.function.remove_block(target).unwrap();
                        self.function.block_mut(node).unwrap().extend(block.0);
                        self.function.set_edges(node, edges);
                        true
                    } else if self.function.entry() != &Some(node) && !self.is_loop_header(node) {
                        // TODO: test
                        for (source, edge) in self
                            .function
                            .graph()
                            .edges_directed(node, Direction::Incoming)
                            .map(|e| (e.source(), e.id()))
                            .collect::<Vec<_>>()
                        {
                            let edge = self.function.graph_mut().remove_edge(edge).unwrap();
                            self.function.graph_mut().add_edge(source, target, edge);
                            self.try_remove_unnecessary_condition(source);
                        }
                        let mut block = self.function.remove_block(node).unwrap();
                        block.extend(std::mem::take(self.function.block_mut(target).unwrap()).0);
                        *self.function.block_mut(target).unwrap() = block;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        }
        // node is terminating
        // TODO: block_is_no_op returns true for blocks with comments, do we wanna remove the block if it has comments?
        else if Self::block_is_no_op(self.function.block(node).unwrap())
            && self.function.entry() != &Some(node)
            && !self.is_loop_header(node)
            && !self.is_for_next(node)
        {
            let mut invalid = false;
            for pred in self.function.predecessor_blocks(node).collect_vec() {
                if self.function.successor_blocks(pred).collect_vec().len() != 1 {
                    invalid = true;
                    break;
                }
            }
            if !invalid {
                for edge in self
                    .function
                    .graph()
                    .edges_directed(node, Direction::Incoming)
                    .map(|e| e.id())
                    .collect::<Vec<_>>()
                {
                    assert_eq!(
                        self.function
                            .graph_mut()
                            .remove_edge(edge)
                            .unwrap()
                            .branch_type,
                        BranchType::Unconditional
                    );
                }
                self.function.remove_block(node);
                true
            } else {
                false
            }
        } else {
            false
        }
    }
}
