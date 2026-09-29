use std::iter;

use ast::{LocalRw, RcLocal, Traverse};
use ast::{FxIndexMap as IndexMap, FxIndexSet as IndexSet};
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

// IndexSet fixes DFS iteration order independently of its hash function.
// Sealing queries ranks for every predecessor, so use the same inexpensive
// hashing as the other block indexes instead of the default keyed hasher.
type DfsOrder = IndexSet<NodeIndex>;

/// Lookup-only SSA availability for one source register. Most straight-line
/// registers never need a per-block hash table. Keep exactly one owned SSA
/// handle per occupied block, and preserve ordinary HashMap insertion once a
/// register reaches more than one block (including duplicate-insert growth).
#[derive(Clone)]
enum CurrentDefinitions {
    Empty,
    One(NodeIndex, RcLocal),
    Many(FxHashMap<NodeIndex, RcLocal>),
}

impl Default for CurrentDefinitions {
    fn default() -> Self {
        #[cfg(test)]
        if tests::REFERENCE_RENAMING.with(std::cell::Cell::get) {
            return Self::Many(FxHashMap::default());
        }
        Self::Empty
    }
}

impl CurrentDefinitions {
    fn get(&self, node: &NodeIndex) -> Option<&RcLocal> {
        match self {
            Self::Empty => None,
            Self::One(block, value) => (block == node).then_some(value),
            Self::Many(values) => values.get(node),
        }
    }

    fn insert(&mut self, node: NodeIndex, value: RcLocal) {
        match self {
            Self::Empty => *self = Self::One(node, value),
            Self::One(block, current) if *block == node => *current = value,
            Self::One(_, _) => {
                let Self::One(block, current) = std::mem::take(self) else { unreachable!() };
                // Repeated writes while singleton cannot grow the legacy
                // table. Replaying its first occupied pair and this second
                // block produces the same table without extra local owners.
                let mut values = FxHashMap::default();
                values.insert(block, current);
                values.insert(node, value);
                *self = Self::Many(values);
            }
            Self::Many(values) => { values.insert(node, value); }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_, _) => 1,
            Self::Many(values) => values.len(),
        }
    }
}

struct SsaConstructor<'a> {
    function: &'a mut Function,
    dfs: DfsOrder,
    incomplete_params: FxHashMap<NodeIndex, FxHashMap<RcLocal, RcLocal>>,
    sealed_blocks: FxHashSet<NodeIndex>,
    // TODO: combine current/all/old into one map
    /// Dense slots for registers and the versions minted for them.
    index: ast::dense::LocalIndex,
    current_definition: ast::dense::LocalVec<CurrentDefinitions>,
    /// Versions of each register, and whether a version is already recorded.
    all_definitions: ast::dense::LocalVec<Vec<RcLocal>>,
    recorded: ast::dense::LocalVec<bool>,
    old_locals: FxHashMap<RcLocal, RcLocal>,
    local_count: usize,
    local_map: FxHashMap<RcLocal, RcLocal>,
    new_upvalues_in: IndexMap<RcLocal, FxHashSet<RcLocal>>,
    upvalues_passed: FxHashMap<RcLocal, FxHashMap<(NodeIndex, usize), FxHashSet<RcLocal>>>,
    definition_records: usize,
}

/// The CFG topology and DFS order stay fixed throughout construction. A block
/// becomes sealable when both it and its final predecessor have been visited.
/// Bucket those events instead of polling every still-unsealed loop header on
/// every block. Reverse insertion keeps simultaneous events in DFS order, as
/// in the former retain scan, without allocating a Vec for every bucket.
struct SealSchedule {
    heads: Vec<usize>,
    next: Vec<usize>,
}

impl SealSchedule {
    fn new(function: &Function, dfs: &DfsOrder) -> Self {
        let mut schedule = Self {
            heads: vec![usize::MAX; dfs.len()],
            next: vec![usize::MAX; dfs.len()],
        };
        for rank in (1..dfs.len()).rev() {
            let ready_at = function.predecessor_blocks(dfs[rank])
                .map(|predecessor| dfs.get_index_of(&predecessor).unwrap())
                .max().unwrap_or(rank).max(rank);
            schedule.next[rank] = schedule.heads[ready_at];
            schedule.heads[ready_at] = rank;
        }
        schedule
    }
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

#[cfg(test)]
#[path = "construct/params_reference.rs"]
mod params_reference;

fn remove_trivial_dependency(
    graph: &mut Option<ParamDependencyGraph>,
    deferred: &mut Vec<RcLocal>,
    local: &RcLocal,
) {
    if let Some(graph) = graph {
        if let Some(&node) = graph.local_to_node.get(local) { graph.remove_node(node); }
    } else {
        deferred.push(local.clone());
    }
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
    let mut graphs_built = 0u64;
    let mut graphs_skipped = 0u64;
    for node in function.blocks().map(|(i, _)| i).collect::<Vec<_>>() {
        if !function.edges_to_block(node).any(|(_, edge)| !edge.arguments.is_empty()) {
            continue;
        }
        let mut removable_params = FxHashMap::default();
        let edges = function
            .graph()
            .edges_directed(node, Direction::Incoming)
            .collect::<Vec<_>>();
        // The existing graph builder can fail on malformed incoming schemas.
        // Retain its eager failure boundary for those public hand-built CFGs.
        // Aligned schemas make construction total, so candidate discovery may
        // precede it without changing a failure or any visible mutation.
        let aligned = edges.first().is_some_and(|first| edges.iter().skip(1).all(|edge| {
            edge.weight().arguments.len() == first.weight().arguments.len()
                && edge.weight().arguments.iter().zip(&first.weight().arguments)
                    .all(|((param, _), (first_param, _))| param == first_param)
        }));
        let mut dependency_graph = (!aligned).then(|| ParamDependencyGraph::new(function, node));
        let mut deferred_trivial = Vec::new();
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
                        } else {
                            remove_trivial_dependency(&mut dependency_graph, &mut deferred_trivial, &resolved_param);
                        }
                        params_to_remove.insert(resolved_param.clone());
                    } else if arg_set.is_empty() {
                        // all-self phi `p = phi(p, p)` — the original code removed it
                        // as trivial; reproduce that (preserves byte-identity).
                        remove_trivial_dependency(&mut dependency_graph, &mut deferred_trivial, &resolved_param);
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
                            if !param.source_bindings_compatible(arg) {
                                continue;
                            }
                            // A captured cell's version merging one uncaptured
                            // value (`local conn = nil`, then an unrelated `if`,
                            // then `conn = sig:Connect(function() conn:Disconnect()
                            // end)`): when the cell has other versions (a later
                            // write), keep its identity by renaming the value to
                            // the phi instead. Every use of the value precedes the
                            // merge, so it reads the same content. A cell with no
                            // other version is only read; the value can stand in.
                            let joins_cell = upvalue_to_group.is_some_and(|groups| {
                                groups.get(param).is_some_and(|cell| {
                                    !groups.contains_key(arg)
                                        && groups.iter().any(|(version, root)| root == cell && version != param)
                                })
                            }) && !function.parameters.contains(arg);
                            if joins_cell {
                                let (arg, param) = (arg.clone(), param.clone());
                                local_map.insert(arg, param.clone());
                                remove_trivial_dependency(&mut dependency_graph, &mut deferred_trivial, &param);
                                params_to_remove.insert(param);
                                continue;
                            }
                            // param is not trivial, replace the param with the arg
                            removable_params.insert(param.clone(), arg.clone());
                        } else {
                            // param is trivial: x = phi(x, x, ..., x)
                            remove_trivial_dependency(&mut dependency_graph, &mut deferred_trivial, param);
                        }
                        params_to_remove.insert(param.clone());
                    }
                }
            }
            if !removable_params.is_empty() && dependency_graph.is_none() {
                // Build from the untouched raw arguments. Replaying by local
                // identity in the original order also retains DiGraph's
                // existing swap-removal/local_to_node behavior exactly.
                let mut graph = ParamDependencyGraph::new(function, node);
                for local in deferred_trivial {
                    if let Some(&param_node) = graph.local_to_node.get(&local) {
                        graph.remove_node(param_node);
                    }
                }
                dependency_graph = Some(graph);
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

        let Some(mut dependency_graph) = dependency_graph else {
            graphs_skipped += 1;
            continue;
        };
        graphs_built += 1;
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
    if graphs_built + graphs_skipped != 0 {
        ast::telemetry::count("ssa_param_dependency_candidates", graphs_built + graphs_skipped);
        ast::telemetry::count("ssa_param_dependency_graphs_skipped", graphs_skipped);
        ast::telemetry::count("ssa_param_dependency_graphs_built", graphs_built);
    }
    changed
}

// TODO: STYLE: rename function
// TODO: STYLE: rename `uses_local`, we need a generic name for ast nodes, maybe `traversible`?
fn apply_local_map_to_values_referenced<T: LocalRw + Traverse>(
    uses_local: &mut T,
    local_map: &FxHashMap<RcLocal, RcLocal>,
) {
    // Preserve the write-then-read order, including repeated references, while
    // walking operands directly instead of building a Vec at every tree node.
    let mut replace = |from: &mut RcLocal| {
        if let Some(mut to) = local_map.get(from) {
            while let Some(to_to) = local_map.get(to) {
                to = to_to;
            }
            *from = to.clone();
        }
        true
    };
    uses_local.visit_local_writes_mut(&mut replace);
    uses_local.visit_local_reads_mut(&mut replace);
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
    if local_map.is_empty() {
        return;
    }
    if let Some(trace) = &mut function.provenance {
        let mut entries = local_map.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(from, to)| (from.stable_id(), to.stable_id()));
        for (from, to) in entries { trace.local_map(trace.phase, from, to); }
    }
    super::close_provenance::apply_local_map(function, &local_map);
    // A coalesced local inherits the bytecode-type naming hint of the versions
    // it absorbs (first hint wins; the hints of one source local agree anyway).
    for (old, new) in &local_map {
        let mut target = new;
        for _ in 0..64 {
            match local_map.get(target) {
                Some(next) if next != target => target = next,
                _ => break,
            }
        }
        target.inherit_source_bindings(old);
        let Some(hint) = old.0.lock().type_hint().map(str::to_string) else {
            continue;
        };
        let mut new = new;
        // Bounded so a (malformed) cyclic map can never hang the pass.
        for _ in 0..64 {
            match local_map.get(new) {
                Some(new_to) if new_to != new => new = new_to,
                _ => break,
            }
        }
        let mut target = new.0.lock();
        if target.type_hint().is_none() {
            target.1 = Some(hint);
        }
    }
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
    fn apply_pending_local_map(&mut self) {
        let _timer = ast::prof::Timer::new(&ast::prof::C_APPLY_MAP);
        let _phase = ast::telemetry::Span::new("SSA_CONSTRUCT_APPLY_MAP");
        let map = std::mem::take(&mut self.local_map);
        // Phi elimination can replace the version originally captured by a
        // closure with an entry parameter. Keep capture membership on that
        // exact representative, or destruction writes a new local while the
        // closure keeps observing the old parameter. Close certificates are
        // still remapped/intersected by apply_local_map, never copied by name.
        for group in self.new_upvalues_in.values_mut().chain(
            self.upvalues_passed.values_mut().flat_map(|groups| groups.values_mut())) {
            *group = group.iter().map(|local| {
                let mut current = local;
                while let Some(next) = map.get(current) { current = next; }
                current.clone()
            }).collect();
        }
        apply_local_map(self.function, map);
    }

    fn fresh_phi(&mut self, node: NodeIndex, register: &RcLocal) -> RcLocal {
        let local = RcLocal::default();
        if Some(node) == *self.function.entry() || self.new_upvalues_in.contains_key(register) {
            local.inherit_source_bindings(register);
        }
        if let Some(binding) = self.function.entry_source_bindings.get(&(node, register.clone())) {
            local.0.lock().add_source_binding(binding.clone());
        }
        if let Some(trace) = &mut self.function.provenance {
            trace.definition(&local, register, node, None, Vec::new());
        }
        local
    }
    /// A fresh SSA version for the `local_index`-th local written by statement
    /// `stat_index` of `node`, carrying the lifter's bytecode-type naming hint
    /// for that definition when one was recorded (see
    /// `Function::local_type_hints`).
    fn fresh_local(&mut self, node: NodeIndex, stat_index: usize, local_index: usize, register: &RcLocal) -> RcLocal {
        let local = match self
            .function
            .local_type_hints
            .remove(&(node, stat_index, local_index))
        {
            Some(hint) => RcLocal::new(ast::Local::with_type_hint(hint)),
            None => RcLocal::default(),
        };
        if let Some(bindings) = self.function.local_source_bindings.remove(&(node, stat_index, local_index)) {
            for binding in bindings { local.0.lock().add_source_binding(binding); }
        }
        if let Some(trace) = &mut self.function.provenance {
            trace.definition(&local, register, node, Some((stat_index, local_index)), Vec::new());
        }
        local
    }

    fn write_local(&mut self, node: NodeIndex, local: &RcLocal, new_local: &RcLocal) {
        self.definition_records += 1;
        let register = self.index.slot(local);
        let version = self.index.slot(new_local);
        let recorded = self.recorded.get_mut(version);
        if !*recorded {
            *recorded = true;
            self.all_definitions.get_mut(register).push(new_local.clone());
        }
        self.current_definition.get_mut(register).insert(node, new_local.clone());
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
            if let Some(trace) = &mut self.function.provenance
                && let Some(record) = trace.definitions.get_mut(&param_local.stable_id()) {
                record.dependencies.push(argument_local.stable_id());
                record.dependencies.sort_unstable();
                record.dependencies.dedup();
            }
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
        if let Some(slot) = self.index.find(local)
            && let Some(new_local) = self.current_definition.get(slot).get(&node)
        {
            // This version was already recorded in both definition maps.
            // A read must not pay to reinsert it into both maps on every hit.
            return new_local.clone();
        }
        let res = {
            // search globally
            if !self.sealed_blocks.contains(&node) {
                // TODO: this code is repeated multiple times, create new_local function
                let param_local = self.fresh_phi(node, local);
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
                let param_local = self.fresh_phi(node, local);
                self.old_locals.insert(param_local.clone(), local.clone());
                if let Some(upvalues) = self.new_upvalues_in.get_mut(local) {
                    upvalues.insert(param_local.clone());
                }
                self.local_count += 1;
                self.write_local(node, local, &param_local);

                self.add_param_args(node, local, param_local)
            }
        };
        // Preserve this insertion even for forwarded or already-seeded
        // definitions. A duplicate HashSet insert may grow its table before
        // checking membership; its iteration order reaches later coalescing.
        self.write_local(node, local, &res);
        res
    }

    fn propagate_copies(&mut self) {
        let _timer = ast::prof::Timer::new(&ast::prof::C_PROPAGATE);
        let _phase = ast::telemetry::Span::new("SSA_PROPAGATE_COPIES");
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
                        && from.source_bindings_compatible(to)
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

    fn mark_upvalues(&mut self) {
        let _timer = ast::prof::Timer::new(&ast::prof::C_MARK_UPVALUES);
        let _phase = ast::telemetry::Span::new("SSA_MARK_UPVALUES");
        #[cfg(test)]
        if tests::REFERENCE_RENAMING.with(std::cell::Cell::get) {
            self.mark_upvalues_reference();
            return;
        }
        let outgoing_refs = super::close_provenance::record(self.function, &self.old_locals);
        if !outgoing_refs {
            // Incoming cells already contain every version minted by find_local
            // and the definition loop. Open-cell propagation only starts from
            // outgoing Ref captures, so no such site means no passed group.
            // CLOSE removal and certificate clearing still occur on this path.
            ast::telemetry::count("ssa_capture_free_functions", 1);
            ast::telemetry::count("ssa_capture_free_incoming_groups", self.new_upvalues_in.len() as u64);
            for &node in &self.dfs {
                self.function.block_mut(node).unwrap()
                    .retain(|statement| !matches!(statement, ast::Statement::Close(_)));
            }
            return;
        }
        ast::telemetry::count("ssa_open_capture_functions", 1);
        ast::telemetry::count("ssa_open_borrowed_definition_entries", self.old_locals.len() as u64);
        let old_locals = &self.old_locals;
        let incoming = &self.new_upvalues_in;
        let passed = &mut self.upvalues_passed;
        let upvalues_open = UpvaluesOpen::new(self.function, old_locals);
        let mut queries = 0usize;
        let mut inserted = 0usize;
        for &node in &self.dfs {
            if let Some(open) = upvalues_open.open.get(&node).filter(|open| !open.is_empty()) {
                let mut mark = |value: &RcLocal, stat_index: usize| {
                    queries += 1;
                    let old_local = &old_locals[value];
                    let Some(locations) = open.get(old_local).and_then(|ranges| ranges.get(&stat_index)) else {
                        return;
                    };
                    if let Some(group) = incoming.get(old_local) {
                        assert!(group.contains(value));
                    } else {
                        inserted += usize::from(passed.entry(old_local.clone()).or_default()
                            .entry(*locations.first().unwrap()).or_default().insert(value.clone()));
                    }
                };
                // Retain the old sorted/deduplicated parameter order, including
                // hand-built edges, without cloning handles before eligibility.
                let mut params = self.function.edges_to_block(node)
                    .flat_map(|(_, edge)| edge.arguments.iter().map(|(param, _)| param))
                    .collect::<Vec<_>>();
                params.sort();
                params.dedup();
                for param in params { mark(param, 0); }
                for (stat_index, statement) in self.function.block(node).unwrap().iter().enumerate() {
                    statement.visit_local_reads(&mut |value| { mark(value, stat_index); true });
                    statement.visit_local_writes(&mut |value| { mark(value, stat_index); true });
                }
            }
            self.function.block_mut(node).unwrap()
                .retain(|statement| !matches!(statement, ast::Statement::Close(_)));
        }
        ast::telemetry::count("ssa_open_version_queries", queries as u64);
        ast::telemetry::count("ssa_passed_versions_inserted", inserted as u64);
    }

    #[cfg(test)]
    fn mark_upvalue_version_reference(
        &mut self,
        upvalues_open: &UpvaluesOpen<'_>,
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

    #[cfg(test)]
    fn mark_upvalues_reference(&mut self) {
        super::close_provenance::record(self.function, &self.old_locals);
        let old_locals = self.old_locals.clone();
        let upvalues_open = UpvaluesOpen::new(self.function, &old_locals);
        let nodes: Vec<NodeIndex> = self.dfs.iter().copied().collect();
        for node in nodes {
            let has_open = upvalues_open.open.get(&node).is_some_and(|locals| !locals.is_empty());
            #[cfg(test)]
            let has_open = has_open || tests::REFERENCE_RENAMING.with(std::cell::Cell::get);
            if !has_open {
                // No cell can be marked in this block. Most blocks have no
                // reference capture, so avoid cloning every statement's locals
                // solely to perform guaranteed-negative range lookups.
                self.function.block_mut(node).unwrap()
                    .retain(|statement| !matches!(statement, ast::Statement::Close(_)));
                continue;
            }
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
                self.mark_upvalue_version_reference(&upvalues_open, node, 0, param);
            }

            for stat_index in 0..self.function.block(node).unwrap().len() {
                let statement = self.function.block(node).unwrap().get(stat_index).unwrap();
                let values = statement.values().into_iter().cloned().collect::<Vec<_>>();
                for value in values {
                    self.mark_upvalue_version_reference(&upvalues_open, node, stat_index, value);
                }
            }
            self.function
                .block_mut(node)
                .unwrap()
                .retain(|statement| !matches!(statement, ast::Statement::Close(_)))
        }
    }

    fn read(&mut self, node: NodeIndex, stat_index: usize, read: &mut Vec<RcLocal>) -> usize {
        #[cfg(test)]
        if tests::REFERENCE_RENAMING.with(std::cell::Cell::get) {
            return self.read_reference(node, stat_index);
        }
        debug_assert!(read.is_empty());
        let statement = self
            .function
            .block_mut(node)
            .unwrap()
            .get_mut(stat_index)
            .unwrap();
        statement.visit_local_reads(&mut |local| {
            read.push(local.clone());
            true
        });
        if read.is_empty() {
            return 0;
        }
        let count = read.len();
        // Preserve lookup/local-creation order, including repeated reads, then
        // rewrite in one traversal. Rebuilding all mutable reads for each slot
        // made wide calls, tables and returns quadratic and allocated per read.
        for local in read.iter_mut() {
            *local = self.find_local(node, local);
        }
        // Drain moves the renamed handles into their slots while retaining the
        // allocation for the next statement in this function.
        let mut renamed = read.drain(..);
        self.function.block_mut(node).unwrap()[stat_index]
            .visit_local_reads_mut(&mut |local| {
                *local = renamed.next().unwrap();
                true
            });
        debug_assert!(renamed.next().is_none());
        count
    }

    #[cfg(test)]
    fn read_reference(&mut self, node: NodeIndex, stat_index: usize) -> usize {
        let read = self.function.block(node).unwrap()[stat_index]
            .values_read().into_iter().cloned().collect::<Vec<_>>();
        let count = read.len();
        let mut map = FxHashMap::default();
        map.reserve(read.len());
        for local in &read {
            let new_local = self.find_local(node, local);
            map.insert(local.clone(), new_local);
        }
        for (local_index, local) in read.into_iter().enumerate() {
            *self.function.block_mut(node).unwrap()[stat_index]
                .values_read_mut()[local_index] = map[&local].clone();
        }
        count
    }

    fn construct(
        mut self,
    ) -> (
        usize,
        Vec<Vec<RcLocal>>,
        Vec<(RcLocal, FxHashSet<RcLocal>)>,
        Vec<FxHashSet<RcLocal>>,
    ) {
        let entry = self.function.entry().unwrap();
        let rename_timer = ast::prof::Timer::new(&ast::prof::C_RENAME);
        let phase = ast::telemetry::Span::new("SSA_RENAME");
        let seals = SealSchedule::new(self.function, &self.dfs);
        let mut read = Vec::new();
        let mut written = Vec::new();
        let mut read_operands = 0usize;
        let mut written_operands = 0usize;
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
                    written_operands += 1;
                    let new_local = self.fresh_local(node, stat_index, 0, &local);
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
                    read_operands += self.read(node, stat_index, &mut read);
                } else {
                    #[cfg(not(test))]
                    let reference_writes = false;
                    #[cfg(test)]
                    let reference_writes = tests::REFERENCE_RENAMING.with(std::cell::Cell::get);
                    debug_assert!(written.is_empty());
                    if reference_writes {
                        written = statement.values_written().into_iter().cloned().collect();
                    } else {
                        statement.visit_local_writes(&mut |local| {
                            written.push(local.clone());
                            true
                        });
                    }
                    written_operands += written.len();
                    read_operands += self.read(node, stat_index, &mut read);
                    // write
                    for (local_index, local) in written.iter_mut().enumerate() {
                        let new_local = self.fresh_local(node, stat_index, local_index, local);
                        self.old_locals.insert(new_local.clone(), local.clone());
                        if let Some(upvalues) = self.new_upvalues_in.get_mut(local) {
                            upvalues.insert(new_local.clone());
                        }
                        self.local_count += 1;
                        self.write_local(node, local, &new_local);
                        if reference_writes {
                            *self.function.block_mut(node).unwrap()[stat_index]
                                .values_written_mut()[local_index] = new_local.clone();
                        }
                        *local = new_local;
                    }
                    if !reference_writes && !written.is_empty() {
                        // All source slots were saved before read renaming.
                        // Publish writes once, after creating SSA definitions in
                        // the same order (including duplicate destinations).
                        let mut renamed = written.drain(..);
                        self.function.block_mut(node).unwrap()[stat_index]
                            .visit_local_writes_mut(&mut |destination| {
                                *destination = renamed.next().unwrap();
                                true
                            });
                        debug_assert!(renamed.next().is_none());
                    }
                    written.clear();
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
            let mut seal = seals.heads[i];
            while seal != usize::MAX {
                let node = self.dfs[seal];
                if let Some(incomplete_params) = self.incomplete_params.remove(&node) {
                    // Seal in phi creation order: argument order must not depend
                    // on how the locals hash.
                    let mut incomplete_params = incomplete_params.into_iter().collect::<Vec<_>>();
                    incomplete_params.sort_unstable_by_key(|(_, param_local)| param_local.stable_id());
                    for (local, param_local) in incomplete_params {
                        // TODO: this is a bit weird, maybe we should have a upvalue rvalue
                        if !self.new_upvalues_in.contains_key(&local) {
                            self.add_param_args(node, &local, param_local);
                        }
                    }
                }
                self.sealed_blocks.insert(node);
                seal = seals.next[seal];
            }
        }

        // TODO: this is a bit meh, maybe we should have an argument rvalue
        if let Some(mut incomplete_params) = self.incomplete_params.remove(&entry) {
            for param in &mut self.function.parameters {
                let new = incomplete_params.remove(param).unwrap_or_default();
                new.inherit_source_bindings(param);
                *param = new;
            }
        }
        assert!(self.incomplete_params.is_empty());
        if ast::telemetry::enabled() {
            let mut single_block = 0u64;
            let mut multiple_blocks = 0u64;
            let mut retained_versions = 0u64;
            for definitions in self.current_definition.values().filter(|definitions| definitions.len() != 0) {
                single_block += u64::from(matches!(definitions, CurrentDefinitions::One(_, _)));
                multiple_blocks += u64::from(matches!(definitions, CurrentDefinitions::Many(_)));
                retained_versions += definitions.len() as u64;
            }
            ast::telemetry::count("ssa_current_definition_single_block", single_block);
            ast::telemetry::count("ssa_current_definition_multi_block", multiple_blocks);
            ast::telemetry::count("ssa_current_definition_retained_versions", retained_versions);
            // Entries are only inserted or replaced, never removed, so this
            // is also the peak number of version owners in this lookup cache.
            ast::telemetry::count("ssa_current_definition_peak_version_owners", retained_versions);
            // Slot payloads exclude control bytes/allocator rounding. These
            // counters expose the enum's cost as well as avoided inner tables.
            let capacity = self.current_definition.values().count() as u64;
            ast::telemetry::count("ssa_current_definition_outer_slot_bytes", capacity
                * std::mem::size_of::<(RcLocal, CurrentDefinitions)>() as u64);
            ast::telemetry::count("ssa_current_definition_legacy_outer_slot_bytes", capacity
                * std::mem::size_of::<(RcLocal, FxHashMap<NodeIndex, RcLocal>)>() as u64);
        }
        ast::telemetry::count("ssa_read_operands", read_operands as u64);
        ast::telemetry::count("ssa_written_operands", written_operands as u64);
        ast::telemetry::count("ssa_read_scratch_capacity", read.capacity() as u64);
        ast::telemetry::count("ssa_write_scratch_capacity", written.capacity() as u64);
        ast::telemetry::count("ssa_definition_records", self.definition_records as u64);
        drop(read);
        drop(written);
        drop(phase);
        drop(rename_timer);

        // Record original SSA dependencies while lifted statement positions
        // are still valid, before copy propagation removes statements.
        if self.function.provenance.is_some() {
            let dependencies = self.function.blocks().flat_map(|(_, block)| block.iter().flat_map(|statement| {
                let reads = statement.values_read().into_iter().map(RcLocal::stable_id).collect::<Vec<_>>();
                statement.values_written().into_iter().map(move |local| (local.stable_id(), reads.clone()))
            })).collect::<Vec<_>>();
            let trace = self.function.provenance.as_mut().unwrap();
            for (id, dependencies) in dependencies {
                if let Some(definition) = trace.definitions.get_mut(&id) { definition.dependencies = dependencies; }
            }
            crate::provenance::record_selects(self.function, "constructed_ssa");
            crate::provenance::record_values(self.function);
        }

        // TODO: irreducible control flow (see the paper this algorithm is from)
        // TODO: apply_local_map unnecessary number of calls
        self.apply_pending_local_map();

        self.mark_upvalues();
        self.propagate_copies();
        self.apply_pending_local_map();

        // TODO: loop until returns false?
        // During construction the upvalue cell groups are not built yet, so the
        // C4 self-exclusion is disabled here (None) — verbatim original behavior.
        {
            let _timer = ast::prof::Timer::new(&ast::prof::C_REMOVE_PARAMS);
            let _phase = ast::telemetry::Span::new("SSA_CONSTRUCT_REMOVE_PARAMS");
            remove_unnecessary_params(self.function, &mut self.local_map, None);
        }
        self.apply_pending_local_map();

        (
            self.local_count,
            self.all_definitions.values().filter(|group| !group.is_empty()).cloned().collect(),
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
    Vec<Vec<RcLocal>>,
    Vec<(RcLocal, FxHashSet<RcLocal>)>,
    Vec<FxHashSet<RcLocal>>,
) {
    let setup_timer = ast::prof::Timer::new(&ast::prof::C_SETUP);
    if let Some(trace) = &mut function.provenance { trace.phase = "ssa_construction"; }
    for parameter in &function.parameters { parameter.0.lock().4.parameter = true; }
    // if entry has predecessors, this might risk it never being incomplete
    // resulting in broken params
    // TODO: verify ^ and insert temporary entry that's removed if there is no block params (if its an issue)
    assert!(function
        .predecessor_blocks(function.entry().unwrap())
        .next()
        .is_none());
    let mut new_upvalues_in = IndexMap::with_capacity_and_hasher(upvalues_in.len(), Default::default());
    for upvalue in upvalues_in {
        new_upvalues_in.insert(upvalue.clone(), FxHashSet::default());
    }

    let dfs = Dfs::new(function.graph(), function.entry().unwrap())
        .iter(function.graph())
        .collect::<DfsOrder>();

    // remove all nodes that will never execute
    for node in function.blocks().map(|(n, _)| n).collect::<Vec<_>>() {
        if !dfs.contains(&node) {
            function.remove_block(node);
        }
    }
    let node_count = function.graph().node_count();
    drop(setup_timer);
    // Lookup-only tables (never iterated for output): size them for about
    // one definition per statement instead of growing through rehashes.
    let statements = function.blocks().map(|(_, block)| block.len()).sum::<usize>();
    let index = function.local_index();
    SsaConstructor {
        function,
        dfs,
        incomplete_params: FxHashMap::with_capacity_and_hasher(node_count, Default::default()),
        sealed_blocks: FxHashSet::with_capacity_and_hasher(node_count, Default::default()),
        index,
        current_definition: ast::dense::LocalVec::new(CurrentDefinitions::default()),
        all_definitions: ast::dense::LocalVec::new(Vec::new()),
        recorded: ast::dense::LocalVec::new(false),
        old_locals: FxHashMap::with_capacity_and_hasher(statements, Default::default()),
        local_count: 0,
        local_map: FxHashMap::default(),
        new_upvalues_in,
        upvalues_passed: FxHashMap::default(),
        definition_records: 0,
    }
    .construct()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockEdge;

    #[test]
    fn direct_local_map_matches_vector_reference_with_chains_and_duplicate_sites() {
        fn reference<T: LocalRw>(node: &mut T, map: &FxHashMap<RcLocal, RcLocal>) {
            for from in node.values_written_mut() {
                if let Some(mut to) = map.get(from) {
                    while let Some(next) = map.get(to) { to = next; }
                    *from = to.clone();
                }
            }
            for from in node.values_read_mut() {
                if let Some(mut to) = map.get(from) {
                    while let Some(next) = map.get(to) { to = next; }
                    *from = to.clone();
                }
            }
        }
        for seed in 0..64 {
            let mut actual = renaming_fixture(seed, 1 + seed * 3);
            let mut locals = FxHashMap::default();
            for (_, block) in actual.blocks() {
                for statement in block.iter() {
                    for local in statement.values_read().into_iter().chain(statement.values_written()) {
                        locals.insert(local.stable_id(), local.clone());
                    }
                }
            }
            let mut locals = locals.into_values().collect::<Vec<_>>();
            locals.sort_by_key(RcLocal::stable_id);
            let end = *actual.entry().as_ref().unwrap();
            actual.block_mut(end).unwrap().push(ast::GenericForNext::new(
                vec![locals[0].clone(), locals[0].clone()], locals[1].clone().into(),
                locals[2].clone(), locals[3].clone(),
            ).into());
            let mut map = FxHashMap::default();
            for pair in locals.windows(2) { map.insert(pair[0].clone(), pair[1].clone()); }
            let mut expected = actual.clone();
            let next_id = ast::current_local_id();
            for block in actual.blocks_mut() {
                for statement in block.iter_mut() {
                    apply_local_map_to_values_referenced(statement, &map);
                }
            }
            for block in expected.blocks_mut() {
                for statement in block.iter_mut() { reference(statement, &map); }
            }
            assert_eq!(ast::current_local_id(), next_id);
            assert_constructed_functions_equal(&actual, &expected);
            // Both algorithms retain the same local identities at every site.
            for ((_, actual), (_, expected)) in actual.blocks().zip(expected.blocks()) {
                for (actual, expected) in actual.iter().zip(expected.iter()) {
                    assert_eq!(actual.values_read(), expected.values_read());
                    assert_eq!(actual.values_written(), expected.values_written());
                }
            }
        }
    }

    #[test]
    fn inline_current_definitions_match_hash_growth_replacements_and_owners() {
        eprintln!("CurrentDefinitions bytes={}, legacy map bytes={}, outer pair bytes={} vs {}",
            std::mem::size_of::<CurrentDefinitions>(),
            std::mem::size_of::<FxHashMap<NodeIndex, RcLocal>>(),
            std::mem::size_of::<(RcLocal, CurrentDefinitions)>(),
            std::mem::size_of::<(RcLocal, FxHashMap<NodeIndex, RcLocal>)>());
        for seed in 0..64usize {
            let values: Vec<_> = (0..9).map(|_| RcLocal::default()).collect();
            let mut actual = CurrentDefinitions::default();
            let mut expected = FxHashMap::default();
            // Repeated singleton writes precede the first upgrade; duplicate
            // writes immediately at 3/7/14 occupied blocks exercise the
            // pinned HashMap's reserve-before-membership growth boundaries.
            let nodes = (0..32).map(|_| 11).chain((0..32).flat_map(|node| [node * 19, node * 19]));
            for (step, node) in nodes.enumerate() {
                let node = NodeIndex::new(node);
                let value = &values[(step + seed) % values.len()];
                actual.insert(node, value.clone());
                expected.insert(node, value.clone());
                assert_eq!(actual.len(), expected.len());
                for query in [NodeIndex::new(11), NodeIndex::new(999), node] {
                    assert_eq!(actual.get(&query), expected.get(&query));
                }
                if let CurrentDefinitions::Many(actual) = &actual {
                    assert_eq!(actual.capacity(), expected.capacity());
                    assert_eq!(actual.iter().collect::<Vec<_>>(), expected.iter().collect::<Vec<_>>());
                }
            }
            drop(expected);
            for value in &values {
                let retained = match &actual {
                    CurrentDefinitions::Empty => false,
                    CurrentDefinitions::One(_, current) => current == value,
                    CurrentDefinitions::Many(current) => current.values().any(|current| current == value),
                };
                assert_eq!(value.0.0.is_unique(), !retained, "discarded versions must not remain owned");
            }
            drop(actual);
            assert!(values.iter().all(|value| value.0.0.is_unique()), "the cache must release every retained owner");
        }
    }

    thread_local! {
        pub(super) static REFERENCE_RENAMING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct ReferenceRenaming(bool);

    impl ReferenceRenaming {
        fn enter(enabled: bool) -> Self {
            Self(REFERENCE_RENAMING.with(|flag| flag.replace(enabled)))
        }
    }

    impl Drop for ReferenceRenaming {
        fn drop(&mut self) { REFERENCE_RENAMING.with(|flag| flag.set(self.0)); }
    }

    #[test]
    fn sealing_schedule_matches_polling_order_with_cycles_holes_and_parallel_edges() {
        for seed in 1..=384usize {
            let mut state = seed;
            let mut random = |limit: usize| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) % limit
            };
            let mut function = Function::new(0);
            let nodes: Vec<_> = (0..3 + random(24)).map(|_| function.new_block()).collect();
            function.set_entry(nodes[0]);
            for pair in nodes.windows(2) {
                function.graph_mut().add_edge(pair[0], pair[1], BlockEdge::default());
            }
            for &node in &nodes {
                for _ in 0..random(8) {
                    let target = nodes[1 + random(nodes.len() - 1)];
                    function.graph_mut().add_edge(node, target, BlockEdge::default());
                }
            }
            if nodes.len() > 6 {
                function.remove_block(nodes[3]);
            }
            let dfs: DfsOrder = Dfs::new(function.graph(), nodes[0]).iter(function.graph()).collect();
            // Construction removes unreachable blocks before planning seals.
            for &node in &nodes {
                if !dfs.contains(&node) { function.remove_block(node); }
            }
            let schedule = SealSchedule::new(&function, &dfs);
            let mut filled = FxHashSet::default();
            let mut unsealed = Vec::new();
            for rank in 0..dfs.len() {
                let node = dfs[rank];
                filled.insert(node);
                if node != nodes[0] { unsealed.push(node); }
                let mut expected = Vec::new();
                unsealed.retain(|&node| {
                    if function.predecessor_blocks(node).any(|predecessor| !filled.contains(&predecessor)) {
                        true
                    } else {
                        expected.push(node);
                        false
                    }
                });
                let mut actual = Vec::new();
                let mut next = schedule.heads[rank];
                while next != usize::MAX {
                    actual.push(dfs[next]);
                    next = schedule.next[next];
                }
                assert_eq!(actual, expected, "seed={seed}, rank={rank}");
            }
            assert!(unsealed.is_empty());
        }
    }

    #[test]
    fn nested_backedge_sealing_uses_linear_schedule_and_keeps_dfs_tie_order() {
        let mut function = Function::new(0);
        let nodes: Vec<_> = (0..20_000).map(|_| function.new_block()).collect();
        function.set_entry(nodes[0]);
        for pair in nodes.windows(2) {
            function.graph_mut().add_edge(pair[0], pair[1], BlockEdge::default());
        }
        for &header in &nodes[1..] {
            function.graph_mut().add_edge(*nodes.last().unwrap(), header, BlockEdge::default());
        }
        let dfs = nodes.iter().copied().collect::<DfsOrder>();
        let schedule = SealSchedule::new(&function, &dfs);
        assert_eq!(schedule.heads.len(), nodes.len());
        assert_eq!(schedule.next.len(), nodes.len());
        assert!(schedule.heads[..nodes.len() - 1].iter().all(|&head| head == usize::MAX));
        let mut next = schedule.heads[nodes.len() - 1];
        for expected in 1..nodes.len() {
            assert_eq!(next, expected);
            next = schedule.next[next];
        }
        assert_eq!(next, usize::MAX);
    }

    fn renaming_fixture(seed: usize, width: usize) -> Function {
        let mut function = Function::new(seed);
        let nodes: Vec<_> = (0..5).map(|_| function.new_block()).collect();
        function.set_entry(nodes[0]);
        // A join followed by a backedge exercises both incomplete and sealed
        // block parameters. Parallel input edges preserve duplicate read sites.
        for (from, to) in [(0, 1), (0, 2), (1, 3), (2, 3), (3, 2), (3, 4)] {
            function.graph_mut().add_edge(nodes[from], nodes[to], BlockEdge::default());
        }
        let locals: Vec<_> = (0..7).map(|_| RcLocal::default()).collect();
        function.parameters = locals[..3].to_vec();
        let input = locals.iter().cloned().map(ast::RValue::Local).collect::<Vec<_>>();
        function.block_mut(nodes[0]).unwrap().push(ast::Assign::new(
            locals[3..].iter().cloned().map(Into::into).collect(),
            vec![ast::Literal::Nil.into(); 4],
        ).into());
        for (offset, &node) in nodes[1..4].iter().enumerate() {
            let values = (0..width).map(|index| input[(index * 3 + seed + offset) % input.len()].clone()).collect();
            function.block_mut(node).unwrap().push(ast::Call::new(
                ast::Global::from("consume").into(), values,
            ).into());
            // Writes to the same source register must still create two distinct
            // SSA definitions; subsequent reads observe the final destination.
            function.block_mut(node).unwrap().push(ast::Assign::new(
                vec![locals[1].clone().into(), locals[1].clone().into(), locals[2].clone().into()],
                vec![locals[2].clone().into(), locals[0].clone().into(), locals[1].clone().into()],
            ).into());
            function.block_mut(node).unwrap().push(ast::Assign::new(
                vec![ast::Index::new(locals[3].clone().into(), locals[1].clone().into()).into()],
                vec![locals[1].clone().into()],
            ).into());
        }
        if seed % 3 != 0 {
            let closure = ast::Closure {
                node_origin: Default::default(),
                function: Default::default(),
                upvalues: vec![
                    ast::Upvalue::Ref(locals[4].clone()), // recursive capture
                    ast::Upvalue::Ref(locals[1].clone()),
                    ast::Upvalue::Copy(locals[1].clone()),
                ],
            };
            function.block_mut(nodes[1]).unwrap().push(ast::Assign::new(
                vec![locals[4].clone().into()], vec![closure.into()],
            ).into());
        }
        if seed % 2 == 0 {
            function.block_mut(nodes[3]).unwrap().push(ast::Close {
                locals: vec![locals[1].clone(), locals[4].clone()],
            }.into());
        }
        function.block_mut(nodes[4]).unwrap().push(ast::Return::new(
            (0..width).map(|index| input[(index + seed) % input.len()].clone()).collect(),
        ).into());
        function
    }

    type ConstructionGroups = (
        usize, Vec<Vec<RcLocal>>, Vec<(RcLocal, FxHashSet<RcLocal>)>, Vec<FxHashSet<RcLocal>>,
    );

    fn group_iteration(groups: &ConstructionGroups) -> (Vec<Vec<u64>>, Vec<(u64, Vec<u64>)>, Vec<Vec<u64>>) {
        let ids = |group: &FxHashSet<RcLocal>| group.iter().map(RcLocal::stable_id).collect();
        (groups.1.iter().map(|group| group.iter().map(RcLocal::stable_id).collect()).collect(),
            groups.2.iter().map(|(root, group)| (root.stable_id(), ids(group))).collect(),
            groups.3.iter().map(ids).collect())
    }

    #[test]
    fn lazy_param_dependencies_match_eager_raw_graphs_and_alias_maps() {
        fn outcome(result: std::thread::Result<bool>) -> Result<bool, String> {
            result.map_err(|error| {
                if let Some(message) = error.downcast_ref::<String>() { message.clone() }
                else if let Some(message) = error.downcast_ref::<&str>() { (*message).to_owned() }
                else { "non-string panic".to_owned() }
            })
        }
        for seed in 0..128usize {
            let mut function = Function::new(0);
            let nodes: Vec<_> = (0..5).map(|_| function.new_block()).collect();
            function.set_entry(nodes[0]);
            function.remove_block(nodes[2]); // sparse NodeIndex storage
            let width = 1 + seed % 9;
            let params: Vec<_> = (0..width).map(|_| RcLocal::default()).collect();
            let external: Vec<_> = (0..4).map(|_| RcLocal::default()).collect();
            function.parameters = external.clone();
            for node in [nodes[1], nodes[3], nodes[4]] {
                for (edge_index, pred) in [nodes[0], node, nodes[0]].into_iter().enumerate() {
                    let arguments = params.iter().enumerate().map(|(index, param)| {
                        let value = match (seed / 9 + index) % 6 {
                            0 => param.clone().into(),
                            1 => external[0].clone().into(),
                            2 => external[edge_index % external.len()].clone().into(),
                            3 => params[(index + 1) % params.len()].clone().into(),
                            4 => ast::Binary::new(external[0].clone().into(), external[1].clone().into(),
                                ast::BinaryOperation::Add).into(),
                            _ if edge_index == 1 => param.clone().into(),
                            _ => external[2].clone().into(),
                        };
                        (param.clone(), value)
                    }).collect();
                    function.graph_mut().add_edge(pred, node, BlockEdge { arguments, ..Default::default() });
                }
            }
            let mut map = FxHashMap::default();
            if seed % 3 == 0 { map.insert(external[0].clone(), external[1].clone()); }
            if seed % 7 == 0 && params.len() > 1 { map.insert(params[0].clone(), params[1].clone()); }
            let groups = (seed % 2 == 0).then(|| IndexMap::from_iter(
                params.iter().chain(&external).cloned().map(|local| (local, external[3].clone()))));
            let mut expected_function = function.clone();
            let mut expected_map = map.clone();
            let before = ast::current_local_id();
            let expected = outcome(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                params_reference::remove_unnecessary_params(&mut expected_function, &mut expected_map, groups.as_ref())
            })));
            let actual = outcome(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                remove_unnecessary_params(&mut function, &mut map, groups.as_ref())
            })));
            assert_eq!(actual, expected, "seed={seed}");
            assert_eq!(ast::current_local_id(), before);
            assert_eq!(map.iter().collect::<Vec<_>>(), expected_map.iter().collect::<Vec<_>>(), "map order seed={seed}");
            assert_constructed_functions_equal(&function, &expected_function);
        }

        // Mismatched incoming schemas may fail during the eager graph build.
        // Preserve both the failure text and the graph state at that boundary.
        let mut function = Function::new(0);
        let [entry, other, join] = std::array::from_fn(|_| function.new_block());
        function.set_entry(entry);
        let [param, absent] = std::array::from_fn(|_| RcLocal::default());
        function.graph_mut().add_edge(entry, join, BlockEdge {
            arguments: vec![(absent, param.clone().into())], ..Default::default()
        });
        function.graph_mut().add_edge(other, join, BlockEdge {
            arguments: vec![(param.clone(), param.into())], ..Default::default()
        });
        let mut expected_function = function.clone();
        let expected = outcome(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            params_reference::remove_unnecessary_params(&mut expected_function, &mut FxHashMap::default(), None)
        })));
        let actual = outcome(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            remove_unnecessary_params(&mut function, &mut FxHashMap::default(), None)
        })));
        assert!(expected.is_err());
        assert_eq!(actual, expected);
        assert_constructed_functions_equal(&function, &expected_function);
    }

    fn local_metadata(function: &Function) -> std::collections::BTreeMap<u64, ast::Local> {
        let mut metadata = std::collections::BTreeMap::new();
        let mut record = |local: &RcLocal| {
            metadata.insert(local.stable_id(), local.0.lock().clone());
            true
        };
        for local in &function.parameters { record(local); }
        for (node, block) in function.blocks() {
            for statement in block.iter() {
                statement.visit_local_reads(&mut record);
                statement.visit_local_writes(&mut record);
            }
            for edge in function.edges(node) {
                for (local, value) in &edge.weight().arguments {
                    record(local);
                    value.visit_local_reads(&mut record);
                }
            }
        }
        metadata
    }

    fn assert_constructed_functions_equal(actual: &Function, expected: &Function) {
        assert_eq!(actual.parameters, expected.parameters);
        assert_eq!(actual.iteration_capture_proofs, expected.iteration_capture_proofs);
        assert_eq!(actual.iteration_capture_obligations, expected.iteration_capture_obligations);
        assert_eq!(local_metadata(actual), local_metadata(expected));
        assert_eq!(format!("{:?}", actual.provenance), format!("{:?}", expected.provenance));
        assert_eq!(actual.graph().node_count(), expected.graph().node_count());
        assert_eq!(actual.graph().edge_count(), expected.graph().edge_count());
        for ((node, block), (expected_node, expected_block)) in actual.blocks().zip(expected.blocks()) {
            assert_eq!(node, expected_node);
            assert_eq!(block, expected_block, "node={node:?}");
            for (edge, expected_edge) in actual.edges(node).zip(expected.edges(node)) {
                assert_eq!(edge.target(), expected_edge.target());
                assert_eq!(edge.weight().arguments, expected_edge.weight().arguments);
            }
        }
    }

    #[test]
    fn batched_renaming_matches_reference_ids_phis_captures_and_duplicate_writes() {
        for seed in 0..36 {
            let function = renaming_fixture(seed, 1 + seed * 3);
            let local_base = ast::current_local_id();
            let mut expected = function.clone();
            let expected_groups = {
                let _reference = ReferenceRenaming::enter(true);
                construct(&mut expected, &Vec::new())
            };
            let expected_end = ast::current_local_id();
            ast::set_local_id_base(local_base);
            let mut actual = function;
            let actual_groups = construct(&mut actual, &Vec::new());
            assert_eq!(ast::current_local_id(), expected_end, "local count seed={seed}");
            assert_eq!(actual_groups, expected_groups, "capture/definition groups seed={seed}");
            assert_eq!(group_iteration(&actual_groups), group_iteration(&expected_groups), "group order seed={seed}");
            assert_constructed_functions_equal(&actual, &expected);
            assert_eq!(actual.parameters, expected.parameters, "parameters seed={seed}");
            for ((node, block), (expected_node, expected_block)) in actual.blocks().zip(expected.blocks()) {
                assert_eq!(node, expected_node);
                assert_eq!(block, expected_block, "statements seed={seed}, node={node:?}");
                for (edge, expected_edge) in actual.edges(node).zip(expected.edges(node)) {
                    assert_eq!(edge.target(), expected_edge.target());
                    assert_eq!(edge.weight().arguments, expected_edge.weight().arguments, "phis seed={seed}");
                }
            }
        }
    }

    #[test]
    fn incoming_cells_survive_capture_free_close_paths_and_copy_only_children() {
        for capture_kind in 0..3 {
            for with_loop in [false, true] {
                let mut function = Function::new(0);
                let nodes: Vec<_> = (0..4).map(|_| function.new_block()).collect();
                function.set_entry(nodes[0]);
                for (from, to) in [(0, 1), (1, 2), (1, 3), (2, 3)] {
                    function.graph_mut().add_edge(nodes[from], nodes[to], BlockEdge::default());
                }
                if with_loop { function.graph_mut().add_edge(nodes[2], nodes[1], BlockEdge::default()); }
                let incoming = RcLocal::default();
                incoming.0.lock().add_source_binding(ast::SourceBinding {
                    origin: ast::BindingOrigin::DebugUpvalue { prototype: 0, slot: 0 }, name: "cell".into(),
                });
                let value = RcLocal::default();
                function.parameters.push(value.clone());
                function.block_mut(nodes[0]).unwrap().push(ast::Assign::new(
                    vec![incoming.clone().into()], vec![value.clone().into()],
                ).into());
                function.block_mut(nodes[1]).unwrap().push(ast::Call::new(
                    ast::Global::from("observe").into(), vec![incoming.clone().into()],
                ).into());
                // Duplicate destinations must mint two versions of the same
                // incoming cell, including when there are no outgoing captures.
                function.block_mut(nodes[2]).unwrap().extend([
                    ast::Assign::new(vec![incoming.clone().into(), incoming.clone().into()],
                        vec![value.clone().into(), ast::Literal::Number(2.0).into()]).into(),
                    ast::Close { locals: vec![incoming.clone(), value.clone()] }.into(),
                ]);
                if capture_kind != 0 {
                    function.block_mut(nodes[2]).unwrap().push(ast::Assign::new(
                        vec![RcLocal::default().into()], vec![ast::Closure {
                            node_origin: Default::default(), function: Default::default(),
                            upvalues: vec![if capture_kind == 1 {
                                ast::Upvalue::Copy(incoming.clone())
                            } else { ast::Upvalue::Ref(incoming.clone()) }],
                        }.into()],
                    ).into());
                }
                function.block_mut(nodes[3]).unwrap().push(ast::Return::new(vec![incoming.clone().into()]).into());
                function.provenance = Some(Box::new(crate::provenance::FunctionTrace::new(0, "incoming-cell".into())));
                let base = ast::current_local_id();
                let mut reference = function.clone();
                let expected = {
                    let _reference = ReferenceRenaming::enter(true);
                    construct(&mut reference, &vec![incoming.clone()])
                };
                let end = ast::current_local_id();
                ast::set_local_id_base(base);
                let actual = construct(&mut function, &vec![incoming.clone()]);
                assert_eq!(ast::current_local_id(), end);
                assert_eq!(actual, expected, "capture={capture_kind}, loop={with_loop}");
                assert_eq!(group_iteration(&actual), group_iteration(&expected));
                assert_constructed_functions_equal(&function, &reference);
                assert!(actual.3.is_empty(), "incoming cells must not become passed-cell groups");
                let group = &actual.2.iter().find(|(root, _)| root == &incoming).unwrap().1;
                assert!(group.len() >= 3, "fresh incoming-cell writes must remain grouped");
                assert!(function.blocks().all(|(_, block)| block.iter().all(|statement| statement.as_close().is_none())));
                for statement in function.block(nodes[2]).unwrap().iter() {
                    for local in statement.values_written() {
                        if statement.as_assign().is_some_and(|assign| assign.left.len() == 2) {
                            assert!(group.contains(local));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reused_operand_buffers_preserve_wide_writes_marker_reads_and_metadata() {
        for width in [1, 3, 64, 257] {
            let mut function = Function::new(0);
            let node = function.new_block();
            function.set_entry(node);
            let locals: Vec<_> = (0..6).map(|_| RcLocal::default()).collect();
            function.parameters = locals.clone();
            for count in [width, 1, width / 2, 0, width] {
                function.block_mut(node).unwrap().push(ast::Assign::new(
                    (0..count).map(|index| locals[index % locals.len()].clone().into()).collect(),
                    (0..count).map(|index| locals[(index + 1) % locals.len()].clone().into()).collect(),
                ).into());
            }
            function.block_mut(node).unwrap().extend([
                ast::NumForInit::new(locals[0].clone(), locals[1].clone(), locals[2].clone()).into(),
                ast::NumForNext::new(locals[0].clone(), locals[1].clone().into(), locals[2].clone().into()).into(),
                ast::GenericForNext::new(vec![locals[3].clone(), locals[3].clone()],
                    locals[0].clone().into(), locals[1].clone(), locals[2].clone()).into(),
                ast::Return::new(locals.iter().cloned().map(Into::into).collect()).into(),
            ]);
            function.local_type_hints.insert((node, 0, 0), "number".into());
            function.local_source_bindings.insert((node, 0, 0), vec![ast::SourceBinding {
                origin: ast::BindingOrigin::DebugLocal { prototype: 0, register: 0, start_pc: 1, end_pc: 2 },
                name: "retained".into(),
            }]);
            function.provenance = Some(Box::new(crate::provenance::FunctionTrace::new(0, "buffer-test".into())));
            let base = ast::current_local_id();
            let mut reference = function.clone();
            let expected = {
                let _reference = ReferenceRenaming::enter(true);
                construct(&mut reference, &Vec::new())
            };
            let end = ast::current_local_id();
            ast::set_local_id_base(base);
            let actual = construct(&mut function, &Vec::new());
            assert_eq!(ast::current_local_id(), end, "width={width}");
            assert_eq!(actual, expected, "width={width}");
            assert_eq!(group_iteration(&actual), group_iteration(&expected), "width={width}");
            assert_constructed_functions_equal(&function, &reference);
        }
    }

    #[test]
    #[ignore = "manual isolated scaling probe; not a whole-engine benchmark"]
    fn benchmark_batched_ssa_renaming() {
        for width in [16, 64, 256, 1024, 4096] {
            let function = renaming_fixture(1, width);
            let local_base = ast::current_local_id();
            for reference in [true, false] {
                let _reference = ReferenceRenaming::enter(reference);
                let mut elapsed = std::time::Duration::ZERO;
                for _ in 0..5 {
                    ast::set_local_id_base(local_base);
                    let mut candidate = function.clone();
                    let start = std::time::Instant::now();
                    std::hint::black_box(construct(&mut candidate, &Vec::new()));
                    elapsed += start.elapsed();
                }
                eprintln!("width={width} reference={reference} mean_us={}", elapsed.as_micros() / 5);
            }
        }
    }

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
            &IndexMap::default()
        ));
        assert!(map.is_empty());
    }
}
