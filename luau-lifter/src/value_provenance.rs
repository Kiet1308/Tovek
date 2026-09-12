//! Bounded projection from immutable input value graphs to exact output regions.
//! These are dependency/source ancestry relations, not value-identity proofs.
use std::collections::{BTreeMap, BTreeSet};
use ast::{RcLocal, emission_map::EmissionMap};
use cfg::provenance::FunctionTrace;
use serde_json::{Value, json};

const SITE_LIMIT: usize = 64;
const WALK_LIMIT: usize = 256;
const WORK_LIMIT: usize = 2_000_000;

pub fn report(traces: &[Box<FunctionTrace>], locals: &BTreeMap<u64, RcLocal>, map: &EmissionMap,
              producers: &[ast::local_producers::Pass]) -> Value {
    let mut sites = BTreeMap::new();
    let mut definition_sites = BTreeMap::new();
    let mut dependencies: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    let mut input_parameters = BTreeSet::new();
    let mut compiler_temporaries = BTreeSet::new();
    let mut incoming_captures = BTreeSet::new();
    let mut incomplete_ids = BTreeSet::new();
    let function_indices: BTreeMap<_, _> = traces.iter().enumerate()
        .map(|(index, trace)| (trace.function_id.as_str(), index)).collect();
    let introduced: BTreeSet<_> = producers.iter().flat_map(|p| &p.ledger.records).map(|r| r.binding_id.as_str()).collect();
    for (function, trace) in traces.iter().enumerate() {
        for site in trace.lifted.values() {
            let id = format!("f{function}:b{}:s{}", site.block, site.index);
            sites.insert(id, json!({"function_id": trace.function_id, "prototype": trace.prototype,
                "block": site.block, "statement_index": site.index,
                "instruction_pcs": site.pcs, "source_lines": site.lines, "instruction_role": site.kind}));
        }
        for register in trace.registers.values() {
            if register.kind == "parameter" { input_parameters.insert(register.id); }
            if register.kind == "incoming_upvalue" { incoming_captures.insert(register.id); }
        }
        for definition in trace.definitions.values() {
            if definition.kind == "compiler_loop_control_definition" { compiler_temporaries.insert(definition.id); }
            if let Some(statement) = definition.statement {
                let site = format!("f{function}:b{}:s{statement}", definition.block);
                if sites.contains_key(&site) { definition_sites.insert(definition.id, site); }
                else { incomplete_ids.insert(definition.id); }
            }
            dependencies.entry(definition.id).or_default().extend(&definition.dependencies);
            if trace.dropped_records != 0 { incomplete_ids.insert(definition.id); }
        }
        // Mapping is ancestry, not equality: keep both contributors even when
        // register reuse made their values or source declarations different.
        for event in &trace.maps {
            dependencies.entry(event.to).or_default().insert(event.from);
        }
        for event in &trace.inlines {
            for sink in &event.consumer_bindings {
                dependencies.entry(*sink).or_default().insert(event.producer);
            }
        }
    }
    let mut fuel = WORK_LIMIT;
    let mut output_roles: BTreeMap<u64, BTreeSet<&str>> = BTreeMap::new();
    for token in &map.bindings { output_roles.entry(token.binding_id).or_default().insert(token.role); }
    for region in &map.regions {
        if region.kind == "closure" {
            for binding in &region.bindings { output_roles.entry(*binding).or_default().insert("closure_capture"); }
        }
    }
    let mut binding_sites = BTreeMap::new();
    let mut binding_rows = Vec::new();
    for (&id, local) in locals {
        let local = local.0.lock();
        let mut pending = local.3.as_ref().map(|l| l.definitions.clone()).unwrap_or_default();
        let mut seen = BTreeSet::new();
        let mut found = BTreeSet::new();
        let mut incomplete = local.3.as_ref().is_none_or(|l| l.incomplete);
        while let Some(origin) = pending.pop() {
            if seen.contains(&origin) { continue; }
            if fuel == 0 || seen.len() == WALK_LIMIT { incomplete = true; break; }
            fuel -= 1;
            seen.insert(origin);
            incomplete |= incomplete_ids.contains(&origin);
            if let Some(site) = definition_sites.get(&origin) { found.insert(site.clone()); }
            if found.len() > SITE_LIMIT { incomplete = true; found.pop_last(); }
            if let Some(edges) = dependencies.get(&origin) { pending.extend(edges.iter().rev()); }
        }
        let roles = output_roles.get(&id).cloned().unwrap_or_default();
        let mut classification = Vec::new();
        if roles.contains("parameter") { classification.push("output_parameter"); }
        if roles.contains("iteration_binding") { classification.push("output_iteration_binding"); }
        if roles.contains("closure_capture") { classification.push("closure_constructor_capture_ancestry"); }
        if local.2.iter().any(|b| matches!(b.origin, ast::BindingOrigin::DebugLocal { .. })) {
            classification.push("recorded_source_local");
        }
        if local.4.conditional_result { classification.push("inferred_conditional_result"); }
        if local.3.as_ref().is_some_and(|l| l.definitions.iter().any(|id| compiler_temporaries.contains(id))) {
            classification.push("compiler_loop_temporary_ancestry");
        }
        if local.3.as_ref().is_some_and(|l| l.definitions.iter().any(|id| incoming_captures.contains(id))) {
            classification.push("incoming_capture_ancestry");
        }
        if introduced.contains(format!("b{id}").as_str()) { classification.push("recorded_emitter_synthesis"); }
        if local.3.as_ref().is_some_and(|l| l.copied_local_metadata) {
            classification.push("copied_local_metadata_ancestry");
        }
        if seen.iter().any(|id| input_parameters.contains(id)) { classification.push("parameter_ancestry"); }
        if classification.is_empty() { classification.push("unclassified"); }
        binding_rows.push(json!({"binding_id": format!("b{id}"), "classifications": classification,
            "source_sites": found, "incomplete": incomplete,
            "storage_and_source_identity_distinct": true}));
        binding_sites.insert(id, (found, incomplete));
    }
    let regions = map.regions.iter().map(|region| {
        let mut found = BTreeSet::new();
        let mut incomplete = false;
        for binding in &region.bindings {
            if let Some((sites, partial)) = binding_sites.get(binding) {
                incomplete |= partial;
                for site in sites {
                    found.insert(site.clone());
                    if found.len() > SITE_LIMIT { incomplete = true; found.pop_last(); }
                }
            } else { incomplete = true; }
        }
        let empty = found.is_empty();
        let mut node_inputs = Vec::new();
        let mut node_incomplete = false;
        if let Some(data) = &region.origin.0 {
            node_incomplete = data.incomplete;
            for input in &data.inputs {
                let Some(&function) = function_indices.get(input.function.as_ref()) else {
                    node_incomplete = true; continue;
                };
                let site = format!("f{function}:b{}:s{}", input.block, input.statement);
                let trace = &traces[function];
                let valid_value = input.value.is_none_or(|id| trace.values.get(id).is_some_and(|v|
                    v.block == input.block && v.statement == input.statement));
                if !sites.contains_key(&site) || !valid_value {
                    node_incomplete = true; continue;
                }
                node_inputs.push(json!({"function_id": input.function.as_ref(),
                    "value_origin": input.value, "source_site": site}));
            }
        }
        let synthesized = region.origin.0.as_ref().and_then(|o| o.synthesized);
        let node_relation = if node_inputs.is_empty() {
            if synthesized.is_some() { "synthesized_node" } else { "unknown" }
        } else { "retained_node_ancestry" };
        let node_ancestry = json!({"inputs": node_inputs,
            "inlined": region.origin.0.as_ref().is_some_and(|o| o.inlined),
            "cloned": region.origin.0.as_ref().is_some_and(|o| o.cloned),
            "synthesized_by": synthesized, "relation": node_relation,
            "incomplete": node_incomplete || node_relation == "unknown",
            "exact_value_producer": false});
        json!({"kind": region.kind,
            "start_byte": region.span.start.byte_offset, "end_byte": region.span.end.byte_offset,
            "bindings": region.bindings.iter().map(|id| format!("b{id}")).collect::<Vec<_>>(),
            "source_sites": found, "incomplete": incomplete || empty,
            "relation": if empty { "unknown" } else { "storage_dependency_ancestry" },
            "node_ancestry": node_ancestry,
            "exact_value_producer": false})
    }).collect::<Vec<_>>();
    json!({"schema_version": 1, "model": "input-value-graph-and-output-regions-v1",
        "limits": {"sites_per_region": SITE_LIMIT, "binding_walk": WALK_LIMIT,
            "work": WORK_LIMIT, "output_regions": ast::emission_map::REGION_LIMIT},
        "visited_dependencies": WORK_LIMIT - fuel, "work_exhausted": fuel == 0,
        "omitted_output_regions": map.omitted_regions,
        "source_sites": sites, "bindings": binding_rows, "output_regions": regions,
        "node_input_limit": ast::node_origins::INPUT_LIMIT,
        "contract": "Input value paths are immutable occurrences before SSA copy propagation. Final spans identify exact emitted syntax. Storage dependencies and retained node tags are separate ancestry relations, never exact producers, evaluation order, lifetime or source spelling. Node clone flags record actual AST copies surviving into output, not duplicated runtime execution; shared storage ancestry alone never proves cloning. Committed inline substitutions mark the installed node. Reductions union parent and child node origins. Newly rebuilt unattributed nodes and detached scalar leaves remain unknown. Explicit reconstruction producers identify synthetic nodes without claiming original call sites. Missing debug data never proves a compiler temporary.",
        "pass_contracts": ast::node_origins::PASS_CONTRACTS.iter().map(|(pass, policy)|
            json!({"pass": pass, "policy": policy, "transfers_effect_or_lifetime_proof": false})).collect::<Vec<_>>()})
}
