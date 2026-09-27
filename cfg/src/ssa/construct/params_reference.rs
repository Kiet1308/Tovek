// Independent eager dependency-graph oracle retained for scheduling differentials.
use super::*;

pub(super) fn remove_unnecessary_params(
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
        if !function.edges_to_block(node).any(|(_, edge)| !edge.arguments.is_empty()) {
            continue;
        }
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
                            if !param.source_bindings_compatible(arg) {
                                continue;
                            }
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
