//! Prove immutable incoming cells from VAL roots and UPVAL forwarding.
//! REF captures, external root cells, missing sites and uncertain graphs refuse.
use std::collections::VecDeque;

use crate::{
    deserializer::{chunk::Chunk, constant::Constant},
    instruction::Instruction,
    op_code::OpCode,
};

#[derive(Debug, Default)]
pub(crate) struct CaptureEffects {
    pub readonly: Vec<Vec<bool>>,
    pub refusal: Option<&'static str>,
}

impl CaptureEffects {
    pub fn build(chunk: &Chunk) -> Self {
        match compute(chunk) {
            Ok(readonly) => Self {
                readonly,
                refusal: None,
            },
            Err(reason) => Self {
                readonly: Vec::new(),
                refusal: Some(reason),
            },
        }
    }

    pub fn report(&self) -> serde_json::Value {
        let rows = self
            .readonly
            .iter()
            .enumerate()
            .filter_map(|(prototype, values)| {
                let slots = values
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, &readonly)| readonly.then_some(slot))
                    .collect::<Vec<_>>();
                (!slots.is_empty())
                    .then(|| serde_json::json!({"prototype": prototype, "slots": slots}))
            })
            .collect::<Vec<_>>();
        serde_json::json!({"schema_version": 1, "model": "luau-v9-val-upval-immutability-v1",
            "status": if self.refusal.is_some() { "refused" } else { "complete" },
            "refusal": self.refusal, "readonly_slots": rows,
            "analyzed_slots": if self.refusal.is_none() { Some(self.readonly.iter().map(Vec::len).sum::<usize>()) } else { None },
            "limits": {"prototypes": 50000, "slots": 200000, "instructions": 1000000, "capture_edges": 200000},
            "scope": "Incoming values rooted in VAL at every constructor, or UPVAL from proven immutable slots; no own/forwarded SETUPVAL. REF roots and external root slots remain unknown. This does not certify table contents, callee purity, ownership or source binding. SSA exceptions use only this construction's incoming-group numeric IDs; no extra AST owners or cross-pass proof inheritance."})
    }
}

fn compute(chunk: &Chunk) -> Result<Vec<Vec<bool>>, &'static str> {
    if chunk.version != 9 {
        return Err("unsupported bytecode version");
    }
    if chunk.functions.len() > 50_000 || chunk.main >= chunk.functions.len() {
        return Err("prototype budget or root");
    }
    let mut offsets = vec![0usize];
    let mut instructions = 0usize;
    for function in &chunk.functions {
        let count = offsets.last().unwrap() + usize::from(function.num_upvalues);
        instructions = instructions
            .checked_add(function.instructions.len())
            .ok_or("instruction budget")?;
        if count > 200_000 || instructions > 1_000_000 {
            return Err("slot/instruction budget");
        }
        offsets.push(count);
    }
    let total = *offsets.last().unwrap();
    let mut writes = vec![false; total];
    let mut blocked = vec![false; total];
    let mut incoming = vec![0usize; total];
    let mut pending = vec![0usize; total];
    let mut children = vec![Vec::<usize>::new(); total];
    let mut parents = vec![Vec::<usize>::new(); total];
    let mut captures = 0usize;
    for slot in offsets[chunk.main]..offsets[chunk.main + 1] {
        blocked[slot] = true;
    }
    for (parent_id, parent) in chunk.functions.iter().enumerate() {
        let mut pc = 0;
        while pc < parent.instructions.len() {
            match parent.instructions[pc] {
                Instruction::BC {
                    op_code: OpCode::LOP_SETUPVAL,
                    b,
                    ..
                } => {
                    if b >= parent.num_upvalues {
                        return Err("invalid write slot");
                    }
                    writes[offsets[parent_id] + usize::from(b)] = true;
                }
                Instruction::BC {
                    op_code: OpCode::LOP_CAPTURE,
                    ..
                } => return Err("orphan capture"),
                Instruction::AD {
                    op_code: op @ (OpCode::LOP_NEWCLOSURE | OpCode::LOP_DUPCLOSURE),
                    d,
                    ..
                } => {
                    let index = usize::try_from(d).map_err(|_| "negative constructor index")?;
                    let child_id = if op == OpCode::LOP_NEWCLOSURE {
                        *parent.functions.get(index).ok_or("child index")?
                    } else {
                        match parent.constants.get(index) {
                            Some(Constant::Closure(id)) => *id,
                            _ => return Err("closure constant"),
                        }
                    };
                    let child = chunk.functions.get(child_id).ok_or("child prototype")?;
                    for ordinal in 0..usize::from(child.num_upvalues) {
                        captures += 1;
                        if captures > 200_000 {
                            return Err("capture edge budget");
                        }
                        let Instruction::BC {
                            op_code: OpCode::LOP_CAPTURE,
                            a,
                            b,
                            ..
                        } = *parent
                            .instructions
                            .get(pc + 1 + ordinal)
                            .ok_or("truncated captures")?
                        else {
                            return Err("missing capture");
                        };
                        let target = offsets[child_id] + ordinal;
                        incoming[target] += 1;
                        match a {
                            0 | 1 => {
                                if b >= parent.max_stack_size {
                                    return Err("capture register");
                                }
                                // A REF can share a parent/sibling mutable cell.
                                // VAL owns a fresh copied cell, even if its source
                                // register or referenced table changes later.
                                blocked[target] |= a == 1;
                            }
                            2 => {
                                if b >= parent.num_upvalues {
                                    return Err("capture upvalue slot");
                                }
                                let source = offsets[parent_id] + usize::from(b);
                                children[source].push(target);
                                parents[target].push(source);
                                pending[target] += 1;
                            }
                            _ => return Err("unknown capture mode"),
                        }
                    }
                    pc += usize::from(child.num_upvalues);
                }
                _ => {}
            }
            pc += 1;
        }
    }
    // A descendant SETUPVAL also writes each UPVAL ancestor cell. VAL/REF
    // edges do not alias an incoming parent upvalue through this relation.
    let mut queue: VecDeque<_> = writes
        .iter()
        .enumerate()
        .filter_map(|(id, &v)| v.then_some(id))
        .collect();
    while let Some(child) = queue.pop_front() {
        for &parent in &parents[child] {
            if !writes[parent] {
                writes[parent] = true;
                queue.push_back(parent);
            }
        }
    }
    let mut readonly = vec![false; total];
    for id in 0..total {
        if incoming[id] != 0 && pending[id] == 0 && !blocked[id] && !writes[id] {
            readonly[id] = true;
            queue.push_back(id);
        }
    }
    // AND over every static constructor, seeded only by copied cells. Cycles
    // without an established immutable root remain unknown rather than assume
    // their own conclusion. Each edge is processed at most once per worklist.
    while let Some(parent) = queue.pop_front() {
        for &child in &children[parent] {
            pending[child] -= 1;
            if pending[child] == 0 && !blocked[child] && !writes[child] && !readonly[child] {
                readonly[child] = true;
                queue.push_back(child);
            }
        }
    }
    Ok(offsets
        .windows(2)
        .map(|range| readonly[range[0]..range[1]].to_vec())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deserializer::function::Function;

    fn bc(op_code: OpCode, a: u8, b: u8) -> Instruction {
        Instruction::BC {
            op_code,
            a,
            b,
            c: 0,
            aux: 0,
        }
    }
    fn make(op_code: OpCode, d: i16) -> Instruction {
        Instruction::AD {
            op_code,
            a: 0,
            d,
            aux: 0,
        }
    }
    fn function(upvalues: u8, instructions: Vec<Instruction>, children: Vec<usize>) -> Function {
        Function {
            max_stack_size: 4,
            num_parameters: 0,
            num_upvalues: upvalues,
            is_vararg: false,
            instructions,
            constants: Vec::new(),
            functions: children,
            line_defined: 0,
            function_name: 0,
            line_gap_log2: None,
            line_info_delta: None,
            abs_line_info_delta: None,
            has_debug_info: false,
            debug_locals: Vec::new(),
            debug_upvalue_name_indices: Vec::new(),
            type_info: None,
        }
    }
    fn chunk(functions: Vec<Function>) -> Chunk {
        Chunk {
            version: 9,
            main: 0,
            functions,
            string_table: Vec::new(),
            userdata_type_names: Vec::new(),
        }
    }
    fn chain(mode: u8, writes_leaf: bool) -> Chunk {
        chunk(vec![
            function(
                0,
                vec![
                    make(OpCode::LOP_NEWCLOSURE, 0),
                    bc(OpCode::LOP_CAPTURE, mode, 0),
                ],
                vec![1],
            ),
            function(
                1,
                vec![
                    make(OpCode::LOP_NEWCLOSURE, 0),
                    bc(OpCode::LOP_CAPTURE, 2, 0),
                ],
                vec![2],
            ),
            function(
                1,
                vec![bc(
                    if writes_leaf {
                        OpCode::LOP_SETUPVAL
                    } else {
                        OpCode::LOP_GETUPVAL
                    },
                    0,
                    0,
                )],
                vec![],
            ),
        ])
    }

    #[test]
    fn copied_roots_forward_but_descendant_writes_and_refs_refuse() {
        assert_eq!(
            compute(&chain(0, false)).unwrap(),
            vec![vec![], vec![true], vec![true]]
        );
        assert_eq!(
            compute(&chain(0, true)).unwrap(),
            vec![vec![], vec![false], vec![false]]
        );
        assert_eq!(
            compute(&chain(1, false)).unwrap(),
            vec![vec![], vec![false], vec![false]]
        );
    }

    #[test]
    fn every_constructor_is_required_including_dupclosure() {
        let mut data = chain(0, false);
        data.functions[0].constants.push(Constant::Closure(1));
        data.functions[0].instructions.extend([
            make(OpCode::LOP_DUPCLOSURE, 0),
            bc(OpCode::LOP_CAPTURE, 0, 1),
        ]);
        assert!(compute(&data).unwrap()[2][0]);
        *data.functions[0].instructions.last_mut().unwrap() = bc(OpCode::LOP_CAPTURE, 1, 1);
        assert!(!compute(&data).unwrap()[1][0]);
        assert!(!compute(&data).unwrap()[2][0]);
    }

    #[test]
    fn external_missing_and_self_justifying_cells_stay_unknown() {
        let mut external = chain(0, false);
        external.functions[0].num_upvalues = 1;
        external.functions[0].instructions[1] = bc(OpCode::LOP_CAPTURE, 2, 0);
        assert_eq!(
            compute(&external).unwrap(),
            vec![vec![false], vec![false], vec![false]]
        );
        let missing = chunk(vec![
            function(0, vec![], vec![]),
            function(1, vec![], vec![]),
        ]);
        assert!(!compute(&missing).unwrap()[1][0]);
        let mut cyclic = chain(0, false);
        cyclic.functions[1].functions[0] = 1;
        assert!(!compute(&cyclic).unwrap()[1][0]);
    }

    #[test]
    fn malformed_and_out_of_profile_input_never_produces_a_certificate() {
        let mut data = chain(0, false);
        data.version = 10;
        assert!(CaptureEffects::build(&data).refusal.is_some());
        data.version = 9;
        data.functions[0].instructions[1] = bc(OpCode::LOP_CAPTURE, 2, 4);
        assert!(CaptureEffects::build(&data).readonly.is_empty());
        data.functions[0].instructions = vec![bc(OpCode::LOP_CAPTURE, 0, 0)];
        assert!(CaptureEffects::build(&data).refusal.is_some());
        data.functions[0].instructions = vec![make(OpCode::LOP_NEWCLOSURE, 0)];
        assert!(CaptureEffects::build(&data).refusal.is_some());
    }
}
