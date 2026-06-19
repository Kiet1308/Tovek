use std::collections::BTreeMap;

use array_tool::vec::Intersect;
use by_address::ByAddress;
use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;
use parking_lot::Mutex;
use petgraph::{
    algo::dominators::simple_fast,
    prelude::{DiGraph, NodeIndex},
    Direction,
};
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Assign, Block, LocalRw, RcLocal, Statement};

#[derive(Default)]
pub struct LocalDeclarer {
    block_to_node: FxHashMap<ByAddress<Arc<Mutex<Block>>>, NodeIndex>,
    graph: DiGraph<(Option<Arc<Mutex<Block>>>, usize), ()>,
    local_usages: IndexMap<RcLocal, FxHashMap<NodeIndex, usize>>,
    declarations: FxHashMap<ByAddress<Arc<Mutex<Block>>>, BTreeMap<usize, IndexSet<RcLocal>>>,
}

impl LocalDeclarer {
    fn record_usage(
        &mut self,
        node: NodeIndex,
        stat_index: usize,
        local: &RcLocal,
        locals_declared_by_scope: &FxHashSet<RcLocal>,
    ) {
        if locals_declared_by_scope.contains(local) {
            return;
        }
        self.local_usages
            .entry(local.clone())
            .or_default()
            .entry(node)
            .and_modify(|old| *old = (*old).min(stat_index))
            .or_insert(stat_index);
    }

    fn visit(
        &mut self,
        block: Arc<Mutex<Block>>,
        stat_index: usize,
        locals_declared_by_scope: &FxHashSet<RcLocal>,
    ) -> NodeIndex {
        let node = self.graph.add_node((Some(block.clone()), stat_index));
        self.block_to_node.insert(block.clone().into(), node);
        for (stat_index, stat) in block.lock().iter().enumerate() {
            for local in stat.values_read() {
                self.record_usage(node, stat_index, local, locals_declared_by_scope);
            }

            // for loops already declare their own locals.
            if !matches!(stat, Statement::GenericFor(_) | Statement::NumericFor(_)) {
                for local in stat.values_written() {
                    self.record_usage(node, stat_index, local, locals_declared_by_scope);
                }
            }

            match stat {
                Statement::If(r#if) => {
                    let if_node = self.graph.add_node((None, stat_index));
                    self.graph.add_edge(node, if_node, ());
                    let then_node =
                        self.visit(r#if.then_block.clone(), stat_index, locals_declared_by_scope);
                    self.graph.add_edge(if_node, then_node, ());
                    let else_node =
                        self.visit(r#if.else_block.clone(), stat_index, locals_declared_by_scope);
                    self.graph.add_edge(if_node, else_node, ());
                }
                Statement::While(r#while) => {
                    let child =
                        self.visit(r#while.block.clone(), stat_index, locals_declared_by_scope);
                    self.graph.add_edge(node, child, ());
                }
                Statement::Repeat(repeat) => {
                    let child =
                        self.visit(r#repeat.block.clone(), stat_index, locals_declared_by_scope);
                    self.graph.add_edge(node, child, ());
                }
                Statement::NumericFor(numeric_for) => {
                    let mut child_scope = locals_declared_by_scope.clone();
                    child_scope.insert(numeric_for.counter.clone());
                    let child = self.visit(r#numeric_for.block.clone(), stat_index, &child_scope);
                    self.graph.add_edge(node, child, ());
                }
                Statement::GenericFor(generic_for) => {
                    let mut child_scope = locals_declared_by_scope.clone();
                    child_scope.extend(generic_for.res_locals.iter().cloned());
                    let child = self.visit(r#generic_for.block.clone(), stat_index, &child_scope);
                    self.graph.add_edge(node, child, ());
                }
                _ => {}
            }
        }
        node
    }

    fn insertion_index_for_usage(
        &self,
        common_dominator: NodeIndex,
        usage_node: NodeIndex,
        usage_stat_index: usize,
    ) -> usize {
        if usage_node == common_dominator {
            return usage_stat_index;
        }

        let mut child = usage_node;
        loop {
            let parent = self
                .graph
                .neighbors_directed(child, Direction::Incoming)
                .exactly_one()
                .unwrap();
            if parent == common_dominator {
                return self.graph.node_weight(child).unwrap().1;
            }
            child = parent;
        }
    }

    pub fn declare_locals(
        mut self,
        root_block: Arc<Mutex<Block>>,
        locals_to_ignore: &FxHashSet<RcLocal>,
    ) {
        let root_node = self.visit(root_block, 0, &FxHashSet::default());
        let dominators = simple_fast(&self.graph, root_node);
        let local_usages = std::mem::take(&mut self.local_usages);
        for (local, usages) in local_usages {
            if locals_to_ignore.contains(&local) {
                continue;
            }
            let (mut node, mut first_stat_index) = if usages.len() == 1 {
                usages.into_iter().next().unwrap()
            } else {
                let node_dominators = usages
                    .keys()
                    .map(|&n| dominators.dominators(n).unwrap().collect_vec())
                    .collect_vec();
                let mut dom_iter = node_dominators.iter().cloned();
                let mut common_dominators = dom_iter.next().unwrap();
                for node_dominators in dom_iter {
                    common_dominators = common_dominators.intersect(node_dominators);
                }
                let common_dominator = common_dominators[0];
                let first_stat_index = usages
                    .iter()
                    .map(|(&usage_node, &usage_stat_index)| {
                        self.insertion_index_for_usage(
                            common_dominator,
                            usage_node,
                            usage_stat_index,
                        )
                    })
                    .min()
                    .unwrap();
                (common_dominator, first_stat_index)
            };
            while let (block, parent_stat_index) = self.graph.node_weight(node).unwrap()
                && block.is_none()
            {
                let parent = self
                    .graph
                    .neighbors_directed(node, Direction::Incoming)
                    .exactly_one()
                    .unwrap();
                (node, first_stat_index) = (parent, *parent_stat_index);
            }
            let block = self
                .graph
                .node_weight(node)
                .unwrap()
                .0
                .as_ref()
                .unwrap()
                .clone();
            self.declarations
                .entry(block.into())
                .or_default()
                .entry(first_stat_index)
                .or_default()
                .insert(local);
        }

        for (ByAddress(block), declarations) in self.declarations {
            let mut block = block.lock();
            for (stat_index, mut locals) in declarations.into_iter().rev() {
                match &mut block[stat_index] {
                    Statement::Assign(assign)
                        if assign
                            .left
                            .iter()
                            .all(|l| l.as_local().is_some_and(|l| locals.contains(l))) =>
                    {
                        let left_locals = assign
                            .left
                            .iter()
                            .map(|l| l.as_local().unwrap())
                            .collect_vec();
                        let reads_declared_local = assign
                            .right
                            .iter()
                            .flat_map(|r| r.values_read())
                            .any(|r| left_locals.contains(&r));
                        if !reads_declared_local {
                            locals.retain(|l| !left_locals.contains(&l));
                            assign.prefix = true;
                        }
                    }
                    _ => {}
                }
                if !locals.is_empty() {
                    let mut declaration =
                        Assign::new(locals.into_iter().map(|l| l.into()).collect_vec(), vec![]);
                    declaration.prefix = true;
                    block.insert(stat_index, declaration.into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LocalDeclarer;
    use crate::{
        Assign, Block, Call, Global, LValue, Literal, Local, NumericFor, RValue, RcLocal, Statement,
        While,
    };
    use parking_lot::Mutex;
    use rustc_hash::FxHashSet;
    use triomphe::Arc;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_string())))
    }

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn assign_local(local: &RcLocal, value: RValue) -> Statement {
        Assign::new(vec![LValue::Local(local.clone())], vec![value]).into()
    }

    fn print_local(local: &RcLocal) -> Statement {
        Call::new(global("print"), vec![RValue::Local(local.clone())]).into()
    }

    #[test]
    fn declares_before_child_block_write_when_parent_reads_later() {
        let sound = local("sound");
        let root = Arc::new(Mutex::new(Block(vec![While::new(
            Literal::Boolean(true).into(),
            Block(vec![
                While::new(
                    Literal::Boolean(true).into(),
                    Block(vec![assign_local(&sound, number(1.0))]),
                )
                .into(),
                print_local(&sound),
            ]),
        )
        .into()])));

        LocalDeclarer::default().declare_locals(root.clone(), &FxHashSet::default());

        let root = root.lock();
        let outer = root[0].as_while().unwrap();
        let outer_block = outer.block.lock();
        assert!(
            matches!(&outer_block[0], Statement::Assign(assign)
                if assign.prefix
                    && assign.left == [LValue::Local(sound.clone())]
                    && assign.right.is_empty()),
            "cross-block local must be declared before the child loop:\n{}",
            *root
        );

        let inner = outer_block[1].as_while().unwrap();
        let inner_block = inner.block.lock();
        assert!(
            matches!(&inner_block[0], Statement::Assign(assign)
                if !assign.prefix && assign.left == [LValue::Local(sound.clone())]),
            "inner assignment should write the hoisted local, not redeclare it:\n{}",
            *root
        );
    }

    #[test]
    fn does_not_declare_for_loop_locals_from_body_reads() {
        let i = local("i");
        let root = Arc::new(Mutex::new(Block(vec![NumericFor::new(
            number(1.0),
            number(3.0),
            number(1.0),
            i.clone(),
            Block(vec![print_local(&i)]),
        )
        .into()])));

        LocalDeclarer::default().declare_locals(root.clone(), &FxHashSet::default());

        let root = root.lock();
        let numeric_for = root[0].as_numeric_for().unwrap();
        let body = numeric_for.block.lock();
        assert_eq!(
            body.len(),
            1,
            "for-loop locals are scoped by the loop header and must not be redeclared:\n{}",
            *root
        );
    }

    #[test]
    fn splits_self_referential_local_initializer() {
        let object = local("object");
        let root = Arc::new(Mutex::new(Block(vec![assign_local(
            &object,
            RValue::Local(object.clone()),
        )])));

        LocalDeclarer::default().declare_locals(root.clone(), &FxHashSet::default());

        let root = root.lock();
        assert!(
            matches!(&root[0], Statement::Assign(assign)
                if assign.prefix
                    && assign.left == [LValue::Local(object.clone())]
                    && assign.right.is_empty()),
            "self-referential initializer must be predeclared:\n{}",
            *root
        );
        assert!(
            matches!(&root[1], Statement::Assign(assign)
                if !assign.prefix
                    && assign.left == [LValue::Local(object.clone())]
                    && assign.right == [RValue::Local(object.clone())]),
            "initializer assignment must stay non-local so RHS sees the declared local:\n{}",
            *root
        );
    }
}
