use std::collections::BTreeMap;

use by_address::ByAddress;
use crate::{FxIndexMap as IndexMap, FxIndexSet as IndexSet};
use itertools::Itertools;
use parking_lot::Mutex;
use petgraph::{
    prelude::{DiGraph, NodeIndex},
    Direction,
};
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Assign, Block, LocalRw, RcLocal, Statement};

#[derive(Default)]
pub struct LocalDeclarer {
    graph: DiGraph<(Option<Arc<Mutex<Block>>>, usize), ()>,
    local_usages: IndexMap<RcLocal, FxHashMap<NodeIndex, usize>>,
    declarations: FxHashMap<ByAddress<Arc<Mutex<Block>>>, BTreeMap<usize, IndexSet<RcLocal>>>,
}

/// The declaration graph is a lexical tree, including synthetic if nodes.
/// Binary lifting gives both common scopes and their first child on a use path
/// in O(log scopes), without general dominators or ancestor-vector intersections.
struct ScopeAncestors {
    depth: Vec<usize>,
    jumps: Vec<Vec<NodeIndex>>,
}

impl ScopeAncestors {
    fn new(graph: &DiGraph<(Option<Arc<Mutex<Block>>>, usize), ()>, root: NodeIndex) -> Self {
        let len = graph.node_count();
        let levels = (usize::BITS - len.leading_zeros()) as usize;
        let mut depth = vec![0; len];
        let mut jumps = vec![vec![root; len]; levels];
        // Parents are allocated before children by LocalDeclarer::visit.
        for node in graph.node_indices() {
            if node == root { continue; }
            let parent = graph.neighbors_directed(node, Direction::Incoming).exactly_one().unwrap();
            depth[node.index()] = depth[parent.index()] + 1;
            jumps[0][node.index()] = parent;
            for level in 1..levels {
                jumps[level][node.index()] = jumps[level - 1][jumps[level - 1][node.index()].index()];
            }
        }
        Self { depth, jumps }
    }

    fn lift(&self, mut node: NodeIndex, mut distance: usize) -> NodeIndex {
        while distance != 0 {
            let level = distance.trailing_zeros() as usize;
            node = self.jumps[level][node.index()];
            distance &= distance - 1;
        }
        node
    }

    fn common(&self, mut left: NodeIndex, mut right: NodeIndex) -> NodeIndex {
        if self.depth[left.index()] > self.depth[right.index()] { std::mem::swap(&mut left, &mut right); }
        right = self.lift(right, self.depth[right.index()] - self.depth[left.index()]);
        if left == right { return left; }
        for level in (0..self.jumps.len()).rev() {
            if self.jumps[level][left.index()] != self.jumps[level][right.index()] {
                left = self.jumps[level][left.index()];
                right = self.jumps[level][right.index()];
            }
        }
        self.jumps[0][left.index()]
    }

    fn child_of(&self, ancestor: NodeIndex, descendant: NodeIndex) -> NodeIndex {
        self.lift(descendant, self.depth[descendant.index()] - self.depth[ancestor.index()] - 1)
    }
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
        for (stat_index, stat) in block.lock().iter().enumerate() {
            stat.visit_local_reads(&mut |local| {
                self.record_usage(node, stat_index, local, locals_declared_by_scope);
                true
            });

            // for loops already declare their own locals.
            if !matches!(stat, Statement::GenericFor(_) | Statement::NumericFor(_)) {
                stat.visit_local_writes(&mut |local| {
                    self.record_usage(node, stat_index, local, locals_declared_by_scope);
                    true
                });
            }

            match stat {
                Statement::If(r#if) => {
                    let if_node = self.graph.add_node((None, stat_index));
                    self.graph.add_edge(node, if_node, ());
                    let then_node = self.visit(
                        r#if.then_block.clone(),
                        stat_index,
                        locals_declared_by_scope,
                    );
                    self.graph.add_edge(if_node, then_node, ());
                    let else_node = self.visit(
                        r#if.else_block.clone(),
                        stat_index,
                        locals_declared_by_scope,
                    );
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

    pub fn declare_locals(
        mut self,
        root_block: Arc<Mutex<Block>>,
        locals_to_ignore: &FxHashSet<RcLocal>,
    ) {
        let root_node = self.visit(root_block, 0, &FxHashSet::default());
        let ancestors = ScopeAncestors::new(&self.graph, root_node);
        let local_usages = std::mem::take(&mut self.local_usages);
        for (local, usages) in local_usages {
            if locals_to_ignore.contains(&local) {
                continue;
            }
            let (mut node, mut first_stat_index) = if usages.len() == 1 {
                usages.into_iter().next().unwrap()
            } else {
                let common_dominator = usages.keys().copied().reduce(|left, right| ancestors.common(left, right)).unwrap();
                let first_stat_index = usages
                    .iter()
                    .map(|(&usage_node, &usage_stat_index)| {
                        if usage_node == common_dominator { usage_stat_index }
                        else { self.graph[ancestors.child_of(common_dominator, usage_node)].1 }
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
            apply_declarations(&mut block, declarations);
        }
    }
}

/// Prefix decisions still run in descending original-statement order. Delay
/// physical insertions until those decisions finish so each old statement is
/// shifted at most once, even when many separate declarations are needed.
fn apply_declarations(block: &mut Block, declarations: BTreeMap<usize, IndexSet<RcLocal>>) {
    let mut first = None;
    let mut rest = Vec::new();
    for (stat_index, mut locals) in declarations.into_iter().rev() {
        match &mut block[stat_index] {
            Statement::Assign(assign)
                if assign.left.iter().all(|l| l.as_local().is_some_and(|l| locals.contains(l))) =>
            {
                let left_locals = assign.left.iter().map(|l| l.as_local().unwrap()).collect_vec();
                let reads_declared_local = assign.right.iter().any(|value| {
                    !value.visit_local_reads(&mut |read| !left_locals.contains(&read))
                });
                if !reads_declared_local {
                    locals.retain(|l| !left_locals.contains(&l));
                    assign.prefix = true;
                }
            }
            _ => {}
        }
        if !locals.is_empty() {
            let mut declaration = Assign::new(locals.into_iter().map(|l| l.into()).collect_vec(), vec![]);
            declaration.prefix = true;
            let pending = (stat_index, declaration.into());
            if first.is_none() { first = Some(pending); }
            else { rest.push(pending); }
        }
    }
    if let Some(first) = first {
        crate::telemetry::count("local_declaration_insertions", 1 + rest.len() as u64);
        let shifted = insert_declarations(block, first, rest);
        crate::telemetry::count("local_declaration_shifted_statements", shifted as u64);
    }
}

fn insert_declarations(
    block: &mut Block,
    first: (usize, Statement),
    rest: Vec<(usize, Statement)>,
) -> usize {
    if rest.is_empty() {
        let shifted = block.len() - first.0;
        block.insert(first.0, first.1);
        return shifted;
    }
    let mut read = block.len();
    let old_len = read;
    let mut write = read + 1 + rest.len();
    block.resize_with(write, || Statement::Empty(crate::Empty {}));
    // The unused suffix consists of empty slots. Swaps move those holes back
    // toward the next insertion while moving actual statements only forward.
    for (index, statement) in std::iter::once(first).chain(rest) {
        debug_assert!(index < read);
        while read > index {
            read -= 1;
            write -= 1;
            block.swap(read, write);
        }
        write -= 1;
        block[write] = statement;
    }
    debug_assert_eq!(read, write);
    old_len - read
}

#[cfg(test)]
mod tests {
    // Keep the original insertion implementation independent of the batched
    // mover, including prefixing, duplicate destinations and self reads.
    fn legacy_apply_declarations(
        block: &mut crate::Block,
        declarations: std::collections::BTreeMap<usize, crate::FxIndexSet<crate::RcLocal>>,
    ) {
        use crate::LocalRw;
        for (index, mut locals) in declarations.into_iter().rev() {
            if let crate::Statement::Assign(assign) = &mut block[index]
                && assign.left.iter().all(|l| l.as_local().is_some_and(|l| locals.contains(l)))
            {
                let left: Vec<_> = assign.left.iter().map(|l| l.as_local().unwrap()).collect();
                let self_read = assign.right.iter().flat_map(|v| v.values_read()).any(|r| left.contains(&r));
                if !self_read {
                    locals.retain(|l| !left.contains(&l));
                    assign.prefix = true;
                }
            }
            if !locals.is_empty() {
                let mut declaration = crate::Assign::new(locals.into_iter().map(Into::into).collect(), vec![]);
                declaration.prefix = true;
                block.insert(index, declaration.into());
            }
        }
    }

    #[test]
    fn batched_declarations_match_original_order_prefixes_and_origins() {
        use crate::{Index, Return};
        use crate::node_origins::{self, Input, Origin};
        for seed in 1..=256u64 {
            let mut state = seed;
            let mut random = || { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state };
            let locals: Vec<_> = (0..6).map(|i| local(&format!("v{i}"))).collect();
            let mut statements = Vec::new();
            let mut declarations = std::collections::BTreeMap::new();
            for position in 0..32 {
                let a = &locals[random() as usize % locals.len()];
                let b = &locals[random() as usize % locals.len()];
                let mut statement: Statement = match random() % 8 {
                    0 => assign_local(a, number(position as f64)),
                    1 => assign_local(a, RValue::Local(a.clone())),
                    2 => Assign::new(vec![a.clone().into(), b.clone().into()], vec![number(1.0)]).into(),
                    3 => Assign::new(vec![a.clone().into(), a.clone().into()], vec![b.clone().into()]).into(),
                    4 => Assign::new(vec![Index::new(a.clone().into(), b.clone().into()).into()], vec![number(2.0)]).into(),
                    5 => Return::new(vec![a.clone().into()]).into(),
                    6 => Assign::new(vec![], vec![number(3.0)]).into(),
                    _ => Assign::new(vec![Global::from("global").into()], vec![a.clone().into()]).into(),
                };
                if let Some(origin) = node_origins::statement_mut(&mut statement) {
                    *origin = Origin::input(Input { function: "decl".into(), block: 0, statement: position, value: None });
                }
                statements.push(statement);
                if random() % 3 != 0 {
                    let set: crate::FxIndexSet<_> = locals.iter().filter(|_| random() % 2 == 0).cloned().collect();
                    declarations.insert(position, set);
                }
            }
            let original = Block(statements);
            let mut expected = original.clone();
            let mut actual = original.clone();
            legacy_apply_declarations(&mut expected, declarations.clone());
            super::apply_declarations(&mut actual, declarations);
            assert_eq!(actual, expected, "seed {seed}");
            for (actual, expected) in actual.iter().zip(expected.iter()) {
                let tags = |statement: &Statement| node_origins::statement(statement).and_then(|o| o.0.as_ref()).map(|d|
                    (d.inputs.clone(), d.cloned, d.inlined, d.incomplete, d.synthesized));
                assert_eq!(tags(actual), tags(expected), "origin seed {seed}");
            }
        }
    }

    #[test]
    fn declaration_insertion_moves_each_old_statement_at_most_once() {
        for size in [64, 256, 1024] {
            let original: Vec<_> = (0..size).map(|i| crate::Return::new(vec![number(i as f64)]).into()).collect();
            let mut actual = Block(original.clone());
            let inserted = || Statement::Empty(crate::Empty {});
            let first = (size - 1, inserted());
            let rest = (0..size - 1).rev().map(|i| (i, inserted())).collect();
            let shifted = super::insert_declarations(&mut actual, first, rest);
            assert_eq!(shifted, size);
            assert_eq!(actual.len(), size * 2);
            for index in 0..size {
                assert!(matches!(actual[index * 2], Statement::Empty(_)));
                assert_eq!(actual[index * 2 + 1], original[index]);
            }
            assert_eq!((0..size).map(|index| size - index).sum::<usize>(), size * (size + 1) / 2);
        }
    }

    #[test]
    fn lexical_lca_and_insertion_child_match_dominator_paths() {
        use petgraph::{algo::dominators::simple_fast, graph::DiGraph};
        for seed in 1..32u64 {
            let mut random = seed;
            let mut graph = DiGraph::new();
            let root = graph.add_node((None, 0));
            for index in 1..120 {
                random ^= random << 13; random ^= random >> 7; random ^= random << 17;
                let node = graph.add_node((None, index));
                graph.add_edge(petgraph::graph::NodeIndex::new(random as usize % index), node, ());
            }
            let index = super::ScopeAncestors::new(&graph, root);
            let legacy = simple_fast(&graph, root);
            for left in graph.node_indices() {
                for right in graph.node_indices() {
                    let expected = legacy.dominators(left).unwrap().find(|candidate| legacy.dominators(right).unwrap().any(|other| other == *candidate)).unwrap();
                    assert_eq!(index.common(left, right), expected);
                    if expected != right {
                        let path: Vec<_> = legacy.dominators(right).unwrap().collect();
                        let position = path.iter().position(|&node| node == expected).unwrap();
                        assert_eq!(index.child_of(expected, right), path[position - 1]);
                    }
                }
            }
        }
    }

    use super::LocalDeclarer;
    use crate::{
        Assign, Block, Call, Global, LValue, Literal, Local, NumericFor, RValue, RcLocal,
        Statement, While,
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
