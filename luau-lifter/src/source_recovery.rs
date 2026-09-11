//! Binding-level audit of compiler-recorded names, generated only with analysis.
use std::collections::{BTreeMap, BTreeSet};

use ast::{BindingOrigin, LocalRw, RcLocal, SourceBinding, Traverse};
use serde_json::{json, Value};

pub(crate) fn naming_report(report: ast::refine_names::Report, legacy: ast::naming_evidence::Report) -> Value {
    let final_ids: BTreeSet<_> = report.bindings.iter().map(|binding| binding.id).collect();
    let legacy_candidates = json!({
        "enabled": legacy.enabled,
        "contract": "Hint proposals accepted by legacy rules, including losing and subsequently invalidated hints. Evidence does not change selection or transfer between bindings. Pre-candidate conflicting facts may be refused by the rule.",
        "limits": {"bindings": ast::naming_evidence::BINDING_LIMIT, "candidates_per_binding": ast::naming_evidence::CANDIDATE_LIMIT, "name_bytes": ast::naming_evidence::NAME_BYTE_LIMIT},
        "candidate_attempts": legacy.candidate_attempts, "omitted_attempts": legacy.omitted_attempts,
        "binding_budget_exhausted": legacy.binding_budget_exhausted,
        "rows": legacy.bindings.into_iter().map(|binding| json!({
            "binding_id": format!("b{}", binding.id),
            "final_binding_present": final_ids.contains(&binding.id),
            "selected_hint": binding.selected_hint.map(|(name, priority)| json!({"name": name, "priority": priority})),
            "invalidations": binding.invalidations, "truncated": binding.truncated,
            "candidates": binding.candidates.into_iter().map(|candidate| json!({
                "name": candidate.name, "priority": candidate.priority, "reason": candidate.rule,
                "rule_site": {"file": candidate.file, "line": candidate.line, "column": candidate.column},
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    json!({
        "schema_version": 2, "phase": "final_binding_graph",
        "evidence": "inferred roles, except explicitly recorded_source_binding candidates",
        "priority": "ordinal rule priority, not a probability or effect proof",
        "legacy_coverage": "bounded proposals and invalidations, keyed by pre-cleanup stable binding identity",
        "legacy_candidates": legacy_candidates,
        "api_naming_metadata": {"version": ast::naming_api::VERSION,
            "compiler_commit": ast::naming_api::COMPILER_COMMIT, "source_path": ast::naming_api::SOURCE_PATH,
            "contract": "inferred positional roles for exact syntactic API members; no runtime callee, type, effect or totality proof"},
        "type_evidence_contract": "Bytecode annotations/tags are compiler-recorded representations, not evidence that an author wrote the same annotation. API and usage names are inferred roles; no complex source aliases or generics are invented.",
        "limits": {"nodes": 100000, "bindings": 50000, "depth": 256, "candidates_per_binding": 24, "propagation_rounds": 4},
        "visited_nodes": report.visited_nodes, "bindings": report.binding_count,
        "scopes": report.scope_count, "renamed": report.renamed, "conflicts": report.conflicts,
        "unresolved_calls": report.unresolved_calls, "refused_edges": report.refused_edges,
        "budget_exhausted": report.budget_exhausted,
        "rows": report.bindings.into_iter().map(|binding| json!({
            "binding_id": format!("b{}", binding.id), "before": binding.before,
            "after": binding.after, "kind": binding.kind, "scope": binding.scope,
            "status": binding.status,
            "type_evidence": binding.type_evidence.into_iter().map(|e| json!({
                "representation": e.representation, "origin": e.origin, "semantic_proof": false,
            })).collect::<Vec<_>>(),
            "candidates": binding.candidates.into_iter().map(|c| json!({
                "name": c.name, "priority": c.priority, "reason": c.reason, "witness": c.witness,
                "from_binding": c.from_binding.map(|id| format!("b{id}")),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

pub(crate) fn provenance_report(
    traces: Vec<Box<cfg::provenance::FunctionTrace>>,
    body: &mut ast::Block,
    emission_map: ast::emission_map::EmissionMap,
) -> Value {
    let id = |id| format!("b{id}");
    let ids = |items: &[u64]| items.iter().map(|&item| id(item)).collect::<Vec<_>>();
    let mut locals = BTreeMap::new();
    collect(body, &mut locals, &mut BTreeSet::new());
    let mut emitted_by_origin: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut emitted = Vec::new();
    let mut known_origins = BTreeSet::new();
    let mut select_origins = BTreeSet::new();
    for trace in &traces {
        known_origins.extend(trace.registers.keys().copied());
        known_origins.extend(trace.definitions.keys().copied());
        select_origins.extend(trace.selects.iter().map(|select| select.binding));
    }
    let mut unlocated = 0;
    let mut incomplete = 0;
    let emitted_ids: BTreeSet<_> = emission_map.bindings.iter().map(|item| item.binding_id).collect();
    let token_count = emission_map.bindings.len();
    let annotation_count = emission_map.annotations.len();
    let opaque_count = emission_map.opaque_regions.len();
    let omitted_count = emission_map.omitted_occurrences;
    let span = |span: ast::formatter::SourceSpan| {
        let position = |p: ast::formatter::SourcePosition| json!({
            "byte_offset": p.byte_offset, "line_one_based": p.line_one_based, "column_one_based": p.column_one_based,
        });
        json!({"start": position(span.start), "end": position(span.end)})
    };
    let output_map = json!({
        "schema_version": 1, "model": "final-emission-binding-spans-v1",
        "contract": "Exact final identifier spans reference stable final binding IDs. Follow a binding's lineage to SSA definitions and their lifted statement PC sets for storage ancestry only; those sets are not precise producer PCs for an individual use. No value, close, purity or source-equality proof is inferred.",
        "positions": "Half-open UTF-8 byte offsets; lines and Unicode-scalar columns are one-based; tabs count as one column.",
        "limits": {"occurrences": ast::emission_map::OCCURRENCE_LIMIT, "annotation_text_bytes": ast::emission_map::ANNOTATION_BYTE_LIMIT},
        "omitted_occurrences": omitted_count,
        "bindings": emission_map.bindings.into_iter().map(|item| json!({
            "binding_id": id(item.binding_id), "role": item.role, "span": span(item.span),
        })).collect::<Vec<_>>(),
        "annotations": emission_map.annotations.into_iter().map(|item| json!({
            "classification": "emitter_annotation", "text": item.text, "text_truncated": item.truncated,
            "span": span(item.span), "instruction_origin": "unknown",
        })).collect::<Vec<_>>(),
        "opaque_regions": emission_map.opaque_regions.into_iter().map(|item| json!({
            "reason": item.reason, "span": span(item.span),
        })).collect::<Vec<_>>(),
        "limitations": "Implicit method receiver declarations have no identifier token. Interpolation sub-rendering and display fallbacks are explicit opaque regions. An absent token is not evidence of dead code, inlining or synthesis. An annotation's text is not promoted to a verified source/proof claim.",
    });
    let bindings_without_tokens = locals.keys().filter(|id| !emitted_ids.contains(id)).count();
    for (binding, local) in locals {
        let local = local.0.lock();
        if let Some(lineage) = &local.3 {
            for &origin in &lineage.definitions { emitted_by_origin.entry(origin).or_default().push(binding); }
            let missing = lineage.definitions.iter().filter(|origin| !known_origins.contains(origin)).copied().collect::<Vec<_>>();
            incomplete += usize::from(lineage.incomplete || !missing.is_empty());
            emitted.push(json!({"binding_id": id(binding), "name": local.0, "lineage": ids(&lineage.definitions),
                "incomplete": lineage.incomplete || !missing.is_empty(), "unknown_origins": ids(&missing),
                "has_conditional_result_ancestry": lineage.definitions.iter().any(|origin| select_origins.contains(origin)),
                "recorded_source_origins": local.2.iter().map(|b| origin_json(&b.origin)).collect::<Vec<_>>(),
            }));
        } else {
            unlocated += 1;
            emitted.push(json!({"binding_id": id(binding), "name": local.0, "lineage": [],
                "incomplete": true, "reason": "unattributed_or_synthesized_after_ssa"}));
        }
    }
    let mapping = |origin| emitted_by_origin.get(&origin).map(|bindings| ids(bindings)).unwrap_or_default();
    let mut source_sites = 0;
    let mut source_sites_with_pc = 0;
    let mut definition_count = 0;
    let mut mapped_definitions = 0;
    let mut dropped_records = 0;
    let mut select_count = 0;
    let functions = traces.into_iter().map(|trace| {
        source_sites += trace.lifted.len();
        source_sites_with_pc += trace.lifted.values().filter(|site| !site.pcs.is_empty()).count();
        definition_count += trace.definitions.len();
        mapped_definitions += trace.definitions.keys().filter(|definition| emitted_by_origin.contains_key(definition)).count();
        dropped_records += trace.dropped_records;
        select_count += trace.selects.len();
        json!({
            "function_id": trace.function_id, "prototype": trace.prototype,
            "instruction_count": trace.instruction_count,
            "dropped_records": trace.dropped_records,
            "registers": trace.registers.values().map(|r| json!({"id": id(r.id), "slot": r.slot, "kind": r.kind,
                "source_bindings": r.source_bindings.iter().map(|b| json!({"name": b.name, "origin": origin_json(&b.origin)})).collect::<Vec<_>>(),
                "final_bindings": mapping(r.id),
            })).collect::<Vec<_>>(),
            "lifted_statements": trace.lifted.values().map(|s| json!({
                "block": s.block, "statement_index": s.index, "instruction_pcs": s.pcs, "source_lines": s.lines,
                "kind": s.kind, "read_registers": ids(&s.read_registers), "written_registers": ids(&s.written_registers),
            })).collect::<Vec<_>>(),
            "definitions": trace.definitions.values().map(|d| json!({"id": id(d.id), "kind": d.kind,
                "original_register": id(d.register), "block": d.block, "statement_index": d.statement,
                "write_index": d.write_index, "dependencies": ids(&d.dependencies),
                "source_bindings": d.source_bindings.iter().map(|b| json!({"name": b.name, "origin": origin_json(&b.origin)})).collect::<Vec<_>>(),
                "final_bindings": mapping(d.id),
                "status": if emitted_by_origin.contains_key(&d.id) { "mapped_storage_ancestry" } else { "no_final_binding_mapping" },
            })).collect::<Vec<_>>(),
            "local_maps": trace.maps.iter().map(|m| json!({"phase": m.phase, "from": id(m.from), "to": id(m.to)})).collect::<Vec<_>>(),
            "conditional_results": trace.selects.iter().map(|s| json!({"phase": s.phase,
                "proof": "two_arm_private_block_join", "branch": s.branch, "join": s.join,
                "binding_id": id(s.binding), "condition_bindings": ids(&s.condition_bindings),
                "then_predecessor": s.then_predecessor, "else_predecessor": s.else_predecessor,
                "then_value": id(s.then_value), "else_value": id(s.else_value), "final_bindings": mapping(s.binding),
            })).collect::<Vec<_>>(),
            "pre_destruct_bindings": ids(&trace.pre_destruct_bindings),
            "post_destruct_bindings": ids(&trace.post_destruct_bindings),
        })
    }).collect::<Vec<_>>();
    json!({"schema_version": 1, "model": "ssa-storage-lineage-v1",
        "limits": {"records_per_function": cfg::provenance::RECORD_LIMIT, "ancestry_per_binding": ast::BindingLineage::LIMIT},
        "origin_granularity": "input instruction clusters per lifted statement and SSA definition write slot; exact final identifier spans link to binding storage ancestry, not nested value producer PCs",
        "contract": "Diagnostic ancestry only. Storage coalescing is not value/source-binding equality. No close, ownership, purity or totality certificate is created or transferred by this trace.",
        "summary": {"functions": functions.len(), "lifted_statements": source_sites, "statements_with_pc": source_sites_with_pc,
            "definitions": definition_count, "definitions_with_final_binding_ancestry": mapped_definitions,
            "final_bindings": emitted.len(), "unlocated_final_bindings": unlocated, "incomplete_lineages": incomplete,
            "conditional_result_records": select_count, "dropped_records": dropped_records,
            "identifier_spans": token_count, "annotation_spans": annotation_count,
            "opaque_output_regions": opaque_count, "omitted_output_occurrences": omitted_count,
            "bindings_without_identifier_tokens": bindings_without_tokens},
        "functions": functions, "final_bindings": emitted,
        "output_map": output_map,
        "limitations": "An absent direct mapping does not distinguish inlining, dead code, cloning or synthesis. Conditional results are retained as statements; the trace does not authorize eager evaluation or change source naming. Arbitrary value-producer provenance and pass-complete invalidation remain open.",
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
