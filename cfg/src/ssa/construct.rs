use std::iter;

use ast::{LocalRw, RcLocal, Traverse};
use indexmap::{IndexMap, IndexSet};
use itertools::{Either, Itertools};
use petgraph::{
    algo::kosaraju_scc,
    graph::DiGraph,
    stable_graph::NodeIndex,
    visit::{Dfs, EdgeRef, Walker},
    Direction,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{function::Function, ssa::param_dependency_graph::ParamDependencyGraph};

use super::upvalues::UpvaluesOpen;

struct SsaConstructor<'a> {
    function: &'a mut Function,
    dfs: IndexSet<NodeIndex>,
    incomplete_params: FxHashMap<NodeIndex, FxHashMap<RcLocal, RcLocal>>,
    filled_blocks: FxHashSet<NodeIndex>,
    sealed_blocks: FxHashSet<NodeIndex>,
    // TODO: combine current/all/old into one map
    current_definition: FxHashMap<RcLocal, FxHashMap<NodeIndex, RcLocal>>,
    all_definitions: FxHashMap<RcLocal, FxHashSet<RcLocal>>,
    old_locals: FxHashMap<RcLocal, RcLocal>,
    local_count: usize,
    local_map: FxHashMap<RcLocal, RcLocal>,
    new_upvalues_in: IndexMap<RcLocal, FxHashSet<RcLocal>>,
    upvalues_passed: FxHashMap<RcLocal, FxHashMap<(NodeIndex, usize), FxHashSet<RcLocal>>>,
}

// TODO: REFACTOR: move out of construct module
// TODO: support RValues other than Local and use an local -> rvalue map
// https://github.com/fkie-cad/dewolf/blob/7afe5b46e79a7b56e9904e63f29d54bd8f7302d9/decompiler/pipeline/ssa/phi_cleaner.py
/// Remove mutually-recursive block parameters that merely carry one by-reference
/// upvalue cell through nested loops.
///
/// A single loop produces `p = phi(cell, p)`, which the per-block cleanup below
/// handles by ignoring the self edge. Nested loops instead produce an SCC such as
/// `outer = phi(cell, inner); inner = phi(outer, inner)`. Looking at either phi
/// alone makes the other parameter appear to be a second value, so out-of-SSA
/// materializes a stale `local snapshot = cell`.
///
/// Collapse the whole SCC only when every member has already been marked by SSA
/// construction as the same upvalue cell and every external value agrees with
/// that group. Mixed values, non-local arguments, snapshots, and distinct cells
/// reject the SCC; ordinary loop phis remain untouched.
fn remove_upvalue_param_sccs(
    function: &mut Function,
    local_map: &mut FxHashMap<RcLocal, RcLocal>,
    upvalue_to_group: &IndexMap<RcLocal, RcLocal>,
) -> bool {
    if upvalue_to_group.is_empty() {
        return false;
    }
    #[derive(Clone)]
    struct CellLabel {
        group: RcLocal,
        canonical: RcLocal,
    }

    #[inline]
    fn resolve<'a>(mut local: &'a RcLocal, map: &'a FxHashMap<RcLocal, RcLocal>) -> &'a RcLocal {
        while let Some(next) = map.get(local) {
            local = next;
        }
        local
    }

    // The overwhelmingly common captured-local function has no phi carrying a
    // cell. Avoid all graph allocation/sorting in that case.
    let has_cell_phi = function.graph().edge_weights().any(|edge| {
        edge.arguments.iter().any(|(param, argument)| {
            upvalue_to_group.contains_key(resolve(param, local_map))
                || argument.as_local().is_some_and(|argument| {
                    upvalue_to_group.contains_key(resolve(argument, local_map))
                })
        })
    });
    if !has_cell_phi {
        return false;
    }

    // Stable ordering makes graph construction deterministic even though CFG
    // edge storage itself is not an ordering contract.
    let mut params: Vec<RcLocal> = function
        .graph()
        .edge_weights()
        .flat_map(|edge| {
            edge.arguments
                .iter()
                .map(|(param, _)| resolve(param, local_map).clone())
        })
        .collect();
    params.sort();
    params.dedup();
    if params.is_empty() {
        return false;
    }

    let mut graph = DiGraph::<RcLocal, ()>::new();
    let mut nodes = FxHashMap::default();
    for param in params {
        let node = graph.add_node(param.clone());
        nodes.insert(param, node);
    }
    let mut incoming: FxHashMap<RcLocal, Vec<Option<RcLocal>>> = FxHashMap::default();
    for edge in function.graph().edge_weights() {
        for (param, argument) in &edge.arguments {
            let param = resolve(param, local_map);
            let argument = argument
                .as_local()
                .map(|argument| resolve(argument, local_map).clone());
            incoming
                .entry(param.clone())
                .or_default()
                .push(argument.clone());
            if let Some(argument) = argument {
                if let (Some(&from), Some(&to)) = (nodes.get(param), nodes.get(&argument)) {
                    graph.add_edge(from, to, ());
                }
            }
        }
    }

    let mut components = kosaraju_scc(&graph);
    for component in &mut components {
        component.sort_by(|left, right| graph[*left].cmp(&graph[*right]));
    }
    components.sort_by(|left, right| graph[left[0]].cmp(&graph[right[0]]));

    let mut component_of = FxHashMap::default();
    for (component_index, component) in components.iter().enumerate() {
        for &node in component {
            component_of.insert(graph[node].clone(), component_index);
        }
    }

    // Build the SCC condensation graph once. Kahn-style propagation labels each
    // component exactly once after deterministic O(P log P) graph ordering,
    // instead of repeatedly rescanning every CFG edge for every component.
    let mut direct: Vec<Option<CellLabel>> = vec![None; components.len()];
    let mut expected_group: Vec<Option<RcLocal>> = vec![None; components.len()];
    let mut dependencies: Vec<Vec<usize>> = vec![Vec::new(); components.len()];
    let mut invalid = vec![false; components.len()];
    let mut has_external = vec![false; components.len()];
    for (component_index, component) in components.iter().enumerate() {
        for &node in component {
            let param = &graph[node];
            let Some(group) = upvalue_to_group.get(param) else {
                // An unmarked phi may be a deliberate snapshot seeded from a
                // live cell. Incoming provenance alone must never promote it.
                invalid[component_index] = true;
                continue;
            };
            if let Some(expected) = &expected_group[component_index] {
                if expected != group {
                    invalid[component_index] = true;
                }
            } else {
                expected_group[component_index] = Some(group.clone());
            }
            for argument in incoming.get(param).into_iter().flatten() {
                let Some(argument) = argument else {
                    invalid[component_index] = true;
                    continue;
                };
                if component_of.get(argument) == Some(&component_index) {
                    continue;
                }
                has_external[component_index] = true;
                if let Some(group) = upvalue_to_group.get(argument) {
                    if expected_group[component_index].as_ref() != Some(group) {
                        invalid[component_index] = true;
                    }
                    let label = CellLabel {
                        group: group.clone(),
                        canonical: argument.clone(),
                    };
                    if let Some(current) = &mut direct[component_index] {
                        if current.group != label.group {
                            invalid[component_index] = true;
                        } else if label.canonical < current.canonical {
                            current.canonical = label.canonical;
                        }
                    } else {
                        direct[component_index] = Some(label);
                    }
                } else if let Some(&dependency) = component_of.get(argument) {
                    dependencies[component_index].push(dependency);
                } else {
                    invalid[component_index] = true;
                }
            }
        }
        dependencies[component_index].sort_unstable();
        dependencies[component_index].dedup();
    }

    let mut dependents = vec![Vec::new(); components.len()];
    let mut unresolved: Vec<usize> = dependencies.iter().map(Vec::len).collect();
    for (component, deps) in dependencies.iter().enumerate() {
        for &dependency in deps {
            dependents[dependency].push(component);
        }
    }
    // Components and their dependency lists were visited in stable ascending
    // order, so each dependents list is already deterministic and sorted.

    #[derive(Clone)]
    enum LabelState {
        Pending,
        Resolved(CellLabel),
        Rejected,
    }
    let mut states = vec![LabelState::Pending; components.len()];
    let mut ready: std::collections::VecDeque<usize> = unresolved
        .iter()
        .enumerate()
        .filter_map(|(index, &count)| (count == 0).then_some(index))
        .collect();
    while let Some(component) = ready.pop_front() {
        let mut label = direct[component].clone();
        let mut rejected = invalid[component] || !has_external[component];
        if !rejected {
            for &dependency in &dependencies[component] {
                let LabelState::Resolved(incoming) = &states[dependency] else {
                    rejected = true;
                    break;
                };
                if let Some(current) = &mut label {
                    if current.group != incoming.group {
                        rejected = true;
                        break;
                    }
                    if incoming.canonical < current.canonical {
                        current.canonical = incoming.canonical.clone();
                    }
                } else {
                    label = Some(incoming.clone());
                }
            }
        }
        if label
            .as_ref()
            .is_some_and(|label| expected_group[component].as_ref() != Some(&label.group))
        {
            rejected = true;
        }
        states[component] = if rejected {
            LabelState::Rejected
        } else if let Some(label) = label {
            LabelState::Resolved(label)
        } else {
            LabelState::Rejected
        };
        for &dependent in &dependents[component] {
            unresolved[dependent] -= 1;
            if unresolved[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }

    let mut removed = FxHashSet::default();
    let mut mappings = Vec::new();
    for (component_index, state) in states.into_iter().enumerate() {
        let LabelState::Resolved(label) = state else {
            continue;
        };
        for &node in &components[component_index] {
            let param = graph[node].clone();
            if param != label.canonical {
                mappings.push((param.clone(), label.canonical.clone()));
            }
            removed.insert(param);
        }
    }
    if removed.is_empty() {
        return false;
    }
    // Retain against the caller's pre-existing map before adding this pass's new
    // replacements; otherwise `P -> Q` input maps could leave raw P destinations
    // behind after Q is removed.
    for edge in function.graph_mut().edge_weights_mut() {
        edge.arguments.retain(|(param, _)| {
            let param = resolve(param, local_map);
            !removed.contains(param)
        });
    }
    for (param, canonical) in mappings {
        local_map.insert(param, canonical);
    }
    true
}

pub fn remove_unnecessary_params(
    function: &mut Function,
    local_map: &mut FxHashMap<RcLocal, RcLocal>,
    // When provided (the post-construct fixpoint passes), a self-referential
    // back-edge arg is excluded ONLY for a param that is an upvalue-cell version.
    // This removes the trivial loop phi `p = phi(x, p)` of a by-ref upvalue cell —
    // otherwise materialized as a pinned stale snapshot `local v2 = v` (C4) — while
    // leaving every NON-upvalue loop-header phi exactly as before, so the
    // restructurer (which relies on those phis) is unaffected. `None` reproduces
    // the original behavior verbatim.
    upvalue_to_group: Option<&IndexMap<RcLocal, RcLocal>>,
) -> bool {
    let mut changed = upvalue_to_group
        .is_some_and(|groups| remove_upvalue_param_sccs(function, local_map, groups));
    for node in function.blocks().map(|(i, _)| i).collect::<Vec<_>>() {
        let mut dependency_graph = ParamDependencyGraph::new(function, node);
        let mut removable_params = FxHashMap::default();
        let edges = function
            .graph()
            .edges_directed(node, Direction::Incoming)
            .collect::<Vec<_>>();
        if !edges.is_empty() {
            let params = edges[0].weight().arguments.iter().map(|(p, _)| p);
            let args_in_by_block = edges
                .iter()
                .map(|e| {
                    e.weight()
                        .arguments
                        .iter()
                        .map(|(_, a)| a)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut params_to_remove = FxHashSet::default();
            for (index, mut param) in params.enumerate() {
                if args_in_by_block
                    .iter()
                    .map(|a| a[index])
                    .any(|r| r.as_local().is_none())
                {
                    continue;
                }
                // Is this loop-header param a version of a by-ref upvalue cell? The
                // phi result itself is NOT in `upvalue_to_group` (only the captured
                // versions are), but a cell's loop phi `p = phi(state_0, p)` has an
                // INCOMING ARG that is a grouped cell version. Detecting via the args
                // scopes the C4 self-back-edge exclusion to genuine upvalue-cell loop
                // phis, leaving the non-upvalue loop phis the restructurer needs
                // exactly as before.
                let is_upvalue_cell = upvalue_to_group.is_some_and(|group| {
                    args_in_by_block
                        .iter()
                        .map(|a| a[index])
                        .filter_map(|r| r.as_local())
                        .any(|a| {
                            let mut ra = a;
                            while let Some(t) = local_map.get(ra) {
                                ra = t;
                            }
                            group.contains_key(a) || group.contains_key(ra)
                        })
                });
                if is_upvalue_cell {
                    // C4 path: resolve the param, then collect distinct incoming
                    // locals EXCLUDING the self-referential back-edge arg. The
                    // trivial loop phi `p = phi(x, p)` then reduces to `x` (or, if
                    // all-self, is removed), so a post-loop read binds to the live
                    // cell instead of a pinned pre-loop snapshot.
                    let mut resolved_param = param;
                    while let Some(param_to) = local_map.get(resolved_param) {
                        resolved_param = param_to;
                    }
                    let resolved_param = resolved_param.clone();
                    let mut arg_set: FxHashSet<&RcLocal> = FxHashSet::default();
                    for a in args_in_by_block
                        .iter()
                        .map(|a| a[index])
                        .filter_map(|r| r.as_local())
                    {
                        let mut ra = a;
                        while let Some(t) = local_map.get(ra) {
                            ra = t;
                        }
                        if *ra != resolved_param {
                            arg_set.insert(a);
                        }
                    }
                    if arg_set.len() == 1 {
                        let mut arg = arg_set.into_iter().next().unwrap();
                        while let Some(arg_to) = local_map.get(arg) {
                            arg = arg_to;
                        }
                        if *arg != resolved_param {
                            removable_params.insert(resolved_param.clone(), arg.clone());
                        } else if let Some(&param_node) =
                            dependency_graph.local_to_node.get(&resolved_param)
                        {
                            dependency_graph.remove_node(param_node);
                        }
                        params_to_remove.insert(resolved_param.clone());
                    } else if arg_set.is_empty() {
                        // all-self phi `p = phi(p, p)` — the original code removed it
                        // as trivial; reproduce that (preserves byte-identity).
                        if let Some(&param_node) =
                            dependency_graph.local_to_node.get(&resolved_param)
                        {
                            dependency_graph.remove_node(param_node);
                        }
                        params_to_remove.insert(resolved_param.clone());
                    }
                } else {
                    // ORIGINAL behavior, verbatim (non-upvalue params).
                    // TODO: should we really be doing this by index?
                    let arg_set = args_in_by_block
                        .iter()
                        .map(|a| a[index])
                        .filter_map(|r| r.as_local())
                        .collect::<FxHashSet<_>>();
                    if arg_set.len() == 1 {
                        while let Some(param_to) = local_map.get(param) {
                            param = param_to;
                        }
                        let mut arg = arg_set.into_iter().next().unwrap();
                        while let Some(arg_to) = local_map.get(arg) {
                            arg = arg_to;
                        }
                        if arg != param {
                            // param is not trivial, replace the param with the arg
                            removable_params.insert(param.clone(), arg.clone());
                        } else if let Some(&param_node) = dependency_graph.local_to_node.get(param)
                        {
                            // param is trivial: x = phi(x, x, ..., x)
                            dependency_graph.remove_node(param_node);
                        }
                        params_to_remove.insert(param.clone());
                    }
                }
            }
            if !params_to_remove.is_empty() {
                for edge in edges.into_iter().map(|e| e.id()).collect::<Vec<_>>() {
                    function
                        .graph_mut()
                        .edge_weight_mut(edge)
                        .unwrap()
                        .arguments
                        .retain(|(p, _)| {
                            let mut p = p;
                            while let Some(p_to) = local_map.get(p) {
                                p = p_to;
                            }
                            !params_to_remove.contains(p)
                        });
                }
                changed = true;
            }
        }

        let mut removable_params_degree_zero = removable_params
            .iter()
            .map(|(p, a)| (p.clone(), a))
            .filter(|(p, _)| {
                dependency_graph
                    .graph
                    .neighbors(dependency_graph.local_to_node[p])
                    .count()
                    == 0
            })
            .collect::<Vec<_>>();

        while let Some((param, mut arg)) = removable_params_degree_zero.pop() {
            let param_node = dependency_graph.local_to_node[&param];
            for param_pred_node in dependency_graph
                .graph
                .neighbors_directed(param_node, Direction::Incoming)
            {
                if dependency_graph.graph.neighbors(param_pred_node).count() == 1 {
                    let param_pred = dependency_graph
                        .graph
                        .node_weight(param_pred_node)
                        .unwrap()
                        .clone();
                    if let Some(param_pred_arg) = removable_params.get(&param_pred) {
                        removable_params_degree_zero.push((param_pred, param_pred_arg));
                    }
                }
            }
            dependency_graph.remove_node(param_node);

            while let Some(arg_to) = local_map.get(arg) {
                arg = arg_to;
            }
            local_map.insert(param, arg.clone());
            changed = true;
        }
    }
    changed
}

// TODO: STYLE: rename function
// TODO: STYLE: rename `uses_local`, we need a generic name for ast nodes, maybe `traversible`?
fn apply_local_map_to_values_referenced<T: LocalRw + Traverse>(
    uses_local: &mut T,
    local_map: &FxHashMap<RcLocal, RcLocal>,
) {
    // TODO: figure out values_mut
    for (from, mut to) in uses_local
        .values_written_mut()
        .into_iter()
        .filter_map(|v| local_map.get(v).map(|t| (v, t)))
    {
        while let Some(to_to) = local_map.get(to) {
            to = to_to;
        }
        *from = to.clone();
    }
    for (from, mut to) in uses_local
        .values_read_mut()
        .into_iter()
        .filter_map(|v| local_map.get(v).map(|t| (v, t)))
    {
        while let Some(to_to) = local_map.get(to) {
            to = to_to;
        }
        *from = to.clone();
    }
    // The `local_map`-keyed map this loop used to build (`from -> to`) fed only
    // the commented-out closure-body replace below; with no consumer it was pure
    // overhead (two RcLocal clones per read-local), so it has been removed.
    // uses_local.traverse_rvalues(&mut |rvalue| {
    //     if let Some(closure) = rvalue.as_closure_mut() {
    //         replace_locals(&mut closure.body, &map)
    //     }
    // });
}

// does not replace locals in child closures
pub fn apply_local_map(function: &mut Function, local_map: FxHashMap<RcLocal, RcLocal>) {
    for param in &mut function.parameters {
        if let Some(mut new_param) = local_map.get(param) {
            // TODO: make sure this doesnt cycle if theres a li -> li entry
            while let Some(new_to) = local_map.get(new_param) {
                new_param = new_to;
            }
            *param = new_param.clone();
        }
    }
    // TODO: blocks_mut
    for node in function.graph().node_indices().collect::<Vec<_>>() {
        let block = function.block_mut(node).unwrap();
        for stat in block.iter_mut() {
            apply_local_map_to_values_referenced(stat, &local_map);
        }
        for edge in function.edges(node).map(|e| e.id()).collect::<Vec<_>>() {
            // TODO: rename Stat::values, Expr::values to locals() and refer to locals as locals everywhere
            for local in function
                .graph_mut()
                .edge_weight_mut(edge)
                .unwrap()
                .arguments
                .iter_mut()
                .flat_map(|(p, a)| iter::once(Either::Left(p)).chain(iter::once(Either::Right(a))))
            {
                match local {
                    Either::Left(local) => {
                        if let Some(mut new_local) = local_map.get(local) {
                            // TODO: make sure this doesnt cycle if theres a li -> li entry
                            // also see TODO in destruct.rs
                            while let Some(new_to) = local_map.get(new_local) {
                                new_local = new_to;
                            }
                            *local = new_local.clone();
                        }
                    }
                    Either::Right(rvalue) => {
                        apply_local_map_to_values_referenced(rvalue, &local_map);
                    }
                }
            }
        }
    }
}

// based on "Simple and Efficient Construction of Static Single Assignment Form" (https://pp.info.uni-karlsruhe.de/uploads/publikationen/braun13cc.pdf)
impl<'a> SsaConstructor<'a> {
    fn write_local(&mut self, node: NodeIndex, local: &RcLocal, new_local: &RcLocal) {
        self.all_definitions
            .entry(local.clone())
            .or_default()
            .insert(new_local.clone());
        self.current_definition
            .entry(local.clone())
            .or_default()
            .insert(node, new_local.clone());
    }

    fn add_param_args(
        &mut self,
        node: NodeIndex,
        local: &RcLocal,
        param_local: RcLocal,
    ) -> RcLocal {
        for (source, edge) in self
            .function
            .graph()
            .edges_directed(node, Direction::Incoming)
            .map(|e| (e.source(), e.id()))
            .collect::<Vec<_>>()
        {
            let argument_local = self.find_local(source, local);
            self.function
                .graph_mut()
                .edge_weight_mut(edge)
                .unwrap()
                .arguments
                .push((param_local.clone(), argument_local.into()));
        }
        // TODO: fix lol
        // self.try_remove_trivial_param(node, param_local)
        param_local
    }

    fn try_remove_trivial_param(&mut self, node: NodeIndex, param_local: RcLocal) -> RcLocal {
        let mut same = None;
        let args_in = self.function.edges_to_block(node).map(|(_, e)| {
            &e.arguments
                .iter()
                .find(|(p, _)| p == &param_local)
                .unwrap()
                .1
        });
        for arg in args_in {
            let mut arg = arg.as_local().unwrap();
            while let Some(arg_to) = self.local_map.get(arg) {
                arg = arg_to;
            }

            if Some(&arg) == same.as_ref() || arg == &param_local {
                // unique value or self-reference
                continue;
            }
            if same.is_some() {
                // the param merges at least two values: not trivial
                return param_local;
            }
            same = Some(arg);
        }
        let same = same.unwrap().clone();
        self.local_map.insert(param_local.clone(), same.clone());

        // TODO: optimize
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            let mut edges = self
                .function
                .edges_to_block(node)
                .map(|(_, e)| e)
                .peekable();
            if edges
                .peek()
                .map(|e| !e.arguments.is_empty())
                .unwrap_or(false)
            {
                let edges = edges.collect::<Vec<_>>();
                if edges.iter().any(|e| {
                    e.arguments
                        .iter()
                        .any(|(_, a)| a.as_local().unwrap() == &param_local)
                }) {
                    let params_in = edges
                        .into_iter()
                        .map(|e| {
                            e.arguments
                                .iter()
                                .map(|(p, _)| p)
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    for mut param in params_in[0].iter() {
                        while let Some(param_to) = self.local_map.get(param) {
                            param = param_to;
                        }

                        if param == &param_local
                            || params_in.iter().any(|e| e.iter().any(|p| p == param))
                        {
                            self.try_remove_trivial_param(node, param.clone());
                        }
                    }
                }
            }
        }

        same
    }

    fn find_local(&mut self, node: NodeIndex, local: &RcLocal) -> RcLocal {
        let res = if let Some(new_local) = self
            .current_definition
            .get(local)
            .and_then(|x| x.get(&node))
        {
            // local to block
            new_local.clone()
        } else {
            // search globally
            if !self.sealed_blocks.contains(&node) {
                // TODO: this code is repeated multiple times, create new_local function
                let param_local = RcLocal::default();
                self.old_locals.insert(param_local.clone(), local.clone());
                if let Some(upvalues) = self.new_upvalues_in.get_mut(local) {
                    upvalues.insert(param_local.clone());
                }
                self.local_count += 1;
                self.incomplete_params
                    .entry(node)
                    .or_default()
                    .insert(local.clone(), param_local.clone());
                param_local
            } else if let Ok(pred) = self.function.predecessor_blocks(node).exactly_one() {
                self.find_local(pred, local)
            } else {
                let param_local = RcLocal::default();
                self.old_locals.insert(param_local.clone(), local.clone());
                if let Some(upvalues) = self.new_upvalues_in.get_mut(local) {
                    upvalues.insert(param_local.clone());
                }
                self.local_count += 1;
                self.write_local(node, local, &param_local);

                self.add_param_args(node, local, param_local)
            }
        };
        self.write_local(node, local, &res);
        res
    }

    fn propagate_copies(&mut self) {
        // TODO: blocks_mut
        for node in self.function.graph().node_indices().collect::<Vec<_>>() {
            let block = self.function.block_mut(node).unwrap();
            for index in block
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.as_assign().map(|_| i))
                .collect::<Vec<_>>()
            {
                let block = self.function.block_mut(node).unwrap();
                let assign = block[index].as_assign().unwrap();
                if assign.left.len() == 1
                    && assign.right.len() == 1
                    && let Some(from) = assign.left[0].as_local()
                    && let from_old = &self.old_locals[from]
                    && !self.new_upvalues_in.contains_key(from_old)
                    && !self.upvalues_passed.contains_key(from_old)
                    && let Some(mut to) = assign.right[0].as_local()
                {
                    // TODO: STYLE: this name lol
                    while let Some(to_to) = self.local_map.get(to) {
                        to = to_to;
                    }
                    let to_old = &self.old_locals[to];
                    if !self.new_upvalues_in.contains_key(to_old)
                        && !self.upvalues_passed.contains_key(to_old)
                    {
                        self.local_map.insert(from.clone(), to.clone());
                        block[index] = ast::Empty {}.into();
                    }
                }
            }
            // we check block.ast.len() elsewhere and do `i - ` elsewhere so we need to get rid of empty statements
            // TODO: fix here and elsewhere, see inline.rs
            let block = self.function.block_mut(node).unwrap();
            block.retain(|s| s.as_empty().is_none());
        }
    }

    fn mark_upvalue_version(
        &mut self,
        upvalues_open: &UpvaluesOpen,
        node: NodeIndex,
        stat_index: usize,
        value: RcLocal,
    ) {
        let old_local = &self.old_locals[&value];
        let Some(open_locations) = upvalues_open
            .open
            .get(&node)
            .and_then(|locals| locals.get(old_local))
            .and_then(|ranges| ranges.get(&stat_index))
        else {
            return;
        };
        if let Some(new_upvalues_in) = self.new_upvalues_in.get_mut(old_local) {
            assert!(new_upvalues_in.contains(&value));
        } else {
            self.upvalues_passed
                .entry(old_local.clone())
                .or_default()
                .entry(*open_locations.first().unwrap())
                .or_default()
                .insert(value);
        }
    }

    fn mark_upvalues(&mut self) {
        let upvalues_open = UpvaluesOpen::new(self.function, self.old_locals.clone());
        let nodes: Vec<NodeIndex> = self.dfs.iter().copied().collect();
        for node in nodes {
            // Block parameters are SSA definitions too. If the original local is
            // already an open by-reference cell at block entry, the phi result is
            // another version of that exact cell. Previously only statement values
            // were marked, so nested-loop params lost this provenance and later
            // looked like ordinary snapshots. Collect once from incoming edges
            // (all predecessors carry the same destination params).
            let mut params: Vec<RcLocal> = self
                .function
                .edges_to_block(node)
                .flat_map(|(_, edge)| edge.arguments.iter().map(|(param, _)| param.clone()))
                .collect();
            params.sort();
            params.dedup();
            for param in params {
                self.mark_upvalue_version(&upvalues_open, node, 0, param);
            }

            for stat_index in 0..self.function.block(node).unwrap().len() {
                let statement = self.function.block(node).unwrap().get(stat_index).unwrap();
                let values = statement.values().into_iter().cloned().collect::<Vec<_>>();
                for value in values {
                    self.mark_upvalue_version(&upvalues_open, node, stat_index, value);
                }
            }
            self.function
                .block_mut(node)
                .unwrap()
                .retain(|statement| !matches!(statement, ast::Statement::Close(_)))
        }
    }

    fn read(&mut self, node: NodeIndex, stat_index: usize) {
        let statement = self
            .function
            .block_mut(node)
            .unwrap()
            .get_mut(stat_index)
            .unwrap();
        let read = statement
            .values_read()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        // TODO: do we need two loops?
        let mut map = FxHashMap::default();
        map.reserve(read.len());
        // TODO: REFACTOR: extend
        for local in &read {
            let new_local = self.find_local(node, local);
            map.insert(local.clone(), new_local);
        }
        for (local_index, local) in read.into_iter().enumerate() {
            let statement = self
                .function
                .block_mut(node)
                .unwrap()
                .get_mut(stat_index)
                .unwrap();
            *statement.values_read_mut()[local_index] = map[&local].clone();
        }
    }

    fn construct(
        mut self,
    ) -> (
        usize,
        Vec<FxHashSet<RcLocal>>,
        Vec<(RcLocal, FxHashSet<RcLocal>)>,
        Vec<FxHashSet<RcLocal>>,
    ) {
        let entry = self.function.entry().unwrap();
        // Still-unsealed visited non-entry nodes, in DFS visit order. Replaces a
        // `visited_nodes` Vec that the sealing step below used to re-scan in FULL
        // every outer iteration (O(blocks²) per function). A node, once sealed, was
        // already skipped by the old scan's `!sealed_blocks.contains` guard, so
        // dropping it from this list changes neither which nodes get sealed nor the
        // ORDER in which `add_param_args` runs (that order fixes SSA local-creation
        // order and thus the output) — it only removes the redundant re-scan.
        let mut unsealed = Vec::with_capacity(self.function.graph().node_count());
        for i in 0..self.dfs.len() {
            let node = self.dfs[i];
            for stat_index in 0..self.function.block(node).unwrap().len() {
                let statement = self
                    .function
                    .block_mut(node)
                    .unwrap()
                    .get_mut(stat_index)
                    .unwrap();
                if let Some(assign) = statement.as_assign()
                    && assign.left.len() == 1
                    && assign.right.len() == 1
                    && let Some(local) = assign.left[0].as_local().cloned()
                    && assign.right[0].as_closure().is_some()
                {
                    let new_local = RcLocal::default();
                    self.old_locals.insert(new_local.clone(), local.clone());
                    if let Some(upvalues) = self.new_upvalues_in.get_mut(&local) {
                        upvalues.insert(new_local.clone());
                    }
                    self.local_count += 1;
                    self.write_local(node, &local, &new_local);
                    let statement = self
                        .function
                        .block_mut(node)
                        .unwrap()
                        .get_mut(stat_index)
                        .unwrap();
                    let assign = statement.as_assign_mut().unwrap();
                    *assign.left[0].as_local_mut().unwrap() = new_local.clone();
                    // we do read after bc of recursive closures
                    self.read(node, stat_index);
                } else {
                    let written = statement
                        .values_written()
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    self.read(node, stat_index);
                    // write
                    for (local_index, local) in written.iter().enumerate() {
                        let new_local = RcLocal::default();
                        self.old_locals.insert(new_local.clone(), local.clone());
                        if let Some(upvalues) = self.new_upvalues_in.get_mut(local) {
                            upvalues.insert(new_local.clone());
                        }
                        self.local_count += 1;
                        self.write_local(node, local, &new_local);
                        let statement = self
                            .function
                            .block_mut(node)
                            .unwrap()
                            .get_mut(stat_index)
                            .unwrap();
                        *statement.values_written_mut()[local_index] = new_local;
                    }
                }

                // if !map.is_empty() {
                //     let statement = self
                //         .function
                //         .block_mut(node)
                //         .unwrap()
                //         .get_mut(stat_index)
                //         .unwrap();
                //     statement.traverse_rvalues(&mut |rvalue| {
                //         if let Some(closure) = rvalue.as_closure_mut() {
                //             replace_locals(&mut closure.body, &map)
                //         }
                //     });
                // }
            }
            self.filled_blocks.insert(node);
            if node != entry {
                unsealed.push(node);
            }

            // Seal every still-unsealed visited node whose predecessors are now all
            // filled. `retain` visits in insertion (DFS) order and removes the sealed
            // ones, so this is behaviourally identical to the old full `visited_nodes`
            // scan (which skipped already-sealed nodes) — same seal decisions, same
            // `add_param_args` order — but no longer O(blocks²).
            unsealed.retain(|&node| {
                if self
                    .function
                    .predecessor_blocks(node)
                    .any(|p| !self.filled_blocks.contains(&p))
                {
                    return true; // a predecessor is not yet filled — keep unsealed
                }
                if let Some(incomplete_params) = self.incomplete_params.remove(&node) {
                    for (local, param_local) in incomplete_params {
                        // TODO: this is a bit weird, maybe we should have a upvalue rvalue
                        if !self.new_upvalues_in.contains_key(&local) {
                            self.add_param_args(node, &local, param_local);
                        }
                    }
                }
                self.sealed_blocks.insert(node);
                false // sealed — drop from the unsealed set
            });
        }

        // TODO: this is a bit meh, maybe we should have an argument rvalue
        if let Some(mut incomplete_params) = self.incomplete_params.remove(&entry) {
            for param in &mut self.function.parameters {
                *param = incomplete_params.remove(param).unwrap_or_default();
            }
        }
        assert!(self.incomplete_params.is_empty());

        // TODO: irreducible control flow (see the paper this algorithm is from)
        // TODO: apply_local_map unnecessary number of calls
        apply_local_map(self.function, std::mem::take(&mut self.local_map));

        self.mark_upvalues();
        self.propagate_copies();
        apply_local_map(self.function, std::mem::take(&mut self.local_map));

        // TODO: loop until returns false?
        // During construction the upvalue cell groups are not built yet, so the
        // C4 self-exclusion is disabled here (None) — verbatim original behavior.
        remove_unnecessary_params(self.function, &mut self.local_map, None);
        apply_local_map(self.function, std::mem::take(&mut self.local_map));

        (
            self.local_count,
            self.all_definitions.into_values().collect(),
            self.new_upvalues_in.into_iter().collect(),
            self.upvalues_passed
                .into_values()
                .flat_map(|m| m.into_values())
                .collect(),
        )
    }
}

pub fn construct(
    function: &mut Function,
    upvalues_in: &Vec<RcLocal>,
) -> (
    usize,
    Vec<FxHashSet<RcLocal>>,
    Vec<(RcLocal, FxHashSet<RcLocal>)>,
    Vec<FxHashSet<RcLocal>>,
) {
    // if entry has predecessors, this might risk it never being incomplete
    // resulting in broken params
    // TODO: verify ^ and insert temporary entry that's removed if there is no block params (if its an issue)
    assert!(function
        .predecessor_blocks(function.entry().unwrap())
        .next()
        .is_none());
    let mut new_upvalues_in = IndexMap::with_capacity(upvalues_in.len());
    for upvalue in upvalues_in {
        new_upvalues_in.insert(upvalue.clone(), FxHashSet::default());
    }

    let dfs = Dfs::new(function.graph(), function.entry().unwrap())
        .iter(function.graph())
        .collect::<IndexSet<_>>();

    // remove all nodes that will never execute
    for node in function.blocks().map(|(n, _)| n).collect::<Vec<_>>() {
        if !dfs.contains(&node) {
            function.remove_block(node);
        }
    }
    let node_count = function.graph().node_count();
    SsaConstructor {
        function,
        dfs,
        incomplete_params: FxHashMap::with_capacity_and_hasher(node_count, Default::default()),
        filled_blocks: FxHashSet::with_capacity_and_hasher(node_count, Default::default()),
        sealed_blocks: FxHashSet::with_capacity_and_hasher(node_count, Default::default()),
        current_definition: FxHashMap::default(),
        all_definitions: FxHashMap::default(),
        old_locals: FxHashMap::default(),
        local_count: 0,
        local_map: FxHashMap::default(),
        new_upvalues_in,
        upvalues_passed: FxHashMap::default(),
    }
    .construct()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockEdge;

    fn add_edge(
        function: &mut Function,
        from: NodeIndex,
        to: NodeIndex,
        args: Vec<(RcLocal, RcLocal)>,
    ) {
        add_edge_values(
            function,
            from,
            to,
            args.into_iter()
                .map(|(param, argument)| (param, ast::RValue::Local(argument)))
                .collect(),
        );
    }

    fn add_edge_values(
        function: &mut Function,
        from: NodeIndex,
        to: NodeIndex,
        arguments: Vec<(RcLocal, ast::RValue)>,
    ) {
        function.graph_mut().add_edge(
            from,
            to,
            BlockEdge {
                arguments,
                ..Default::default()
            },
        );
    }

    #[test]
    fn removes_nested_loop_upvalue_phi_scc() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let outer_header = function.new_block();
        let inner_header = function.new_block();
        function.set_entry(entry);

        let cell = RcLocal::default();
        let group = RcLocal::default();
        let outer = RcLocal::default();
        let inner = RcLocal::default();
        let sentinel_initial = RcLocal::default();
        let sentinel_outer = RcLocal::default();
        let sentinel_inner = RcLocal::default();
        add_edge(
            &mut function,
            entry,
            outer_header,
            vec![
                (outer.clone(), cell.clone()),
                (sentinel_outer.clone(), sentinel_initial),
            ],
        );
        add_edge(
            &mut function,
            outer_header,
            inner_header,
            vec![
                (inner.clone(), outer.clone()),
                (sentinel_inner.clone(), sentinel_outer.clone()),
            ],
        );
        add_edge(
            &mut function,
            inner_header,
            inner_header,
            vec![
                (inner.clone(), inner.clone()),
                (sentinel_inner.clone(), sentinel_inner.clone()),
            ],
        );
        add_edge(
            &mut function,
            inner_header,
            outer_header,
            vec![
                (outer.clone(), inner.clone()),
                (sentinel_outer.clone(), sentinel_inner.clone()),
            ],
        );

        let groups = IndexMap::from_iter([
            (cell.clone(), group.clone()),
            (outer.clone(), group.clone()),
            (inner.clone(), group),
        ]);
        let mut map = FxHashMap::default();
        assert!(remove_upvalue_param_sccs(&mut function, &mut map, &groups));
        assert_eq!(map.get(&outer), Some(&cell));
        assert_eq!(map.get(&inner), Some(&cell));
        assert!(!map.contains_key(&sentinel_outer));
        assert!(!map.contains_key(&sentinel_inner));
        assert!(function
            .graph()
            .edge_weights()
            .all(|edge| edge.arguments.len() == 1));
        assert!(function.graph().edge_weights().all(|edge| {
            matches!(
                edge.arguments.as_slice(),
                [(param, _)] if param == &sentinel_outer || param == &sentinel_inner
            )
        }));
        assert!(!remove_upvalue_param_sccs(&mut function, &mut map, &groups));
    }

    #[test]
    fn retains_phi_scc_with_a_non_cell_input() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let other_path = function.new_block();
        let header = function.new_block();
        function.set_entry(entry);

        let cell = RcLocal::default();
        let group = RcLocal::default();
        let unrelated = RcLocal::default();
        let param = RcLocal::default();
        add_edge(
            &mut function,
            entry,
            header,
            vec![(param.clone(), cell.clone())],
        );
        add_edge(
            &mut function,
            other_path,
            header,
            vec![(param.clone(), unrelated)],
        );
        add_edge(
            &mut function,
            header,
            header,
            vec![(param.clone(), param.clone())],
        );

        let groups = IndexMap::from_iter([(cell, group.clone()), (param.clone(), group)]);
        let mut map = FxHashMap::default();
        assert!(!remove_upvalue_param_sccs(&mut function, &mut map, &groups));
        assert!(map.is_empty());
        assert_eq!(
            function
                .graph()
                .edge_weights()
                .map(|edge| edge.arguments.len())
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn retains_unmarked_snapshot_phi_seeded_from_a_live_cell() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let header = function.new_block();
        function.set_entry(entry);

        let cell = RcLocal::default();
        let group = RcLocal::default();
        let snapshot = RcLocal::default();
        add_edge(
            &mut function,
            entry,
            header,
            vec![(snapshot.clone(), cell.clone())],
        );
        add_edge(
            &mut function,
            header,
            header,
            vec![(snapshot.clone(), snapshot.clone())],
        );

        // Only the source is a cell. The phi result deliberately has no cell
        // provenance and must stay a value snapshot.
        let groups = IndexMap::from_iter([(cell, group)]);
        let mut map = FxHashMap::default();
        assert!(!remove_upvalue_param_sccs(&mut function, &mut map, &groups));
        assert!(map.is_empty());
        assert_eq!(
            function
                .graph()
                .edge_weights()
                .map(|edge| edge.arguments.len())
                .sum::<usize>(),
            2
        );
    }

    #[test]
    fn retains_phi_scc_joining_two_distinct_upvalue_cells() {
        let mut function = Function::new(0);
        let entry_a = function.new_block();
        let entry_b = function.new_block();
        let outer_header = function.new_block();
        let inner_header = function.new_block();
        function.set_entry(entry_a);

        let cell_a = RcLocal::default();
        let cell_b = RcLocal::default();
        let group_a = RcLocal::default();
        let group_b = RcLocal::default();
        let outer = RcLocal::default();
        let inner = RcLocal::default();
        add_edge(
            &mut function,
            entry_a,
            outer_header,
            vec![(outer.clone(), cell_a.clone())],
        );
        add_edge(
            &mut function,
            entry_b,
            inner_header,
            vec![(inner.clone(), cell_b.clone())],
        );
        add_edge(
            &mut function,
            outer_header,
            inner_header,
            vec![(inner.clone(), outer.clone())],
        );
        add_edge(
            &mut function,
            inner_header,
            outer_header,
            vec![(outer.clone(), inner.clone())],
        );

        let groups = IndexMap::from_iter([
            (cell_a, group_a.clone()),
            (outer.clone(), group_a),
            (cell_b, group_b.clone()),
            (inner.clone(), group_b),
        ]);
        let mut map = FxHashMap::default();
        assert!(!remove_upvalue_param_sccs(&mut function, &mut map, &groups));
        assert!(map.is_empty());
        assert_eq!(
            function
                .graph()
                .edge_weights()
                .map(|edge| edge.arguments.len())
                .sum::<usize>(),
            4
        );
    }

    #[test]
    fn retains_upvalue_phi_with_a_non_local_input() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let other_path = function.new_block();
        let header = function.new_block();
        function.set_entry(entry);

        let cell = RcLocal::default();
        let group = RcLocal::default();
        let param = RcLocal::default();
        add_edge(
            &mut function,
            entry,
            header,
            vec![(param.clone(), cell.clone())],
        );
        add_edge_values(
            &mut function,
            other_path,
            header,
            vec![(
                param.clone(),
                ast::RValue::Literal(ast::Literal::Number(1.0)),
            )],
        );
        add_edge(
            &mut function,
            header,
            header,
            vec![(param.clone(), cell.clone())],
        );

        let groups = IndexMap::from_iter([(cell, group.clone()), (param.clone(), group)]);
        let mut map = FxHashMap::default();
        assert!(!remove_upvalue_param_sccs(&mut function, &mut map, &groups));
        assert!(map.is_empty());
        assert_eq!(
            function
                .graph()
                .edge_weights()
                .map(|edge| edge.arguments.len())
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn removes_raw_param_aliases_when_input_map_is_nonempty() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let header = function.new_block();
        function.set_entry(entry);

        let cell = RcLocal::default();
        let group = RcLocal::default();
        let raw_param = RcLocal::default();
        let resolved_param = RcLocal::default();
        add_edge(
            &mut function,
            entry,
            header,
            vec![(raw_param.clone(), cell.clone())],
        );
        add_edge(
            &mut function,
            header,
            header,
            vec![(raw_param.clone(), raw_param.clone())],
        );

        let groups = IndexMap::from_iter([
            (cell.clone(), group.clone()),
            (resolved_param.clone(), group),
        ]);
        let mut map = FxHashMap::from_iter([(raw_param.clone(), resolved_param.clone())]);
        assert!(remove_upvalue_param_sccs(&mut function, &mut map, &groups));
        assert_eq!(map.get(&raw_param), Some(&resolved_param));
        assert_eq!(map.get(&resolved_param), Some(&cell));
        assert!(function
            .graph()
            .edge_weights()
            .all(|edge| edge.arguments.is_empty()));
    }

    #[test]
    fn retains_ordinary_nested_loop_phi_scc() {
        let mut function = Function::new(0);
        let entry = function.new_block();
        let outer_header = function.new_block();
        let inner_header = function.new_block();
        function.set_entry(entry);

        let initial = RcLocal::default();
        let outer = RcLocal::default();
        let inner = RcLocal::default();
        add_edge(
            &mut function,
            entry,
            outer_header,
            vec![(outer.clone(), initial)],
        );
        add_edge(
            &mut function,
            outer_header,
            inner_header,
            vec![(inner.clone(), outer.clone())],
        );
        add_edge(
            &mut function,
            inner_header,
            outer_header,
            vec![(outer, inner)],
        );

        let mut map = FxHashMap::default();
        assert!(!remove_upvalue_param_sccs(
            &mut function,
            &mut map,
            &IndexMap::new()
        ));
        assert!(map.is_empty());
    }
}
