//! Immutable upvalue provenance extracted directly from Luau bytecode.
//!
//! This module deliberately runs before lifting/SSA. Later passes may rename,
//! merge, materialize, or synthesize locals and closures, but they must never be
//! asked to reconstruct the ordered VM slots or raw VAL/REF/UPVAL capture chain.

use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

use crate::{
    deserializer::{chunk::Chunk, constant::Constant, function::Function},
    instruction::Instruction,
    op_code::OpCode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AnalysisDiagnostic {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proto_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pc: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpvalueAccessKind {
    Read,
    Write,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UpvalueAccess {
    pub slot_zero_based: usize,
    pub pc_zero_based: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytecode_source_line: Option<u32>,
    pub kind: UpvalueAccessKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum ClosureConstructor {
    #[serde(rename = "NEWCLOSURE")]
    NewClosure,
    #[serde(rename = "DUPCLOSURE")]
    DupClosure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CaptureKind {
    #[serde(rename = "VAL")]
    Value,
    #[serde(rename = "REF")]
    Reference,
    #[serde(rename = "UPVAL")]
    ParentUpvalue,
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RawCapture {
    pub target_slot_zero_based: usize,
    pub ordinal_one_based: usize,
    pub capture_pc_zero_based: usize,
    pub kind: CaptureKind,
    /// Register for VAL/REF, parent upvalue slot for UPVAL.
    pub source_index_zero_based: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_debug_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_local_lifetime: Option<PcRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PcRange {
    pub start_pc_inclusive: usize,
    pub end_pc_exclusive: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClosureSiteAnalysis {
    pub site_id: String,
    pub parent_proto_id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_proto_id: Option<usize>,
    pub constructor_pc_zero_based: usize,
    pub constructor: ClosureConstructor,
    pub captures: Vec<RawCapture>,
    pub status: AnalysisStatus,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PrototypeAnalysis {
    pub proto_id: usize,
    pub line_defined: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_name: Option<String>,
    pub upvalue_count: usize,
    pub debug_upvalue_names: Vec<Option<String>>,
    pub accesses: Vec<UpvalueAccess>,
    pub closure_sites: Vec<ClosureSiteAnalysis>,
    pub status: AnalysisStatus,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RawUpvalueAnalysis {
    pub bytecode_version: u8,
    pub main_proto_id: usize,
    pub status: AnalysisStatus,
    pub prototypes: Vec<PrototypeAnalysis>,
    pub functions: Vec<StaticFunctionOccurrence>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StaticFunctionOccurrence {
    pub function_id: String,
    pub proto_id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_function_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_site_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prototype_closure_site_id: Option<String>,
    pub lexical_depth: usize,
    pub child_function_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameProvenance {
    DebugUpvalue,
    DebugCaptureSource,
    DecompilerBinding,
    Fallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NameConfidence {
    Exact,
    High,
    Medium,
    Low,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolvedName {
    pub text: String,
    pub provenance: NameProvenance,
    pub confidence: NameConfidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UpvalueSlotAnalysis {
    pub slot_binding_id: String,
    pub slot_zero_based: usize,
    pub ordinal_one_based: usize,
    pub name: ResolvedName,
    pub name_varies_by_occurrence: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_source_binding: Option<BindingReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_source_semantic_id: Option<String>,
    pub emitted_bindings: Vec<EmittedBindingReference>,
    pub capture_chain: Vec<CaptureChainStep>,
    pub read_count: usize,
    pub write_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<RawCapture>,
    pub explanation: String,
    pub status: AnalysisStatus,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BindingReference {
    pub binding_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EmittedBindingReference {
    pub occurrence_id: String,
    pub binding: BindingReference,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CaptureChainStep {
    pub function_id: String,
    pub slot_zero_based: usize,
    pub kind: CaptureKind,
    pub source_index_zero_based: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_function_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_binding: Option<BindingReference>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FunctionUpvalueAnalysis {
    pub function_id: String,
    pub proto_id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_function_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_site_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prototype_closure_site_id: Option<String>,
    pub lexical_depth: usize,
    pub child_function_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_name: Option<String>,
    pub bytecode_source_line: usize,
    pub upvalues: Vec<UpvalueSlotAnalysis>,
    pub emitted: bool,
    pub occurrences: Vec<EmittedOccurrence>,
    pub status: AnalysisStatus,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DecompiledPosition {
    pub byte_offset: usize,
    pub line_one_based: usize,
    pub column_one_based: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DecompiledSpan {
    pub start: DecompiledPosition,
    pub end: DecompiledPosition,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EmittedOccurrence {
    pub occurrence_id: String,
    pub syntax_kind: EmittedSyntaxKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub human_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_occurrence_id: Option<String>,
    pub span: DecompiledSpan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmittedSyntaxKind {
    ScriptRoot,
    Anonymous,
    AssignedClosure,
    LocalFunction,
    NamedFunction,
    MethodFunction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScriptUpvalueAnalysis {
    pub schema_version: u32,
    pub bytecode_version: u8,
    pub main_proto_id: usize,
    pub status: AnalysisStatus,
    pub prototypes: Vec<PrototypeAnalysis>,
    pub functions: Vec<FunctionUpvalueAnalysis>,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

pub(crate) fn reconcile_bindings(
    raw: RawUpvalueAnalysis,
    linked_bindings: &BTreeMap<String, Vec<Vec<ast::RcLocal>>>,
    source: &str,
    source_occurrences: &[ast::formatter::ClosureSourceOccurrence],
) -> ScriptUpvalueAnalysis {
    let occurrence_proto = raw
        .functions
        .iter()
        .map(|function| (function.function_id.as_str(), function.proto_id))
        .collect::<std::collections::HashMap<_, _>>();
    let functions_by_id = raw
        .functions
        .iter()
        .map(|function| (function.function_id.as_str(), function))
        .collect::<BTreeMap<_, _>>();
    let mut occurrence_seeds: BTreeMap<&str, Vec<&ast::formatter::ClosureSourceOccurrence>> =
        BTreeMap::new();
    for occurrence in source_occurrences {
        occurrence_seeds
            .entry(&occurrence.function_id)
            .or_default()
            .push(occurrence);
    }
    for seeds in occurrence_seeds.values_mut() {
        seeds.sort_by_key(|occurrence| occurrence.span.start.byte_offset);
    }
    let source_end = end_position(source);
    let mut occurrences_by_function: BTreeMap<String, Vec<EmittedOccurrence>> = BTreeMap::new();
    let mut emitted_bindings_by_function: BTreeMap<String, Vec<Vec<ast::RcLocal>>> =
        BTreeMap::new();

    for function in &raw.functions {
        let seeds = occurrence_seeds
            .remove(function.function_id.as_str())
            .unwrap_or_default();
        emitted_bindings_by_function.insert(
            function.function_id.clone(),
            seeds
                .iter()
                .map(|seed| seed.upvalue_bindings.clone())
                .collect(),
        );
        let mut occurrences = if function.parent_function_id.is_none() {
            vec![EmittedOccurrence {
                occurrence_id: format!("{}#o1", function.function_id),
                syntax_kind: EmittedSyntaxKind::ScriptRoot,
                display_name: None,
                human_path: "<script>".to_string(),
                parent_occurrence_id: None,
                span: DecompiledSpan {
                    start: DecompiledPosition {
                        byte_offset: 0,
                        line_one_based: 1,
                        column_one_based: 1,
                    },
                    end: source_end,
                },
            }]
        } else {
            seeds
                .into_iter()
                .enumerate()
                .map(|(index, seed)| {
                    let span = DecompiledSpan {
                        start: convert_position(seed.span.start),
                        end: convert_position(seed.span.end),
                    };
                    let display_name = seed.display_name.clone();
                    EmittedOccurrence {
                        occurrence_id: format!("{}#o{}", function.function_id, index + 1),
                        syntax_kind: map_syntax_kind(seed.syntax_kind),
                        display_name,
                        human_path: String::new(),
                        parent_occurrence_id: None,
                        span,
                    }
                })
                .collect()
        };
        occurrences.sort_by_key(|occurrence| occurrence.span.start.byte_offset);
        occurrences_by_function.insert(function.function_id.clone(), occurrences);
    }

    assign_occurrence_paths(&mut occurrences_by_function);

    let functions: Vec<FunctionUpvalueAnalysis> = raw
        .functions
        .iter()
        .map(|function| {
            let prototype = &raw.prototypes[function.proto_id];
            let closure_site = closure_site_for_function(&raw, &occurrence_proto, function);
            let linked_sets = linked_bindings.get(&function.function_id);
            let linked = linked_sets.and_then(|sets| sets.first());
            let emitted = emitted_bindings_by_function.get(&function.function_id);
            let occurrences = occurrences_by_function
                .remove(&function.function_id)
                .unwrap_or_default();
            let mut access_counts = vec![(0usize, 0usize); prototype.upvalue_count];
            for access in &prototype.accesses {
                if let Some((reads, writes)) = access_counts.get_mut(access.slot_zero_based) {
                    match access.kind {
                        UpvalueAccessKind::Read => *reads += 1,
                        UpvalueAccessKind::Write => *writes += 1,
                    }
                }
            }
            let mut function_diagnostics = prototype.diagnostics.clone();
            for child_site in &prototype.closure_sites {
                for diagnostic in &child_site.diagnostics {
                    if !function_diagnostics.contains(diagnostic) {
                        function_diagnostics.push(diagnostic.clone());
                    }
                }
            }
            if let Some(site) = closure_site {
                for diagnostic in &site.diagnostics {
                    if !function_diagnostics.contains(diagnostic) {
                        function_diagnostics.push(diagnostic.clone());
                    }
                }
            }
            if linked_sets.is_some_and(|sets| !binding_sets_equivalent(sets)) {
                function_diagnostics.push(AnalysisDiagnostic {
                    code: "inconsistent_linked_bindings",
                    message: "multiple AST instances of this static closure resolved to different source bindings"
                        .to_string(),
                    proto_id: Some(function.proto_id),
                    pc: closure_site.map(|site| site.constructor_pc_zero_based),
                });
            }
            if linked_sets.is_some_and(|sets| {
                sets.iter()
                    .any(|bindings| bindings.len() != prototype.upvalue_count)
            }) {
                function_diagnostics.push(AnalysisDiagnostic {
                    code: "linked_binding_count_mismatch",
                    message: format!(
                        "one or more linked AST instances have a binding count different from the prototype's {} upvalue slots",
                        prototype.upvalue_count
                    ),
                    proto_id: Some(function.proto_id),
                    pc: closure_site.map(|site| site.constructor_pc_zero_based),
                });
            }
            if function.parent_function_id.is_some()
                && emitted.is_some_and(|bindings| bindings.len() != occurrences.len())
            {
                function_diagnostics.push(AnalysisDiagnostic {
                    code: "emitted_binding_occurrence_mismatch",
                    message: format!(
                        "recorded {} emitted binding sets for {} source occurrences",
                        emitted.map_or(0, Vec::len),
                        occurrences.len()
                    ),
                    proto_id: Some(function.proto_id),
                    pc: closure_site.map(|site| site.constructor_pc_zero_based),
                });
            }
            if emitted.is_some_and(|sets| {
                sets.iter()
                    .any(|bindings| bindings.len() != prototype.upvalue_count)
            }) {
                function_diagnostics.push(AnalysisDiagnostic {
                    code: "emitted_binding_count_mismatch",
                    message: format!(
                        "one or more emitted occurrences have a binding count different from the prototype's {} upvalue slots",
                        prototype.upvalue_count
                    ),
                    proto_id: Some(function.proto_id),
                    pc: closure_site.map(|site| site.constructor_pc_zero_based),
                });
            }
            let upvalues = (0..prototype.upvalue_count)
                .map(|slot| {
                    let capture = closure_site.and_then(|site| site.captures.get(slot)).cloned();
                    let capture_source_binding = linked
                        .and_then(|locals| locals.get(slot))
                        .map(binding_reference);
                    let capture_source_semantic_id = capture.as_ref().map(|capture| {
                        capture_source_semantic_id(function, capture)
                    });
                    let emitted_bindings = emitted
                        .into_iter()
                        .flat_map(|sets| sets.iter().enumerate())
                        .filter_map(|(index, locals)| {
                            let occurrence = occurrences.get(index)?;
                            let local = locals.get(slot)?;
                            Some(EmittedBindingReference {
                                occurrence_id: occurrence.occurrence_id.clone(),
                                binding: binding_reference(local),
                            })
                        })
                        .collect::<Vec<_>>();
                    let debug_name = prototype
                        .debug_upvalue_names
                        .get(slot)
                        .and_then(Clone::clone);
                    let (name, name_varies_by_occurrence) = resolve_slot_name(
                        debug_name.as_ref(),
                        capture.as_ref(),
                        capture_source_binding.as_ref(),
                        &emitted_bindings,
                        slot,
                    );
                    let capture_chain = resolve_capture_chain(
                        &raw,
                        &occurrence_proto,
                        &functions_by_id,
                        linked_bindings,
                        function,
                        slot,
                    );
                    let (read_count, write_count) = access_counts[slot];
                    let explanation = capture
                        .as_ref()
                        .map(|capture| {
                            capture_explanation(capture, &name.text, &capture_chain)
                        })
                        .unwrap_or_else(|| {
                            format!(
                                "upvalue ordinal {} (bytecode slot {slot}) maps to `{}`",
                                slot + 1,
                                name.text
                            )
                        });
                    let mut diagnostics = Vec::new();
                    if let (Some(site), Some(capture)) = (closure_site, capture.as_ref()) {
                        diagnostics.extend(
                            site.diagnostics
                                .iter()
                                .filter(|diagnostic| {
                                    diagnostic.pc == Some(capture.capture_pc_zero_based)
                                })
                                .cloned(),
                        );
                    }
                    if function.parent_function_id.is_some() && capture.is_none() {
                        diagnostics.push(AnalysisDiagnostic {
                            code: "missing_capture_for_slot",
                            message: format!(
                                "no valid capture maps child upvalue slot {slot} at this closure site"
                            ),
                            proto_id: Some(function.proto_id),
                            pc: closure_site.map(|site| site.constructor_pc_zero_based),
                        });
                    }
                    if emitted.is_some_and(|sets| {
                        sets.iter().any(|bindings| bindings.get(slot).is_none())
                    }) {
                        diagnostics.push(AnalysisDiagnostic {
                            code: "missing_emitted_binding_for_slot",
                            message: format!(
                                "at least one emitted occurrence has no final binding for upvalue slot {slot}"
                            ),
                            proto_id: Some(function.proto_id),
                            pc: closure_site.map(|site| site.constructor_pc_zero_based),
                        });
                    }
                    let status = if diagnostics.is_empty() {
                        AnalysisStatus::Complete
                    } else {
                        AnalysisStatus::Partial
                    };
                    UpvalueSlotAnalysis {
                        slot_binding_id: format!("{}:u{slot}", function.function_id),
                        slot_zero_based: slot,
                        ordinal_one_based: slot + 1,
                        name,
                        name_varies_by_occurrence,
                        debug_name,
                        capture_source_binding,
                        capture_source_semantic_id,
                        emitted_bindings,
                        capture_chain,
                        read_count,
                        write_count,
                        capture,
                        explanation,
                        status,
                        diagnostics,
                    }
                })
                .collect::<Vec<_>>();
            for slot in &upvalues {
                for diagnostic in &slot.diagnostics {
                    if !function_diagnostics.contains(diagnostic) {
                        function_diagnostics.push(diagnostic.clone());
                    }
                }
            }
            let status = if prototype.status == AnalysisStatus::Partial
                || closure_site.is_some_and(|site| site.status == AnalysisStatus::Partial)
                || !function_diagnostics.is_empty()
            {
                AnalysisStatus::Partial
            } else {
                AnalysisStatus::Complete
            };

            FunctionUpvalueAnalysis {
                function_id: function.function_id.clone(),
                proto_id: function.proto_id,
                parent_function_id: function.parent_function_id.clone(),
                closure_site_id: function.closure_site_id.clone(),
                prototype_closure_site_id: function.prototype_closure_site_id.clone(),
                lexical_depth: function.lexical_depth,
                child_function_ids: function.child_function_ids.clone(),
                debug_name: prototype.debug_name.clone(),
                bytecode_source_line: prototype.line_defined,
                upvalues,
                emitted: !occurrences.is_empty(),
                occurrences,
                status,
                diagnostics: function_diagnostics,
            }
        })
        .collect();

    let status = if raw.status == AnalysisStatus::Partial
        || functions
            .iter()
            .any(|function| function.status == AnalysisStatus::Partial)
    {
        AnalysisStatus::Partial
    } else {
        AnalysisStatus::Complete
    };

    ScriptUpvalueAnalysis {
        schema_version: 2,
        bytecode_version: raw.bytecode_version,
        main_proto_id: raw.main_proto_id,
        status,
        prototypes: raw.prototypes,
        functions,
        diagnostics: raw.diagnostics,
    }
}

fn convert_position(position: ast::formatter::SourcePosition) -> DecompiledPosition {
    DecompiledPosition {
        byte_offset: position.byte_offset,
        line_one_based: position.line_one_based,
        column_one_based: position.column_one_based,
    }
}

fn end_position(source: &str) -> DecompiledPosition {
    let mut line = 1;
    let mut column = 1;
    for character in source.chars() {
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    DecompiledPosition {
        byte_offset: source.len(),
        line_one_based: line,
        column_one_based: column,
    }
}

fn resolve_slot_name(
    debug_name: Option<&String>,
    capture: Option<&RawCapture>,
    capture_source_binding: Option<&BindingReference>,
    emitted_bindings: &[EmittedBindingReference],
    slot: usize,
) -> (ResolvedName, bool) {
    let emitted_names = emitted_bindings
        .iter()
        .filter_map(|binding| binding.binding.display_name.as_ref())
        .collect::<std::collections::BTreeSet<_>>();
    if emitted_names.len() == 1 {
        return (
            ResolvedName {
                text: (*emitted_names.iter().next().unwrap()).clone(),
                provenance: NameProvenance::DecompilerBinding,
                confidence: NameConfidence::Exact,
            },
            false,
        );
    }
    if emitted_names.len() > 1 {
        let text = capture_source_binding
            .and_then(|binding| binding.display_name.clone())
            .unwrap_or_else(|| (*emitted_names.iter().next().unwrap()).clone());
        return (
            ResolvedName {
                text,
                provenance: NameProvenance::DecompilerBinding,
                confidence: NameConfidence::Medium,
            },
            true,
        );
    }
    if let Some(name) = capture_source_binding.and_then(|binding| binding.display_name.as_ref()) {
        return (
            ResolvedName {
                text: name.clone(),
                provenance: NameProvenance::DecompilerBinding,
                confidence: NameConfidence::High,
            },
            false,
        );
    }
    if let Some(name) = debug_name {
        return (
            ResolvedName {
                text: name.clone(),
                provenance: NameProvenance::DebugUpvalue,
                confidence: NameConfidence::High,
            },
            false,
        );
    }
    if let Some(name) = capture.and_then(|capture| capture.source_debug_name.as_ref()) {
        return (
            ResolvedName {
                text: name.clone(),
                provenance: NameProvenance::DebugCaptureSource,
                confidence: NameConfidence::High,
            },
            false,
        );
    }
    (
        ResolvedName {
            text: format!("upvalue_{}", slot + 1),
            provenance: NameProvenance::Fallback,
            confidence: NameConfidence::Low,
        },
        false,
    )
}

fn capture_explanation(
    capture: &RawCapture,
    name: &str,
    capture_chain: &[CaptureChainStep],
) -> String {
    let source = capture
        .source_debug_name
        .as_deref()
        .map(|source| format!("`{source}`"))
        .unwrap_or_else(|| match capture.kind {
            CaptureKind::ParentUpvalue => {
                format!("parent upvalue slot {}", capture.source_index_zero_based)
            }
            _ => format!("parent register {}", capture.source_index_zero_based),
        });
    let semantics = match capture.kind {
        CaptureKind::Value => "captures a value snapshot from",
        CaptureKind::Reference => "shares a mutable reference with",
        CaptureKind::ParentUpvalue => "forwards",
        CaptureKind::Unknown => "has an unknown capture from",
    };
    let forwarding = (capture_chain.len() > 1)
        .then(|| format!(" through {} ordered capture steps", capture_chain.len()))
        .unwrap_or_default();
    format!(
        "upvalue ordinal {} (bytecode slot {}) maps to `{name}` and {semantics} {source}{forwarding}",
        capture.ordinal_one_based, capture.target_slot_zero_based,
    )
}

fn binding_reference(local: &ast::RcLocal) -> BindingReference {
    BindingReference {
        binding_id: format!("local:{:016x}", local.stable_id()),
        display_name: local.0 .0.lock().0.clone(),
    }
}

fn capture_source_semantic_id(function: &StaticFunctionOccurrence, capture: &RawCapture) -> String {
    let parent = function.parent_function_id.as_deref().unwrap_or("<root>");
    match capture.kind {
        CaptureKind::Value | CaptureKind::Reference => {
            let lifetime = capture
                .source_local_lifetime
                .map(|range| format!("@pc{}-{}", range.start_pc_inclusive, range.end_pc_exclusive))
                .unwrap_or_else(|| format!("@capture-pc{}", capture.capture_pc_zero_based));
            format!("{parent}:r{}{lifetime}", capture.source_index_zero_based)
        }
        CaptureKind::ParentUpvalue => {
            format!("{parent}:u{}", capture.source_index_zero_based)
        }
        CaptureKind::Unknown => format!(
            "{parent}:unknown{}@pc{}",
            capture.source_index_zero_based, capture.capture_pc_zero_based
        ),
    }
}

fn map_syntax_kind(kind: ast::formatter::ClosureSyntaxKind) -> EmittedSyntaxKind {
    match kind {
        ast::formatter::ClosureSyntaxKind::Anonymous => EmittedSyntaxKind::Anonymous,
        ast::formatter::ClosureSyntaxKind::AssignedClosure => EmittedSyntaxKind::AssignedClosure,
        ast::formatter::ClosureSyntaxKind::LocalFunction => EmittedSyntaxKind::LocalFunction,
        ast::formatter::ClosureSyntaxKind::NamedFunction => EmittedSyntaxKind::NamedFunction,
        ast::formatter::ClosureSyntaxKind::MethodFunction => EmittedSyntaxKind::MethodFunction,
    }
}

fn assign_occurrence_paths(occurrences_by_function: &mut BTreeMap<String, Vec<EmittedOccurrence>>) {
    #[derive(Clone)]
    struct Item {
        function_id: String,
        index: usize,
        occurrence_id: String,
        display_name: Option<String>,
        syntax_kind: EmittedSyntaxKind,
        span: DecompiledSpan,
    }

    let mut items = occurrences_by_function
        .iter()
        .flat_map(|(function_id, occurrences)| {
            occurrences
                .iter()
                .enumerate()
                .map(|(index, occurrence)| Item {
                    function_id: function_id.clone(),
                    index,
                    occurrence_id: occurrence.occurrence_id.clone(),
                    display_name: occurrence.display_name.clone(),
                    syntax_kind: occurrence.syntax_kind,
                    span: occurrence.span,
                })
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| {
        (
            item.span.start.byte_offset,
            std::cmp::Reverse(item.span.end.byte_offset),
            item.occurrence_id.clone(),
        )
    });

    let mut stack: Vec<(String, DecompiledSpan, String)> = Vec::new();
    for item in items {
        while stack
            .last()
            .is_some_and(|(_, parent_span, _)| !strictly_contains(parent_span, &item.span))
        {
            stack.pop();
        }
        let parent_occurrence_id = stack.last().map(|(id, _, _)| id.clone());
        let human_path = if item.syntax_kind == EmittedSyntaxKind::ScriptRoot {
            "<script>".to_string()
        } else {
            let parent_path = stack
                .last()
                .map(|(_, _, path)| path.as_str())
                .unwrap_or("<script>");
            let segment = item.display_name.as_deref().unwrap_or("<anonymous>");
            format!(
                "{parent_path}/{segment}@{}:{}",
                item.span.start.line_one_based, item.span.start.column_one_based
            )
        };
        if let Some(occurrence) = occurrences_by_function
            .get_mut(&item.function_id)
            .and_then(|occurrences| occurrences.get_mut(item.index))
        {
            occurrence.parent_occurrence_id = parent_occurrence_id;
            occurrence.human_path = human_path.clone();
        }
        stack.push((item.occurrence_id, item.span, human_path));
    }
}

fn strictly_contains(parent: &DecompiledSpan, child: &DecompiledSpan) -> bool {
    parent.start.byte_offset <= child.start.byte_offset
        && child.end.byte_offset <= parent.end.byte_offset
        && (parent.start.byte_offset < child.start.byte_offset
            || child.end.byte_offset < parent.end.byte_offset)
}

fn closure_site_for_function<'a>(
    raw: &'a RawUpvalueAnalysis,
    occurrence_proto: &std::collections::HashMap<&str, usize>,
    function: &StaticFunctionOccurrence,
) -> Option<&'a ClosureSiteAnalysis> {
    let parent_id = function.parent_function_id.as_deref()?;
    let parent_proto_id = occurrence_proto.get(parent_id).copied()?;
    let site_id = function.prototype_closure_site_id.as_deref()?;
    raw.prototypes
        .get(parent_proto_id)?
        .closure_sites
        .iter()
        .find(|site| site.site_id == site_id)
}

fn resolve_capture_chain(
    raw: &RawUpvalueAnalysis,
    occurrence_proto: &std::collections::HashMap<&str, usize>,
    functions_by_id: &BTreeMap<&str, &StaticFunctionOccurrence>,
    linked_bindings: &BTreeMap<String, Vec<Vec<ast::RcLocal>>>,
    start: &StaticFunctionOccurrence,
    start_slot: usize,
) -> Vec<CaptureChainStep> {
    let mut chain = Vec::new();
    let mut function = start;
    let mut slot = start_slot;
    let mut seen = HashSet::new();

    while seen.insert((function.function_id.as_str(), slot)) {
        let Some(site) = closure_site_for_function(raw, occurrence_proto, function) else {
            break;
        };
        let Some(capture) = site.captures.get(slot) else {
            break;
        };
        let parent_function_id = function.parent_function_id.clone();
        let source_binding = linked_bindings
            .get(&function.function_id)
            .and_then(|sets| sets.first())
            .and_then(|locals| locals.get(slot))
            .map(binding_reference);
        chain.push(CaptureChainStep {
            function_id: function.function_id.clone(),
            slot_zero_based: slot,
            kind: capture.kind,
            source_index_zero_based: capture.source_index_zero_based,
            parent_function_id: parent_function_id.clone(),
            source_binding,
        });
        if capture.kind != CaptureKind::ParentUpvalue {
            break;
        }
        let Some(parent) = parent_function_id
            .as_deref()
            .and_then(|parent_id| functions_by_id.get(parent_id).copied())
        else {
            break;
        };
        function = parent;
        slot = capture.source_index_zero_based;
    }
    chain
}

fn binding_sets_equivalent(sets: &[Vec<ast::RcLocal>]) -> bool {
    let Some(first) = sets.first() else {
        return true;
    };
    sets.iter().skip(1).all(|other| {
        first.len() == other.len()
            && first
                .iter()
                .zip(other)
                .all(|(left, right)| left.stable_id() == right.stable_id())
    })
}

impl RawUpvalueAnalysis {
    pub(crate) fn build(chunk: &Chunk) -> Self {
        let mut prototypes = Vec::with_capacity(chunk.functions.len());
        let mut diagnostics = Vec::new();

        for (proto_id, function) in chunk.functions.iter().enumerate() {
            prototypes.push(analyze_prototype(chunk, proto_id, function));
        }

        if chunk.main >= chunk.functions.len() {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_main_proto",
                message: format!(
                    "main prototype {} is outside the {}-prototype chunk",
                    chunk.main,
                    chunk.functions.len()
                ),
                proto_id: None,
                pc: None,
            });
        }

        let mut functions = Vec::new();
        if chunk.main < prototypes.len() {
            build_occurrences(&prototypes, chunk.main, &mut functions, &mut diagnostics);
        }

        let partial = !diagnostics.is_empty()
            || prototypes
                .iter()
                .any(|prototype| prototype.status == AnalysisStatus::Partial);
        Self {
            bytecode_version: chunk.version,
            main_proto_id: chunk.main,
            status: if partial {
                AnalysisStatus::Partial
            } else {
                AnalysisStatus::Complete
            },
            prototypes,
            functions,
            diagnostics,
        }
    }
}

fn analyze_prototype(chunk: &Chunk, proto_id: usize, function: &Function) -> PrototypeAnalysis {
    let source_lines = decode_source_lines(function);
    let debug_name = resolve_string(&chunk.string_table, function.function_name);
    let debug_upvalue_names = (0..function.num_upvalues as usize)
        .map(|slot| {
            function
                .debug_upvalue_name_indices
                .get(slot)
                .and_then(|&index| resolve_string(&chunk.string_table, index))
        })
        .collect();
    let mut accesses = Vec::new();
    let mut closure_sites = Vec::new();
    let mut diagnostics = Vec::new();
    let mut claimed_captures = vec![false; function.instructions.len()];

    validate_debug_metadata(chunk, proto_id, function, &mut diagnostics);

    for (pc, instruction) in function.instructions.iter().enumerate() {
        match *instruction {
            Instruction::BC {
                op_code: OpCode::LOP_GETUPVAL,
                b,
                ..
            } => record_access(
                function,
                proto_id,
                pc,
                b as usize,
                UpvalueAccessKind::Read,
                &source_lines,
                &mut accesses,
                &mut diagnostics,
            ),
            Instruction::BC {
                op_code: OpCode::LOP_SETUPVAL,
                b,
                ..
            } => record_access(
                function,
                proto_id,
                pc,
                b as usize,
                UpvalueAccessKind::Write,
                &source_lines,
                &mut accesses,
                &mut diagnostics,
            ),
            Instruction::AD {
                op_code: OpCode::LOP_NEWCLOSURE,
                d,
                ..
            } => closure_sites.push(analyze_closure_site(
                chunk,
                function,
                proto_id,
                pc,
                d,
                ClosureConstructor::NewClosure,
                &mut claimed_captures,
            )),
            Instruction::AD {
                op_code: OpCode::LOP_DUPCLOSURE,
                d,
                ..
            } => closure_sites.push(analyze_closure_site(
                chunk,
                function,
                proto_id,
                pc,
                d,
                ClosureConstructor::DupClosure,
                &mut claimed_captures,
            )),
            _ => {}
        }
    }

    for (pc, instruction) in function.instructions.iter().enumerate() {
        if matches!(
            instruction,
            Instruction::BC {
                op_code: OpCode::LOP_CAPTURE,
                ..
            }
        ) && !claimed_captures[pc]
        {
            diagnostics.push(AnalysisDiagnostic {
                code: "orphan_capture",
                message: "CAPTURE is not claimed by a preceding closure constructor".to_string(),
                proto_id: Some(proto_id),
                pc: Some(pc),
            });
        }
    }

    let partial = !diagnostics.is_empty()
        || closure_sites
            .iter()
            .any(|site| site.status == AnalysisStatus::Partial);
    PrototypeAnalysis {
        proto_id,
        line_defined: function.line_defined,
        debug_name,
        upvalue_count: function.num_upvalues as usize,
        debug_upvalue_names,
        accesses,
        closure_sites,
        status: if partial {
            AnalysisStatus::Partial
        } else {
            AnalysisStatus::Complete
        },
        diagnostics,
    }
}

#[allow(clippy::too_many_arguments)]
fn record_access(
    function: &Function,
    proto_id: usize,
    pc: usize,
    slot: usize,
    kind: UpvalueAccessKind,
    source_lines: &[Option<u32>],
    accesses: &mut Vec<UpvalueAccess>,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) {
    if slot >= function.num_upvalues as usize {
        diagnostics.push(AnalysisDiagnostic {
            code: "invalid_upvalue_slot",
            message: format!(
                "{kind:?} references upvalue slot {slot}, but the prototype has {} slots",
                function.num_upvalues
            ),
            proto_id: Some(proto_id),
            pc: Some(pc),
        });
        return;
    }

    accesses.push(UpvalueAccess {
        slot_zero_based: slot,
        pc_zero_based: pc,
        bytecode_source_line: source_lines.get(pc).copied().flatten(),
        kind,
    });
}

fn analyze_closure_site(
    chunk: &Chunk,
    parent: &Function,
    parent_proto_id: usize,
    pc: usize,
    operand: i16,
    constructor: ClosureConstructor,
    claimed_captures: &mut [bool],
) -> ClosureSiteAnalysis {
    let mut diagnostics = Vec::new();
    let child_proto_id = match resolve_child_proto(parent, operand, constructor) {
        Some(child_proto_id) if child_proto_id < chunk.functions.len() => Some(child_proto_id),
        _ => {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_child_proto",
                message: format!(
                    "{constructor:?} operand {operand} does not resolve to a valid child prototype"
                ),
                proto_id: Some(parent_proto_id),
                pc: Some(pc),
            });
            None
        }
    };
    let expected = chunk
        .functions
        .get(child_proto_id.unwrap_or(usize::MAX))
        .map_or(0, |child| child.num_upvalues as usize);
    let mut captures = Vec::with_capacity(expected);

    for target_slot in 0..expected {
        let capture_pc = pc + 1 + target_slot;
        let Some(instruction) = parent.instructions.get(capture_pc) else {
            diagnostics.push(AnalysisDiagnostic {
                code: "missing_capture",
                message: format!(
                    "closure for proto {:?} expects {expected} captures, but slot {target_slot} is missing",
                    child_proto_id
                ),
                proto_id: Some(parent_proto_id),
                pc: Some(pc),
            });
            break;
        };
        let Instruction::BC {
            op_code: OpCode::LOP_CAPTURE,
            a,
            b,
            ..
        } = *instruction
        else {
            diagnostics.push(AnalysisDiagnostic {
                code: "missing_capture",
                message: format!(
                    "closure for proto {:?} expected CAPTURE for slot {target_slot}, found {instruction:?}",
                    child_proto_id
                ),
                proto_id: Some(parent_proto_id),
                pc: Some(capture_pc),
            });
            break;
        };
        claimed_captures[capture_pc] = true;

        let kind = match a {
            0 => CaptureKind::Value,
            1 => CaptureKind::Reference,
            2 => CaptureKind::ParentUpvalue,
            _ => {
                diagnostics.push(AnalysisDiagnostic {
                    code: "unknown_capture_kind",
                    message: format!("CAPTURE kind {a} is not supported"),
                    proto_id: Some(parent_proto_id),
                    pc: Some(capture_pc),
                });
                CaptureKind::Unknown
            }
        };
        if constructor == ClosureConstructor::DupClosure && kind == CaptureKind::Reference {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_dupclosure_ref_capture",
                message: "DUPCLOSURE cannot use a REF capture; only VAL and UPVAL are valid"
                    .to_string(),
                proto_id: Some(parent_proto_id),
                pc: Some(capture_pc),
            });
        }
        let source = b as usize;
        match kind {
            CaptureKind::Value | CaptureKind::Reference
                if source >= parent.max_stack_size as usize =>
            {
                diagnostics.push(AnalysisDiagnostic {
                    code: "invalid_capture_register",
                    message: format!(
                        "CAPTURE {kind:?} uses register {source}, but max stack size is {}",
                        parent.max_stack_size
                    ),
                    proto_id: Some(parent_proto_id),
                    pc: Some(capture_pc),
                });
            }
            CaptureKind::ParentUpvalue if source >= parent.num_upvalues as usize => {
                diagnostics.push(AnalysisDiagnostic {
                    code: "invalid_parent_upvalue_slot",
                    message: format!(
                        "CAPTURE UPVAL forwards parent slot {source}, but the parent has {} slots",
                        parent.num_upvalues
                    ),
                    proto_id: Some(parent_proto_id),
                    pc: Some(capture_pc),
                });
            }
            _ => {}
        }

        let (source_debug_name, source_local_lifetime) =
            capture_source_debug_info(chunk, parent, pc, kind, source);
        captures.push(RawCapture {
            target_slot_zero_based: target_slot,
            ordinal_one_based: target_slot + 1,
            capture_pc_zero_based: capture_pc,
            kind,
            source_index_zero_based: source,
            source_debug_name,
            source_local_lifetime,
        });
    }

    let mut extra_pc = pc + 1 + expected;
    while matches!(
        parent.instructions.get(extra_pc),
        Some(Instruction::BC {
            op_code: OpCode::LOP_CAPTURE,
            ..
        })
    ) {
        claimed_captures[extra_pc] = true;
        diagnostics.push(AnalysisDiagnostic {
            code: "extra_capture",
            message: format!(
                "closure for proto {:?} has an unexpected CAPTURE after its {expected} slots",
                child_proto_id
            ),
            proto_id: Some(parent_proto_id),
            pc: Some(extra_pc),
        });
        extra_pc += 1;
    }

    ClosureSiteAnalysis {
        site_id: format!("p{parent_proto_id}@pc{pc}"),
        parent_proto_id,
        child_proto_id,
        constructor_pc_zero_based: pc,
        constructor,
        captures,
        status: if diagnostics.is_empty() {
            AnalysisStatus::Complete
        } else {
            AnalysisStatus::Partial
        },
        diagnostics,
    }
}

fn validate_debug_metadata(
    chunk: &Chunk,
    proto_id: usize,
    function: &Function,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) {
    if function.has_debug_info
        && function.debug_upvalue_name_indices.len() != function.num_upvalues as usize
    {
        diagnostics.push(AnalysisDiagnostic {
            code: "debug_upvalue_count_mismatch",
            message: format!(
                "debug metadata has {} upvalue names, but the prototype has {} slots",
                function.debug_upvalue_name_indices.len(),
                function.num_upvalues
            ),
            proto_id: Some(proto_id),
            pc: None,
        });
    }

    let valid_name_index = |index: usize| index == 0 || index <= chunk.string_table.len();
    if !valid_name_index(function.function_name) {
        diagnostics.push(AnalysisDiagnostic {
            code: "invalid_debug_function_name",
            message: format!(
                "function name index {} is outside the {}-entry string table",
                function.function_name,
                chunk.string_table.len()
            ),
            proto_id: Some(proto_id),
            pc: None,
        });
    }

    for (slot, &name_index) in function.debug_upvalue_name_indices.iter().enumerate() {
        if !valid_name_index(name_index) {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_debug_upvalue_name",
                message: format!(
                    "debug name index {name_index} for upvalue slot {slot} is outside the {}-entry string table",
                    chunk.string_table.len()
                ),
                proto_id: Some(proto_id),
                pc: None,
            });
        }
    }

    for local in &function.debug_locals {
        if !valid_name_index(local.name_index) {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_debug_local_name",
                message: format!(
                    "debug local name index {} is outside the {}-entry string table",
                    local.name_index,
                    chunk.string_table.len()
                ),
                proto_id: Some(proto_id),
                pc: Some(local.start_pc),
            });
        }
        if local.register >= function.max_stack_size {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_debug_local_register",
                message: format!(
                    "debug local uses register {}, but max stack size is {}",
                    local.register, function.max_stack_size
                ),
                proto_id: Some(proto_id),
                pc: Some(local.start_pc),
            });
        }
        if local.start_pc > local.end_pc
            || local.end_pc > function.instructions.len()
            || (local.start_pc == local.end_pc && local.start_pc >= function.instructions.len())
        {
            diagnostics.push(AnalysisDiagnostic {
                code: "invalid_debug_local_lifetime",
                message: format!(
                    "debug local lifetime {}..{} is outside the {}-word prototype",
                    local.start_pc,
                    local.end_pc,
                    function.instructions.len()
                ),
                proto_id: Some(proto_id),
                pc: Some(local.start_pc),
            });
        }
    }
}

fn capture_source_debug_info(
    chunk: &Chunk,
    parent: &Function,
    constructor_pc: usize,
    kind: CaptureKind,
    source: usize,
) -> (Option<String>, Option<PcRange>) {
    match kind {
        CaptureKind::Value | CaptureKind::Reference => {
            let local = parent
                .debug_locals
                .iter()
                .filter(|local| {
                    local.register as usize == source
                        && local.start_pc <= constructor_pc
                        && constructor_pc < local.end_pc
                })
                .max_by_key(|local| local.start_pc);
            match local {
                Some(local) => (
                    resolve_string(&chunk.string_table, local.name_index),
                    Some(PcRange {
                        start_pc_inclusive: local.start_pc,
                        end_pc_exclusive: local.end_pc,
                    }),
                ),
                None => (None, None),
            }
        }
        CaptureKind::ParentUpvalue => (
            parent
                .debug_upvalue_name_indices
                .get(source)
                .and_then(|&index| resolve_string(&chunk.string_table, index)),
            None,
        ),
        CaptureKind::Unknown => (None, None),
    }
}

fn build_occurrences(
    prototypes: &[PrototypeAnalysis],
    main_proto_id: usize,
    out: &mut Vec<StaticFunctionOccurrence>,
    diagnostics: &mut Vec<AnalysisDiagnostic>,
) {
    enum Work {
        Enter {
            proto_id: usize,
            function_id: String,
            parent_function_id: Option<String>,
            closure_site_id: Option<String>,
            prototype_closure_site_id: Option<String>,
            lexical_depth: usize,
        },
        Exit(usize),
    }

    let mut ancestry = HashSet::new();
    let mut stack = vec![Work::Enter {
        proto_id: main_proto_id,
        function_id: format!("root:p{main_proto_id}"),
        parent_function_id: None,
        closure_site_id: None,
        prototype_closure_site_id: None,
        lexical_depth: 0,
    }];

    while let Some(work) = stack.pop() {
        match work {
            Work::Exit(proto_id) => {
                ancestry.remove(&proto_id);
            }
            Work::Enter {
                proto_id,
                function_id,
                parent_function_id,
                closure_site_id,
                prototype_closure_site_id,
                lexical_depth,
            } => {
                let cyclic = !ancestry.insert(proto_id);
                let children = prototypes
                    .get(proto_id)
                    .map(|prototype| {
                        prototype
                            .closure_sites
                            .iter()
                            .filter_map(|site| {
                                let child_proto_id = site.child_proto_id?;
                                (child_proto_id < prototypes.len()).then(|| {
                                    let closure_site_id = format!("{function_id}/{}", site.site_id);
                                    let child_function_id =
                                        format!("{closure_site_id}:p{child_proto_id}");
                                    (site, child_proto_id, closure_site_id, child_function_id)
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                out.push(StaticFunctionOccurrence {
                    function_id: function_id.clone(),
                    proto_id,
                    parent_function_id,
                    closure_site_id,
                    prototype_closure_site_id,
                    lexical_depth,
                    child_function_ids: children
                        .iter()
                        .map(|(_, _, _, child_function_id)| child_function_id.clone())
                        .collect(),
                });

                if cyclic {
                    diagnostics.push(AnalysisDiagnostic {
                        code: "cyclic_proto_graph",
                        message: format!("prototype {proto_id} recursively reaches itself"),
                        proto_id: Some(proto_id),
                        pc: None,
                    });
                    continue;
                }

                stack.push(Work::Exit(proto_id));
                for (site, child_proto_id, closure_site_id, child_function_id) in
                    children.into_iter().rev()
                {
                    stack.push(Work::Enter {
                        proto_id: child_proto_id,
                        function_id: child_function_id,
                        parent_function_id: Some(function_id.clone()),
                        closure_site_id: Some(closure_site_id),
                        prototype_closure_site_id: Some(site.site_id.clone()),
                        lexical_depth: lexical_depth + 1,
                    });
                }
            }
        }
    }
}

fn resolve_child_proto(
    parent: &Function,
    operand: i16,
    constructor: ClosureConstructor,
) -> Option<usize> {
    let index = usize::try_from(operand).ok()?;
    match constructor {
        ClosureConstructor::NewClosure => parent.functions.get(index).copied(),
        ClosureConstructor::DupClosure => match parent.constants.get(index)? {
            Constant::Closure(proto_id) => Some(*proto_id),
            _ => None,
        },
    }
}

fn resolve_string(string_table: &[Vec<u8>], index: usize) -> Option<String> {
    let bytes = string_table.get(index.checked_sub(1)?)?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn decode_source_lines(function: &Function) -> Vec<Option<u32>> {
    let Some(gap_log2) = function.line_gap_log2 else {
        return vec![None; function.instructions.len()];
    };
    let (Some(line_deltas), Some(abs_deltas)) = (
        function.line_info_delta.as_ref(),
        function.abs_line_info_delta.as_ref(),
    ) else {
        return vec![None; function.instructions.len()];
    };

    let mut offsets = Vec::with_capacity(line_deltas.len());
    let mut last_offset = 0u8;
    for &delta in line_deltas {
        last_offset = last_offset.wrapping_add(delta);
        offsets.push(last_offset);
    }

    let mut absolute_lines = Vec::with_capacity(abs_deltas.len());
    let mut last_line = 0i32;
    for &delta in abs_deltas {
        last_line = last_line.wrapping_add(delta as i32);
        absolute_lines.push(last_line);
    }

    (0..function.instructions.len())
        .map(|pc| {
            let interval = pc >> gap_log2;
            let line = absolute_lines
                .get(interval)?
                .checked_add(i32::from(*offsets.get(pc)?))?;
            u32::try_from(line).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deserializer::function::DebugLocal;

    fn function(num_upvalues: u8, max_stack_size: u8, instructions: Vec<Instruction>) -> Function {
        Function {
            max_stack_size,
            num_parameters: 0,
            num_upvalues,
            is_vararg: false,
            instructions,
            constants: Vec::new(),
            functions: Vec::new(),
            line_defined: 0,
            function_name: 0,
            line_gap_log2: None,
            line_info_delta: None,
            abs_line_info_delta: None,
            has_debug_info: false,
            debug_locals: Vec::<DebugLocal>::new(),
            debug_upvalue_name_indices: Vec::new(),
        }
    }

    #[test]
    fn records_ordered_capture_kinds_and_child_accesses() {
        let mut root = function(
            1,
            4,
            vec![
                Instruction::AD {
                    op_code: OpCode::LOP_NEWCLOSURE,
                    a: 0,
                    d: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    a: 0,
                    b: 2,
                    c: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    a: 2,
                    b: 0,
                    c: 0,
                    aux: 0,
                },
            ],
        );
        root.functions.push(1);
        let child = function(
            2,
            1,
            vec![
                Instruction::BC {
                    op_code: OpCode::LOP_GETUPVAL,
                    a: 0,
                    b: 0,
                    c: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_SETUPVAL,
                    a: 0,
                    b: 1,
                    c: 0,
                    aux: 0,
                },
            ],
        );
        let chunk = Chunk {
            version: 9,
            string_table: Vec::new(),
            functions: vec![root, child],
            main: 0,
        };

        let analysis = RawUpvalueAnalysis::build(&chunk);
        assert_eq!(analysis.status, AnalysisStatus::Complete);
        let site = &analysis.prototypes[0].closure_sites[0];
        assert_eq!(site.child_proto_id, Some(1));
        assert_eq!(site.captures.len(), 2);
        assert_eq!(site.captures[0].kind, CaptureKind::Value);
        assert_eq!(site.captures[0].source_index_zero_based, 2);
        assert_eq!(site.captures[1].kind, CaptureKind::ParentUpvalue);
        assert_eq!(site.captures[1].source_index_zero_based, 0);
        assert_eq!(analysis.prototypes[1].accesses.len(), 2);
        assert_eq!(
            analysis.prototypes[1].accesses[0].kind,
            UpvalueAccessKind::Read
        );
        assert_eq!(
            analysis.prototypes[1].accesses[1].kind,
            UpvalueAccessKind::Write
        );
        assert_eq!(analysis.functions.len(), 2);
        assert_eq!(
            analysis.functions[1].parent_function_id.as_deref(),
            Some("root:p0")
        );
    }

    #[test]
    fn invalid_capture_chain_is_partial_not_a_panic() {
        let mut root = function(
            0,
            1,
            vec![Instruction::AD {
                op_code: OpCode::LOP_NEWCLOSURE,
                a: 0,
                d: 0,
                aux: 0,
            }],
        );
        root.functions.push(1);
        let child = function(1, 1, Vec::new());
        let chunk = Chunk {
            version: 9,
            string_table: Vec::new(),
            functions: vec![root, child],
            main: 0,
        };

        let analysis = RawUpvalueAnalysis::build(&chunk);
        assert_eq!(analysis.status, AnalysisStatus::Partial);
        assert_eq!(
            analysis.prototypes[0].closure_sites[0].status,
            AnalysisStatus::Partial
        );
        assert_eq!(
            analysis.prototypes[0].closure_sites[0].diagnostics[0].code,
            "missing_capture"
        );

        let reconciled = reconcile_bindings(analysis, &BTreeMap::new(), "", &[]);
        let root = &reconciled.functions[0];
        assert_eq!(root.status, AnalysisStatus::Partial);
        assert!(root
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "missing_capture"));
    }

    #[test]
    fn dupclosure_ref_capture_is_partial() {
        let mut root = function(
            0,
            1,
            vec![
                Instruction::AD {
                    op_code: OpCode::LOP_DUPCLOSURE,
                    a: 0,
                    d: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    a: 1,
                    b: 0,
                    c: 0,
                    aux: 0,
                },
            ],
        );
        root.constants.push(Constant::Closure(1));
        let child = function(
            1,
            1,
            vec![Instruction::BC {
                op_code: OpCode::LOP_RETURN,
                a: 0,
                b: 1,
                c: 0,
                aux: 0,
            }],
        );
        let chunk = Chunk {
            version: 9,
            string_table: Vec::new(),
            functions: vec![root, child],
            main: 0,
        };

        let raw = RawUpvalueAnalysis::build(&chunk);
        let site = &raw.prototypes[0].closure_sites[0];
        assert_eq!(site.status, AnalysisStatus::Partial);
        assert_eq!(site.diagnostics[0].code, "invalid_dupclosure_ref_capture");

        let reconciled = reconcile_bindings(raw, &BTreeMap::new(), "", &[]);
        let slot = &reconciled.functions[1].upvalues[0];
        assert_eq!(slot.status, AnalysisStatus::Partial);
        assert!(slot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "invalid_dupclosure_ref_capture"));
    }

    #[test]
    fn reconciles_multiple_emitted_occurrences_with_distinct_bindings() {
        let mut root = function(
            0,
            1,
            vec![
                Instruction::AD {
                    op_code: OpCode::LOP_NEWCLOSURE,
                    a: 0,
                    d: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    a: 1,
                    b: 0,
                    c: 0,
                    aux: 0,
                },
            ],
        );
        root.functions.push(1);
        let child = function(1, 1, Vec::new());
        let chunk = Chunk {
            version: 9,
            string_table: Vec::new(),
            functions: vec![root, child],
            main: 0,
        };
        let raw = RawUpvalueAnalysis::build(&chunk);
        let function_id = raw.functions[1].function_id.clone();
        let first = ast::RcLocal::new(ast::Local::new(Some("first_name".to_string())));
        let second = ast::RcLocal::new(ast::Local::new(Some("second_name".to_string())));
        let position = |byte_offset, line_one_based| ast::formatter::SourcePosition {
            byte_offset,
            line_one_based,
            column_one_based: 1,
        };
        let occurrences = vec![
            ast::formatter::ClosureSourceOccurrence {
                function_id: function_id.clone(),
                syntax_kind: ast::formatter::ClosureSyntaxKind::Anonymous,
                display_name: None,
                upvalue_bindings: vec![first],
                span: ast::formatter::SourceSpan {
                    start: position(0, 1),
                    end: position(14, 1),
                },
            },
            ast::formatter::ClosureSourceOccurrence {
                function_id,
                syntax_kind: ast::formatter::ClosureSyntaxKind::Anonymous,
                display_name: None,
                upvalue_bindings: vec![second],
                span: ast::formatter::SourceSpan {
                    start: position(15, 2),
                    end: position(29, 2),
                },
            },
        ];

        let reconciled = reconcile_bindings(
            raw,
            &BTreeMap::new(),
            "function() end\nfunction() end",
            &occurrences,
        );
        let child = &reconciled.functions[1];
        assert_eq!(child.occurrences.len(), 2);
        assert_eq!(child.upvalues[0].emitted_bindings.len(), 2);
        assert!(child.upvalues[0].name_varies_by_occurrence);
        let names = child.upvalues[0]
            .emitted_bindings
            .iter()
            .filter_map(|binding| binding.binding.display_name.as_deref())
            .collect::<HashSet<_>>();
        assert_eq!(names, HashSet::from(["first_name", "second_name"]));
    }

    #[test]
    fn overflowing_source_line_metadata_is_ignored() {
        let mut proto = function(
            0,
            1,
            vec![Instruction::BC {
                op_code: OpCode::LOP_RETURN,
                a: 0,
                b: 1,
                c: 0,
                aux: 0,
            }],
        );
        proto.line_gap_log2 = Some(0);
        proto.line_info_delta = Some(vec![u8::MAX]);
        proto.abs_line_info_delta = Some(vec![i32::MAX as u32]);

        assert_eq!(decode_source_lines(&proto), vec![None]);
    }

    #[test]
    fn extra_capture_marks_site_partial() {
        let mut root = function(
            0,
            1,
            vec![
                Instruction::AD {
                    op_code: OpCode::LOP_NEWCLOSURE,
                    a: 0,
                    d: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    a: 0,
                    b: 0,
                    c: 0,
                    aux: 0,
                },
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    a: 0,
                    b: 0,
                    c: 0,
                    aux: 0,
                },
            ],
        );
        root.functions.push(1);
        let child = function(
            1,
            1,
            vec![Instruction::BC {
                op_code: OpCode::LOP_RETURN,
                a: 0,
                b: 1,
                c: 0,
                aux: 0,
            }],
        );
        let chunk = Chunk {
            version: 9,
            string_table: Vec::new(),
            functions: vec![root, child],
            main: 0,
        };

        let analysis = RawUpvalueAnalysis::build(&chunk);
        let site = &analysis.prototypes[0].closure_sites[0];
        assert_eq!(site.status, AnalysisStatus::Partial);
        assert!(site.diagnostics.iter().any(|d| d.code == "extra_capture"));
        assert!(!analysis.prototypes[0]
            .diagnostics
            .iter()
            .any(|d| d.code == "orphan_capture"));
    }

    #[test]
    fn invalid_debug_metadata_is_partial() {
        let mut proto = function(
            1,
            1,
            vec![Instruction::BC {
                op_code: OpCode::LOP_RETURN,
                a: 0,
                b: 1,
                c: 0,
                aux: 0,
            }],
        );
        proto.has_debug_info = true;
        proto.debug_upvalue_name_indices = Vec::new();
        proto.debug_locals.push(DebugLocal {
            name_index: 2,
            start_pc: 1,
            end_pc: 1,
            register: 1,
        });
        let chunk = Chunk {
            version: 9,
            string_table: vec![b"only_name".to_vec()],
            functions: vec![proto],
            main: 0,
        };

        let prototype = &RawUpvalueAnalysis::build(&chunk).prototypes[0];
        assert_eq!(prototype.status, AnalysisStatus::Partial);
        let codes = prototype
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<HashSet<_>>();
        assert!(codes.contains("debug_upvalue_count_mismatch"));
        assert!(codes.contains("invalid_debug_local_name"));
        assert!(codes.contains("invalid_debug_local_register"));
        assert!(codes.contains("invalid_debug_local_lifetime"));
    }

    #[test]
    fn zero_length_debug_local_inside_code_is_valid() {
        let mut proto = function(
            0,
            1,
            vec![Instruction::BC {
                op_code: OpCode::LOP_RETURN,
                a: 0,
                b: 1,
                c: 0,
                aux: 0,
            }],
        );
        proto.has_debug_info = true;
        proto.debug_locals.push(DebugLocal {
            name_index: 1,
            start_pc: 0,
            end_pc: 0,
            register: 0,
        });
        let chunk = Chunk {
            version: 9,
            string_table: vec![b"temporary".to_vec()],
            functions: vec![proto],
            main: 0,
        };

        assert_eq!(
            RawUpvalueAnalysis::build(&chunk).prototypes[0].status,
            AnalysisStatus::Complete
        );
    }
}
