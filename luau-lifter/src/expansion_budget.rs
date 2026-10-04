//! Bound work before an acyclic prototype graph is expanded into closure
//! occurrences. A DAG can have exponentially many paths even when it has only
//! a few serialized prototypes; every constructor site is a separate weight.
use std::fmt;

use crate::{
    deserializer::{chunk::Chunk, constant::Constant},
    instruction::Instruction,
    op_code::OpCode,
};

/// These are limits on materialized work, not just on the encoded input. They
/// deliberately leave room above ordinary scripts while bounding recursive
/// source trees and the work performed before later dead-code cleanup.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExpansionLimits {
    pub instances: u64,
    pub instruction_words: u64,
    /// Number of enclosing constructors; the script root has depth zero.
    pub depth: u64,
}

impl Default for ExpansionLimits {
    fn default() -> Self {
        Self { instances: 65_536, instruction_words: 8_000_000, depth: 256 }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ExpansionEstimate {
    pub instances: u64,
    pub instruction_words: u64,
    pub depth: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpansionResource { Instances, InstructionWords, Depth }

impl ExpansionResource {
    fn code(self) -> &'static str {
        match self {
            Self::Instances => "instance_budget_exceeded",
            Self::InstructionWords => "instruction_budget_exceeded",
            Self::Depth => "depth_budget_exceeded",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Instances => "function instances",
            Self::InstructionWords => "expanded instruction words",
            Self::Depth => "closure depth",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExpansionFailure {
    InvalidGraph { prototype: usize, reason: &'static str },
    Exceeded {
        prototype: usize,
        resource: ExpansionResource,
        limit: u64,
        /// Saturated at limit + 1: exact enormous expansion sizes are neither
        /// needed for admission nor representable in a fixed-width integer.
        required_at_least: u64,
    },
}

impl ExpansionFailure {
    pub(crate) fn into_decompile_failure(self) -> crate::DecompileFailure {
        let message = self.to_string();
        let (prototype, code) = match self {
            Self::InvalidGraph { prototype, .. } => (prototype, "malformed_prototype_graph"),
            Self::Exceeded { prototype, resource, .. } => (prototype, resource.code()),
        };
        crate::DecompileFailure {
            message: message.clone(),
            diagnostics: vec![crate::DecompileDiagnostic {
                stage: "prototype_expansion".into(),
                code: code.into(),
                function: format!("prototype:p{prototype}"),
                message,
            }],
        }
    }
}

impl fmt::Display for ExpansionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGraph { prototype, reason } => {
                write!(f, "malformed prototype graph at prototype {prototype}: {reason}")
            }
            Self::Exceeded { prototype, resource, limit, required_at_least } => write!(f,
                "prototype expansion budget exceeded at prototype {prototype}: {} requires at least {required_at_least}, limit {limit}",
                resource.description()),
        }
    }
}

/// Invoke after serialization/opcode/operand validation and before raw
/// occurrence analysis or lifting. Defensive graph checks also make the raw
/// analysis API safe when its caller intentionally requests partial metadata.
pub(crate) fn check(chunk: &Chunk<'_>) -> Result<ExpansionEstimate, ExpansionFailure> {
    check_with_limits(chunk, ExpansionLimits::default())
}

fn check_with_limits(chunk: &Chunk<'_>, limits: ExpansionLimits) -> Result<ExpansionEstimate, ExpansionFailure> {
    let invalid = |prototype, reason| ExpansionFailure::InvalidGraph { prototype, reason };
    if chunk.main >= chunk.functions.len() {
        return Err(invalid(chunk.main, "main prototype is out of range"));
    }

    // Coalesce equal targets while retaining multiplicity. Serialized child
    // table entries without a constructor are not materialized by the lifter.
    let mut edges = Vec::with_capacity(chunk.functions.len());
    for (prototype, function) in chunk.functions.iter().enumerate() {
        let mut counts = rustc_hash::FxHashMap::<usize, u64>::default();
        for instruction in &function.instructions {
            let Instruction::AD { op_code, d, .. } = *instruction else { continue; };
            if !matches!(op_code, OpCode::LOP_NEWCLOSURE | OpCode::LOP_DUPCLOSURE) { continue; }
            let index = usize::try_from(d).map_err(|_| invalid(prototype, "negative constructor operand"))?;
            let child = match op_code {
                OpCode::LOP_NEWCLOSURE => *function.functions.get(index)
                    .ok_or_else(|| invalid(prototype, "constructor child index is out of range"))?,
                OpCode::LOP_DUPCLOSURE => match function.constants.get(index) {
                    Some(Constant::Closure(child)) => *child,
                    _ => return Err(invalid(prototype, "constructor constant is not a closure")),
                },
                _ => unreachable!(),
            };
            if child >= chunk.functions.len() {
                return Err(invalid(prototype, "constructor prototype is out of range"));
            }
            let count = counts.entry(child).or_default();
            *count = count.saturating_add(1);
        }
        let mut children = counts.into_iter().collect::<Vec<_>>();
        children.sort_unstable_by_key(|&(child, _)| child);
        edges.push(children);
    }

    // An iterative postorder handles deeply nested input without using the
    // host stack. No closure occurrence, local or AST node is allocated here.
    let mut state = vec![0u8; edges.len()];
    let mut estimates = vec![ExpansionEstimate::default(); edges.len()];
    let mut stack = vec![(chunk.main, 0usize)];
    state[chunk.main] = 1;
    while let Some((prototype, next_child)) = stack.last_mut() {
        if let Some(&(child, _)) = edges[*prototype].get(*next_child) {
            *next_child += 1;
            match state[child] {
                0 => { state[child] = 1; stack.push((child, 0)); }
                1 => return Err(invalid(*prototype, "constructor graph contains a cycle")),
                _ => {}
            }
            continue;
        }
        let prototype = *prototype;
        let mut estimate = ExpansionEstimate {
            instances: 1,
            instruction_words: chunk.functions[prototype].instructions.len() as u64,
            depth: 0,
        };
        let instance_cap = limits.instances.saturating_add(1);
        let instruction_cap = limits.instruction_words.saturating_add(1);
        for &(child, multiplicity) in &edges[prototype] {
            let child = estimates[child];
            estimate.instances = estimate.instances.saturating_add(child.instances.saturating_mul(multiplicity)).min(instance_cap);
            estimate.instruction_words = estimate.instruction_words
                .saturating_add(child.instruction_words.saturating_mul(multiplicity)).min(instruction_cap);
            estimate.depth = estimate.depth.max(child.depth.saturating_add(1));
        }
        for (resource, required, limit) in [
            (ExpansionResource::Instances, estimate.instances, limits.instances),
            (ExpansionResource::InstructionWords, estimate.instruction_words, limits.instruction_words),
            (ExpansionResource::Depth, estimate.depth, limits.depth),
        ] {
            if required > limit {
                return Err(ExpansionFailure::Exceeded {
                    prototype, resource, limit, required_at_least: required.min(limit.saturating_add(1)),
                });
            }
        }
        estimates[prototype] = estimate;
        state[prototype] = 2;
        stack.pop();
    }
    Ok(estimates[chunk.main])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(out: &mut Vec<u8>, mut value: usize) {
        loop {
            let part = (value & 127) as u8;
            value >>= 7;
            out.push(part | if value == 0 { 0 } else { 128 });
            if value == 0 { return; }
        }
    }

    /// Two actual constructor sites per level, not two child-table entries.
    /// It encodes only O(depth) words but emits 2^(depth+1)-1 occurrences.
    fn repeated(depth: usize, duplicated: bool) -> Vec<u8> {
        let mut bytes = vec![9, 1, 0];
        varint(&mut bytes, depth + 1);
        for prototype in 0..=depth {
            bytes.extend([2, 0, 0, 0, 0, 0]);
            let op = if duplicated { OpCode::LOP_DUPCLOSURE } else { OpCode::LOP_NEWCLOSURE } as u32;
            let words = if prototype == 0 { vec![OpCode::LOP_RETURN as u32 | (1 << 16)] }
                else { vec![op, op | (1 << 8), OpCode::LOP_RETURN as u32 | (3 << 16)] };
            varint(&mut bytes, words.len());
            for word in words { bytes.extend(word.to_le_bytes()); }
            if duplicated && prototype != 0 {
                bytes.extend([1, 6]); // one Constant::Closure
                varint(&mut bytes, prototype - 1);
            } else { bytes.push(0); }
            if !duplicated && prototype != 0 {
                bytes.push(1);
                varint(&mut bytes, prototype - 1);
            } else { bytes.push(0); }
            bytes.extend([0, 0, 0, 0]);
        }
        varint(&mut bytes, depth);
        bytes
    }

    fn chunk(bytes: &[u8]) -> Chunk<'_> {
        let crate::deserializer::bytecode::Bytecode::Chunk(chunk) = crate::deserializer::deserialize(bytes, 1).unwrap()
            else { panic!("test chunk"); };
        chunk
    }

    #[test]
    fn weighted_constructor_sites_have_exact_boundaries() {
        for duplicated in [false, true] {
            let bytes = repeated(3, duplicated);
            let chunk = chunk(&bytes);
            let limits = ExpansionLimits { instances: 15, instruction_words: 29, depth: 3 };
            assert_eq!(check_with_limits(&chunk, limits).unwrap(),
                ExpansionEstimate { instances: 15, instruction_words: 29, depth: 3 });
            for (limits, resource) in [
                (ExpansionLimits { instances: 14, ..limits }, ExpansionResource::Instances),
                (ExpansionLimits { instruction_words: 28, ..limits }, ExpansionResource::InstructionWords),
                (ExpansionLimits { depth: 2, ..limits }, ExpansionResource::Depth),
            ] {
                assert!(matches!(check_with_limits(&chunk, limits), Err(ExpansionFailure::Exceeded { resource: actual, .. }) if actual == resource));
            }
        }
    }

    #[test]
    fn serialized_but_uninstantiated_children_do_not_expand() {
        let bytes = repeated(20, false);
        let mut chunk = chunk(&bytes);
        // Only the root's return remains; its serialized child table is unused.
        chunk.functions[20].instructions.drain(..2);
        assert_eq!(check(&chunk).unwrap(), ExpansionEstimate { instances: 1, instruction_words: 1, depth: 0 });
    }

    #[test]
    fn enormous_counts_saturate_and_cycles_fail_without_occurrences() {
        let bytes = repeated(128, false);
        let mut chunk = chunk(&bytes);
        assert!(matches!(check(&chunk), Err(ExpansionFailure::Exceeded { resource: ExpansionResource::Instances, required_at_least: 65_537, .. })));
        chunk.functions[128].functions[0] = 128;
        assert!(matches!(check(&chunk), Err(ExpansionFailure::InvalidGraph { .. })));
    }

    #[test]
    fn single_constructor_depth_is_bounded_independently() {
        let bytes = repeated(1000, false);
        let mut chunk = chunk(&bytes);
        for function in chunk.functions.iter_mut().skip(1) { function.instructions.remove(1); }
        assert!(matches!(check(&chunk), Err(ExpansionFailure::Exceeded { resource: ExpansionResource::Depth, required_at_least: 257, .. })));
    }

    #[test]
    fn fresh_source_artifact_and_raw_analysis_reject_before_expansion() {
        let bytes = repeated(16, false);
        let source = crate::try_decompile_bytecode_with_options(&bytes, 1, None, crate::DecompileOptions::default()).unwrap_err();
        assert!(source.contains("prototype expansion budget exceeded"), "{source}");
        let artifact = crate::try_decompile_bytecode_artifact_with_diagnostics(&bytes, 1, None, crate::DecompileOptions::default()).unwrap_err();
        assert!(artifact.to_string().contains("prototype expansion budget exceeded"), "{artifact}");
        assert_eq!(artifact.diagnostics[0].stage, "prototype_expansion");
        assert_eq!(artifact.diagnostics[0].code, "instance_budget_exceeded");
        assert!(crate::analyze_upvalues_raw(&bytes, 1).unwrap_err().contains("prototype expansion budget exceeded"));
        let ordinary = repeated(2, false);
        assert!(crate::try_decompile_bytecode_with_options(&ordinary, 1, None, crate::DecompileOptions::default()).is_ok());
        assert!(crate::analyze_upvalues_raw(&ordinary, 1).is_ok());
    }
}
