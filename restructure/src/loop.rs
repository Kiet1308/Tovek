use array_tool::vec::Intersect;
use ast::{Reduce, SideEffects};
use cfg::block::{BlockEdge, BranchType};
use itertools::Itertools;
use rustc_hash::FxHashSet;
use tuple::Map;

use crate::GraphStructurer;
use petgraph::{algo::dominators::Dominators, stable_graph::NodeIndex, visit::EdgeRef};

impl GraphStructurer {
    pub(crate) fn is_loop_header(&self, node: NodeIndex) -> bool {
        self.loop_headers.contains(&node)
    }

    pub(crate) fn is_for_next(&self, node: NodeIndex) -> bool {
        self.function
            .block(node)
            .unwrap()
            .first()
            .map(|s| {
                matches!(
                    s,
                    ast::Statement::GenericForNext(_) | ast::Statement::NumForNext(_)
                )
            })
            .unwrap_or(false)
    }

    // TODO: for init should always be at the end of a block?
    fn find_for_init(&mut self, for_loop: NodeIndex) -> (NodeIndex, usize) {
        let predecessors = self
            .function
            .predecessor_blocks(for_loop)
            .filter(|&p| p != for_loop)
            .collect_vec();
        let init_blocks = predecessors.into_iter().filter_map(|p| {
            self.function
                .block_mut(p)
                .unwrap()
                .iter_mut()
                .enumerate()
                .rev()
                // TODO: REFACTOR: this is confusing
                .find(|(_, s)| {
                    s.has_side_effects()
                        || s.as_num_for_init().is_some()
                        || s.as_generic_for_init().is_some()
                })
                .and_then(|(i, s)| {
                    if s.as_num_for_init().is_some() || s.as_generic_for_init().is_some() {
                        Some((p, i))
                    } else {
                        None
                    }
                })
        });
        init_blocks.exactly_one().unwrap()
    }

    pub(crate) fn try_collapse_loop(
        &mut self,
        header: NodeIndex,
        dominators: &Dominators<NodeIndex>,
        post_dom: &Dominators<NodeIndex>,
    ) -> bool {
        if !self.is_loop_header(header) {
            if self.is_for_next(header) {
                // https://github.com/luau-lang/luau/issues/679
                // we cant get rid of the for loop cuz it's feature not a bug

                let (then_node, else_node) = self
                    .function
                    .conditional_edges(header)
                    .unwrap()
                    .map(|e| e.target());
                let then_successors = self.function.successor_blocks(then_node).collect_vec();

                if then_successors.len() > 1 {
                    return false;
                }

                let (init_block, init_index) = self.find_for_init(header);
                if then_node != else_node
                    && self.function.predecessor_blocks(then_node).count() != 1
                {
                    return false;
                }

                let else_successors = self.function.successor_blocks(else_node).collect_vec();
                if !(!then_successors.is_empty() && then_successors[0] == else_node)
                    && !(else_successors.len() == 1 && then_successors[0] == else_successors[0])
                    && !(then_successors[0] == header && else_node == init_block)
                {
                    return false;
                }

                let statement = self.function.block_mut(header).unwrap().pop().unwrap();
                let statements = std::mem::take(&mut self.function.block_mut(header).unwrap().0);

                let body_ast = if then_node == init_block {
                    vec![ast::Break {}.into()].into()
                } else {
                    let mut body_ast = self.function.remove_block(then_node).unwrap();
                    body_ast.extend(statements.iter().cloned());
                    if !matches!(body_ast.last(), Some(ast::Statement::Return(_))) {
                        body_ast.push(ast::Break {}.into());
                    }
                    body_ast
                };
                let init_ast = &mut self.function.block_mut(init_block).unwrap();
                init_ast.extend(statements);
                let new_stat = match statement {
                    ast::Statement::NumForNext(num_for_next) => {
                        let for_init = init_ast.remove(init_index).into_num_for_init().unwrap();
                        ast::NumericFor::new(
                            for_init.counter.1,
                            for_init.limit.1,
                            for_init.step.1,
                            num_for_next.counter.0.as_local().unwrap().clone(),
                            body_ast,
                        )
                        .into()
                    }
                    ast::Statement::GenericForNext(generic_for_next) => {
                        let for_init = init_ast.remove(init_index).into_generic_for_init().unwrap();
                        ast::GenericFor::new(
                            generic_for_next
                                .res_locals
                                .iter()
                                .map(|l| l.as_local().unwrap().clone())
                                .collect(),
                            for_init.0.right,
                            body_ast,
                        )
                        .into()
                    }
                    _ => {
                        unreachable!();
                    }
                };
                init_ast.push(new_stat);
                self.function.remove_block(header);

                self.function.set_edges(
                    init_block,
                    vec![(else_node, BlockEdge::new(BranchType::Unconditional))],
                );

                self.match_jump(init_block, Some(else_node));
                return true;
            }
            return false;
        }

        let successors = self.function.successor_blocks(header).collect::<Vec<_>>();
        if successors.contains(&header) {
            if !self.is_for_next(header) {
                if successors.len() == 2 {
                    let if_stat = self
                        .function
                        .block_mut(header)
                        .unwrap()
                        .pop()
                        .unwrap()
                        .into_if()
                        .unwrap();
                    let mut condition = if_stat.condition;
                    let (then_edge, else_edge) = self.function.conditional_edges(header).unwrap();
                    let next = if then_edge.target() == header {
                        condition =
                            ast::Unary::new(condition, ast::UnaryOperation::Not).reduce_condition();
                        else_edge.target()
                    } else {
                        then_edge.target()
                    };
                    let header_block = self.function.block_mut(header).unwrap();
                    *header_block = if header_block.is_empty() {
                        vec![ast::While::new(
                            ast::Unary::new(condition, ast::UnaryOperation::Not).reduce_condition(),
                            header_block.clone(),
                        )
                        .into()]
                        .into()
                    } else {
                        vec![ast::Repeat::new(condition, header_block.clone()).into()].into()
                    };
                    self.function.set_edges(
                        header,
                        vec![(next, BlockEdge::new(BranchType::Unconditional))],
                    );
                    self.match_jump(header, Some(next));
                } else {
                    let header_block = self.function.block_mut(header).unwrap();
                    *header_block = vec![ast::While::new(
                        ast::Literal::Boolean(true).into(),
                        header_block.clone(),
                    )
                    .into()]
                    .into();
                    self.function.remove_edges(header);
                    self.match_jump(header, None);
                }
            } else {
                let next = match successors.len() {
                    1 => None,
                    2 => {
                        let (then_edge, else_edge) =
                            self.function.conditional_edges(header).unwrap();
                        assert!(then_edge.target() == header);
                        Some(else_edge.target())
                    }
                    _ => unreachable!(),
                };
                let statement = self.function.block_mut(header).unwrap().pop().unwrap();
                let statements = std::mem::take(&mut self.function.block_mut(header).unwrap().0);

                let (init_block, init_index) = self.find_for_init(header);

                let body_ast: ast::Block = statements.to_vec().into();
                let init_ast = &mut self.function.block_mut(init_block).unwrap();
                init_ast.extend(statements);
                let new_stat = match statement {
                    ast::Statement::NumForNext(num_for_next) => {
                        let for_init = init_ast.remove(init_index).into_num_for_init().unwrap();
                        ast::NumericFor::new(
                            for_init.counter.1,
                            for_init.limit.1,
                            for_init.step.1,
                            num_for_next.counter.0.as_local().unwrap().clone(),
                            body_ast,
                        )
                        .into()
                    }
                    ast::Statement::GenericForNext(generic_for_next) => {
                        let for_init = init_ast.remove(init_index).into_generic_for_init().unwrap();
                        ast::GenericFor::new(
                            generic_for_next
                                .res_locals
                                .iter()
                                .map(|l| l.as_local().unwrap().clone())
                                .collect(),
                            for_init.0.right,
                            body_ast,
                        )
                        .into()
                    }
                    _ => {
                        unreachable!();
                    }
                };
                init_ast.push(new_stat);
                self.function.remove_block(header);

                // TODO: REFACTOR: make a seperate function that set_edges unconditional
                // and calls match_jump
                // remove edges do the same
                if let Some(next) = next {
                    self.function.set_edges(
                        init_block,
                        vec![(next, BlockEdge::new(BranchType::Unconditional))],
                    );
                } else {
                    self.function.remove_edges(init_block);
                }
                self.match_jump(init_block, next);
            }

            true
        } else if successors.len() == 2 {
            //if successors.iter().find(|s| self.function.successor_blocks(s).exactly_one() == Ok())
            let (mut next, mut body) = (successors[0], successors[1]);
            if post_dom.immediate_dominator(header) == Some(body) {
                std::mem::swap(&mut next, &mut body);
            }
            assert!(body != header);

            if self
                .function
                .predecessor_blocks(body)
                .filter(|&p| p != body)
                .count()
                != 1
            {
                std::mem::swap(&mut next, &mut body);
                if self
                    .function
                    .predecessor_blocks(body)
                    .filter(|&p| p != body)
                    .count()
                    != 1
                {
                    return false;
                }
            }
            let continues = self
                .function
                .predecessor_blocks(header)
                // TODO: the line below fixes `for i = 1, 10 do end`, but a different approach might be preferable
                .filter(|&n| n != header)
                .filter(|&n| {
                    dominators
                        .dominators(n)
                        .map(|mut x| x.contains(&header))
                        .unwrap_or(false)
                })
                .collect_vec();

            let mut changed = false;
            let common_post_doms = post_dom
                .dominators(body)
                .map(|d| d.collect_vec())
                .unwrap_or_default()
                .intersect(
                    post_dom
                        .dominators(next)
                        .map(|d| d.collect_vec())
                        .unwrap_or_default(),
                );
            // TODO: add for next support?
            if !self.is_for_next(header)
                && let Some(new_next) = common_post_doms.into_iter().find(|&p| {
                    self.function.has_block(p)
                        && continues
                            .iter()
                            .all(|&n| post_dom.dominators(n).unwrap().contains(&p))
                })
                && new_next != next
            {
                // TODO: this is uh, yeah
                next = new_next;
                let condition_block = self.function.new_block();
                body = condition_block;
                let mut new_header_block = ast::Block::default();
                new_header_block.push(
                    ast::If::new(
                        ast::Literal::Boolean(true).into(),
                        ast::Block::default(),
                        ast::Block::default(),
                    )
                    .into(),
                );
                *self.function.block_mut(condition_block).unwrap() =
                    std::mem::replace(self.function.block_mut(header).unwrap(), new_header_block);
                let edges = self.function.remove_edges(header);
                self.function.set_edges(condition_block, edges);
                self.function.set_edges(
                    header,
                    vec![
                        (body, BlockEdge::new(BranchType::Then)),
                        (next, BlockEdge::new(BranchType::Else)),
                    ],
                );
                changed = true;
            }

            let breaks = self
                .function
                .predecessor_blocks(next)
                .filter(|&n| n != header)
                .filter(|&n| dominators.dominators(n).unwrap().contains(&body))
                .collect_vec();
            //println!("breaks: {:?}", breaks);

            // TODO: is this needed?
            if self
                .function
                .predecessor_blocks(header)
                .filter(|&n| n != header)
                .any(|n| {
                    !dominators
                        .dominators(n)
                        .is_some_and(|mut d| d.contains(&body))
                        && dominators
                            .dominators(n)
                            .is_some_and(|mut d| d.contains(&header))
                        && dominators
                            .dominators(n)
                            .is_some_and(|mut d| d.contains(&next))
                })
                && self.function.successor_blocks(body).exactly_one().ok() != Some(header)
            {
                return changed;
            }

            let next = if self.function.successor_blocks(body).exactly_one().ok() == Some(header)
                || post_dom
                    .dominators(header)
                    .is_some_and(|mut p| p.contains(&next))
            {
                Some(next)
            } else {
                None
            };
            for node in breaks
                .into_iter()
                .chain(continues)
                .collect::<FxHashSet<_>>()
            {
                if let Some((then_edge, else_edge)) = self.function.conditional_edges(node) {
                    changed |= self.refine_virtual_edge_conditional(
                        post_dom,
                        node,
                        then_edge.target(),
                        else_edge.target(),
                        header,
                        next,
                    );
                } else if let Some(edge) = self.function.unconditional_edge(node) {
                    changed |=
                        self.refine_virtual_edge_jump(post_dom, node, edge.target(), header, next);
                } else {
                    unreachable!();
                }
            }

            if self.function.successor_blocks(body).exactly_one().ok() == Some(header)
                && let Some(next) = next
            {
                let statement = self.function.block_mut(header).unwrap().pop().unwrap();
                if let ast::Statement::If(if_stat) = statement {
                    let mut if_condition = if_stat.condition;
                    let header_else_target =
                        self.function.conditional_edges(header).unwrap().1.target();
                    let block = self.function.remove_block(body).unwrap();

                    let while_stat = if !self.function.block_mut(header).unwrap().is_empty() {
                        let mut body_block =
                            std::mem::take(self.function.block_mut(header).unwrap());
                        if header_else_target != body {
                            // TODO: is this correct?
                            if_condition = ast::Unary::new(if_condition, ast::UnaryOperation::Not)
                                .reduce_condition();
                        }
                        body_block.push(
                            ast::If::new(
                                if_condition,
                                vec![ast::Break {}.into()].into(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        body_block.extend(block.0);

                        ast::While::new(ast::Literal::Boolean(true).into(), body_block)
                    } else {
                        if header_else_target == body {
                            if_condition = ast::Unary::new(if_condition, ast::UnaryOperation::Not)
                                .reduce_condition();
                        }

                        ast::While::new(if_condition, block)
                    };

                    self.function
                        .block_mut(header)
                        .unwrap()
                        .push(while_stat.into());
                    self.function.set_edges(
                        header,
                        vec![(next, BlockEdge::new(BranchType::Unconditional))],
                    );
                    self.match_jump(header, Some(next));
                    return true;
                } else {
                    let statements =
                        std::mem::take(&mut self.function.block_mut(header).unwrap().0);
                    let (init_block, init_index) = self.find_for_init(header);

                    let mut body_ast = self.function.remove_block(body).unwrap();
                    body_ast.extend(statements.iter().cloned());
                    let init_ast = &mut self.function.block_mut(init_block).unwrap();
                    init_ast.extend(statements);
                    let new_stat = match statement {
                        ast::Statement::NumForNext(num_for_next) => {
                            let for_init = init_ast.remove(init_index).into_num_for_init().unwrap();
                            ast::NumericFor::new(
                                for_init.counter.1,
                                for_init.limit.1,
                                for_init.step.1,
                                num_for_next.counter.0.as_local().unwrap().clone(),
                                body_ast,
                            )
                            .into()
                        }
                        ast::Statement::GenericForNext(generic_for_next) => {
                            let for_init =
                                init_ast.remove(init_index).into_generic_for_init().unwrap();
                            ast::GenericFor::new(
                                generic_for_next
                                    .res_locals
                                    .iter()
                                    .map(|l| l.as_local().unwrap().clone())
                                    .collect(),
                                for_init.0.right,
                                body_ast,
                            )
                            .into()
                        }
                        _ => {
                            unreachable!();
                        }
                    };
                    init_ast.push(new_stat);
                    self.function.remove_block(header);

                    // TODO: REFACTOR: make a seperate function that set_edges unconditional
                    // and calls match_jump
                    self.function.set_edges(
                        init_block,
                        vec![(next, BlockEdge::new(BranchType::Unconditional))],
                    );
                    self.match_jump(init_block, Some(next));
                    return true;
                }
            }
            changed
        } else if let Ok(&body) = successors.iter().exactly_one()
            && self
                .function
                .successor_blocks(body)
                .exactly_one()
                .is_ok_and(|s| s == header)
        {
            let block = self.function.remove_block(body).unwrap();

            let mut body_block = std::mem::take(self.function.block_mut(header).unwrap());
            body_block.extend(block.0);

            self.function
                .block_mut(header)
                .unwrap()
                .push(ast::While::new(ast::Literal::Boolean(true).into(), body_block).into());
            self.function.set_edges(header, Vec::new());
            true
        } else {
            false
        }
    }
}
