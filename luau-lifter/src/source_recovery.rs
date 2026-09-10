//! Binding-level audit of compiler-recorded names, generated only with analysis.
use std::collections::{BTreeMap, BTreeSet};

use ast::{BindingOrigin, LocalRw, RcLocal, SourceBinding, Traverse};
use serde_json::{json, Value};

pub(crate) fn naming_report(report: ast::refine_names::Report) -> Value {
    json!({
        "schema_version": 1, "phase": "final_binding_graph",
        "evidence": "inferred roles, except explicitly recorded_source_binding candidates",
        "priority": "ordinal rule priority, not a probability or effect proof",
        "legacy_coverage": "selected legacy names only; legacy alternatives are not yet collected",
        "limits": {"nodes": 100000, "bindings": 50000, "depth": 256, "candidates_per_binding": 24, "propagation_rounds": 4},
        "visited_nodes": report.visited_nodes, "bindings": report.binding_count,
        "scopes": report.scope_count, "renamed": report.renamed, "conflicts": report.conflicts,
        "unresolved_calls": report.unresolved_calls, "refused_edges": report.refused_edges,
        "budget_exhausted": report.budget_exhausted,
        "rows": report.bindings.into_iter().map(|binding| json!({
            "binding_id": format!("b{}", binding.id), "before": binding.before,
            "after": binding.after, "kind": binding.kind, "scope": binding.scope,
            "status": binding.status, "candidates": binding.candidates.into_iter().map(|c| json!({
                "name": c.name, "priority": c.priority, "reason": c.reason, "witness": c.witness,
                "from_binding": c.from_binding.map(|id| format!("b{id}")),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn collect(
    block: &mut ast::Block,
    locals: &mut BTreeMap<u64, RcLocal>,
    protos: &mut BTreeSet<usize>,
) {
    for statement in block.iter_mut() {
        for local in statement.values() {
            locals.insert(local.stable_id(), local.clone());
        }
        statement.post_traverse_rvalues(&mut |value| -> Option<()> {
            if let ast::RValue::Closure(closure) = value {
                let mut function = closure.function.lock();
                if let Some(proto) = function.bytecode_proto_id {
                    protos.insert(proto);
                }
                for parameter in &function.parameters {
                    locals.insert(parameter.stable_id(), parameter.clone());
                }
                collect(&mut function.body, locals, protos);
            }
            None
        });
        match statement {
            ast::Statement::If(branch) => {
                collect(&mut branch.then_block.lock(), locals, protos);
                collect(&mut branch.else_block.lock(), locals, protos);
            }
            ast::Statement::While(loop_) => collect(&mut loop_.block.lock(), locals, protos),
            ast::Statement::Repeat(loop_) => collect(&mut loop_.block.lock(), locals, protos),
            ast::Statement::NumericFor(loop_) => collect(&mut loop_.block.lock(), locals, protos),
            ast::Statement::GenericFor(loop_) => collect(&mut loop_.block.lock(), locals, protos),
            _ => {}
        }
    }
}

fn origin_json(origin: &BindingOrigin) -> Value {
    match origin {
        BindingOrigin::DebugLocal {
            prototype,
            register,
            start_pc,
            end_pc,
        } => json!({
            "kind": "debug_local", "prototype": prototype, "register": register,
            "start_pc": start_pc, "end_pc": end_pc,
        }),
        BindingOrigin::DebugUpvalue { prototype, slot } => json!({
            "kind": "debug_upvalue", "prototype": prototype, "slot": slot,
        }),
        BindingOrigin::Function { prototype } => {
            json!({"kind": "function_name", "prototype": prototype})
        }
    }
}

pub(crate) fn audit(
    chunk: &crate::deserializer::chunk::Chunk,
    body: &mut ast::Block,
    functions: &[crate::upvalue_analysis::FunctionUpvalueAnalysis],
) -> Value {
    let mut locals = BTreeMap::new();
    let mut emitted_protos = BTreeSet::from([chunk.main]);
    collect(body, &mut locals, &mut emitted_protos);
    let mut mapped: BTreeMap<SourceBinding, Vec<Value>> = BTreeMap::new();
    let mut emitted_bindings = Vec::new();
    for (id, local) in locals {
        let local = local.0.lock();
        let chosen = local.source_name();
        for binding in &local.2 {
            mapped.entry(binding.clone()).or_default().push(json!({
                "binding_id": format!("b{id}"), "emitted_name": local.0,
                "chosen_source_name": chosen,
                "exact_spelling": chosen == Some(binding.name.as_str()) && local.0.as_deref() == chosen,
            }));
        }
        emitted_bindings.push(json!({"binding_id": format!("b{id}"), "name": local.0,
            "name_provenance": if chosen.is_some() { "recorded" } else if local.2.is_empty() { "inferred_or_generated" } else { "ambiguous" },
            "origins": local.2.iter().map(|b| origin_json(&b.origin)).collect::<Vec<_>>() }));
    }
    let mut records = Vec::new();
    let mut record = |prototype: usize, index: usize, origin: BindingOrigin| {
        let bytes = index.checked_sub(1).and_then(|i| chunk.string_table.get(i));
        let valid_name = bytes
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .filter(|name| ast::valid_source_name(name));
        let binding = valid_name.map(|name| SourceBinding {
            origin: origin.clone(),
            name: name.to_string(),
        });
        let mut emitted = binding
            .as_ref()
            .and_then(|b| mapped.get(b))
            .cloned()
            .unwrap_or_default();
        if matches!(origin, BindingOrigin::Function { .. }) && emitted.is_empty() {
            if let Some(name) = valid_name {
                for function in functions.iter().filter(|f| f.proto_id == prototype) {
                    for occurrence in &function.occurrences {
                        if let Some(display) = &occurrence.display_name {
                            if display == name
                                || display.ends_with(&format!(".{name}"))
                                || display.ends_with(&format!(":{name}"))
                            {
                                emitted.push(json!({"function_id": function.function_id,
                                    "occurrence_id": occurrence.occurrence_id,
                                    "mapping_kind": "named_function_occurrence", "emitted_name": display,
                                    "chosen_source_name": name, "exact_spelling": true,
                                    "span": occurrence.span}));
                            }
                        }
                    }
                }
            }
        }
        let reason = if valid_name.is_none() {
            "invalid_source_name"
        } else if !emitted_protos.contains(&prototype) {
            "prototype_not_emitted"
        } else if emitted.is_empty() {
            "no_proven_binding_mapping"
        } else if emitted.iter().all(|b| b["chosen_source_name"].is_null()) {
            "conflicting_source_bindings"
        } else {
            "mapped"
        };
        records.push(json!({"origin": origin_json(&origin),
            "recorded_name": bytes.map(|b| String::from_utf8_lossy(b).into_owned()),
            "status": reason, "emitted_bindings": emitted}));
    };
    for (prototype, function) in chunk.functions.iter().enumerate() {
        if function.function_name != 0 {
            record(
                prototype,
                function.function_name,
                BindingOrigin::Function { prototype },
            );
        }
        for local in &function.debug_locals {
            record(
                prototype,
                local.name_index,
                BindingOrigin::DebugLocal {
                    prototype,
                    register: local.register,
                    start_pc: local.start_pc,
                    end_pc: local.end_pc,
                },
            );
        }
        for (slot, &index) in function.debug_upvalue_name_indices.iter().enumerate() {
            record(
                prototype,
                index,
                BindingOrigin::DebugUpvalue { prototype, slot },
            );
        }
    }
    let mapped_count = records.iter().filter(|r| r["status"] == "mapped").count();
    json!({"schema_version": 1, "recorded_names": records.len(), "mapped_names": mapped_count,
        "unmapped_names": records.len() - mapped_count, "records": records, "bindings": emitted_bindings})
}
