use std::collections::VecDeque;

use ast::LocalRw;
use ast::FxIndexSet as IndexSet;
use petgraph::stable_graph::NodeIndex;
use rangemap::RangeInclusiveMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::function::Function;

/// The by-reference upvalues a statement captures — closures sitting directly on
/// an assignment's right-hand side, matching the forward pass's detection in
/// `UpvaluesOpen::new`. (At SSA-construction time the lifter emits every
/// `NEWCLOSURE` as its own `tmp = function … end`, so a closure passed as a call
/// argument is still a top-level RHS here.)
fn ref_upvalues(statement: &ast::Statement) -> impl Iterator<Item = &ast::RcLocal> {
    statement
        .as_assign()
        .into_iter()
        .flat_map(|assign| assign.right.iter())
        .filter_map(|r| r.as_closure())
        .flat_map(|c| c.upvalues.iter())
        .filter_map(|u| match u {
            ast::Upvalue::Ref(l) => Some(l),
            ast::Upvalue::Copy(_) => None,
        })
}

/// How a statement defines a given (old) local — see `UpvaluesOpen::def_kind`.
enum DefKind {
    /// A `nil`-literal assignment (declaration shape) writing this SSA version.
    Nil(ast::RcLocal),
    /// A definition by any other means.
    Other,
    /// Not a definition of the local.
    NotDef,
}

/// Dense handles keep reaching-open state constant-sized per register. Union
/// by size and path compression bound union/find work; a separate minimum site
/// preserves the old canonical label regardless of predecessor visitation order.
#[derive(Default)]
struct OpenLabels {
    sites: FxHashMap<(ast::RcLocal, NodeIndex, usize), usize>,
    cells: Vec<OpenCell>,
}

struct OpenCell {
    parent: usize,
    size: usize,
    first: (NodeIndex, usize),
}

impl OpenLabels {
    fn site(&mut self, local: ast::RcLocal, node: NodeIndex, index: usize) -> usize {
        *self.sites.entry((local, node, index)).or_insert_with(|| {
            let parent = self.cells.len();
            self.cells.push(OpenCell { parent, size: 1, first: (node, index) });
            parent
        })
    }

    fn root(&mut self, mut label: usize) -> usize {
        while self.cells[label].parent != label {
            let parent = self.cells[label].parent;
            self.cells[label].parent = self.cells[parent].parent;
            label = parent;
        }
        label
    }

    fn union(&mut self, left: usize, right: usize) -> usize {
        let mut left = self.root(left);
        let mut right = self.root(right);
        if left != right {
            if self.cells[left].size < self.cells[right].size {
                std::mem::swap(&mut left, &mut right);
            }
            self.cells[right].parent = left;
            self.cells[left].size += self.cells[right].size;
            self.cells[left].first = self.cells[left].first.min(self.cells[right].first);
        }
        left
    }

    fn representative(&mut self, label: usize) -> (NodeIndex, usize) {
        let root = self.root(label);
        self.cells[root].first
    }
}

#[derive(Debug)]
pub(crate) struct UpvaluesOpen<'a> {
    // Each interval carries the stable minimum `(block, statement)` open site.
    // During dataflow, dense union-find handles replace growing sets of sites.
    // `mark_upvalues` uses that representative as the cell-group label; CLOSE is a transfer kill, not a name heuristic.
    pub open: FxHashMap<
        NodeIndex,
        FxHashMap<ast::RcLocal, RangeInclusiveMap<usize, IndexSet<(NodeIndex, usize)>>>,
    >,
    // Construction keeps this immutable until every open-cell query finishes.
    // Borrow it instead of cloning every SSA identity and register handle.
    old_locals: &'a FxHashMap<ast::RcLocal, ast::RcLocal>,
}

#[cfg(test)]
thread_local! {
    static CONSUMED_CENSUSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockEdge;

    fn capture(local: ast::RcLocal) -> ast::Statement {
        let closure = ast::Closure {
            node_origin: Default::default(),
            function: Default::default(),
            upvalues: vec![ast::Upvalue::Ref(local)],
        };
        ast::Assign::new(vec![ast::RcLocal::default().into()], vec![closure.into()]).into()
    }

    #[test]
    fn late_predecessor_unifies_open_cell_with_capture_after_join() {
        for close_before_join in [false, true] {
            let mut function = Function::new(0);
            let entry = function.new_block();
            let join = function.new_block(); // deliberately visited before arm
            let arm = function.new_block();
            function.set_entry(entry);
            function.graph_mut().add_edge(entry, join, BlockEdge::default());
            function.graph_mut().add_edge(entry, arm, BlockEdge::default());
            function.graph_mut().add_edge(arm, join, BlockEdge::default());
            let register = ast::RcLocal::default();
            let before = ast::RcLocal::default();
            let after = ast::RcLocal::default();
            function.block_mut(arm).unwrap().push(capture(before.clone()));
            if close_before_join {
                function.block_mut(arm).unwrap().push(ast::Close { locals: vec![register.clone()] }.into());
            }
            function.block_mut(join).unwrap().push(capture(after.clone()));
            let old_locals = FxHashMap::from_iter([
                (before, register.clone()), (after, register.clone()),
            ]);
            let open = UpvaluesOpen::new(&function, &old_locals);
            let first = open.open[&arm][&register].get(&0).unwrap().first();
            let second = open.open[&join][&register].get(&0).unwrap().first();
            assert_eq!(first == second, !close_before_join);
        }
    }

    #[test]
    fn close_at_join_entry_does_not_unify_killed_predecessor_cells() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let join = function.new_block();
        let left = function.new_block();
        let right = function.new_block();
        function.set_entry(entry);
        for (from, to) in [(entry, left), (entry, right), (left, join), (right, join)] {
            function.graph_mut().add_edge(from, to, BlockEdge::default());
        }
        let register = ast::RcLocal::default();
        let versions: Vec<_> = (0..3).map(|_| ast::RcLocal::default()).collect();
        function.block_mut(left).unwrap().push(capture(versions[0].clone()));
        function.block_mut(right).unwrap().push(capture(versions[1].clone()));
        function.block_mut(join).unwrap().extend([
            ast::Close { locals: vec![register.clone()] }.into(),
            capture(versions[2].clone()),
        ]);
        let old_locals: FxHashMap<_, _> = versions.iter().cloned().map(|version| (version, register.clone())).collect();
        let reference = UpvaluesOpen::new_reference(&function, &old_locals);
        let result = UpvaluesOpen::new(&function, &old_locals);
        assert_eq!(result.open, reference.open);
        let labels: Vec<_> = [(left, 0), (right, 0), (join, 1)].into_iter().map(|(node, index)| {
            *result.open[&node][&register].get(&index).unwrap().first().unwrap()
        }).collect();
        assert_eq!(labels, [(left, 0), (right, 0), (join, 1)]);
    }

    #[test]
    fn dense_labels_match_reaching_site_sets_on_randomized_cfgs() {
        for seed in 1..=768u64 {
            let mut state = seed;
            let mut random = |limit: usize| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((state >> 32) as usize) % limit
            };
            let mut function = Function::new(0);
            let nodes: Vec<_> = (0..4 + random(10)).map(|_| function.new_block()).collect();
            function.set_entry(nodes[0]);
            let registers: Vec<_> = (0..4).map(|_| ast::RcLocal::default()).collect();
            let mut old_locals = FxHashMap::default();
            for &node in &nodes {
                for _ in 0..random(7) {
                    let register = registers[random(registers.len())].clone();
                    let statement = match random(5) {
                        0 => ast::Close { locals: vec![register] }.into(),
                        1 => ast::Comment::new("noop".into()).into(),
                        kind => {
                            let version = ast::RcLocal::default();
                            old_locals.insert(version.clone(), register);
                            if kind == 2 {
                                ast::Assign::new(vec![version.into()], vec![ast::Literal::Nil.into()]).into()
                            } else {
                                capture(version)
                            }
                        }
                    };
                    function.block_mut(node).unwrap().push(statement);
                }
                for _ in 0..random(4) {
                    let target = nodes[random(nodes.len())];
                    function.graph_mut().add_edge(node, target, BlockEdge::default());
                }
            }
            let reference = UpvaluesOpen::new_reference(&function, &old_locals);
            let result = UpvaluesOpen::new(&function, &old_locals);
            assert_eq!(result.open, reference.open, "seed={seed}");
        }
    }

    #[test]
    fn repeated_captures_keep_one_interval_and_minimum_site() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);
        let register = ast::RcLocal::default();
        let version = ast::RcLocal::default();
        for _ in 0..10_000 {
            function.block_mut(entry).unwrap().push(capture(version.clone()));
        }
        let old_locals = FxHashMap::from_iter([(version, register.clone())]);
        let result = UpvaluesOpen::new(&function, &old_locals);
        let ranges = &result.open[&entry][&register];
        assert_eq!(ranges.iter().count(), 1);
        assert_eq!(ranges.get(&9_999).unwrap().first(), Some(&(entry, 0)));
    }

    #[test]
    fn backward_consumed_census_is_lazy_without_changing_ordered_cell_intervals() {
        fn ordered(open: &UpvaluesOpen<'_>) -> Vec<(usize, u64, usize, usize, Vec<(usize, usize)>)> {
            let mut rows = Vec::new();
            for (node, locals) in &open.open {
                for (local, ranges) in locals {
                    for (range, sites) in ranges.iter() {
                        rows.push((node.index(), local.stable_id(), *range.start(), *range.end(),
                            sites.iter().map(|(node, index)| (node.index(), *index)).collect()));
                    }
                }
            }
            rows.sort();
            rows
        }
        for mode in 0..8 {
            let mut function = Function::new(0);
            let entry = function.new_block();
            function.set_entry(entry);
            let capture_node = if mode == 0 { entry } else { function.new_block() };
            let [register, before, phi, captured] = std::array::from_fn(|_| ast::RcLocal::default());
            let old_locals = FxHashMap::from_iter([
                (before.clone(), register.clone()), (phi.clone(), register.clone()),
                (captured.clone(), register.clone()),
            ]);
            function.block_mut(entry).unwrap().push(ast::Assign::new(vec![before.clone().into()],
                vec![if mode == 2 { ast::Literal::Number(1.0).into() } else { ast::Literal::Nil.into() }]).into());
            if mode == 3 { function.block_mut(entry).unwrap().push(
                ast::Call::new(ast::Global::from("use").into(), vec![before.clone().into()]).into()); }
            if mode != 0 {
                if matches!(mode, 6 | 7) {
                    let middle = function.new_block();
                    function.set_edges(entry, vec![(middle, BlockEdge { arguments: vec![(phi.clone(), before.clone().into())], ..Default::default() })]);
                    function.set_edges(middle, vec![(capture_node, BlockEdge::default())]);
                    if mode == 7 { function.block_mut(middle).unwrap().push(
                        ast::Call::new(ast::Global::from("use").into(), vec![phi.clone().into()]).into()); }
                } else { function.set_edges(entry, vec![(capture_node, BlockEdge::default())]); }
            }
            if mode == 4 { function.block_mut(capture_node).unwrap().push(
                ast::Close { locals: vec![register.clone()] }.into()); }
            if mode == 5 { function.block_mut(entry).unwrap().push(capture(before.clone())); }
            function.block_mut(capture_node).unwrap().push(capture(captured));
            let expected = UpvaluesOpen::new_reference(&function, &old_locals);
            CONSUMED_CENSUSES.with(|count| count.set(0));
            let actual = UpvaluesOpen::new(&function, &old_locals);
            assert_eq!(ordered(&actual), ordered(&expected), "mode {mode}");
            assert_eq!(CONSUMED_CENSUSES.with(std::cell::Cell::get), usize::from(matches!(mode, 1 | 3 | 6 | 7)),
                "only a reached cross-block nil definition needs the full census, mode {mode}");
        }
    }

    #[test]
    fn def_kind_visitor_matches_original_write_slot_query() {
        let [register, first, second, generator, state, control] = std::array::from_fn(|_| ast::RcLocal::default());
        let old_locals = FxHashMap::from_iter([(first.clone(), register.clone()), (second.clone(), register.clone()),
            (control.clone(), register.clone())]);
        let analysis = UpvaluesOpen { open: Default::default(), old_locals: &old_locals };
        let statements: Vec<ast::Statement> = vec![
            ast::Assign::new(vec![first.clone().into(), second.clone().into()], vec![ast::Literal::Nil.into(), ast::Literal::Number(1.0).into()]).into(),
            ast::Assign::new(vec![first.clone().into(), first.clone().into()], vec![ast::Literal::Number(1.0).into(), ast::Literal::Nil.into()]).into(),
            ast::NumForInit::new(first.clone(), second.clone(), control.clone()).into(),
            ast::GenericForNext::new(vec![generator.clone()], generator.into(), state, control).into(),
            ast::Close { locals: vec![register.clone()] }.into(),
        ];
        for statement in statements {
            let expected = if let Some(assign) = statement.as_assign() {
                assign.left.iter().enumerate().find_map(|(index, left)| left.as_local()
                    .filter(|local| old_locals.get(*local) == Some(&register))
                    .map(|local| if matches!(assign.right.get(index), Some(ast::RValue::Literal(ast::Literal::Nil))) {
                        Some(local.stable_id())
                    } else { None }))
            } else { None };
            let expected = expected.map(|nil| (true, nil)).unwrap_or_else(||
                (statement.values_written().into_iter().any(|local| old_locals.get(local) == Some(&register)), None));
            let actual = match analysis.def_kind(&statement, &register) {
                DefKind::Nil(local) => (true, Some(local.stable_id())),
                DefKind::Other => (true, None),
                DefKind::NotDef => (false, None),
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn consumed_census_ignores_child_body_publication_but_keeps_outer_capture_slots() {
        let [register, reference, copy, child_only] = std::array::from_fn(|_| ast::RcLocal::default());
        let closure = ast::Closure { node_origin: Default::default(), function: Default::default(),
            upvalues: vec![ast::Upvalue::Ref(reference.clone()), ast::Upvalue::Copy(copy.clone())] };
        let child = closure.function.clone();
        let mut function = Function::new(0);
        let entry = function.new_block();
        function.set_entry(entry);
        function.block_mut(entry).unwrap().push(ast::Assign::new(vec![ast::RcLocal::default().into()], vec![closure.into()]).into());
        let old_locals = FxHashMap::from_iter([(reference.clone(), register)]);
        let analysis = UpvaluesOpen { open: Default::default(), old_locals: &old_locals };
        let before = analysis.consumed_versions(&function);
        assert!(!before.contains(&reference));
        assert!(before.contains(&copy));
        child.lock().body = ast::Block(vec![
            ast::Assign::new(vec![reference.clone().into()], vec![ast::Literal::Nil.into()]).into(),
            ast::Return::new(vec![child_only.clone().into(), reference.clone().into()]).into(),
        ]);
        assert_eq!(analysis.consumed_versions(&function), before);
        assert!(!before.contains(&child_only));
        // LocalRw reads captures from the outer Closure value, not its shared
        // function Arc. The immutable census cannot observe body publication.
        assert_eq!(function.block(entry).unwrap()[0].values_read(), vec![&reference, &copy]);
    }

}

impl<'a> UpvaluesOpen<'a> {
    pub fn new(function: &Function, old_locals: &'a FxHashMap<ast::RcLocal, ast::RcLocal>) -> Self {
        let phase = ast::telemetry::Span::new("SSA_UPVALUES_OPEN");
        type Open = FxHashMap<ast::RcLocal, usize>;
        let mut labels = OpenLabels::default();
        let mut ranges_by_node = FxHashMap::default();
        let entry = function.entry().unwrap();
        let mut incoming: FxHashMap<NodeIndex, Open> = FxHashMap::default();
        let mut work = VecDeque::from([entry]);
        let mut queued = FxHashSet::from_iter([entry]);
        let mut visited = FxHashSet::default();
        incoming.insert(entry, Open::default());

        // A CLOSE at statement zero erases the entire incoming interval. Its
        // reaching sites never coexist in an output range, so merging them at
        // the join would incorrectly identify cells from distinct close epochs.
        let killed_at_entry: FxHashMap<_, FxHashSet<_>> = function.blocks()
            .filter_map(|(node, block)| match block.first() {
                Some(ast::Statement::Close(close)) =>
                    Some((node, close.locals.iter().cloned().collect())),
                _ => None,
            })
            .collect();
        while let Some(node) = work.pop_front() {
            queued.remove(&node);
            visited.insert(node);
            let block = function.block(node).unwrap();
            let end = block.len().saturating_sub(1);
            let mut current = incoming[&node].clone();
            let mut ranges: FxHashMap<ast::RcLocal, RangeInclusiveMap<usize, usize>> =
                FxHashMap::default();
            for (local, &label) in &current {
                ranges.entry(local.clone()).or_default().insert(0..=end, label);
            }
            for (index, statement) in block.iter().enumerate() {
                for version in ref_upvalues(statement) {
                    let local = old_locals[version].clone();
                    let site = labels.site(local.clone(), node, index);
                    let label = current.entry(local.clone()).or_insert(site);
                    *label = labels.union(*label, site);
                    ranges.entry(local).or_default().insert(index..=end, *label);
                }
                if let ast::Statement::Close(close) = statement {
                    for local in &close.locals {
                        current.remove(local);
                        if let Some(ranges) = ranges.get_mut(local) {
                            ranges.remove(index..=end);
                        }
                    }
                }
            }
            ranges_by_node.insert(node, ranges);
            let mut successors = function.successor_blocks(node).collect::<Vec<_>>();
            successors.sort();
            successors.dedup();
            for successor in successors {
                let next = incoming.entry(successor).or_default();
                let killed = killed_at_entry.get(&successor);
                let mut changed = false;
                for (local, &label) in &current {
                    match next.entry(local.clone()) {
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(label);
                            changed = true;
                        }
                        std::collections::hash_map::Entry::Occupied(mut slot) => {
                            if !killed.is_some_and(|locals| locals.contains(local)) {
                                let joined = labels.union(*slot.get(), label);
                                slot.insert(joined);
                            }
                        }
                    }
                }
                // Only newly open registers need propagation. All previously
                // emitted ranges and successor states retain label handles;
                // unioning a late predecessor updates their cells transitively.
                if (changed || !visited.contains(&successor)) && queued.insert(successor) {
                    work.push_back(successor);
                }
            }
        }
        drop(incoming);
        drop(killed_at_entry);
        drop(visited);
        drop(queued);
        drop(work);
        labels.sites = FxHashMap::default();
        drop(phase);
        let phase = ast::telemetry::Span::new("SSA_UPVALUES_CANONICALIZE");
        let open = ranges_by_node.into_iter().map(|(node, locals)| {
            let locals = locals.into_iter().map(|(local, ranges)| {
                let ranges = ranges.iter().map(|(range, &label)| {
                    let site = labels.representative(label);
                    (range.clone(), IndexSet::from_iter([site]))
                }).collect();
                (local, ranges)
            }).collect();
            (node, locals)
        }).collect();
        drop(labels);
        let mut this = Self { open, old_locals };
        drop(phase);
        let _phase = ast::telemetry::Span::new("SSA_UPVALUES_EXTEND_BACKWARD");
        this.extend_open_backward::<true>(function);
        this
    }

    #[cfg(test)]
    fn new_reference(function: &Function, old_locals: &'a FxHashMap<ast::RcLocal, ast::RcLocal>) -> Self {
        type Sites = IndexSet<(NodeIndex, usize)>;
        type Open = FxHashMap<ast::RcLocal, Sites>;
        let mut this = Self { open: Default::default(), old_locals };
        let entry = function.entry().unwrap();
        let mut incoming: FxHashMap<NodeIndex, Open> = FxHashMap::default();
        let mut work = VecDeque::from([entry]);
        let mut queued = FxHashSet::from_iter([entry]);
        let mut visited = FxHashSet::default();
        incoming.insert(entry, Open::default());
        // Monotone reaching-open dataflow. A visited successor must be revisited
        // when another predecessor contributes an open cell. The former DFS
        // skipped that edge, splitting a conditionally-created callback's cell
        // from a second callback created after the merge.
        while let Some(node) = work.pop_front() {
            queued.remove(&node);
            visited.insert(node);
            let block = function.block(node).unwrap();
            let end = block.len().saturating_sub(1);
            let mut current = incoming[&node].clone();
            let mut ranges: FxHashMap<ast::RcLocal, RangeInclusiveMap<usize, Sites>> = FxHashMap::default();
            for (local, sites) in &current {
                ranges.entry(local.clone()).or_default().insert(0..=end, sites.clone());
            }
            for (index, statement) in block.iter().enumerate() {
                for version in ref_upvalues(statement) {
                    let local = this.old_locals[version].clone();
                    let sites = current.entry(local.clone()).or_default();
                    sites.insert((node, index));
                    ranges.entry(local).or_default().insert(index..=end, sites.clone());
                }
                if let ast::Statement::Close(close) = statement {
                    for local in &close.locals {
                        current.remove(local);
                        if let Some(ranges) = ranges.get_mut(local) {
                            ranges.remove(index..=end);
                        }
                    }
                }
            }
            this.open.insert(node, ranges);
            let mut successors = function.successor_blocks(node).collect::<Vec<_>>();
            successors.sort();
            successors.dedup();
            for successor in successors {
                let next = incoming.entry(successor).or_default();
                let mut changed = false;
                for (local, sites) in &current {
                    let next_sites = next.entry(local.clone()).or_default();
                    let before = next_sites.len();
                    next_sites.extend(sites.iter().copied());
                    changed |= before != next_sites.len();
                }
                if (changed || !visited.contains(&successor)) && queued.insert(successor) {
                    work.push_back(successor);
                }
            }
        }
        this.canonicalize_overlapping_opens();
        this.extend_open_backward::<false>(function);
        this
    }

    /// Two opens of the same VM register belong to one cell when a path reaches
    /// the latter without CLOSE. Unify their site labels transitively; a CLOSE
    /// kills the reaching set above and therefore keeps distinct epochs apart.
    #[cfg(test)]
    fn canonicalize_overlapping_opens(&mut self) {
        use std::collections::BTreeMap;
        type Key = (ast::RcLocal, NodeIndex, usize);
        fn root(parents: &BTreeMap<Key, Key>, key: &Key) -> Key {
            let mut current = key;
            while let Some(next) = parents.get(current) {
                if next == current { break; }
                current = next;
            }
            current.clone()
        }
        let mut parents = BTreeMap::<Key, Key>::new();
        for locals in self.open.values() {
            for (local, ranges) in locals {
                for (_, sites) in ranges.iter() {
                    let Some(&(node, index)) = sites.first() else { continue; };
                    let first = (local.clone(), node, index);
                    for &(node, index) in sites.iter().skip(1) {
                        let left = root(&parents, &first);
                        let right = root(&parents, &(local.clone(), node, index));
                        if left != right {
                            let (lower, higher) = if left < right { (left, right) } else { (right, left) };
                            parents.insert(higher, lower);
                        }
                    }
                }
            }
        }
        for locals in self.open.values_mut() {
            for (local, ranges) in locals {
                *ranges = ranges.iter().map(|(range, sites)| {
                    let &(node, index) = sites.first().unwrap();
                    let (_, node, index) = root(&parents, &(local.clone(), node, index));
                    (range.clone(), IndexSet::from_iter([(node, index)]))
                }).collect();
            }
        }
    }

    /// Pull a by-reference-captured local's *cross-block `nil` initializer* into
    /// the same open region as its captures, so every version of the cell is
    /// grouped into one variable by `construct::mark_upvalues`.
    ///
    /// The forward pass above marks a captured local open only from the
    /// closure-creating statement onward (and into successors). When the local
    /// is declared (and `nil`-initialized) in a block that *dominates* the block
    /// where the connection is assigned — the classic
    /// ```text
    /// local conn            -- entry block
    /// if cond then
    ///     conn = sig:Connect(function() conn:Disconnect() end)  -- successor block
    /// end
    /// ```
    /// pattern — that `nil` version is never seen as open. `mark_upvalues` then
    /// leaves it out of the upvalue group: it survives as a separate dead
    /// `local conn = nil`, the reassignment is re-declared as a fresh `local`,
    /// and a final captured write whose result no surviving reader references
    /// collapses to `local _ = ...`. That is a correctness bug — the closures
    /// call `:Disconnect()` on the still-`nil` declaration.
    ///
    /// The fix walks backward from each capture to the reaching definition,
    /// carrying the *same* open-location set so the group key
    /// (`open_locations.first()`, the only thing `mark_upvalues` reads) is
    /// preserved and the declaration lands in the capture's group. It is
    /// deliberately conservative to avoid absorbing unrelated values that merely
    /// share a bytecode register (the lifter maps one register to one original
    /// local for the whole function):
    ///   * It never scans the capture's *own* block. A same-block reaching
    ///     definition is already handled correctly by the forward pass, and is
    ///     usually the `:Connect` receiver temp (`conn = sig:Connect(...)`
    ///     reuses `conn`'s register for `sig`) — absorbing it would wrongly
    ///     forbid inlining `sig`.
    ///   * It only groups a cross-block definition that is a `nil` literal — the
    ///     shape of a `local x`/`local x = nil` declaration. A non-`nil`
    ///     reaching definition is a distinct value (or a parameter), so the walk
    ///     stops without grouping it.
    ///   * It stops at a `Close` of the local (a reused register's previous
    ///     cell boundary) and never marks live-through blocks, so it only ever
    ///     adds coverage at the one declaration site.
    fn extend_open_backward<const LAZY_CONSUMED: bool>(&mut self, function: &Function) {
        // Extension only publishes at a predecessor's reaching declaration.
        // An edgeless graph cannot enqueue one, regardless of captures/CLOSE
        // inside its block. Keep the forward intervals and cell labels intact.
        if LAZY_CONSUMED && function.graph().edge_count() == 0 {
            ast::telemetry::count("ssa_upvalues_backward_edgeless_skipped", 1);
            return;
        }
        // Seed: one entry per (block, captured local) — the lowest open
        // statement index there and its location set. Collected into a Vec and
        // sorted so processing order, and therefore the result, is independent
        // of `FxHashMap`/`IndexSet` iteration order (determinism).
        let mut starts: Vec<(NodeIndex, usize, ast::RcLocal, IndexSet<(NodeIndex, usize)>)> =
            Vec::new();
        for (&node, locals) in &self.open {
            for (local, ranges) in locals {
                if let Some((range, locs)) = ranges.iter().next() {
                    starts.push((node, *range.start(), local.clone(), locs.clone()));
                }
            }
        }
        starts.sort_by(|a, b| (a.0.index(), a.1, &a.2).cmp(&(b.0.index(), b.1, &b.2)));

        if starts.is_empty() {
            return;
        }

        // SSA versions whose value is consumed by *real* code — read by some
        // statement other than as a closure's by-reference upvalue, then
        // propagated backward through block-parameters (phis), since a phi whose
        // result is consumed also consumes its incoming arguments. (A `Close`
        // reads nothing, so it never marks anything consumed.) We only pull a
        // `nil` declaration into a captured cell when that exact `nil` version is
        // NOT consumed, i.e. it is purely a captured handle whose only readers
        // are the closures (the connection pattern). When the value is also used
        // by ordinary code — e.g. `local x; if c then x = ... end; if not x then
        // ... end` with `x` captured elsewhere — the regular phi/copy coalescing
        // already unifies the `nil` default with the assigned versions;
        // force-merging it into the upvalue cell would only make the out-of-SSA
        // pass materialize the default in every branch (a readability
        // regression). Working at version (not original-local) granularity also
        // keeps an unrelated temp that merely reuses the register — e.g. the
        // `game:GetService(...)` receiver — from being mistaken for a read of
        // the captured cell.
        // Most captures never reach a cross-block nil declaration. Build this
        // whole-function read/phi census only when that decision is needed.
        // Its inputs (function and old_locals) are immutable here; mark_open
        // changes only self.open and therefore cannot invalidate the result.
        let consumed = std::cell::OnceCell::new();
        if !LAZY_CONSUMED { consumed.set(self.consumed_versions(function)).unwrap(); }

        // The set of blocks in which each original local is captured by
        // reference. We only pull a declaration into a cell whose captures are
        // confined to a *single* block. When a local is captured by closures in
        // more than one block, its cell spans a control-flow merge (e.g. a task
        // handle captured by a worker closure in one branch and by a cleanup
        // closure after the merge): the forward pass groups each capture site
        // separately, so force-merging the shared `nil` declaration into one of
        // them would desynchronize the others — an unsound coalescing. Confining
        // to one capture block keeps the cell free of such merges.
        let mut capture_blocks: FxHashMap<ast::RcLocal, FxHashSet<NodeIndex>> =
            FxHashMap::default();
        for (node, block) in function.blocks() {
            for statement in block.iter() {
                for upvalue in ref_upvalues(statement) {
                    if let Some(old) = self.old_locals.get(upvalue) {
                        capture_blocks.entry(old.clone()).or_default().insert(node);
                    }
                }
            }
        }

        // Predecessor blocks left to scan, from their end: (block, local,
        // carried location set). `visited` makes each (block, local) processed
        // at most once, which both bounds the work (no exponential re-walk over
        // loop back-edges) and keeps the outcome deterministic.
        let mut work: VecDeque<(NodeIndex, ast::RcLocal, IndexSet<(NodeIndex, usize)>)> =
            VecDeque::new();
        let mut visited: FxHashSet<(NodeIndex, ast::RcLocal)> = FxHashSet::default();

        // Only walk back when the reaching definition is *not* in the capture's
        // own block (the cross-block case is the bug). A same-block definition
        // or `Close` means the cell originates here and the forward pass already
        // covers it.
        for (node, sc, local, locs) in &starts {
            // Skip locals captured across more than one block (cell may span a
            // merge — see `capture_blocks`).
            if capture_blocks.get(local).map_or(0, |b| b.len()) != 1 {
                continue;
            }
            if self.block_defines_or_closes(function, *node, *sc, local) {
                continue;
            }
            Self::enqueue_predecessors(function, *node, local, locs, &mut work);
        }
        for (node, _, local, _) in &starts {
            visited.insert((*node, local.clone()));
        }

        while let Some((node, local, locs)) = work.pop_front() {
            if !visited.insert((node, local.clone())) {
                continue;
            }
            let block = function.block(node).unwrap();
            let len = block.len();
            let mut decided = false;
            for i in (0..len).rev() {
                match block.get(i).unwrap() {
                    ast::Statement::Close(close) => {
                        if close.locals.contains(&local) {
                            decided = true; // cell boundary: stop, do not group
                            break;
                        }
                    }
                    statement => match self.def_kind(statement, &local) {
                        // Cross-block `nil` declaration whose value no real code
                        // consumes: the cell's initializer — group it.
                        DefKind::Nil(ref version) if !consumed
                            .get_or_init(|| self.consumed_versions(function)).contains(version) => {
                            self.mark_open(node, &local, i, len, &locs);
                            decided = true;
                            break;
                        }
                        // A non-`nil` (re)definition, or a `nil` default whose
                        // value ordinary code also consumes (already unified by
                        // phi coalescing): a distinct value — stop, don't group.
                        DefKind::Nil(_) | DefKind::Other => {
                            decided = true;
                            break;
                        }
                        DefKind::NotDef => {}
                    },
                }
            }
            if decided {
                continue;
            }
            // No statement defines the local here. If a block-parameter (phi)
            // does, that merge IS the reaching definition — stop, and leave it to
            // the regular phi coalescing. Recursing past it would reach the
            // individual phi arms and group only the `nil` one (e.g. the `else`
            // of `if c then x = v else x = nil end`), splitting the merge and
            // making the out-of-SSA pass materialize the default.
            if self.block_has_phi_def(function, node, &local) {
                continue;
            }
            // Truly live-through: keep walking back toward the declaration.
            Self::enqueue_predecessors(function, node, &local, &locs, &mut work);
        }
        if consumed.get().is_none() {
            ast::telemetry::count("ssa_upvalues_backward_consumed_skipped", 1);
        }
    }

    /// True if `block` contains a definition or a `Close` of `local` that the
    /// backward walk must respect: a block-parameter (phi) at the block entry, or
    /// a statement def / `Close` in `[0, scan_upto)` (scanned back-to-front).
    fn block_defines_or_closes(
        &self,
        function: &Function,
        node: NodeIndex,
        scan_upto: usize,
        local: &ast::RcLocal,
    ) -> bool {
        if self.block_has_phi_def(function, node, local) {
            return true;
        }
        let block = function.block(node).unwrap();
        for i in (0..scan_upto).rev() {
            match block.get(i).unwrap() {
                ast::Statement::Close(close) => {
                    if close.locals.contains(local) {
                        return true;
                    }
                }
                statement => {
                    if !matches!(self.def_kind(statement, local), DefKind::NotDef) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// True if `node` has a *real-merge* incoming block-parameter (phi) that
    /// defines `local` — i.e. its incoming arguments are not all the same SSA
    /// version. The backward walk must stop at such a merge (it is the reaching
    /// definition, and recursing into the arms would group only the `nil` arm),
    /// but should pass *through* a phi that is merely a copy of one version:
    ///   * a single-predecessor block (trivial rename), or
    ///   * a *degenerate* phi whose argument is the same version on every edge
    ///     (the local was not actually written on any path through the merge —
    ///     e.g. an unrelated `if` sits between the declaration and the capture).
    /// Passing through the degenerate case lets the walk reach the real
    /// declaration; a degenerate phi is a pure copy, so this stays sound.
    fn block_has_phi_def(
        &self,
        function: &Function,
        node: NodeIndex,
        local: &ast::RcLocal,
    ) -> bool {
        let mut seen: Option<Option<&ast::RcLocal>> = None;
        for (_, edge) in function.edges_to_block(node) {
            let Some((_, arg)) = edge
                .arguments
                .iter()
                .find(|(param, _)| self.old_locals.get(param) == Some(local))
            else {
                continue;
            };
            let arg = arg.as_local();
            match seen {
                None => seen = Some(arg),
                // A different incoming version, or a non-local argument we can't
                // prove identical, means a genuine merge.
                Some(prev) if prev != arg || arg.is_none() => return true,
                Some(_) => {}
            }
        }
        false
    }

    /// Classify how `statement` defines (the old) `local`:
    ///   * `Nil(v)` — a `nil`-literal assignment writing SSA version `v` (the
    ///     shape of a `local x`/`local x = nil` declaration).
    ///   * `Other` — a definition by any other means (a real value, a for-loop
    ///     counter, …).
    ///   * `NotDef` — does not define `local`.
    fn def_kind(&self, statement: &ast::Statement, local: &ast::RcLocal) -> DefKind {
        if let ast::Statement::Assign(assign) = statement {
            for (j, lhs) in assign.left.iter().enumerate() {
                if let Some(version) = lhs
                    .as_local()
                    .filter(|w| self.old_locals.get(w) == Some(local))
                {
                    return if matches!(
                        assign.right.get(j),
                        Some(ast::RValue::Literal(ast::Literal::Nil))
                    ) {
                        DefKind::Nil(version.clone())
                    } else {
                        DefKind::Other
                    };
                }
            }
        }
        // Non-`Assign` writers (e.g. for-loop counters) still count as a def.
        if statement.any_local_write(&mut |written| self.old_locals.get(written) == Some(local)) {
            return DefKind::Other;
        }
        DefKind::NotDef
    }

    /// SSA versions whose value is consumed by *real* code: those read by a
    /// statement otherwise than as a closure's by-reference upvalue, then closed
    /// backward through block-parameters (a phi whose result is consumed
    /// consumes each of its incoming arguments). A `Close` reads nothing, so it
    /// never marks anything consumed. This distinguishes a purely-captured handle
    /// (whose `nil` initializer flows only to the closures and the cell's
    /// `Close`) from a local that ordinary code also uses — at version
    /// granularity, so an unrelated temp reusing the same register does not
    /// count.
    fn consumed_versions(&self, function: &Function) -> FxHashSet<ast::RcLocal> {
        ast::telemetry::count("ssa_upvalues_consumed_censuses", 1);
        #[cfg(test)]
        CONSUMED_CENSUSES.with(|count| count.set(count.get() + 1));
        let mut consumed: FxHashSet<ast::RcLocal> = FxHashSet::default();
        for (_, block) in function.blocks() {
            for statement in block.iter() {
                // By-ref upvalues captured directly on this statement (a
                // `temp = function() ... end` assignment) are captures, not real
                // reads — exclude them.
                let captured: FxHashSet<&ast::RcLocal> = ref_upvalues(statement).collect();
                for read in statement.values_read() {
                    if !captured.contains(read) {
                        consumed.insert(read.clone());
                    }
                }
            }
        }

        // Block-parameter (phi) back-propagation: result consumed ⇒ arguments
        // consumed.
        let mut param_args: FxHashMap<ast::RcLocal, Vec<ast::RcLocal>> = FxHashMap::default();
        for edge in function.graph().edge_weights() {
            for (param, arg) in &edge.arguments {
                if let ast::RValue::Local(a) = arg {
                    param_args.entry(param.clone()).or_default().push(a.clone());
                }
            }
        }
        let mut worklist: Vec<ast::RcLocal> = consumed.iter().cloned().collect();
        while let Some(version) = worklist.pop() {
            if let Some(args) = param_args.get(&version) {
                for arg in args {
                    if consumed.insert(arg.clone()) {
                        worklist.push(arg.clone());
                    }
                }
            }
        }
        consumed
    }

    /// Mark `local` open over `[from, block_len - 1]` in `node`, carrying `locs`
    /// (whose first element is the cell's first capture, the grouping key). Any
    /// pre-existing locations at `from` are appended after, so the carried
    /// first-open location stays the consumer's `.first()`.
    fn mark_open(
        &mut self,
        node: NodeIndex,
        local: &ast::RcLocal,
        from: usize,
        block_len: usize,
        locs: &IndexSet<(NodeIndex, usize)>,
    ) {
        if block_len == 0 {
            return;
        }
        let ranges = self
            .open
            .entry(node)
            .or_default()
            .entry(local.clone())
            .or_default();
        let mut new_locs = locs.clone();
        if let Some(prev) = ranges.get(&from) {
            new_locs.extend(prev.iter().copied());
        }
        ranges.insert(from..=block_len - 1, new_locs);
    }

    fn enqueue_predecessors(
        function: &Function,
        node: NodeIndex,
        local: &ast::RcLocal,
        locs: &IndexSet<(NodeIndex, usize)>,
        work: &mut VecDeque<(NodeIndex, ast::RcLocal, IndexSet<(NodeIndex, usize)>)>,
    ) {
        let mut preds: Vec<NodeIndex> = function.predecessor_blocks(node).collect();
        // Stable order so the worklist (and therefore the result) is deterministic.
        preds.sort_by_key(|n| n.index());
        for pred in preds {
            work.push_back((pred, local.clone(), locs.clone()));
        }
    }
}
