//! Presentation constraints chosen while values are still SSA definitions.
//! No graph, evaluation order, capture group or close proof is changed here.
use ast::{LocalRw, RcLocal, Statement};
use rustc_hash::FxHashSet;

use crate::function::Function;

/// A selected value returned alongside a predicate derived from it deserves
/// its own result binding. Keep the parameter spelling for the incoming value.
/// This bounded rule deliberately leaves ordinary parameter updates alone.
pub fn preserve_conditional_results(function: &Function, captured: &FxHashSet<RcLocal>) {
    let _phase = ast::telemetry::Span::new("F_PRESERVE_BINDINGS");
    if function.graph().node_count() > 512 { return; }
    // Every select result requires a terminal If with exactly two tagged
    // outgoing edges. Refuse this common no-op before scanning local operands.
    if !function.blocks().any(|(node, block)| matches!(block.last(), Some(Statement::If(_)))
        && function.conditional_edges(node).is_some())
    {
        ast::telemetry::count("binding_preservation_no_branch", 1);
        return;
    }
    let mut locals = std::collections::BTreeSet::new();
    for (_, block) in function.blocks() {
        for statement in block.iter() {
            let mut collect = |local: &RcLocal| {
                locals.insert(local.stable_id());
                locals.len() <= 160
            };
            if !statement.visit_local_reads(&mut collect) || !statement.visit_local_writes(&mut collect) {
                return;
            }
        }
    }
    // Splitting can increase register pressure; retain headroom for the emitter.
    if locals.len() > 160 { return; }
    let (results, dropped) = crate::provenance::select_results(function, "binding_preservation");
    if dropped != 0 || results.is_empty() { return; }
    let mut observations = std::collections::BTreeMap::<u64, (usize, bool)>::new();
    for (_, block) in function.blocks() {
        for statement in block.iter() {
            statement.visit_local_reads(&mut |local| {
                observations.entry(local.stable_id()).or_default().0 += 1;
                true
            });
            if let Statement::Return(ret) = statement {
                for local in ret.values.iter().filter_map(|v| v.as_local()) {
                    observations.entry(local.stable_id()).or_default().1 = true;
                }
            }
        }
    }
    for result in results {
        let join = petgraph::stable_graph::NodeIndex::new(result.join);
        let Some(parameter) = function.edges_to_block(join)
            .flat_map(|(_, e)| &e.arguments).map(|(p, _)| p)
            .find(|p| p.stable_id() == result.binding) else { continue; };
        if captured.contains(parameter) || parameter.has_source_binding()
            || parameter.0.lock().4.parameter { continue; }
        // SSA identity makes observations in later dominated blocks refer to
        // this definition. A lone return or plain reassignment is insufficient.
        let (reads, returned) = observations.get(&parameter.stable_id()).copied().unwrap_or_default();
        if reads >= 2 && returned {
            let mut local = parameter.0.lock();
            local.4.conditional_result = true;
            // Normalizing one optional argument (e.g. index = #items) keeps
            // that argument's role. A new selected role is justified when
            // choosing between two distinct incoming parameter identities.
            local.4.separate_from_parameter = [result.then_value, result.else_value].iter()
                .all(|id| function.parameters.iter().any(|p| p.stable_id() == *id));
        }
    }
}

#[cfg(test)]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockEdge, BranchType};
    use ast::{Block, If, Local, Return};

    #[test]
    fn candidate_gates_and_numeric_census_match_legacy_presentation_roles() {
        for seed in 0..96usize {
            let mut function = Function::new(0);
            let [branch, yes, no, join] = std::array::from_fn(|_| function.new_block());
            function.set_entry(branch);
            let locals = (0..4).map(|_| RcLocal::new(Local::new(None))).collect::<Vec<_>>();
            function.parameters = locals[1..3].to_vec();
            if seed % 5 != 0 {
                function.block_mut(branch).unwrap().push(If::new(locals[0].clone().into(),
                    Block::default(), Block::default()).into());
            }
            let mut add = |from, to, kind, value: Option<&RcLocal>| {
                function.graph_mut().add_edge(from, to, BlockEdge {
                    branch_type: kind,
                    arguments: value.map(|value| vec![(locals[3].clone(), value.clone().into())]).unwrap_or_default(),
                });
            };
            add(branch, yes, BranchType::Then, None);
            add(branch, no, BranchType::Else, None);
            add(yes, join, BranchType::Unconditional, Some(&locals[1]));
            add(no, join, BranchType::Unconditional, Some(&locals[if seed % 3 == 0 { 1 } else { 2 }]));
            if seed % 11 == 0 { add(branch, yes, BranchType::Then, None); }
            let mut returned = vec![locals[3].clone().into(); 1 + seed % 3];
            let extra = if seed % 4 == 0 { 157 + seed % 3 } else { seed % 9 };
            returned.extend((0..extra).map(|_| RcLocal::default().into()));
            function.block_mut(join).unwrap().push(Return::new(returned).into());
            if seed % 13 == 0 { locals[3].0.lock().4.parameter = true; }
            let captured = if seed % 7 == 0 { FxHashSet::from_iter([locals[3].clone()]) } else { FxHashSet::default() };
            let initial = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
            let next = ast::current_local_id();
            reference::preserve_conditional_results(&function, &captured);
            let expected = locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>();
            for (local, data) in locals.iter().zip(initial) { *local.0.lock() = data; }
            preserve_conditional_results(&function, &captured);
            assert_eq!(locals.iter().map(|local| local.0.lock().clone()).collect::<Vec<_>>(), expected, "seed={seed}");
            assert_eq!(ast::current_local_id(), next);
        }
    }
}
