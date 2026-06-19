use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use ast::{LocalRw, RcLocal};
use indexmap::IndexMap;
use itertools::Itertools;
use petgraph::{
    algo::dominators::simple_fast,
    prelude::DiGraphMap,
    stable_graph::NodeIndex,
    visit::{Dfs, DfsPostOrder, EdgeRef},
    Direction,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    block::{BlockEdge, BranchType},
    function::Function,
};

mod liveness;

use self::liveness::{LiveSets, Liveness};

#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Copy, Debug)]
enum ParamOrStatIndex {
    Param(usize),
    Stat(usize),
}

#[derive(PartialEq, Eq)]
enum RedOrBlue {
    Red,
    Blue,
}

type CongruenceClass = BTreeMap<(usize, ParamOrStatIndex), RcLocal>;

// Benoit Boissinot, Alain Darte, Fabrice Rastello, Benoît Dupont de Dinechin, Christophe Guillon.
// Revisiting Out-of-SSA Translation for Correctness, Code Quality, and Efficiency. [Research Report]
// 2008, pp.14. inria-00349925v3
// https://hal.inria.fr/inria-00349925/file/RR.pdf
// Slides: https://compilers.cs.uni-saarland.de/ssasem/talks/Alain.Darte.pdf
// https://github.com/LLVM-but-worse/maple-ir/blob/f8711230b7c63ce5fd916f86563912ec36f1217e/org.mapleir.ir/src/main/java/org/mapleir/ir/algorithms/BoissinotDestructor.java
pub struct Destructor<'a> {
    function: &'a mut Function,
    upvalue_to_group: IndexMap<RcLocal, RcLocal>,
    upvalues_in: FxHashSet<RcLocal>,
    values: FxHashMap<RcLocal, Rc<RefCell<FxHashSet<RcLocal>>>>,
    // map( local -> rc_map( local -> (pre-order block index, param index) ) )
    // TODO: hash map?
    congruence_classes: FxHashMap<RcLocal, Rc<RefCell<CongruenceClass>>>,
    equal_ancestor_in: FxHashMap<RcLocal, RcLocal>,
    equal_ancestor_out: FxHashMap<RcLocal, RcLocal>,
    local_defs: FxHashMap<RcLocal, (usize, NodeIndex, ParamOrStatIndex)>,
    local_last_use: FxHashMap<RcLocal, FxHashMap<NodeIndex, (usize, ParamOrStatIndex)>>,
    dominator_tree: DiGraphMap<NodeIndex, ()>,
    // does not include the node itself, use the `dominates` function
    dominators: FxHashMap<NodeIndex, FxHashSet<NodeIndex>>,
    liveness: FxHashMap<NodeIndex, LiveSets>,
    undesirable_blocks: FxHashSet<NodeIndex>,
}

impl<'a> Destructor<'a> {
    pub fn new(
        function: &'a mut Function,
        upvalue_to_group: IndexMap<RcLocal, RcLocal>,
        upvalues_in: FxHashSet<RcLocal>,
        local_count: usize,
    ) -> Self {
        Self {
            function,
            upvalue_to_group,
            upvalues_in,
            values: FxHashMap::with_capacity_and_hasher(local_count, Default::default()),
            congruence_classes: FxHashMap::with_capacity_and_hasher(
                local_count,
                Default::default(),
            ),
            equal_ancestor_in: FxHashMap::default(),
            equal_ancestor_out: FxHashMap::default(),
            local_defs: FxHashMap::with_capacity_and_hasher(local_count, Default::default()),
            local_last_use: FxHashMap::default(),
            dominator_tree: DiGraphMap::new(),
            dominators: FxHashMap::default(),
            liveness: FxHashMap::default(),
            undesirable_blocks: FxHashSet::default(),
        }
    }

    pub fn destruct(mut self) {
        self.lift_params();
        self.sort_params();

        self.liveness = Liveness::calculate(self.function);
        // this is for debugging :)
        //self.add_liveness_comments();
        //crate::dot::render_to(self.function, &mut std::io::stdout()).unwrap();

        self.build_def_use();

        self.compute_value_interference();

        self.coalesce_upvalues();
        self.coalesce_params();
        self.coalesce_copies();

        super::construct::apply_local_map(self.function, self.build_local_map());

        //crate::dot::render_to(self.function, &mut std::io::stdout()).unwrap();

        self.sequentialize();
    }

    fn add_liveness_comments(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            let liveness = &self.liveness[&node];
            let block = self.function.block_mut(node).unwrap();
            block.insert(
                0,
                ast::Comment::new(liveness.live_in.iter().join(", ")).into(),
            );
            block.push(ast::Comment::new(liveness.live_out.iter().join(", ")).into());
        }
    }

    fn coalesce_upvalues(&mut self) {
        for (upvalue, group) in self
            .upvalue_to_group
            .iter()
            .map(|(u, g)| (u.clone(), g.clone()))
            .collect::<Vec<_>>()
        {
            let con_class = self.get_congruence_class(group.clone()).clone();
            let (upval_dom_index, _, upval_stat_index) = self.local_defs[&upvalue];
            con_class
                .borrow_mut()
                .insert((upval_dom_index, upval_stat_index), upvalue.clone());
            self.congruence_classes.insert(upvalue.clone(), con_class);
        }
    }

    fn sequentialize(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            let mut replace_map = Vec::new();
            for (stat_index, stat) in self
                .function
                .block_mut(node)
                .unwrap()
                .0
                .iter_mut()
                .enumerate()
            {
                if let ast::Statement::Assign(assign) = stat {
                    if assign.parallel {
                        if assign.left.len() == 1 {
                            if assign.right[0]
                                .as_local()
                                .is_some_and(|r| r == assign.left[0].as_local().unwrap())
                            {
                                // redundant assign, we can remove it
                                replace_map.push((stat_index, Vec::new()))
                            } else {
                                assign.parallel = false;
                            }
                        } else {
                            let mut ready = Vec::new();
                            let mut to_do = Vec::new();
                            let mut loc = FxHashMap::default();
                            let mut pred = FxHashMap::default();

                            let mut result_end = Vec::new();
                            for i in 0..assign.left.len() {
                                // TODO: unneccessary clones, take assign.left and assign.right
                                let dst = assign.left[i].as_local().unwrap();
                                match &assign.right[i] {
                                    ast::RValue::Local(src) => {
                                        loc.insert(src.clone(), src.clone());
                                        pred.insert(dst.clone(), src.clone());
                                        to_do.push(dst.clone());
                                    }
                                    rvalue => result_end.push(ast::Assign::new(
                                        vec![dst.clone().into()],
                                        vec![rvalue.clone()],
                                    )),
                                }
                            }

                            for i in 0..assign.left.len() {
                                let dst = assign.left[i].as_local().unwrap();
                                if !loc.contains_key(dst) && assign.right[i].as_local().is_some() {
                                    ready.push(dst.clone());
                                }
                            }

                            let mut spill = None;
                            let mut result = Vec::new();
                            while let Some(local_b) = to_do.pop() {
                                while let Some(local_b) = ready.pop() {
                                    let local_a = pred[&local_b].clone();
                                    let local_c = loc[&local_a].clone();
                                    result.push(ast::Assign::new(
                                        vec![local_b.clone().into()],
                                        vec![local_c.clone().into()],
                                    ));
                                    if local_a == local_c && pred.contains_key(&local_a) {
                                        ready.push(local_a.clone());
                                    }
                                    loc.insert(local_a, local_b);
                                }

                                if local_b != loc[&pred[&local_b]] {
                                    let spill = spill.get_or_insert_with(RcLocal::default);
                                    result.push(ast::Assign::new(
                                        vec![spill.clone().into()],
                                        vec![local_b.clone().into()],
                                    ));
                                    loc.insert(local_b.clone(), spill.clone());
                                    ready.push(local_b);
                                }
                            }
                            result.extend(result_end);

                            replace_map.push((stat_index, result))
                        }
                    }
                }
            }

            let block = self.function.block_mut(node).unwrap();
            for (stat_index, assigns) in replace_map.into_iter().rev() {
                block.splice(
                    stat_index..stat_index + 1,
                    // TODO: pad with ast::Empty and then use retain
                    assigns.into_iter().map(|a| a.into()),
                );
            }
        }
    }

    fn build_local_map(&self) -> FxHashMap<RcLocal, RcLocal> {
        let mut map = FxHashMap::default();
        for (local, con_class) in &self.congruence_classes {
            let con_class = con_class.borrow();
            let new_local = con_class.iter().next().unwrap().1;
            // TODO: see apply_local_map TODO,
            // we dont want to handle this here
            if local != new_local {
                map.insert(local.clone(), new_local.clone());
            }
        }
        map
    }

    // TODO: combine with compute value interference
    fn build_def_use(&mut self) {
        let dominators = simple_fast(self.function.graph(), self.function.entry().unwrap());
        for node in self.function.graph().node_indices() {
            if let Some(dominator) = dominators.immediate_dominator(node) {
                self.dominator_tree.add_edge(dominator, node, ());
            }
        }
        self.dominators.reserve(self.dominator_tree.node_count());
        for node in self.dominator_tree.nodes() {
            let mut dominators = FxHashSet::default();
            let mut parent_node = node;
            while let Ok(next_parent_node) = self
                .dominator_tree
                .neighbors_directed(parent_node, Direction::Incoming)
                .exactly_one()
            {
                parent_node = next_parent_node;
                dominators.insert(parent_node);
            }
            self.dominators.insert(node, dominators);
        }

        let mut dominator_index = 0;
        let mut dominator_dfs = Dfs::new(&self.dominator_tree, self.function.entry().unwrap());
        while let Some(node) = dominator_dfs.next(self.function.graph()) {
            if node == self.function.entry().unwrap() {
                assert!(dominator_index == 0);
                assert!(!self
                    .function
                    .edges_to_block(node)
                    .any(|(_, e)| !e.arguments.is_empty()));
                for (i, local) in self
                    .upvalues_in
                    .iter()
                    .chain(self.upvalue_to_group.iter().flat_map(|(u, g)| [u, g]))
                    .chain(self.function.parameters.iter())
                    .enumerate()
                {
                    if !self.local_defs.contains_key(local) {
                        self.local_defs.insert(
                            local.clone(),
                            (dominator_index, node, ParamOrStatIndex::Param(i)),
                        );
                    }
                }
            }

            if let Some((_, edge)) = self.function.edges_to_block(node).next() {
                for (param_index, (param, _)) in
                    edge.arguments.iter().enumerate().collect::<Vec<_>>()
                {
                    self.local_defs.insert(
                        param.clone(),
                        (dominator_index, node, ParamOrStatIndex::Param(param_index)),
                    );
                }
            }
            for (stat_index, stat) in self.function.block(node).unwrap().0.iter().enumerate() {
                for local in stat.values_written() {
                    self.local_defs.insert(
                        local.clone(),
                        (dominator_index, node, ParamOrStatIndex::Stat(stat_index)),
                    );
                }

                for local in stat.values_read() {
                    self.local_last_use
                        .entry(local.clone())
                        .or_default()
                        .insert(node, (dominator_index, ParamOrStatIndex::Stat(stat_index)));
                }
            }
            dominator_index += 1;
        }
    }

    // a dominates b?
    // same as dominates if a == b
    fn check_pre_dom_order(&self, a: &RcLocal, b: &RcLocal) -> bool {
        let (a_dom_index, _, a_stat_index) = self.local_defs[a];
        let (b_dom_index, _, b_stat_index) = self.local_defs[b];
        (a_dom_index, a_stat_index) < (b_dom_index, b_stat_index)
    }

    // initialize congruence classes based on block params and remove block params
    fn coalesce_params(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            for edge in self
                .function
                .graph()
                .edges_directed(node, Direction::Incoming)
                .map(|e| e.id())
                .collect::<Vec<_>>()
            {
                let args = std::mem::take(
                    &mut self
                        .function
                        .graph_mut()
                        .edge_weight_mut(edge)
                        .unwrap()
                        .arguments,
                );

                for (param, arg) in args {
                    let arg = arg.into_local().unwrap();
                    let congruence_class = self.get_congruence_class(param).clone();

                    let (dominator_index, _, stat_index) = self.local_defs[&arg];
                    congruence_class
                        .borrow_mut()
                        .insert((dominator_index, stat_index), arg.clone());
                    self.congruence_classes.insert(arg, congruence_class);
                }
            }
        }
    }

    fn get_congruence_class(&mut self, local: RcLocal) -> &Rc<RefCell<CongruenceClass>> {
        self.congruence_classes
            .entry(local.clone())
            .or_insert_with(|| {
                let mut congruence_class = BTreeMap::default();
                let (dominator_index, _, stat_index) = self.local_defs[&local];
                congruence_class.insert((dominator_index, stat_index), local);
                Rc::new(RefCell::new(congruence_class))
            })
    }

    fn is_for_next(&self, node: NodeIndex) -> bool {
        self.function
            .block(node)
            .unwrap()
            .last()
            .map(|s| {
                matches!(
                    s,
                    ast::Statement::GenericForNext(_) | ast::Statement::NumForNext(_)
                )
            })
            .unwrap_or(false)
    }

    fn coalesce_copies_for_block(&mut self, node: NodeIndex) {
        for stat_index in 0..self.function.block_mut(node).unwrap().0.len() {
            let should_remove = if let ast::Statement::Assign(assign) =
                &self.function.block(node).unwrap()[stat_index]
            {
                let mut to_remove = Vec::new();
                let left = assign
                    .left
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| Some((i, l.as_local()?.clone())))
                    .collect::<Vec<_>>();
                for (i, left, right) in left
                    .into_iter()
                    .filter_map(|(i, l)| Some((i, l, assign.right.get(i)?.as_local()?.clone())))
                    .collect::<Vec<_>>()
                {
                    // upvalues in and parameters cannot be coalesced
                    debug_assert!(
                        !(self.function.parameters.contains(&left)
                            && self.upvalues_in.contains(&right))
                            || self.function.parameters.contains(&right)
                                && self.upvalues_in.contains(&left)
                    );

                    if self.upvalue_to_group.contains_key(&left)
                        || self.upvalue_to_group.contains_key(&right)
                    {
                        continue;
                    }

                    if self.try_coalesce_copy_by_value(right.clone(), left.clone())
                        || self.try_coalesce_copy_by_sharing(&right, &left)
                    {
                        to_remove.push(i);
                    }
                }
                let assign = self.function.block_mut(node).unwrap()[stat_index]
                    .as_assign_mut()
                    .unwrap();
                for i in to_remove.into_iter().rev() {
                    assign.left.remove(i);
                    assign.right.remove(i);
                }
                assign.left.is_empty()
            } else {
                false
            };

            if should_remove {
                let block = self.function.block_mut(node).unwrap();
                block[stat_index] = ast::Empty {}.into();
            }
        }

        // we check block.ast.len() elsewhere and do `i - ` elsewhere so we need to get rid of empty statements
        // TODO: fix here and elsewhere, see inline.rs
        let block = self.function.block_mut(node).unwrap();
        block.retain(|s| s.as_empty().is_none());
    }

    fn coalesce_copies(&mut self) {
        let mut dominator_dfs = Dfs::new(&self.dominator_tree, self.function.entry().unwrap());
        while let Some(node) = dominator_dfs.next(self.function.graph()) {
            if self.undesirable_blocks.contains(&node) {
                self.coalesce_copies_for_block(node);
            }
        }

        let mut dominator_dfs = Dfs::new(&self.dominator_tree, self.function.entry().unwrap());
        while let Some(node) = dominator_dfs.next(self.function.graph()) {
            self.coalesce_copies_for_block(node);
        }
    }

    fn try_coalesce_copy_by_value(&mut self, left: RcLocal, right: RcLocal) -> bool {
        let left_con_class = self.get_congruence_class(left).clone();
        let right_con_class = self.get_congruence_class(right).clone();

        if *left_con_class.borrow() == *right_con_class.borrow() {
            true
        } else if left_con_class.borrow().len() == 1 && right_con_class.borrow().len() == 1 {
            self.check_interfere_single(&left_con_class, &right_con_class)
        } else if !self.check_interfere(&left_con_class, &right_con_class) {
            self.merge_congruence_classes(&left_con_class, &right_con_class);
            true
        } else {
            false
        }
    }

    // TODO: find a test for this
    fn try_coalesce_copy_by_sharing(&mut self, local_a: &RcLocal, local_b: &RcLocal) -> bool {
        let con_class_x = self.get_congruence_class(local_a.clone()).clone();
        let con_class_y = self.get_congruence_class(local_b.clone()).clone();

        let values = self
            .get_value_class(local_a.clone())
            .borrow()
            .iter()
            .cloned()
            .collect_vec();
        for local_c in values {
            if &local_c == local_b
                || &local_c == local_a
                || !self.check_pre_dom_order(&local_c, local_a)
                || !self.intersect(local_a, &local_c)
            {
                continue;
            }

            let con_class_z = self.get_congruence_class(local_c.clone()).clone();
            if con_class_x == con_class_z && con_class_x != con_class_y {
                println!("WOAH COPY SHARING");
                return true;
            }
            if con_class_y != con_class_x
                && con_class_y != con_class_z
                && con_class_x != con_class_z
                && self.try_coalesce_copy_by_value(local_a.clone(), local_c)
            {
                println!("WOAH COPY SHARING");
                return true;
            }
        }

        false
    }

    fn check_interfere_single(
        &mut self,
        red: &Rc<RefCell<CongruenceClass>>,
        blue: &Rc<RefCell<CongruenceClass>>,
    ) -> bool {
        let mut local_a = red.borrow().values().next().unwrap().clone();
        let mut local_b = blue.borrow().values().next().unwrap().clone();
        // assumes one of the blocks dominates the other
        // as check_pre_dom_order depends on this
        if self.check_pre_dom_order(&local_a, &local_b) {
            std::mem::swap(&mut local_a, &mut local_b);
        }
        if self.intersect(&local_a, &local_b)
            // TODO: get many mut
            && self.get_value_class(local_a.clone()).clone() != self.get_value_class(local_b.clone()).clone()
        {
            true
        } else {
            self.equal_ancestor_in.insert(local_a, local_b.clone());
            let (dom_index_b, _, stat_index_b) = self.local_defs[&local_b];
            red.borrow_mut()
                .insert((dom_index_b, stat_index_b), local_b.clone());
            self.congruence_classes.insert(local_b, red.clone());
            false
        }
    }

    fn intersect(&self, local_a: &RcLocal, local_b: &RcLocal) -> bool {
        assert!(local_a != local_b);
        assert!(!self.dominates(local_a, local_b));

        let (_, block_a, _) = self.local_defs[local_a];
        let (_, block_b, _) = self.local_defs[local_b];
        if self.liveness[&block_a].live_out.contains(local_b) {
            true
        } else if !self.liveness[&block_a].live_in.contains(local_b) && block_a != block_b {
            false
        } else if let Some(dom_use_index) = self
            .local_last_use
            .get(local_b)
            .and_then(|m| m.get(&block_a))
        {
            let (def_dom_index, _, def_stat_index) = self.local_defs[local_a];
            dom_use_index > &(def_dom_index, def_stat_index)
        } else {
            false
        }
    }

    fn dominates(&self, local_a: &RcLocal, local_b: &RcLocal) -> bool {
        let (a_dom_index, block_a, a_stat_index) = self.local_defs[local_a];
        let (b_dom_index, block_b, b_stat_index) = self.local_defs[local_b];
        if block_a == block_b {
            // same as check_pre_dom_order
            (a_dom_index, a_stat_index) < (b_dom_index, b_stat_index)
        } else {
            self.dominators[&block_b].contains(&block_a)
        }
    }

    fn check_interfere(
        &mut self,
        red: &Rc<RefCell<CongruenceClass>>,
        blue: &Rc<RefCell<CongruenceClass>>,
    ) -> bool {
        let mut dom = Vec::<(&RcLocal, RedOrBlue)>::new();

        let red = red.borrow();
        let blue = blue.borrow();
        let mut red_iter = red.iter().peekable();
        let mut blue_iter = blue.iter().peekable();
        let mut red_count = 0;
        let mut blue_count = 0;

        self.equal_ancestor_out.remove(red_iter.peek().unwrap().1);
        self.equal_ancestor_out.remove(blue_iter.peek().unwrap().1);
        loop {
            let (curr, curr_class) = if blue_iter.peek().is_none()
                || (red_iter.peek().is_some()
                    && self.check_pre_dom_order(
                        red_iter.peek().unwrap().1,
                        blue_iter.peek().unwrap().1,
                    )) {
                red_count += 1;
                (red_iter.next().unwrap().1, RedOrBlue::Red)
            } else {
                blue_count += 1;
                (blue_iter.next().unwrap().1, RedOrBlue::Blue)
            };

            while !dom.is_empty() && !self.dominates(dom.last().unwrap().0, curr) {
                match dom.pop().unwrap().1 {
                    RedOrBlue::Red => red_count -= 1,
                    RedOrBlue::Blue => blue_count -= 1,
                }
            }

            if !dom.is_empty()
                && self.interference(
                    curr,
                    dom.last().unwrap().0,
                    curr_class == dom.last().unwrap().1,
                )
            {
                return true;
            }

            dom.push((curr, curr_class));

            if (red_iter.peek().is_some() && blue_count > 0)
                || (blue_iter.peek().is_some() && red_count > 0)
                || (red_iter.peek().is_some() && blue_iter.peek().is_some())
            {
                continue;
            }

            break;
        }

        false
    }

    fn interference(&mut self, local_a: &RcLocal, local_b: &RcLocal, same_con_class: bool) -> bool {
        self.equal_ancestor_out.remove(local_a);
        let local_b = if same_con_class {
            self.equal_ancestor_out.get(local_b)
        } else {
            Some(local_b)
        };

        if let Some(local_b) = local_b.cloned() {
            assert!(!self.dominates(local_a, &local_b));

            let mut tmp = Some(&local_b);
            while let Some(curr_tmp) = tmp
                && !self.intersect(local_a, curr_tmp)
            {
                tmp = self.equal_ancestor_in.get(curr_tmp);
            }
            let tmp = tmp.cloned();

            let local_b = local_b.clone();
            // TODO: get many mut
            if self.get_value_class(local_a.clone()).clone()
                != self.get_value_class(local_b).clone()
            {
                tmp.is_some()
            } else {
                if let Some(tmp) = tmp {
                    self.equal_ancestor_out.insert(local_a.clone(), tmp);
                } else {
                    self.equal_ancestor_out.remove(local_a);
                }
                false
            }
        } else {
            false
        }
    }

    fn merge_congruence_classes(
        &mut self,
        con_class_a: &Rc<RefCell<CongruenceClass>>,
        con_class_b: &Rc<RefCell<CongruenceClass>>,
    ) {
        // TODO: move out of con_class_b with con_class_b.unwrap()
        let con_class_b = std::mem::take(&mut *con_class_b.borrow_mut());
        for local in con_class_b.values() {
            self.congruence_classes
                .insert(local.clone(), con_class_a.clone());
        }
        con_class_a.borrow_mut().extend(con_class_b);

        for local in con_class_a.borrow().values() {
            let local_in = self.equal_ancestor_in.get(local);
            let local_out = self.equal_ancestor_out.get(local);
            let new_local_in = match (local_in, local_out) {
                (None, Some(local)) | (Some(local), None) => Some(local),
                (Some(local_in), Some(local_out)) => {
                    Some(if self.check_pre_dom_order(local_in, local_out) {
                        local_out
                    } else {
                        local_in
                    })
                }
                _ => None,
            };
            if let Some(new_local_in) = new_local_in {
                self.equal_ancestor_in
                    .insert(local.clone(), new_local_in.clone());
            }
        }
    }

    fn compute_value_interference(&mut self) {
        // TODO: STYLE: rename to dom_dfs_post_order, along with other dominator_dfs
        let mut dominator_dfs_post_order =
            DfsPostOrder::new(&self.dominator_tree, self.function.entry().unwrap());

        while let Some(node) = dominator_dfs_post_order.next(self.function.graph()) {
            let params = if let Some((_, edge)) = self.function.edges_to_block(node).next() {
                edge.arguments
                    .iter()
                    .map(|(p, _)| p.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            for param in params {
                self.get_value_class(param);
            }
            for stat_index in 0..self.function.block_mut(node).unwrap().0.len() {
                if let ast::Statement::Assign(assign) =
                    &self.function.block(node).unwrap()[stat_index]
                {
                    let left = assign
                        .left
                        .iter()
                        .enumerate()
                        .filter_map(|(i, l)| Some((i, l.as_local()?.clone())));
                    for (left, right) in left
                        .into_iter()
                        .map(|(i, l)| (l, assign.right.get(i).and_then(|r| r.as_local().cloned())))
                        .collect::<Vec<_>>()
                    {
                        if let Some(right) = right {
                            let value_class = self.get_value_class(right.clone()).clone();
                            value_class.borrow_mut().insert(left.clone());
                            let prev_val_class =
                                self.values.insert(left.clone(), value_class.clone());
                            if let Some(prev_val_class) = prev_val_class {
                                // merge value classes
                                let prev_val_class = prev_val_class.take();
                                for local in &prev_val_class {
                                    self.values.insert(local.clone(), value_class.clone());
                                }
                                value_class.borrow_mut().extend(prev_val_class);
                            }
                            //assert!(prev_val_class.is_none() || prev_val_class.unwrap().borrow().is_empty(), "function not in ssa form");
                        }
                    }
                }
            }
        }
    }

    fn get_value_class(&mut self, local: RcLocal) -> &Rc<RefCell<FxHashSet<RcLocal>>> {
        self.values.entry(local.clone()).or_insert_with(|| {
            let mut value_class = FxHashSet::default();
            value_class.insert(local);
            Rc::new(RefCell::new(value_class))
        })
    }

    fn sort_params(&mut self) {
        for edge in self.function.graph_mut().edge_weights_mut() {
            edge.arguments.sort_by(|(p0, _), (p1, _)| p0.cmp(p1));
        }
    }

    fn lift_params(&mut self) {
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            self.lift_block_params(node);
        }
    }

    // Note that the phi-functions do not have a circular dependency and are ordered accordingly (we have to do this before),
    // i.e., no variable that is defined by a Phi-function is used in a 'later' phi-function.
    fn lift_block_params(&mut self, node: NodeIndex) {
        let mut param_map = FxHashMap::default();
        if let Some((_, BlockEdge { arguments, .. })) = self.function.edges_to_block(node).next() {
            for param in arguments.iter().map(|(p, _)| p) {
                let temp_param = RcLocal::default();
                if let Some(group) = self.upvalue_to_group.get(param) {
                    self.upvalue_to_group
                        .insert(temp_param.clone(), group.clone());
                }
                param_map.insert(param.clone(), temp_param);
            }
        }

        if !param_map.is_empty() {
            self.function.block_mut(node).unwrap().insert(
                0,
                ast::Assign {
                    left: param_map.keys().map(|k| k.clone().into()).collect(),
                    right: param_map.values().map(|v| v.clone().into()).collect(),
                    prefix: false,
                    parallel: true,
                }
                .into(),
            );
        }

        let mut visited = FxHashSet::default();
        let mut preds = self.function.predecessor_blocks(node).detach();
        while let Some((_, pred)) = preds.next(self.function.graph()) {
            // if there are multiple edges from b0 -> b1, b0 will occur more than once.
            if visited.contains(&pred) {
                continue;
            }
            visited.insert(pred);

            let edges = self.function.edges(pred).collect::<Vec<_>>();
            let is_unconditional = edges.len() == 1;
            if is_unconditional {
                assert!(edges[0].weight().branch_type == BranchType::Unconditional);
            }

            let edges_to_node = edges
                .iter()
                .filter(|e| e.target() == node)
                .map(|e| e.id())
                .collect::<Vec<_>>();

            for &edge in &edges_to_node {
                let args = self
                    .function
                    .graph_mut()
                    .edge_weight_mut(edge)
                    .unwrap()
                    .arguments
                    .iter_mut();

                let mut parallel_assign = ast::Assign {
                    left: Vec::with_capacity(args.len()),
                    right: Vec::with_capacity(args.len()),
                    prefix: false,
                    parallel: true,
                };

                for (param, arg) in args {
                    let temp_local = RcLocal::default();
                    if let ast::RValue::Local(arg) = arg
                        && let Some(group) = self.upvalue_to_group.get(arg)
                    {
                        self.upvalue_to_group
                            .insert(temp_local.clone(), group.clone());
                    }

                    parallel_assign.left.push(temp_local.clone().into());
                    parallel_assign
                        .right
                        .push(std::mem::replace(arg, temp_local.into()));
                    *param = param_map[param].clone();
                }

                if !parallel_assign.left.is_empty() {
                    let mut assign_block = pred;
                    // always insert a new block if the pred is conditional
                    // this is because it generates output that makes more sense
                    /*
                    local a, b = 1, 2
                    while p do
                        local t = a
                        a = b
                        b = t
                    end
                    return a, b
                    -- if we insert into the conditional block, we get weird output
                    local v1 = 1
                    local v2 = 2
                    repeat
                        local v3 = v1
                        v1 = v2
                        v2 = v3
                    until not p
                    return v2, v1
                    */
                    if !is_unconditional {
                        assign_block = self.function.new_block();
                        if self.is_for_next(self.function.graph().edge_endpoints(edge).unwrap().0) {
                            self.undesirable_blocks.insert(assign_block);
                        }
                        let edge = self.function.graph_mut().remove_edge(edge).unwrap();
                        self.function.set_edges(
                            assign_block,
                            vec![(
                                node,
                                BlockEdge {
                                    branch_type: BranchType::Unconditional,
                                    arguments: edge.arguments,
                                },
                            )],
                        );

                        self.function.graph_mut().add_edge(
                            pred,
                            assign_block,
                            BlockEdge::new(edge.branch_type),
                        );
                        visited.insert(assign_block);
                    }

                    self.function
                        .block_mut(assign_block)
                        .unwrap()
                        .push(parallel_assign.into());
                }
            }
        }
    }
}
