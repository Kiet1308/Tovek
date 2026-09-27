//! Presentation constraints chosen while values are still SSA definitions.
//! No graph, evaluation order, capture group or close proof is changed here.
use ast::{LocalRw, RcLocal, Statement};
use rustc_hash::FxHashSet;

use crate::function::Function;

/// A selected value returned alongside a predicate derived from it deserves
/// its own result binding. Keep the parameter spelling for the incoming value.
/// This bounded rule deliberately leaves ordinary parameter updates alone.
pub fn preserve_conditional_results(function: &Function, captured: &FxHashSet<RcLocal>) {
    if function.graph().node_count() > 512 { return; }
    let locals = function.blocks().flat_map(|(_, block)| block.iter().flat_map(|s| s.values()))
        .collect::<std::collections::BTreeSet<_>>();
    // Splitting can increase register pressure; retain headroom for the emitter.
    if locals.len() > 160 { return; }
    let (results, dropped) = crate::provenance::select_results(function, "binding_preservation");
    if dropped != 0 { return; }
    let mut observations = std::collections::BTreeMap::<u64, (usize, bool)>::new();
    for (_, block) in function.blocks() {
        for statement in block.iter() {
            for local in statement.values_read() {
                observations.entry(local.stable_id()).or_default().0 += 1;
            }
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
