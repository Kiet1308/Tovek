use ast::Reduce;
use cfg::block::{BlockEdge, BranchType};
use itertools::Itertools;
use parking_lot::Mutex;
use petgraph::visit::EdgeRef;
use triomphe::Arc;
use tuple::Map;

use crate::GraphStructurer;
use petgraph::{algo::dominators::Dominators, stable_graph::NodeIndex};

impl GraphStructurer {
    fn simplify_if(if_stat: &mut ast::If) {
        if let Some(unary) = if_stat.condition.as_unary() {
            if unary.operation == ast::UnaryOperation::Not {
                if_stat.condition = *unary.value.clone();
                std::mem::swap(&mut if_stat.then_block, &mut if_stat.else_block);
            }
        }
    }

    fn expand_if(if_stat: &mut ast::If) -> Option<ast::Block> {
        let mut then_block = if_stat.then_block.lock();
        let mut else_block = if_stat.else_block.lock();
        let then_return = then_block.last().and_then(|x| x.as_return());
        let else_return = else_block.last().and_then(|x| x.as_return());
        if let Some(then_return) = then_return
            && let Some(else_return) = else_return
        {
            if then_return.values.is_empty() && else_return.values.is_empty() {
                // Dropping both `return`s is only sound when this `if` sits at the
                // function tail (falling through == returning). The same `if` may
                // later be absorbed as a nested branch, where a fall-through does
                // NOT return — turning an early exit into continued execution
                // (observed: a guard `if not x then ...; return end` losing its
                // return and then nil-indexing `x`). Keeping the returns is always
                // correct, so leave them in place.
                None
            } else if !then_return.values.is_empty() && else_return.values.is_empty() {
                Some(std::mem::take(&mut else_block))
            } else if then_return.values.is_empty() && !else_return.values.is_empty() {
                let then_block = std::mem::replace::<ast::Block>(
                    &mut then_block,
                    std::mem::take(&mut else_block),
                );
                // TODO: unnecessary clone (also other cases)
                if_stat.condition =
                    ast::Unary::new(if_stat.condition.clone(), ast::UnaryOperation::Not)
                        .reduce_condition();
                Some(then_block)
            } else {
                match then_block.len().cmp(&else_block.len()) {
                    std::cmp::Ordering::Less => Some(std::mem::take(&mut else_block)),
                    std::cmp::Ordering::Greater => {
                        let then_block = std::mem::replace::<ast::Block>(
                            &mut then_block,
                            std::mem::take(&mut else_block),
                        );
                        if_stat.condition =
                            ast::Unary::new(if_stat.condition.clone(), ast::UnaryOperation::Not)
                                .reduce_condition();
                        Some(then_block)
                    }
                    // TODO: `Some(std::mem::take(&mut if_stat.else_block))`?
                    std::cmp::Ordering::Equal => None,
                }
            }
        } else {
            None
        }
    }

    // a -> b -> d + a -> c -> d
    // results in a -> d
    fn match_diamond_conditional(
        &mut self,
        entry: NodeIndex,
        then_node: NodeIndex,
        else_node: NodeIndex,
    ) -> bool {
        let mut then_successors = self.function.successor_blocks(then_node).collect_vec();
        let mut else_successors = self.function.successor_blocks(else_node).collect_vec();

        if then_successors.len() > 1 || else_successors.len() > 1 {
            if self.is_loop_header(entry) {
                // TODO: ugly
                let t = if let Some(index) = then_successors.iter().position(|n| *n == entry) {
                    then_successors.swap_remove(index);
                    true
                } else {
                    false
                };
                let e = if let Some(index) = else_successors.iter().position(|n| *n == entry) {
                    else_successors.swap_remove(index);
                    true
                } else {
                    false
                };
                if (!t && then_successors.len() != 1) || (!e && else_successors.len() != 1) {
                    return false;
                }

                if then_successors != else_successors {
                    return false;
                }

                let mut refine = |n| {
                    let (then_target, else_target) = self
                        .function
                        .conditional_edges(n)
                        .unwrap()
                        .map(|e| e.target());
                    let block = self.function.block_mut(n).unwrap();
                    if let Some(if_stat) = block.last_mut().unwrap().as_if_mut() {
                        if then_target == entry {
                            if_stat.then_block =
                                Arc::new(Mutex::new(vec![ast::Continue {}.into()].into()));
                            true
                        } else if else_target == entry {
                            if_stat.else_block =
                                Arc::new(Mutex::new(vec![ast::Continue {}.into()].into()));
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                let then_changed = if t { refine(then_node) } else { false };
                let else_changed = if e { refine(else_node) } else { false };
                if !then_changed && !else_changed {
                    return false;
                }
                if t && e {
                    assert!(then_changed && else_changed)
                }
            } else {
                return false;
            }
        } else if then_successors != else_successors {
            return false;
        }

        if self.function.predecessor_blocks(then_node).count() != 1
            || self.function.predecessor_blocks(else_node).count() != 1
        {
            return false;
        }

        let then_block = self.function.remove_block(then_node).unwrap();
        let else_block = self.function.remove_block(else_node).unwrap();

        let block = self.function.block_mut(entry).unwrap();
        // TODO: STYLE: rename to r#if?
        let if_stat = block.last_mut().unwrap().as_if_mut().unwrap();
        if_stat.then_block = Arc::new(then_block.into());
        if_stat.else_block = Arc::new(else_block.into());
        Self::simplify_if(if_stat);

        let after = Self::expand_if(if_stat);
        if if_stat.then_block.lock().is_empty() {
            // TODO: unnecessary clone
            if_stat.condition =
                ast::Unary::new(if_stat.condition.clone(), ast::UnaryOperation::Not)
                    .reduce_condition();
            std::mem::swap(&mut if_stat.then_block, &mut if_stat.else_block);
        }
        if let Some(after) = after {
            block.extend(after.0);
        }

        let exit = then_successors.first().cloned();
        if let Some(exit) = exit {
            self.function.set_edges(
                entry,
                vec![(exit, BlockEdge::new(BranchType::Unconditional))],
            );
        } else {
            self.function.remove_edges(entry);
        }
        self.match_jump(entry, exit);

        true
    }

    // a -> b -> c + a -> c
    // results in a -> c
    fn match_triangle_conditional(
        &mut self,
        entry: NodeIndex,
        then_node: NodeIndex,
        else_node: NodeIndex,
    ) -> bool {
        let mut _match_triangle_conditional = |then_node, else_node, inverted| {
            let then_successors = self.function.successor_blocks(then_node).collect_vec();

            if then_successors.len() > 1 {
                return false;
            }

            if self.function.predecessor_blocks(then_node).count() != 1 {
                return false;
            }

            if !then_successors.is_empty() && then_successors[0] != else_node {
                return false;
            }

            let then_block = self.function.remove_block(then_node).unwrap();

            let block = self.function.block_mut(entry).unwrap();
            let if_stat = block.last_mut().unwrap().as_if_mut().unwrap();
            if_stat.then_block = Arc::new(then_block.into());

            if inverted {
                if_stat.condition =
                    ast::Unary::new(if_stat.condition.clone(), ast::UnaryOperation::Not)
                        .reduce_condition()
            }

            //Self::simplify_if(if_stat);

            self.function.set_edges(
                entry,
                vec![(else_node, BlockEdge::new(BranchType::Unconditional))],
            );

            self.match_jump(entry, Some(else_node));

            true
        };

        _match_triangle_conditional(then_node, else_node, false)
            || _match_triangle_conditional(else_node, then_node, true)
    }

    // a -> b a -> c
    pub(crate) fn refine_virtual_edge_jump(
        &mut self,
        post_dom: &Dominators<NodeIndex>,
        entry: NodeIndex,
        node: NodeIndex,
        header: NodeIndex,
        next: Option<NodeIndex>,
    ) -> bool {
        if node == header {
            // TODO: only check back edges?
            if !self
                .function
                .predecessor_blocks(header)
                .filter(|&n| n != entry)
                .any(|n| {
                    post_dom
                        .dominators(entry)
                        .is_some_and(|mut p| p.contains(&n))
                })
            {
                return false;
            }
            let block = &mut self.function.block_mut(entry).unwrap();
            block.push(ast::Continue {}.into());
        } else if Some(node) == next {
            let block = &mut self.function.block_mut(entry).unwrap();
            block.push(ast::Break {}.into());
        }
        self.function.set_edges(entry, vec![]);
        true
    }

    pub(crate) fn refine_virtual_edge_conditional(
        &mut self,
        post_dom: &Dominators<NodeIndex>,
        entry: NodeIndex,
        then_node: NodeIndex,
        else_node: NodeIndex,
        header: NodeIndex,
        next: Option<NodeIndex>,
    ) -> bool {
        let then_main_cont = self
            .function
            .predecessor_blocks(header)
            .filter(|&n| n != entry)
            .any(|n| {
                post_dom
                    .dominators(then_node)
                    .is_some_and(|mut p| p.contains(&n))
            });

        let else_main_cont = self
            .function
            .predecessor_blocks(header)
            .filter(|&n| n != entry)
            .any(|n| {
                post_dom
                    .dominators(else_node)
                    .is_some_and(|mut p| p.contains(&n))
            });

        let mut changed = false;
        let header_successors = self.function.successor_blocks(header).collect_vec();
        let block = self.function.block_mut(entry).unwrap();
        if let Some(if_stat) = block.last_mut().unwrap().as_if_mut() {
            if then_node == header && !header_successors.contains(&entry) && then_main_cont {
                if_stat.then_block = Arc::new(Mutex::new(vec![ast::Continue {}.into()].into()));
                changed = true;
            } else if Some(then_node) == next {
                if_stat.then_block = Arc::new(Mutex::new(vec![ast::Break {}.into()].into()));
                changed = true;
            }
            if else_node == header && !header_successors.contains(&entry) && else_main_cont {
                if_stat.else_block = Arc::new(Mutex::new(vec![ast::Continue {}.into()].into()));
                changed = true;
            } else if Some(else_node) == next {
                if_stat.else_block = Arc::new(Mutex::new(vec![ast::Break {}.into()].into()));
                changed = true;
            }
            if !if_stat.then_block.lock().is_empty() && if_stat.else_block.lock().is_empty() {
                self.function.set_edges(
                    entry,
                    vec![(else_node, BlockEdge::new(BranchType::Unconditional))],
                );
                changed = true;
            } else if if_stat.then_block.lock().is_empty() && !if_stat.else_block.lock().is_empty()
            {
                if_stat.condition =
                    ast::Unary::new(if_stat.condition.clone(), ast::UnaryOperation::Not)
                        .reduce_condition();
                std::mem::swap(&mut if_stat.then_block, &mut if_stat.else_block);
                self.function.set_edges(
                    entry,
                    vec![(then_node, BlockEdge::new(BranchType::Unconditional))],
                );
                changed = true;
            } else if !if_stat.then_block.lock().is_empty() && !if_stat.else_block.lock().is_empty()
            {
                self.function.remove_edges(entry);
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn match_conditional(
        &mut self,
        entry: NodeIndex,
        then_node: NodeIndex,
        else_node: NodeIndex,
    ) -> bool {
        let block = self.function.block_mut(entry).unwrap();
        if block.last_mut().unwrap().as_if_mut().is_none() {
            // for loops
            return false;
        }

        self.match_diamond_conditional(entry, then_node, else_node)
            || self.match_triangle_conditional(entry, then_node, else_node)
    }
}
