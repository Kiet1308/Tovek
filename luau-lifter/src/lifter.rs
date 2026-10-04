use anyhow::Result;

use by_address::ByAddress;

use itertools::Itertools;
use petgraph::visit::EdgeRef;
use parking_lot::Mutex;
use petgraph::stable_graph::NodeIndex;

use rustc_hash::FxHashMap;
use triomphe::Arc;

use super::{
    deserializer::{
        constant::Constant as BytecodeConstant, function::Function as BytecodeFunction,
    },
    instruction::Instruction,
    op_code::OpCode,
};
use ast::{self, LocalRw};
use cfg::{
    block::{BlockEdge, BranchType},
    function::Function,
};

/// A naming hint for the source local the compiler kept in `register` over
/// the half-open PC range `start_pc..end_pc`, derived from its bytecode type
/// tag (`vector`, `buffer`, `cframe`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedLocalHint {
    pub register: u8,
    pub start_pc: usize,
    pub end_pc: usize,
    pub name: String,
}

pub struct Lifter<'a> {
    function_list: &'a Vec<BytecodeFunction>,
    string_table: &'a [&'a [u8]],
    typed_locals: &'a [TypedLocalHint],
    debug_ranges: crate::metadata_index::RegisterRanges,
    typed_ranges: crate::metadata_index::RegisterRanges,
    debug_bindings: Vec<Option<ast::SourceBinding>>,
    bytecode_version: u8,
    blocks: FxHashMap<usize, NodeIndex>,
    function: Function,
    // Insertion-ordered (bytecode/PC order, deterministic) rather than a hash map:
    // the consumer sorts by `func_index` (a bytecode-proto index that is NOT unique
    // — one proto can be instantiated by several closure sites), and a stable sort
    // over PC order then breaks ties deterministically. A hash map keyed by the
    // ASLR-randomized `ByAddress` would leave tied entries in a run-dependent order.
    child_functions: Vec<(ByAddress<Arc<Mutex<ast::Function>>>, usize, Option<String>)>,
    static_function_id: Option<String>,
    // Bytecode registers form a small dense range. Keep unmaterialized slots
    // empty so local IDs still follow first use, including reverse block order.
    register_map: Vec<Option<ast::RcLocal>>,
    register_of: FxHashMap<u64, u8>,
    // Provenance indexed by FORGLOOP PC.  Prep discovery already resolves the
    // target step PC, so keeping the pair here avoids rescanning the complete
    // instruction vector for every FORGLOOP marker.
    for_origins_by_step: FxHashMap<usize, ast::ForOrigin>,
    current_node: Option<NodeIndex>,
    upvalues: Vec<ast::RcLocal>,
    source_lines: Vec<Option<u32>>,
}

fn take_top(
    top: &mut Option<(ast::RValue, u8)>,
    pending_pcs: &mut Vec<usize>,
    consumed_pcs: &mut Vec<usize>,
) -> (ast::RValue, u8) {
    consumed_pcs.append(pending_pcs);
    top.take().unwrap()
}

impl<'a> Lifter<'a> {
    pub fn lift(
        f_list: &'a Vec<BytecodeFunction>,
        str_list: &'a [&'a [u8]],
        bytecode_version: u8,
        function_id: usize,
        static_function_id: Option<String>,
        typed_locals: &'a [TypedLocalHint],
        trace_provenance: bool,
    ) -> (
        Function,
        Vec<ast::RcLocal>,
        Vec<(ByAddress<Arc<Mutex<ast::Function>>>, usize, Option<String>)>,
    ) {
        let mut context = Self {
            function_list: f_list,
            string_table: str_list,
            typed_locals,
            debug_ranges: crate::metadata_index::RegisterRanges::new(f_list[function_id].debug_locals.iter()
                .map(|local| (local.register, local.start_pc, local.end_pc))),
            typed_ranges: crate::metadata_index::RegisterRanges::new(typed_locals.iter()
                .map(|local| (local.register, local.start_pc, local.end_pc))),
            debug_bindings: Vec::new(),
            bytecode_version,
            blocks: FxHashMap::default(),
            function: Function::new(function_id),
            child_functions: Vec::new(),
            static_function_id,
            register_map: vec![None; usize::from(f_list[function_id].max_stack_size
                .max(f_list[function_id].num_parameters))],
            register_of: FxHashMap::default(),
            for_origins_by_step: FxHashMap::default(),
            current_node: None,
            upvalues: Vec::new(),
            source_lines: Vec::new(),
        };

        if let (true, Some(identity)) = (trace_provenance, &context.static_function_id) {
            context.function.provenance = Some(Box::new(cfg::provenance::FunctionTrace::new(function_id, identity.clone())));
            context.function.provenance.as_mut().unwrap().instruction_count = f_list[function_id].instructions.len();
            context.source_lines = crate::upvalue_analysis::decode_source_lines(&f_list[function_id]);
        }

        context.debug_bindings = f_list[function_id].debug_locals.iter()
            .map(|local| context.debug_binding(local)).collect();
        context.lift_function();
        (context.function, context.upvalues, context.child_functions)
    }

    fn lift_function(&mut self) {
        self.discover_blocks().unwrap();
        self.build_for_origin_map();

        let mut blocks = self.blocks.keys().cloned().collect::<Vec<_>>();

        blocks.sort_unstable();

        for slot in 0..self.function_list[self.function.id].num_upvalues {
            let local = ast::RcLocal::default();
            if let Some(name) = self.function_list[self.function.id].debug_upvalue_name_indices.get(slot as usize)
                .and_then(|&index| self.debug_name(index))
            {
                local.0.lock().add_source_binding(ast::SourceBinding {
                    origin: ast::BindingOrigin::DebugUpvalue { prototype: self.function.id, slot: slot as usize }, name,
                });
            }
            if let Some(trace) = &mut self.function.provenance { trace.register(&local, slot as usize, "incoming_upvalue"); }
            self.upvalues.push(local);
        }

        for i in 0..self.function_list[self.function.id].num_parameters {
            let parameter = ast::RcLocal::default();
            let mut ranges = Vec::new();
            self.debug_ranges.covering(i, 0, &mut ranges);
            let mut names = ranges.into_iter()
                .filter_map(|index| self.debug_bindings[index].as_ref());
            if let (Some(binding), None) = (names.next(), names.next()) {
                parameter.0.lock().add_source_binding(binding.clone());
            }
            self.function.parameters.push(parameter.clone());
            if let Some(trace) = &mut self.function.provenance { trace.register(&parameter, i as usize, "parameter"); }
            if !self.debug_bindings.is_empty() || !self.typed_locals.is_empty() {
                self.register_of.insert(parameter.stable_id(), i);
            }
            self.register_map[i as usize] = Some(parameter);
        }

        self.function.is_variadic = self.function_list[self.function.id].is_vararg;

        for (index, &start_pc) in blocks.iter().enumerate().rev() {
            let end_pc = blocks.get(index + 1).copied()
                .unwrap_or(self.function_list[self.function.id].instructions.len()) - 1;
            self.current_node = Some(self.block_to_node(start_pc));
            self.function
                .set_block_pc_range(self.current_node.unwrap(), start_pc, end_pc);
            let (statements, edges, statement_pcs, statement_origins) = self.lift_block(start_pc, end_pc);
            self.record_lifted_origins(&statements, &statement_origins);
            self.record_typed_local_hints(&statements, &statement_pcs);
            let block = self.function.block_mut(self.current_node.unwrap()).unwrap();
            block.0.extend(statements);
            self.function.set_edges(self.current_node.unwrap(), edges);
        }

        let entry_node = self.function.new_block();
        self.function.set_edges(entry_node, vec![(
            self.block_to_node(0),
            BlockEdge::new(BranchType::Unconditional),
        )]);
        self.function.set_entry(entry_node);
    }

    /// Attach the compiler's typed-local naming hints to the statements just
    /// lifted for the current block.  A statement writing register `r` at PC
    /// `pc` defines the source local the compiler kept in `r` when some typed
    /// range covers `(r, pc)`; the hint is keyed by the statement's position so
    /// `ssa::construct` can move it onto the fresh SSA version it mints for that
    /// definition.  Only plain register writes qualify: the lifter's register
    /// locals are looked up by identity, so upvalue cells and parameters (never
    /// typed-local targets) fall through.
    fn debug_name(&self, index: usize) -> Option<String> {
        let raw = self.string_table.get(index.checked_sub(1)?)?;
        let name = std::str::from_utf8(raw).ok()?;
        ast::valid_source_name(name).then(|| name.to_string())
    }

    fn record_lifted_origins(&mut self, statements: &[ast::Statement], origins: &[Vec<usize>]) {
        let Some(trace) = &mut self.function.provenance else { return; };
        let node = self.current_node.unwrap().index();
        for (index, (statement, pcs)) in statements.iter().zip(origins).enumerate() {
            let lines = pcs.iter().filter_map(|pc| self.source_lines.get(*pc).copied().flatten())
                .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
            let reads = match statement {
                ast::Statement::Close(close) => close.locals.iter().map(ast::RcLocal::stable_id).collect(),
                _ => statement.values_read().into_iter().map(ast::RcLocal::stable_id).collect(),
            };
            trace.statement(cfg::provenance::LiftedStatement { block: node, index, pcs: pcs.clone(), lines,
                kind: cfg::provenance::statement_kind(statement), read_registers: reads,
                written_registers: statement.values_written().into_iter().map(ast::RcLocal::stable_id).collect() });
        }
    }

    fn debug_binding(&self, local: &super::deserializer::function::DebugLocal) -> Option<ast::SourceBinding> {
        let proto = &self.function_list[self.function.id];
        if local.start_pc >= local.end_pc || local.end_pc > proto.instructions.len()
            || local.register >= proto.max_stack_size { return None; }
        Some(ast::SourceBinding {
            origin: ast::BindingOrigin::DebugLocal { prototype: self.function.id,
                register: local.register, start_pc: local.start_pc, end_pc: local.end_pc },
            name: self.debug_name(local.name_index)?,
        })
    }

    fn record_typed_local_hints(&mut self, statements: &[ast::Statement], pcs: &[usize]) {
        let node = self.current_node.unwrap();
        // The block's statements are appended to whatever the block already
        // holds (always empty for lifted blocks, but stay exact).
        let base = self.function.block(node).map_or(0, |block| block.len());
        // Function names remain direct evidence when local debug info is stripped.
        for (index, statement) in statements.iter().enumerate() {
            if let ast::Statement::Assign(assign) = statement {
                if assign.left.len() != 1 || assign.right.len() != 1 || assign.left[0].as_local().is_none() { continue; }
                let ast::RValue::Closure(closure) = &assign.right[0] else { continue; };
                let function = closure.function.lock();
                if let (Some(prototype), Some(name)) = (function.bytecode_proto_id,
                    function.name.as_ref().filter(|n| ast::valid_source_name(n)))
                {
                    self.function.local_source_bindings.entry((node, base + index, 0)).or_default().push(ast::SourceBinding {
                        origin: ast::BindingOrigin::Function { prototype }, name: name.clone(),
                    });
                }
            }
        }
        let debug_locals = &self.function_list[self.function.id].debug_locals;
        if self.typed_locals.is_empty() && debug_locals.is_empty() { return; }
        let block_start = self.function.block_pc_range(node).unwrap().start;
        let mut ranges = Vec::new();
        for register in 0..self.debug_ranges.register_count() {
            ranges.clear();
            self.debug_ranges.covering(register as u8, block_start, &mut ranges);
            if let [index] = ranges.as_slice() {
                let debug = &debug_locals[*index];
                if let (Some(local), Some(binding)) = (self.register_map.get(debug.register as usize).and_then(Option::as_ref), self.debug_bindings[*index].as_ref()) {
                    self.function.entry_source_bindings.insert((node, local.clone()), binding.clone());
                }
            }
        }
        // Enumerate local writes once, preserving PC/statement/write order.
        let mut writes = Vec::new();
        let mut definitions: FxHashMap<u8, Vec<(usize, usize, usize)>> = FxHashMap::default();
        for (index, (statement, &pc)) in statements.iter().zip(pcs).enumerate() {
            for (written, local) in statement.values_written().iter().enumerate() {
                if let Some(&register) = self.register_of.get(&local.stable_id()) {
                    writes.push((pc, index, written, register));
                    if !debug_locals.is_empty() {
                        definitions.entry(register).or_default().push((pc, index, written));
                    }
                }
            }
        }
        let block_end = self.function.block_pc_range(node).unwrap().end + 1;
        for (pc, index, written_index, register) in writes {
                // A debug interval starts AFTER initialization. Use the last
                // reaching write within this block; never guess across CFG edges.
                ranges.clear();
                self.debug_ranges.covering(register, pc, &mut ranges);
                if !debug_locals.is_empty() {
                    let defs = &definitions[&register];
                    let after = defs.partition_point(|(def_pc, _, _)| *def_pc <= pc);
                    // Only the last write at this PC reaches a later start;
                    // starts at the next write's PC still refer to this write.
                    if after > 0 && defs[after - 1] == (pc, index, written_index) {
                        let through = defs.get(after).map_or(block_end, |&(next_pc, _, _)| next_pc.min(block_end));
                        self.debug_ranges.starting_after_through(register, pc, through, &mut ranges);
                    }
                }
                let mut bindings = ranges.iter().filter_map(|&index| self.debug_bindings[index].as_ref());
                if let (Some(binding), None) = (bindings.next(), bindings.next()) {
                    self.function.local_source_bindings.entry((node, base + index, written_index)).or_default().push(binding.clone());
                }
                ranges.clear();
                self.typed_ranges.covering(register, pc, &mut ranges);
                let Some(hint) = ranges.iter().min().map(|&index| &self.typed_locals[index]) else {
                    continue;
                };
                self.function
                    .local_type_hints
                    .insert((node, base + index, written_index), hint.name.clone());
        }
    }

    /// Pair every generic prep with its FORGLOOP once, before marker lifting.
    /// The previous FORGLOOP path searched all instructions for each loop,
    /// making a function with many loops quadratic in instruction count.
    fn build_for_origin_map(&mut self) {
        let instructions = &self.function_list[self.function.id].instructions;
        for (prep_pc, instruction) in instructions.iter().enumerate() {
            let Instruction::AD {
                op_code:
                    prep_op_code @ (OpCode::LOP_FORGPREP
                    | OpCode::LOP_FORGPREP_NEXT
                    | OpCode::LOP_FORGPREP_INEXT),
                a,
                d,
                ..
            } = instruction
            else {
                continue;
            };
            let step_pc = loop_jump_target(instructions, prep_pc, *d);
            let (step_a, step_d, step_aux) = match instructions.get(step_pc) {
                Some(Instruction::AD {
                    op_code: OpCode::LOP_FORGLOOP,
                    a: step_a,
                    d: step_d,
                    aux: step_aux,
                }) => (*step_a, *step_d, *step_aux),
                _ => panic!(
                    "FORGPREP at PC {prep_pc} has no FORGLOOP partner at PC {step_pc}"
                ),
            };
            let result_count = (step_aux & 0xff) as u8;
            assert!(result_count > 0, "FORGLOOP has zero result locals");
            let explicit_nil_args = Self::has_explicit_nil_args(instructions, prep_pc, *a);
            let origin = ast::ForOrigin {
                prep_pc,
                step_pc,
                body_pc: loop_jump_target(instructions, step_pc, step_d),
                follow_pc: step_pc + 1,
                prep_kind: match prep_op_code {
                    OpCode::LOP_FORGPREP => ast::ForPrepKind::Generic,
                    OpCode::LOP_FORGPREP_NEXT => ast::ForPrepKind::Next,
                    OpCode::LOP_FORGPREP_INEXT => ast::ForPrepKind::Inext,
                    _ => unreachable!(),
                },
                base_register: *a,
                result_count,
                aux: step_aux,
                bytecode_version: self.bytecode_version,
                vm_profile: ast::VmProfileId::Luau,
                explicit_nil_args,
            };
            // Duplicate step targets are malformed bytecode.  Keep the
            // failure explicit instead of allowing one marker to inherit the
            // provenance of an unrelated prep.
            assert!(
                self.for_origins_by_step.insert(step_pc, origin).is_none(),
                "duplicate FORGPREP target at FORGLOOP PC {step_pc}"
            );
            // `step_a` is checked when constructing the paired Next marker;
            // retain the read here to make the malformed-base case explicit
            // without normalizing it into a seemingly valid origin.
            let _ = step_a;
        }
    }

    /// Detect the bytecode shape produced by an explicit single-result call
    /// followed by `, nil, nil` in a generic-for expression.  An implicit
    /// multi-result call is written directly into the protocol base register
    /// with `C=3`; explicit padding uses `C=1` and either writes the base
    /// directly or moves the temporary result into it before the two nil loads.
    /// That temporary is allocated above the loop base; a MOVE from below the
    /// base copies a local (`local t = f(); for k, v in t do`) instead.
    fn has_explicit_nil_args(instructions: &[Instruction], prep_pc: usize, base: u8) -> bool {
        let load_nil = |pc: usize, register: u8| {
            matches!(
                instructions.get(pc),
                Some(Instruction::BC {
                    op_code: OpCode::LOP_LOADNIL,
                    a,
                    ..
                }) if *a == register
            )
        };
        let Some(load_two_pc) = prep_pc.checked_sub(1) else {
            return false;
        };
        let Some(load_one_pc) = prep_pc.checked_sub(2) else {
            return false;
        };
        if !load_nil(load_two_pc, base.saturating_add(2))
            || !load_nil(load_one_pc, base.saturating_add(1))
        {
            return false;
        }
        let is_single_call = |instruction: &Instruction, register: u8| {
            matches!(
                instruction,
                Instruction::BC {
                    op_code: OpCode::LOP_CALL | OpCode::LOP_CALLFB,
                    a,
                    // C stores result count + 1; textual `C=1` (one result)
                    // is represented internally as c == 2.
                    c: 2,
                    ..
                } if *a == register
            )
        };
        // CALLFB is followed by an injected NOP carrying its feedback AUX;
        // that NOP appears between the call and the MOVE/LOADNIL sequence.
        // Walk backwards over only those no-ops, then require the exact
        // single-result call (or a MOVE from its result) to avoid matching an
        // unrelated call in the setup block.
        let is_nop = |pc: usize| {
            matches!(
                instructions.get(pc),
                Some(Instruction::BC {
                    op_code: OpCode::LOP_NOP,
                    ..
                })
            )
        };
        let Some(mut cursor) = prep_pc.checked_sub(3) else {
            return false;
        };
        while cursor > 0 && is_nop(cursor) {
            cursor -= 1;
        }
        match instructions.get(cursor) {
            Some(instruction) if is_single_call(instruction, base) => true,
            Some(Instruction::BC {
                op_code: OpCode::LOP_MOVE,
                a,
                b,
                ..
            }) if *a == base && *b > base => {
                let Some(mut call_pc) = cursor.checked_sub(1) else {
                    return false;
                };
                while call_pc > 0 && is_nop(call_pc) {
                    call_pc -= 1;
                }
                instructions
                    .get(call_pc)
                    .is_some_and(|instruction| is_single_call(instruction, *b))
            }
            _ => false,
        }
    }

    fn discover_blocks(&mut self) -> Result<()> {
        self.blocks.insert(0, self.function.new_block());
        for (insn_index, insn) in self.function_list[self.function.id]
            .instructions
            .iter()
            .enumerate()
        {
            match insn {
                Instruction::BC { op_code, c, .. } => match op_code {
                    OpCode::LOP_LOADB if *c != 0 => {
                        let dest_index = (insn_index + 1).checked_add_signed((*c).into()).unwrap();
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    _ => {}
                },

                Instruction::AD {
                    op_code,
                    a: _,
                    d,
                    aux: _,
                } => match op_code {
                    OpCode::LOP_JUMP
                    | OpCode::LOP_JUMPBACK
                    | OpCode::LOP_JUMPIF
                    | OpCode::LOP_JUMPIFNOT => {
                        let dest_index = (insn_index + 1).checked_add_signed((*d).into()).unwrap();
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    OpCode::LOP_JUMPIFEQ
                    | OpCode::LOP_JUMPIFLE
                    | OpCode::LOP_JUMPIFLT
                    | OpCode::LOP_JUMPIFNOTEQ
                    | OpCode::LOP_JUMPIFNOTLE
                    | OpCode::LOP_JUMPIFNOTLT
                    | OpCode::LOP_JUMPXEQKNIL
                    | OpCode::LOP_JUMPXEQKB
                    | OpCode::LOP_JUMPXEQKN
                    | OpCode::LOP_JUMPXEQKS => {
                        let dest_index = (insn_index + 1).checked_add_signed((*d).into()).unwrap();
                        self.blocks
                            .entry(insn_index + 2)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    OpCode::LOP_FORNPREP => {
                        let dest_index = (insn_index + 1).checked_add_signed((*d).into()).unwrap();
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    OpCode::LOP_FORGPREP
                    | OpCode::LOP_FORGPREP_NEXT
                    | OpCode::LOP_FORGPREP_INEXT => {
                        let dest_index = (insn_index + 1).checked_add_signed((*d).into()).unwrap();
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    OpCode::LOP_FORNLOOP => {
                        let dest_index = (insn_index + 1).checked_add_signed((*d).into()).unwrap();
                        self.blocks
                            .entry(insn_index)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    OpCode::LOP_FORGLOOP => {
                        let dest_index = (insn_index + 1)
                            .checked_add_signed((*d).try_into().unwrap())
                            .unwrap();
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                    OpCode::LOP_CMPPROTO => {
                        unreachable!("CMPPROTO must be rejected before lifting");
                    }
                    _ => {}
                },

                Instruction::E { op_code, e } => {
                    if *op_code == OpCode::LOP_JUMPX {
                        let dest_index = (insn_index + 1)
                            .checked_add_signed((*e).try_into().unwrap())
                            .unwrap();
                        self.blocks
                            .entry(insn_index + 1)
                            .or_insert_with(|| self.function.new_block());
                        self.blocks
                            .entry(dest_index)
                            .or_insert_with(|| self.function.new_block());
                    }
                }
            }
        }

        Ok(())
    }

    fn lift_block(
        &mut self,
        block_start: usize,
        block_end: usize,
    ) -> (Vec<ast::Statement>, Vec<(NodeIndex, BlockEdge)>, Vec<usize>, Vec<Vec<usize>>) {
        let mut statements = Vec::with_capacity((block_start..=block_end).count());
        let mut edges = Vec::new();
        // Bytecode PC of the instruction each statement was lifted from
        // (parallel to `statements`; a multi-instruction lift is attributed to
        // its first instruction).
        let track_statement_pcs = !self.typed_locals.is_empty()
            || !self.function_list[self.function.id].debug_locals.is_empty();
        let mut statement_pcs = if track_statement_pcs {
            Vec::with_capacity(statements.capacity())
        } else {
            Vec::new()
        };

        let mut top: Option<(ast::RValue, u8)> = None;
        // PC of the CALL whose fallback follows the most recent FASTCALL or
        // FASTPCALL in this block.
        let mut fastcall_target: Option<usize> = None;
        let trace_origins = self.function.provenance.is_some();
        let mut pending_pcs = Vec::new();
        let mut statement_origins = Vec::new();

        let mut iter = self.function_list[self.function.id].instructions[block_start..=block_end]
            .iter()
            .enumerate();

        while let Some((index, instruction)) = iter.next() {
            let pc = block_start + index;
            let lifted_before = statements.len();
            let mut consumed_pcs = Vec::new();
            let mut set_pending = false;
            let mut stop = false;
            match *instruction {
                Instruction::BC {
                    op_code,
                    a,
                    b,
                    c,
                    aux,
                } => match op_code {
                    // TODO: do we want to nil initialize all registers here?
                    OpCode::LOP_PREPVARARGS => {}
                    OpCode::LOP_MOVE => {
                        let a = self.register(a as _);
                        let b = self.register(b as _);
                        statements.push(ast::Assign::new(vec![a.into()], vec![b.into()]).into());
                    }
                    OpCode::LOP_GETUPVAL => {
                        let a = self.register(a as _);
                        let up = self.upvalues[b as usize].clone();
                        statements.push(ast::Assign::new(vec![a.into()], vec![up.into()]).into());
                    }
                    OpCode::LOP_SETUPVAL => {
                        let a = self.register(a as _);
                        let up = self.upvalues[b as usize].clone();
                        statements.push(ast::Assign::new(vec![up.into()], vec![a.into()]).into());
                    }
                    OpCode::LOP_LOADNIL => {
                        let target = self.register(a as _);
                        statements.push(
                            ast::Assign::new(vec![target.into()], vec![ast::Literal::Nil.into()])
                                .into(),
                        )
                    }
                    OpCode::LOP_LOADB => {
                        let target = self.register(a as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Literal::Boolean(b != 0).into()],
                            )
                            .into(),
                        );
                        if c != 0 {
                            // LOADB with C != 0 is an UNCONDITIONAL skip-jump of C
                            // (C is an unsigned 8-bit skip count in Luau), so the
                            // successor is I+1+C, exactly as `discover_blocks`
                            // computes it. The old hard-coded I+2 only matched the
                            // stock C==1 pair and panicked (or wired the wrong block)
                            // for C>1 obfuscated bytecode (L1). Mirror discover_blocks
                            // with the same unsigned widening.
                            let dest = (block_start + index + 1)
                                .checked_add_signed((c).into())
                                .unwrap();
                            edges.push((
                                self.block_to_node(dest),
                                BlockEdge::new(BranchType::Unconditional),
                            ));
                            stop = true;
                        }
                    }
                    OpCode::LOP_NEWTABLE => {
                        statements.push(
                            ast::Assign::new(
                                vec![self.register(a as _).into()],
                                vec![ast::Table::default().into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_GETGLOBAL => {
                        let value = self.register(a as _);
                        let global_name = self.constant(aux as _).into_string().unwrap();
                        statements.push(
                            ast::Assign::new(
                                vec![value.into()],
                                vec![ast::Global::new(global_name).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_SETGLOBAL => {
                        let value = self.register(a as _);
                        let global_name = self.constant(aux as _).into_string().unwrap();
                        statements.push(
                            ast::Assign::new(
                                vec![ast::Global::new(global_name).into()],
                                vec![value.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_GETTABLE => {
                        let target = self.register(a as _);
                        let table = self.register(b as _);
                        let key = self.register(c as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Index::new(table.into(), key.into()).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_GETTABLEKS => {
                        let target = self.register(a as _);
                        let table = self.register(b as _);
                        let key = self.constant(aux as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Index::new(table.into(), key.into()).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_GETTABLEN => {
                        let value = self.register(a as _);
                        let table = self.register(b as _);
                        let key = ast::Literal::Number((c as usize + 1) as f64);
                        statements.push(
                            ast::Assign::new(
                                vec![value.into()],
                                vec![ast::Index::new(table.into(), key.into()).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_SETTABLE => {
                        let value = self.register(a as _);
                        let table = self.register(b as _);
                        let key = self.register(c as _);
                        statements.push(
                            ast::Assign::new(
                                vec![ast::Index::new(table.into(), key.into()).into()],
                                vec![value.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_SETTABLEKS => {
                        let value = self.register(a as _);
                        let table = self.register(b as _);
                        let key = self.constant(aux as _);
                        statements.push(
                            ast::Assign::new(
                                vec![ast::Index::new(table.into(), key.into()).into()],
                                vec![value.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_SETTABLEN => {
                        let value = self.register(a as _);
                        let table = self.register(b as _);
                        let key = ast::Literal::Number((c as usize + 1) as f64);
                        statements.push(
                            ast::Assign::new(
                                vec![ast::Index::new(table.into(), key.into()).into()],
                                vec![value.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_ADD
                    | OpCode::LOP_SUB
                    | OpCode::LOP_MUL
                    | OpCode::LOP_DIV
                    | OpCode::LOP_MOD
                    | OpCode::LOP_POW
                    | OpCode::LOP_IDIV => {
                        let op = match op_code {
                            OpCode::LOP_ADD => ast::BinaryOperation::Add,
                            OpCode::LOP_SUB => ast::BinaryOperation::Sub,
                            OpCode::LOP_MUL => ast::BinaryOperation::Mul,
                            OpCode::LOP_DIV => ast::BinaryOperation::Div,
                            OpCode::LOP_MOD => ast::BinaryOperation::Mod,
                            OpCode::LOP_POW => ast::BinaryOperation::Pow,
                            OpCode::LOP_IDIV => ast::BinaryOperation::IDiv,
                            _ => unreachable!(),
                        };
                        let target = self.register(a as _);
                        let left = self.register(b as _);
                        let right = self.register(c as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Binary::new(left.into(), right.into(), op).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_ADDK
                    | OpCode::LOP_SUBK
                    | OpCode::LOP_MULK
                    | OpCode::LOP_DIVK
                    | OpCode::LOP_MODK
                    | OpCode::LOP_POWK
                    | OpCode::LOP_IDIVK => {
                        let op = match op_code {
                            OpCode::LOP_ADDK => ast::BinaryOperation::Add,
                            OpCode::LOP_SUBK => ast::BinaryOperation::Sub,
                            OpCode::LOP_MULK => ast::BinaryOperation::Mul,
                            OpCode::LOP_DIVK => ast::BinaryOperation::Div,
                            OpCode::LOP_MODK => ast::BinaryOperation::Mod,
                            OpCode::LOP_POWK => ast::BinaryOperation::Pow,
                            OpCode::LOP_IDIVK => ast::BinaryOperation::IDiv,
                            _ => unreachable!(),
                        };
                        let target = self.register(a as _);
                        let left = self.register(b as _);
                        let right = self.constant(c as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Binary::new(left.into(), right.into(), op).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_NOT | OpCode::LOP_MINUS | OpCode::LOP_LENGTH => {
                        let op = match op_code {
                            OpCode::LOP_NOT => ast::UnaryOperation::Not,
                            OpCode::LOP_MINUS => ast::UnaryOperation::Negate,
                            OpCode::LOP_LENGTH => ast::UnaryOperation::Length,
                            _ => unreachable!(),
                        };
                        let target = self.register(a as _);
                        let value = self.register(b as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Unary::new(value.into(), op).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_RETURN => {
                        let values = if b != 0 {
                            operand_registers(a, b)
                                .map(|r| self.register(r as _).into())
                                .collect()
                        } else {
                            let (tail, end) = take_top(&mut top, &mut pending_pcs, &mut consumed_pcs);
                            (a..end)
                                .map(|r| self.register(r as _).into())
                                .chain(std::iter::once(tail))
                                .collect()
                        };
                        statements.push(ast::Return::new(values).into());
                        stop = true;
                    }
                    // NATIVECALL is a JIT dispatch hint; the actual call is the
                    // following CALL, so (like FASTCALL) it lifts to nothing (L5).
                    OpCode::LOP_NATIVECALL => {}
                    // FASTCALL (builtins) and FASTPCALL (v14 `pcall`/`xpcall`)
                    // only accelerate the CALL at `pc + 1 + C`; the fallback
                    // path loads the callee and calls it, which is exactly the
                    // source form, so they lift to nothing. Remember the CALL so
                    // its callee can be marked as fetched after the arguments
                    // (see `Call::callee_after_arguments`).
                    OpCode::LOP_FASTCALL
                    | OpCode::LOP_FASTCALL1
                    | OpCode::LOP_FASTCALL2
                    | OpCode::LOP_FASTCALL2K
                    | OpCode::LOP_FASTCALL3
                    | OpCode::LOP_FASTPCALL => {
                        fastcall_target = Some(pc + 1 + c as usize);
                    }
                    OpCode::LOP_NAMECALL | OpCode::LOP_NAMECALLUDATA => {
                        let namecall_base = a;
                        let namecall_object = self.register(b as _);
                        // NAMECALL uses the full aux as the method-name constant index;
                        // NAMECALLUDATA stashes a userdata atom in the high 16 bits, so the
                        // index is only the low 16 bits (LUAU_INSN_AUX_KV16).
                        let method_key = if op_code == OpCode::LOP_NAMECALLUDATA {
                            (aux & 0xFFFF) as usize
                        } else {
                            aux as usize
                        };
                        let namecall_method = match self.constant(method_key) {
                            ast::Literal::String(string) => string,
                            _ => unreachable!(),
                        };
                        assert!(matches!(
                            iter.next().unwrap().1,
                            Instruction::BC {
                                op_code: OpCode::LOP_NOP,
                                ..
                            }
                        ));
                        match iter.next().unwrap().1 {
                            &Instruction::BC {
                                // The followup is CALL, or CALLFB on v11 (same A/B/C call
                                // shape). CALLFB's injected aux NOP is consumed by the outer
                                // loop's LOP_NOP arm on the next iteration.
                                op_code: OpCode::LOP_CALL | OpCode::LOP_CALLFB,
                                a,
                                b,
                                c,
                                ..
                            } => {
                                assert!(a == namecall_base);
                                // TODO: repeated code :(
                                let arguments = if b != 0 {
                                    (a + 2..a + b)
                                        .map(|r| self.register(r as _).into())
                                        .collect()
                                } else {
                                    let top = take_top(&mut top, &mut pending_pcs, &mut consumed_pcs);
                                    (a + 2..top.1)
                                        .map(|r| self.register(r as _).into())
                                        .chain(std::iter::once(top.0))
                                        .collect()
                                };

                                // A method no identifier names is refused before lifting.
                                let method = String::from_utf8(namecall_method)
                                    .expect("NAMECALL methods are identifiers");
                                let call: ast::Select =
                                    ast::MethodCall::new(namecall_object.into(), method, arguments).into();

                                if c != 0 {
                                    if c == 1 {
                                        statements.push(match call {
                                            ast::Select::Call(call) => call.into(),
                                            ast::Select::MethodCall(call) => call.into(),
                                            ast::Select::VarArg(_) => unreachable!(),
                                        });
                                    } else {
                                        statements.push(
                                            ast::Assign::new(
                                                operand_registers(a, c)
                                                    .map(|r| self.register(r as _).into())
                                                    .collect(),
                                                vec![ast::RValue::Select(call)],
                                            )
                                            .into(),
                                        );
                                    }
                                } else {
                                    // An open result list keeps every value: the plain call.
                                    let call = match call {
                                        ast::Select::Call(call) => ast::RValue::Call(call),
                                        ast::Select::MethodCall(call) => ast::RValue::MethodCall(call),
                                        ast::Select::VarArg(_) => unreachable!(),
                                    };
                                    top = Some((call, a));
                                    set_pending = true;
                                }
                            }
                            instruction => unreachable!("{:?}", instruction),
                        }
                    }
                    // CALLFB (v11) is CALL with a runtime feedback slot in aux; identical
                    // source-level call. The aux slot id carries no meaning and its injected
                    // NOP is consumed by the LOP_NOP arm on the next iteration.
                    OpCode::LOP_CALL | OpCode::LOP_CALLFB => {
                        let arguments = if b != 0 {
                            (a + 1..a + b)
                                .map(|r| self.register(r as _).into())
                                .collect()
                        } else {
                            let top = take_top(&mut top, &mut pending_pcs, &mut consumed_pcs);
                            (a + 1..top.1)
                                .map(|r| self.register(r as _).into())
                                .chain(std::iter::once(top.0))
                                .collect()
                        };

                        let mut call = ast::Call::new(self.register(a as _).into(), arguments);
                        if fastcall_target.take_if(|target| *target == pc).is_some() {
                            call.callee_after_arguments = true;
                        }

                        if c != 0 {
                            if c == 1 {
                                statements.push(call.into());
                            } else {
                                statements.push(
                                    ast::Assign::new(
                                        operand_registers(a, c)
                                            .map(|r| self.register(r as _).into())
                                            .collect(),
                                        vec![ast::RValue::Select(call.into())],
                                    )
                                    .into(),
                                );
                            }
                        } else {
                            top = Some((call.into(), a));
                            set_pending = true;
                        }
                    }
                    OpCode::LOP_CLOSEUPVALS => {
                        let locals = (a..self.function_list[self.function.id].max_stack_size)
                            .map(|i| self.register(i as _))
                            .collect();
                        statements.push(ast::Close { locals }.into());
                    }
                    OpCode::LOP_SETLIST => {
                        let setlist = if c != 0 {
                            ast::SetList::new(
                                self.register(a as _),
                                aux as usize,
                                operand_registers(b, c)
                                    .map(|r| self.register(r as _).into())
                                    .collect(),
                                None,
                            )
                        } else {
                            let top = take_top(&mut top, &mut pending_pcs, &mut consumed_pcs);
                            ast::SetList::new(
                                self.register(a as _).clone(),
                                aux as usize,
                                (b..top.1).map(|r| self.register(r as _).into()).collect(),
                                Some(top.0),
                            )
                        };
                        statements.push(setlist.into());
                    }
                    OpCode::LOP_CONCAT => {
                        let operands = (b..=c)
                            .map(|r| self.register(r as _))
                            .rev()
                            .collect::<Vec<_>>();
                        assert!(operands.len() >= 2);
                        let mut operands = operands.into_iter();
                        let right = operands.next().unwrap();
                        let left = operands.next().unwrap();
                        let mut concat = ast::Binary::new(
                            left.into(),
                            right.into(),
                            ast::BinaryOperation::Concat,
                        );
                        for r in operands {
                            concat = ast::Binary::new(
                                r.into(),
                                concat.into(),
                                ast::BinaryOperation::Concat,
                            );
                        }
                        statements.push(
                            ast::Assign::new(
                                vec![self.register(a as _).into()],
                                vec![concat.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_AND => statements.push(
                        ast::Assign::new(
                            vec![self.register(a as _).into()],
                            vec![ast::Binary::new(
                                self.register(b as _).into(),
                                self.register(c as _).into(),
                                ast::BinaryOperation::And,
                            )
                            .into()],
                        )
                        .into(),
                    ),
                    OpCode::LOP_ANDK => statements.push(
                        ast::Assign::new(
                            vec![self.register(a as _).into()],
                            vec![ast::Binary::new(
                                self.register(b as _).into(),
                                self.constant(c as _).into(),
                                ast::BinaryOperation::And,
                            )
                            .into()],
                        )
                        .into(),
                    ),
                    OpCode::LOP_OR => statements.push(
                        ast::Assign::new(
                            vec![self.register(a as _).into()],
                            vec![ast::Binary::new(
                                self.register(b as _).into(),
                                self.register(c as _).into(),
                                ast::BinaryOperation::Or,
                            )
                            .into()],
                        )
                        .into(),
                    ),
                    OpCode::LOP_ORK => statements.push(
                        ast::Assign::new(
                            vec![self.register(a as _).into()],
                            vec![ast::Binary::new(
                                self.register(b as _).into(),
                                self.constant(c as _).into(),
                                ast::BinaryOperation::Or,
                            )
                            .into()],
                        )
                        .into(),
                    ),
                    OpCode::LOP_GETVARARGS => {
                        let vararg = ast::VarArg {};
                        if b != 0 {
                            statements.push(
                                ast::Assign::new(
                                    operand_registers(a, b)
                                        .map(|r| self.register(r as _).into())
                                        .collect(),
                                    vec![ast::RValue::Select(vararg.into())],
                                )
                                .into(),
                            );
                        } else {
                            top = Some((vararg.into(), a));
                            set_pending = true;
                        }
                    }
                    OpCode::LOP_NOP => {}
                    OpCode::LOP_SUBRK | OpCode::LOP_DIVRK => {
                        let op = match op_code {
                            OpCode::LOP_SUBRK => ast::BinaryOperation::Sub,
                            OpCode::LOP_DIVRK => ast::BinaryOperation::Div,
                            _ => unreachable!(),
                        };
                        let target = self.register(a as _);
                        let left = self.constant(b as _);
                        let right = self.register(c as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Binary::new(left.into(), right.into(), op).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_GETUDATAKS => {
                        // Userdata field read by constant string key; same shape as
                        // GETTABLEKS. The key index is the low 16 bits of aux.
                        let target = self.register(a as _);
                        let table = self.register(b as _);
                        let key = self.constant((aux & 0xFFFF) as _);
                        statements.push(
                            ast::Assign::new(
                                vec![target.into()],
                                vec![ast::Index::new(table.into(), key.into()).into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_SETUDATAKS => {
                        // Userdata field write by constant string key; same shape as
                        // SETTABLEKS. The key index is the low 16 bits of aux.
                        let value = self.register(a as _);
                        let table = self.register(b as _);
                        let key = self.constant((aux & 0xFFFF) as _);
                        statements.push(
                            ast::Assign::new(
                                vec![ast::Index::new(table.into(), key.into()).into()],
                                vec![value.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_NEWCLASSMEMBER => {
                        // Experimental Luau Classes member registration, rendered as a field
                        // assignment `class[name] = value`. B is reserved; the member name is
                        // the full-aux constant string and the value is register C.
                        let table = self.register(a as _);
                        let key = self.constant(aux as _);
                        let value = self.register(c as _);
                        statements.push(
                            ast::Assign::new(
                                vec![ast::Index::new(table.into(), key.into()).into()],
                                vec![value.into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_LOADKX => {
                        // LOADK whose constant index exceeds the LOADK D field, so
                        // the full index is carried in `aux` (the deserializer lists
                        // LOADKX among the aux-bearing ops, decoded BC-form). Stock
                        // Luau emits it for a proto with > 32768 constants. Without an
                        // arm it fell to the catch-all `unreachable!` and aborted the
                        // whole proto (L2). Otherwise identical to LOADK. The injected
                        // aux NOP is consumed by the `LOP_NOP` arm next iteration.
                        let constant = self.constant(aux as _);
                        let target = self.register(a as _);
                        statements
                            .push(ast::Assign::new(vec![target.into()], vec![constant.into()]).into());
                    }
                    _ => unreachable!("{:?}", instruction),
                },
                Instruction::AD { op_code, a, d, aux } => match op_code {
                    OpCode::LOP_LOADK => {
                        let constant = self.constant(d as _);
                        let target = self.register(a as _);
                        let statement =
                            ast::Assign::new(vec![target.into()], vec![constant.into()]);
                        statements.push(statement.into());
                    }
                    OpCode::LOP_LOADN => {
                        let target = self.register(a as _);
                        let statement = ast::Assign::new(vec![target.into()], vec![
                            ast::Literal::Number(d as _).into(),
                        ]);
                        statements.push(statement.into());
                    }
                    OpCode::LOP_GETIMPORT => {
                        let target = self.register(a as _);
                        let import_len = (aux >> 30) & 3;
                        assert!(import_len <= 3);
                        let mut import_expression: ast::RValue = ast::Global::new(
                            self.constant(((aux >> 20) & 1023) as usize)
                                .into_string()
                                .unwrap(),
                        )
                        .into();
                        if import_len > 1 {
                            import_expression = ast::Index::new(
                                import_expression,
                                self.constant(((aux >> 10) & 1023) as usize).into(),
                            )
                            .into();
                        }
                        if import_len > 2 {
                            import_expression = ast::Index::new(
                                import_expression,
                                self.constant((aux & 1023) as usize).into(),
                            )
                            .into();
                        }
                        let assign = ast::Assign::new(vec![target.into()], vec![import_expression]);
                        statements.push(assign.into());
                    }
                    OpCode::LOP_JUMPIFNOT => {
                        let condition = self.register(a as _);
                        let statement = ast::If::new(
                            condition.into(),
                            ast::Block::default(),
                            ast::Block::default(),
                        );
                        edges.push((
                            self.block_to_node(block_start + index + 1),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Else),
                        ));
                        statements.push(statement.into());
                    }
                    OpCode::LOP_JUMPIF => {
                        let condition = self.register(a as _);
                        let statement = ast::If::new(
                            condition.into(),
                            ast::Block::default(),
                            ast::Block::default(),
                        );
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(block_start + index + 1),
                            BlockEdge::new(BranchType::Else),
                        ));
                        statements.push(statement.into());
                    }
                    OpCode::LOP_JUMPIFNOTEQ => {
                        let a = self.register(a as _);
                        let aux = self.register(aux as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(a.into(), aux.into(), ast::BinaryOperation::Equal)
                                    .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        edges.push((
                            self.block_to_node(block_start + index + 2),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_JUMPIFNOTLE => {
                        let a = self.register(a as _);
                        let aux = self.register(aux as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    aux.into(),
                                    ast::BinaryOperation::LessThanOrEqual,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        edges.push((
                            self.block_to_node(block_start + index + 2),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_JUMPIFNOTLT => {
                        let a = self.register(a as _);
                        let aux = self.register(aux as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    aux.into(),
                                    ast::BinaryOperation::LessThan,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        edges.push((
                            self.block_to_node(block_start + index + 2),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_JUMPIFEQ => {
                        let a = self.register(a as _);
                        let aux = self.register(aux as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(a.into(), aux.into(), ast::BinaryOperation::Equal)
                                    .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(block_start + index + 2),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_JUMPIFLE => {
                        let a = self.register(a as _);
                        let aux = self.register(aux as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    aux.into(),
                                    ast::BinaryOperation::LessThanOrEqual,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(block_start + index + 2),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_JUMPIFLT => {
                        let a = self.register(a as _);
                        let aux = self.register(aux as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    aux.into(),
                                    ast::BinaryOperation::LessThan,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Then),
                        ));
                        edges.push((
                            self.block_to_node(block_start + index + 2),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_JUMPBACK | OpCode::LOP_JUMP => {
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + d as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Unconditional),
                        ));
                    }
                    OpCode::LOP_JUMPXEQKNIL => {
                        let a = self.register(a as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    ast::Literal::Nil.into(),
                                    ast::BinaryOperation::Equal,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        if aux & (1 << 31) != 0 {
                            edges.push((
                                self.block_to_node(
                                    ((block_start + index + 1) as isize + d as isize) as usize,
                                ),
                                BlockEdge::new(BranchType::Else),
                            ));
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Then),
                            ));
                        } else {
                            edges.push((
                                self.block_to_node(
                                    ((block_start + index + 1) as isize + d as isize) as usize,
                                ),
                                BlockEdge::new(BranchType::Then),
                            ));
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Else),
                            ));
                        }
                    }
                    OpCode::LOP_JUMPXEQKB => {
                        let a = self.register(a as _);
                        let literal = if aux & 1 != 0 {
                            ast::Literal::Boolean(true)
                        } else {
                            ast::Literal::Boolean(false)
                        };
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    literal.into(),
                                    ast::BinaryOperation::Equal,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        if aux & (1 << 31) != 0 {
                            edges.push((
                                self.block_to_node(
                                    ((block_start + index + 1) as isize + d as isize) as usize,
                                ),
                                BlockEdge::new(BranchType::Else),
                            ));
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Then),
                            ));
                        } else {
                            edges.push((
                                self.block_to_node(
                                    ((block_start + index + 1) as isize + d as isize) as usize,
                                ),
                                BlockEdge::new(BranchType::Then),
                            ));
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Else),
                            ));
                        }
                    }
                    OpCode::LOP_JUMPXEQKN | OpCode::LOP_JUMPXEQKS => {
                        let a = self.register(a as _);
                        let literal = self.constant((aux & ((1 << 24) - 1)) as _);
                        statements.push(
                            ast::If::new(
                                ast::Binary::new(
                                    a.into(),
                                    literal.into(),
                                    ast::BinaryOperation::Equal,
                                )
                                .into(),
                                ast::Block::default(),
                                ast::Block::default(),
                            )
                            .into(),
                        );
                        if aux & (1 << 31) != 0 {
                            edges.push((
                                self.block_to_node(
                                    ((block_start + index + 1) as isize + d as isize) as usize,
                                ),
                                BlockEdge::new(BranchType::Else),
                            ));
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Then),
                            ));
                        } else {
                            edges.push((
                                self.block_to_node(
                                    ((block_start + index + 1) as isize + d as isize) as usize,
                                ),
                                BlockEdge::new(BranchType::Then),
                            ));
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Else),
                            ));
                        }
                    }
                    OpCode::LOP_FORNPREP => {
                        // TODO: do this properly
                        let limit = self.register(a as _);
                        let step = self.register((a + 1) as _);
                        let counter = self.register((a + 2) as _);
                        statements.push(ast::NumForInit::new(counter, limit, step).into());

                        // The loop header is the block of the matching FORNLOOP,
                        // which tests the counter (a FORNLOOP starts its own
                        // block). Luau lays a loop out as `FORNPREP exit; body;
                        // FORNLOOP body; exit:` and keeps the loop's registers
                        // live through the body, so no loop inside reuses them:
                        // it is the first FORNLOOP on the same registers after
                        // FORNPREP, whatever the body does (`for ... do break end`
                        // threads the back jump to the exit) and wherever a later
                        // loop reuses the registers. Jump folding retargets
                        // FORNPREP through the forward JUMPs after the loop (and
                        // copies a RETURN such a JUMP reaches), so the exit is
                        // only known to follow the FORNLOOP.
                        let body_node = self.block_to_node(pc + 1);
                        let instructions = &self.function_list[self.function.id].instructions;
                        let exit = loop_jump_target(instructions, pc, d);
                        let loop_node = (pc + 1..exit.min(instructions.len()))
                            .find(|&at| matches!(instructions[at],
                                Instruction::AD { op_code: OpCode::LOP_FORNLOOP, a: loop_a, .. } if loop_a == a))
                            .and_then(|loop_pc| self.blocks.get(&loop_pc).copied())
                            .expect("FORNPREP: no FORNLOOP on its registers before its exit");
                        // The compiler can thread FORNLOOP's backedge through
                        // an empty `break` body, even when the step is dead.
                        // FORNPREP still enters that body on its first trip.
                        // Preserve this otherwise-erased body port so the
                        // structurer can distinguish break from exhaustion.
                        if body_node != loop_node
                            && self.function.block(body_node).is_some_and(|block| block.is_empty())
                            && self.function.unconditional_edge(body_node)
                                .zip(self.function.conditional_edges(loop_node))
                                .is_some_and(|(body_edge, (then_edge, _))|
                                    then_edge.target() == body_edge.target() && then_edge.target() != body_node)
                        {
                            let mut loop_edges = self.function.remove_edges(loop_node);
                            for (target, edge) in &mut loop_edges {
                                if edge.branch_type == BranchType::Then { *target = body_node; }
                            }
                            self.function.set_edges(loop_node, loop_edges);
                        }
                        edges.push((loop_node, BlockEdge::new(BranchType::Unconditional)));
                    }
                    OpCode::LOP_FORNLOOP => {
                        let limit = self.register(a as _);
                        let step = self.register((a + 1) as _);
                        let counter = self.register((a + 2) as _);
                        statements
                            .push(ast::NumForNext::new(counter, limit.into(), step.into()).into());
                        let instructions = &self.function_list[self.function.id].instructions;
                        let body = loop_jump_target(instructions, block_start + index, d);
                        edges.push((self.block_to_node(body), BlockEdge::new(BranchType::Then)));
                        edges.push((
                            self.block_to_node(block_start + index + 1),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_FORGPREP
                    | OpCode::LOP_FORGPREP_INEXT
                    | OpCode::LOP_FORGPREP_NEXT => {
                        let prep_pc = block_start + index;
                        let generator = self.register(a as _);
                        let state = self.register((a + 1) as _);
                        let counter = self.register((a + 2) as _);
                        let instructions = &self.function_list[self.function.id].instructions;
                        let loop_index = loop_jump_target(instructions, prep_pc, d);
                        let origin = *self
                            .for_origins_by_step
                            .get(&loop_index)
                            .expect("generic prep provenance map missing FORGLOOP partner");
                        statements.push(
                            ast::GenericForInit::new_with_origin(generator, state, counter, origin)
                                .into(),
                        );
                        edges.push((
                            self.block_to_node(loop_index),
                            BlockEdge::new(BranchType::Unconditional),
                        ));
                    }
                    // TODO: i think vm can assume generator is next/inext based on aux,
                    // so what happens if the generator passed isnt next and the env isnt tainted?
                    // this could be done with some custom bytecode
                    // same applies to fastcall
                    OpCode::LOP_FORGLOOP => {
                        let step_pc = block_start + index;
                        let generator = self.register(a as _);
                        let state = self.register((a + 1) as _);
                        let result_count = (aux & 0xff) as usize;
                        assert!(result_count > 0, "FORGLOOP has zero result locals");
                        let origin = self
                            .for_origins_by_step
                            .get(&step_pc)
                            .copied()
                            .filter(|origin| {
                                origin.base_register == a
                                    && origin.result_count as usize == result_count
                            });
                        let mut next = ast::GenericForNext::new(
                            (a as usize + 3..a as usize + 3 + result_count)
                                .map(|r| self.register(r))
                                .collect::<Vec<_>>(),
                            generator.into(),
                            state,
                            self.register((a + 2) as _),
                        );
                        next.origin = origin;
                        statements.push(next.into());
                        let instructions = &self.function_list[self.function.id].instructions;
                        let body = loop_jump_target(instructions, block_start + index, d);
                        edges.push((self.block_to_node(body), BlockEdge::new(BranchType::Then)));
                        edges.push((
                            self.block_to_node(block_start + index + 1),
                            BlockEdge::new(BranchType::Else),
                        ));
                    }
                    OpCode::LOP_DUPTABLE => {
                        // Loader templates contain actual zero-valued fields,
                        // observable even if no SETTABLE ever follows DUPTABLE.
                        let template = self.template_to_rvalue(d as usize);
                        statements.push(
                            ast::Assign::new(vec![self.register(a as _).into()], vec![template])
                                .into(),
                        );
                    }
                    OpCode::LOP_DUPCLOSURE | OpCode::LOP_NEWCLOSURE => {
                        let constructor_pc = block_start + index;
                        let dest_local = self.register(a as _);
                        let func_index = match op_code {
                            OpCode::LOP_NEWCLOSURE => {
                                self.function_list[self.function.id].functions[d as usize]
                            }
                            OpCode::LOP_DUPCLOSURE => match self.function_list[self.function.id]
                                .constants
                                .get(d as usize)
                                .unwrap()
                            {
                                &BytecodeConstant::Closure(func_index) => func_index,
                                _ => unreachable!(),
                            },
                            _ => unreachable!(),
                        };
                        let func_name_index = self.function_list[func_index].function_name;
                        let func_name = if func_name_index == 0 {
                            None
                        } else {
                            Some(
                                String::from_utf8_lossy(&self.string_table[func_name_index - 1])
                                    .into_owned(),
                            )
                        };

                        let func = &self.function_list[func_index];
                        let mut upvalues_passed = Vec::with_capacity(func.num_upvalues.into());
                        for _ in 0..func.num_upvalues {
                            let local = match iter.next().as_ref().unwrap().1 {
                                &Instruction::BC {
                                    op_code: OpCode::LOP_CAPTURE,
                                    a: capture_type,
                                    b: source,
                                    ..
                                } => match capture_type {
                                    // capture value
                                    0 => ast::Upvalue::Copy(self.register(source as _)),
                                    // capture ref
                                    1 => ast::Upvalue::Ref(self.register(source as _)),
                                    // capture upval
                                    2 => ast::Upvalue::Ref(self.upvalues[source as usize].clone()),
                                    _ => unreachable!(),
                                },
                                _ => unreachable!(),
                            };
                            upvalues_passed.push(local);
                        }

                        let function = Arc::<Mutex<ast::Function>>::default();
                        let child_function_id = self.static_function_id.as_ref().map(|parent_id| {
                            format!("{parent_id}/p{}@pc{constructor_pc}:p{func_index}", self.function.id)
                        });
                        self.child_functions.push((
                            ByAddress(function.clone()),
                            func_index,
                            child_function_id.clone(),
                        ));
                        {
                            let mut lifted_function = function.lock();
                            lifted_function.bytecode_proto_id = Some(func_index);
                            lifted_function.bytecode_function_id = child_function_id;
                            lifted_function.retain_for_reconstruction =
                                crate::reconstruction_candidates::retain(func, func_name.as_deref());
                            lifted_function.name = func_name;
                        }
                        statements.push(
                            ast::Assign::new(vec![dest_local.into()], vec![
                                ast::Closure {
                                    node_origin: Default::default(),
                                    function: ByAddress(function),
                                    upvalues: upvalues_passed,
                                }
                                .into(),
                            ])
                            .into(),
                        );
                    }
                    OpCode::LOP_CMPPROTO => {
                        unreachable!("CMPPROTO must be rejected before lifting");
                    }
                    _ => unreachable!("{:?}", instruction),
                },
                Instruction::E { op_code, e } => match op_code {
                    OpCode::LOP_JUMPX => {
                        edges.push((
                            self.block_to_node(
                                ((block_start + index + 1) as isize + e as isize) as usize,
                            ),
                            BlockEdge::new(BranchType::Unconditional),
                        ));
                    }
                    // A non-JUMPX E-form op (e.g. LOP_COVERAGE in instrumented
                    // builds) has no source-level effect. Emit a marker and fall
                    // through instead of aborting the whole function (L4); the
                    // `edges.is_empty()` block below wires the natural successor.
                    _ => {
                        statements.push(
                            ast::Comment::new(format!("unhandled E-form op {:?}", op_code)).into(),
                        );
                    }
                },
                _ => unimplemented!("{:?}", instruction),
            }
            if track_statement_pcs {
                statement_pcs.resize(statements.len(), pc);
                debug_assert!(statement_pcs.len() >= lifted_before);
            }
            if trace_origins && (statements.len() != lifted_before || set_pending) {
                let next_pc = iter.clone().next().map_or(block_end + 1, |(index, _)| block_start + index);
                consumed_pcs.extend((pc..next_pc).filter(|&at| at == pc || !matches!(
                    self.function_list[self.function.id].instructions[at], Instruction::BC { op_code: OpCode::LOP_NOP, .. })));
                consumed_pcs.sort_unstable();
                consumed_pcs.dedup();
                if set_pending { pending_pcs = consumed_pcs.clone(); }
                statement_origins.resize(statements.len(), consumed_pcs);
            }
            if stop { break; }
        }

        let last_index = iter
            .next()
            .map(|(i, _)| block_start + i - 1)
            .unwrap_or(block_end);
        if edges.is_empty()
            && !Self::is_terminator(self.function_list[self.function.id].instructions[last_index])
        {
            if last_index + 1 == self.function_list[self.function.id].instructions.len() {
                statements
                    .push(ast::Comment::new("warning: block does not return".to_string()).into());
            } else {
                edges.push((
                    self.block_to_node(last_index + 1),
                    BlockEdge::new(BranchType::Unconditional),
                ));
            }
        }

        // The trailing "block does not return" marker writes no local.
        if track_statement_pcs { statement_pcs.resize(statements.len(), block_end); }
        // A generated warning has no source instruction; preserve that unknown.
        if trace_origins { statement_origins.resize(statements.len(), Vec::new()); }
        (statements, edges, statement_pcs, statement_origins)
    }

    fn register(&mut self, index: usize) -> ast::RcLocal {
        if index >= self.register_map.len() {
            // Keep direct Lifter callers compatible with the old sparse map;
            // validated bytecode already fits its declared register stack.
            self.register_map.resize_with(index + 1, || None);
        }
        let local = match &mut self.register_map[index] {
            Some(local) => local.clone(),
            slot @ None => {
                let local = ast::RcLocal::default();
                if !self.debug_bindings.is_empty() || !self.typed_locals.is_empty() {
                    if let Ok(register) = u8::try_from(index) {
                        self.register_of.insert(local.stable_id(), register);
                    }
                }
                *slot = Some(local.clone());
                local
            }
        };
        if let Some(trace) = &mut self.function.provenance { trace.register(&local, index, "register"); }
        local
    }

    fn constant(&self, index: usize) -> ast::Literal {
        // Numeric conversions are copies, and each string use must own its
        // bytes anyway. Caching a Literal adds a hash lookup and duplicates the
        // first string allocation without sharing any subsequent AST storage.
        match self.function_list[self.function.id]
            .constants
            .get(index)
            .unwrap()
        {
            BytecodeConstant::Nil => ast::Literal::Nil,
            BytecodeConstant::Boolean(v) => ast::Literal::Boolean(*v),
            BytecodeConstant::Number(v) => ast::Literal::Number(*v),
            BytecodeConstant::String(v) => {
                // A string-constant index of 0 is the "no string" sentinel (the
                // same `== 0` convention the function-name path uses). `v - 1` would
                // underflow and `string_table[usize::MAX]` panic the whole proto
                // (L6); decode it as the empty string instead.
                #[cfg(feature = "byte-storage-trace")]
                {
                    // GETGLOBAL/SETGLOBAL/GETIMPORT and NAMECALL move this
                    // buffer into their final name type without another copy.
                    // Count their initial materialization here exactly once.
                    let length = if *v == 0 { 0 } else { self.string_table[*v - 1].len() };
                    ast::telemetry::count("byte_lift_string_copy_calls", 1);
                    ast::telemetry::count("byte_lift_string_copy_bytes", length as u64);
                    ast::telemetry::count("byte_lift_string_copy_nonempty", u64::from(length != 0));
                }
                if *v == 0 {
                    ast::Literal::String(Vec::new())
                } else {
                    ast::Literal::String(self.string_table[*v - 1].to_vec())
                }
            }
            BytecodeConstant::Vector(x, y, z, _) => ast::Literal::Vector(*x, *y, *z),
            BytecodeConstant::VectorD(x, y, z, _) => ast::Literal::VectorD(*x, *y, *z),
            BytecodeConstant::Integer(v) => ast::Literal::Integer(*v),
            _ => unimplemented!(),
        }
    }

    /// The table a DUPTABLE copies, as a constructor. The loader builds
    /// constants in order, so a template entry reads the constants before it:
    /// one naming a later constant reads nil, as `{field = nil}` spells (the
    /// field stays out, as in the source). Luau only ever bakes scalar
    /// constants into a template; a nested table or a closure would be shared
    /// by every copy, which no constructor spells, so such bytecode is refused.
    fn template_to_rvalue(&self, index: usize) -> ast::RValue {
        let constants = &self.function_list[self.function.id].constants;
        let scalar = |at: usize| matches!(constants.get(at), Some(BytecodeConstant::Boolean(_)
            | BytecodeConstant::Number(_) | BytecodeConstant::Integer(_) | BytecodeConstant::String(_)
            | BytecodeConstant::Vector(..) | BytecodeConstant::VectorD(..)));
        let key = |at: usize| -> ast::RValue {
            assert!(at < index && scalar(at), "DUPTABLE template key {at} is not an earlier scalar constant");
            self.constant(at).into()
        };
        let entries = match constants.get(index) {
            Some(BytecodeConstant::TableWithConstants(pairs)) => pairs
                .iter()
                .map(|&(at, value)| {
                    let value = match usize::try_from(value) {
                        Err(_) => ast::Literal::Number(0.0).into(),
                        Ok(value) if value >= index || matches!(constants.get(value), Some(BytecodeConstant::Nil)) => {
                            ast::Literal::Nil.into()
                        }
                        Ok(value) => {
                            assert!(scalar(value), "DUPTABLE template value {value} is not a scalar constant");
                            self.constant(value).into()
                        }
                    };
                    (Some(key(at)), value)
                })
                .collect(),
            Some(BytecodeConstant::Table(keys)) => {
                keys.iter().map(|&at| (Some(key(at)), ast::Literal::Number(0.0).into())).collect()
            }
            _ => panic!("DUPTABLE constant {index} is not a table template"),
        };
        ast::Table::new(entries).into()
    }

    fn block_to_node(&self, insn_index: usize) -> NodeIndex {
        *self.blocks.get(&insn_index).unwrap()
    }

    fn is_terminator(instruction: Instruction) -> bool {
        match instruction {
            Instruction::BC { op_code, c, .. } => match op_code {
                OpCode::LOP_RETURN => true,
                OpCode::LOP_LOADB if c != 0 => true,
                _ => false,
            },
            Instruction::AD { op_code, .. } => matches!(
                op_code,
                OpCode::LOP_JUMP
                    | OpCode::LOP_JUMPBACK
                    | OpCode::LOP_JUMPIF
                    | OpCode::LOP_JUMPIFNOT
                    | OpCode::LOP_JUMPIFEQ
                    | OpCode::LOP_JUMPIFLE
                    | OpCode::LOP_JUMPIFLT
                    | OpCode::LOP_JUMPIFNOTEQ
                    | OpCode::LOP_JUMPIFNOTLE
                    | OpCode::LOP_JUMPIFNOTLT
                    | OpCode::LOP_JUMPXEQKNIL
                    | OpCode::LOP_JUMPXEQKB
                    | OpCode::LOP_JUMPXEQKN
                    | OpCode::LOP_JUMPXEQKS
                    | OpCode::LOP_FORNPREP
                    | OpCode::LOP_FORNLOOP
                    | OpCode::LOP_FORGPREP
                    | OpCode::LOP_FORGLOOP
                    | OpCode::LOP_FORGPREP_INEXT
                    | OpCode::LOP_FORGPREP_NEXT
            ),
            Instruction::E { op_code, .. } => matches!(op_code, OpCode::LOP_JUMPX),
        }
    }
}

/// Where the loop instruction at `pc` jumping by `offset` lands. A target
/// too far for the 16-bit offset is reached through a trampoline Luau emits
/// right before the instruction: `JUMP +1; JUMPX target; <instruction>`.
fn loop_jump_target(instructions: &[Instruction], pc: usize, offset: i16) -> usize {
    let target = ((pc + 1) as isize + offset as isize) as usize;
    let trampoline = target + 1 == pc
        && target.checked_sub(1).and_then(|skip| instructions.get(skip))
            .is_some_and(|skip| matches!(skip, Instruction::AD { op_code: OpCode::LOP_JUMP, d: 1, .. }));
    match instructions.get(target) {
        Some(&Instruction::E { op_code: OpCode::LOP_JUMPX, e }) if trampoline => ((target + 1) as isize + e as isize) as usize,
        _ => target,
    }
}

/// The `encoded - 1` registers from `first`, for an operand that counts one
/// more than its values (Luau's `B` and `C`). Widened first: `R254` alone is
/// `254 + 2 - 1`, past `u8` before the subtraction.
fn operand_registers(first: u8, encoded: u8) -> std::ops::Range<usize> {
    usize::from(first)..usize::from(first) + usize::from(encoded) - 1
}

#[cfg(test)]
mod tests {
    use super::{Instruction, Lifter, OpCode};

    fn prototype(parameters: u8, instructions: Vec<Instruction>) -> super::BytecodeFunction {
        super::BytecodeFunction { max_stack_size: 8, num_parameters: parameters, num_upvalues: 0,
            is_vararg: false, instructions, constants: vec![], functions: vec![], line_defined: 0,
            function_name: 0, line_gap_log2: None, line_info_delta: None, abs_line_info_delta: None,
            has_debug_info: false, debug_locals: vec![], debug_upvalue_name_indices: vec![], type_info: None }
    }

    fn instruction(op_code: OpCode, a: u8, b: u8, c: u8) -> Instruction {
        Instruction::BC { op_code, a, b, c, aux: 0 }
    }

    /// `R254` alone (`A = 254`, `C = 2`) must not overflow while counting.
    #[test]
    fn operand_registers_reach_the_last_register() {
        assert_eq!(super::operand_registers(254, 2), 254..255);
        assert_eq!(super::operand_registers(0, 1), 0..0);
    }

    #[test]
    fn register_ids_follow_first_use_in_reverse_block_order() {
        let ad = |op_code, a, d| Instruction::AD { op_code, a, d, aux: 0 };
        let mut proto = prototype(1, vec![
            ad(OpCode::LOP_JUMPIF, 0, 2),
            ad(OpCode::LOP_LOADN, 254, 1),
            ad(OpCode::LOP_JUMP, 0, 2),
            ad(OpCode::LOP_LOADN, 4, 2),
            ad(OpCode::LOP_LOADN, 4, 3),
            instruction(OpCode::LOP_RETURN, 0, 2, 0),
        ]);
        // Cover both the full declared range and direct callers with an
        // undersized stack. Neither may eagerly mint IDs for unused slots.
        for stack_size in [255, 1, 0] {
            proto.max_stack_size = stack_size;
            let base = ast::current_local_id();
            let protos = vec![proto];
            let (function, _, _) = Lifter::lift(&protos, &vec![], 9, 0,
                Some("root:p0".into()), &[], true);
            let trace = function.provenance.as_ref().unwrap();
            let registers = trace.registers.values()
                .map(|register| (register.slot, register.id - base)).collect::<Vec<_>>();
            assert_eq!(registers, vec![(0, 0), (4, 1), (254, 2)]);
            assert_eq!(ast::current_local_id() - base, 3);
            let mut protos = protos;
            proto = protos.pop().unwrap();
        }
    }

    #[test]
    fn lifted_string_literals_own_arbitrary_bytes_after_input_is_dropped() {
        let function = {
            let input = vec![0, 0xff, 0x80, b'a', b'\n'];
            let mut proto = prototype(0, vec![
                Instruction::AD { op_code: OpCode::LOP_LOADK, a: 0, d: 0, aux: 0 },
                instruction(OpCode::LOP_RETURN, 0, 2, 0),
            ]);
            proto.constants = vec![super::BytecodeConstant::String(1)];
            Lifter::lift(&vec![proto], &[input.as_slice()], 9, 0, None, &[], false).0
        };
        let literals = function.graph().node_weights().flat_map(|block| block.iter())
            .filter_map(|statement| statement.as_assign())
            .flat_map(|assign| assign.right.iter())
            .filter_map(|value| value.as_literal().and_then(ast::Literal::as_string))
            .collect::<Vec<_>>();
        assert_eq!(literals, vec![&vec![0, 0xff, 0x80, b'a', b'\n']]);
    }

    #[test]
    fn repeated_constants_preserve_bits_and_template_defaults() {
        use super::BytecodeConstant as C;
        use ast::Literal as L;
        let nan = f64::from_bits(0x7ff8_0000_0000_1234);
        let mut proto = prototype(0, vec![]);
        proto.constants = vec![
            C::Nil, C::Boolean(true), C::Number(nan), C::Number(f64::INFINITY),
            C::Number(f64::NEG_INFINITY), C::Number(-0.0), C::String(0), C::String(1),
            C::Vector(1.0, 2.0, 3.0, 99.0), C::VectorD(1.0000000000001, 2.0, 3.0, 99.0),
            C::Integer(i64::MIN), C::Table(vec![7]),
            // A value the loader has not built yet (itself, past the pool) or
            // a nil one reads nil.
            C::TableWithConstants(vec![(7, 2), (6, -1), (1, 99), (8, 0), (5, 12)]),
        ];
        // Repeated loads exercise both the former cold-cache and warm-cache
        // behavior, with every non-table literal and both table encodings.
        for _ in 0..2 {
            for index in 0..proto.constants.len() {
                proto.instructions.push(Instruction::AD {
                    op_code: if index < 11 { OpCode::LOP_LOADK } else { OpCode::LOP_DUPTABLE },
                    a: 0, d: index as i16, aux: 0,
                });
            }
        }
        proto.instructions.push(instruction(OpCode::LOP_RETURN, 0, 2, 0));
        let (function, _, _) = Lifter::lift(&vec![proto], &[b"value"],
            13, 0, None, &[], false);
        let values = function.graph().node_weights().flat_map(|block| block.iter())
            .filter_map(|statement| statement.as_assign().map(|assign| &assign.right[0]))
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 26);
        let expected = [L::Nil, L::Boolean(true), L::Number(nan), L::Number(f64::INFINITY),
            L::Number(f64::NEG_INFINITY), L::Number(-0.0), L::String(vec![]),
            L::String(b"value".to_vec()), L::Vector(1.0, 2.0, 3.0),
            L::VectorD(1.0000000000001, 2.0, 3.0), L::Integer(i64::MIN)];
        for loaded in values.chunks_exact(13) {
            for (value, expected) in loaded.iter().zip(&expected) {
                match (value.as_literal().unwrap(), expected) {
                    (L::Number(actual), L::Number(expected)) => assert_eq!(actual.to_bits(), expected.to_bits()),
                    (actual, expected) => assert_eq!(actual, expected),
                }
            }
            let plain = ast::Table::new(vec![
                (Some(L::String(b"value".to_vec()).into()), L::Number(0.0).into()),
            ]);
            assert_eq!(loaded[11], &ast::RValue::from(plain.clone()));
            assert_eq!(loaded[12], &ast::RValue::from(ast::Table::new(vec![
                (Some(L::String(b"value".to_vec()).into()), L::Number(nan).into()),
                (Some(L::String(vec![]).into()), L::Number(0.0).into()),
                (Some(L::Boolean(true).into()), L::Nil.into()),
                (Some(L::Vector(1.0, 2.0, 3.0).into()), L::Nil.into()),
                (Some(L::Number(-0.0).into()), L::Nil.into()),
            ])));
        }
    }

    /// Every copy of a template shares a table or closure baked into it, and
    /// a template naming itself as a key has no key yet: no constructor
    /// spells either, so lifting refuses instead of inventing one.
    #[test]
    fn templates_sharing_objects_or_naming_themselves_are_refused() {
        use super::BytecodeConstant as C;
        let shapes = [
            vec![C::String(1), C::Table(vec![0]), C::TableWithConstants(vec![(0, 1)])],
            vec![C::String(1), C::TableWithConstants(vec![(1, -1)])],
            vec![C::String(1), C::Closure(0), C::TableWithConstants(vec![(0, 1)])],
        ];
        for constants in shapes {
            let template = constants.len() - 1;
            let mut proto = prototype(0, vec![
                Instruction::AD { op_code: OpCode::LOP_DUPTABLE, a: 0, d: template as i16, aux: 0 },
                instruction(OpCode::LOP_RETURN, 0, 2, 0),
            ]);
            proto.constants = constants;
            let lifted = std::panic::catch_unwind(|| Lifter::lift(&vec![proto], &[b"key"], 9, 0, None, &[], false));
            assert!(lifted.is_err());
        }
    }

    #[test]
    fn indexed_metadata_matches_linear_initializers_and_first_typed_hint() {
        use crate::deserializer::function::DebugLocal;
        for seed in 0..12 {
            let mut instructions = (0..24).map(|pc| Instruction::AD {
                op_code: OpCode::LOP_LOADN, a: (pc % 3) as u8, d: pc, aux: 0,
            }).collect::<Vec<_>>();
            instructions.push(instruction(OpCode::LOP_RETURN, 0, 4, 0));
            let mut proto = prototype(2, instructions);
            // Unsorted, overlapping, out-of-bounds and zero-length metadata.
            proto.debug_locals = (0..20).rev().map(|i| DebugLocal {
                name_index: 1 + i % 3, register: ((i + seed) % 5) as u8,
                start_pc: (i * 5 + seed) % 27,
                end_pc: (i * 5 + seed) % 27 + i % 7,
            }).collect();
            let typed = (0..24).rev().map(|i| super::TypedLocalHint {
                register: (i % 3) as u8, start_pc: i / 2,
                end_pc: i + 4, name: format!("type{i}"),
            }).collect::<Vec<_>>();
            let protos = vec![proto];
            let strings: Vec<&[u8]> = vec![b"first", b"second", b"for"];
            let (function, _, _) = Lifter::lift(&protos, &strings, 9, 0, Some("root:p0".into()), &typed, true);
            let trace = function.provenance.as_ref().unwrap();
            let valid = |debug: &&DebugLocal| debug.start_pc < debug.end_pc
                && debug.end_pc <= 25 && debug.register < 8 && debug.name_index != 3;
            for site in trace.lifted.values() {
                let node = petgraph::stable_graph::NodeIndex::new(site.block);
                let pc = site.pcs[0];
                for (write, id) in site.written_registers.iter().enumerate() {
                    let register = trace.registers[id].slot as u8;
                    let expected = protos[0].debug_locals.iter().filter(|debug| {
                        if debug.register != register { return false; }
                        if debug.start_pc <= pc && pc < debug.end_pc { return true; }
                        if debug.start_pc > 25 { return false; }
                        trace.lifted.values().filter(|def| def.block == site.block
                            && def.pcs[0] < debug.start_pc
                            && def.written_registers.contains(id)).last()
                            .is_some_and(|def| def.index == site.index)
                    }).filter(valid).map(|debug| ast::SourceBinding {
                        origin: ast::BindingOrigin::DebugLocal { prototype: 0, register,
                            start_pc: debug.start_pc, end_pc: debug.end_pc },
                        name: String::from_utf8(strings[debug.name_index - 1].to_vec()).unwrap(),
                    }).collect::<Vec<_>>();
                    let actual = function.local_source_bindings.get(&(node, site.index, write));
                    if expected.len() == 1 { assert_eq!(actual, Some(&expected)); }
                    else { assert!(actual.is_none(), "ambiguous metadata must not bind"); }
                    let expected_type = typed.iter().find(|hint| hint.register == register
                        && hint.start_pc <= pc && pc < hint.end_pc).map(|hint| &hint.name);
                    assert_eq!(function.local_type_hints.get(&(node, site.index, write)), expected_type);
                }
            }
            for (register, parameter) in function.parameters.iter().enumerate() {
                let expected = protos[0].debug_locals.iter().filter(|debug|
                    debug.register as usize == register && debug.start_pc == 0)
                    .filter(valid).collect::<Vec<_>>();
                assert_eq!(parameter.0.lock().2.len(), usize::from(expected.len() == 1));
            }
        }
    }

    #[test]
    fn provenance_keeps_deferred_call_and_vararg_instruction_clusters() {
        for vararg in [false, true] {
            let first = if vararg { instruction(OpCode::LOP_GETVARARGS, 1, 0, 0) }
                else { instruction(OpCode::LOP_CALL, 1, 1, 0) };
            let mut proto = prototype(if vararg { 1 } else { 2 }, vec![first,
                instruction(OpCode::LOP_CALL, 0, 0, 0), instruction(OpCode::LOP_RETURN, 0, 0, 0)]);
            proto.is_vararg = vararg;
            proto.line_gap_log2 = Some(0);
            proto.line_info_delta = Some(vec![0, 0, 0]);
            proto.abs_line_info_delta = Some(vec![9, 1, 1]);
            let (function, _, _) = Lifter::lift(&vec![proto], &vec![], 9, 0, Some("root:p0".into()), &[], true);
            let trace = function.provenance.unwrap();
            let sites = trace.lifted.values().collect::<Vec<_>>();
            assert_eq!(sites.len(), 1);
            assert_eq!(sites[0].kind, "return");
            assert_eq!(sites[0].pcs, vec![0, 1, 2]);
            assert_eq!(sites[0].lines, vec![9, 10, 11]);
        }
    }

    #[test]
    fn provenance_keeps_namecall_pair_but_excludes_auxiliary_slot() {
        let mut proto = prototype(1, vec![instruction(OpCode::LOP_NAMECALL, 1, 0, 0),
            instruction(OpCode::LOP_NOP, 0, 0, 0), instruction(OpCode::LOP_CALL, 1, 2, 0),
            instruction(OpCode::LOP_RETURN, 1, 0, 0)]);
        proto.constants.push(super::BytecodeConstant::String(1));
        let (function, _, _) = Lifter::lift(&vec![proto], &[b"method"], 9, 0, Some("root:p0".into()), &[], true);
        let trace = function.provenance.unwrap();
        assert_eq!(trace.lifted.values().next().unwrap().pcs, vec![0, 2, 3]);
    }

    #[test]
    fn provenance_keeps_capture_cluster_and_fixed_return_separate() {
        let mut root = prototype(1, vec![Instruction::AD { op_code: OpCode::LOP_NEWCLOSURE, a: 1, d: 0, aux: 0 },
            instruction(OpCode::LOP_CAPTURE, 0, 0, 0), instruction(OpCode::LOP_RETURN, 1, 2, 0)]);
        root.functions.push(1);
        let mut child = prototype(0, vec![instruction(OpCode::LOP_GETUPVAL, 0, 0, 0), instruction(OpCode::LOP_RETURN, 0, 2, 0)]);
        child.num_upvalues = 1;
        let (function, _, _) = Lifter::lift(&vec![root, child], &vec![], 9, 0, Some("root:p0".into()), &[], true);
        let trace = function.provenance.unwrap();
        let sites = trace.lifted.values().collect::<Vec<_>>();
        assert_eq!(sites[0].pcs, vec![0, 1]);
        assert_eq!(sites[1].pcs, vec![2]);
    }

    #[test]
    fn provenance_records_do_not_keep_local_owners_alive() {
        let mut trace = cfg::provenance::FunctionTrace::new(0, "root:p0".into());
        let register = ast::RcLocal::default();
        let version = ast::RcLocal::default();
        let count = triomphe::Arc::count(&register.0.0);
        trace.register(&register, 0, "parameter");
        trace.definition(&version, &register, petgraph::stable_graph::NodeIndex::new(0), Some((0, 0)), vec![]);
        assert_eq!(triomphe::Arc::count(&register.0.0), count);
    }

    #[test]
    fn detects_explicit_nil_padding_through_callfb_nop() {
        // CALLFB's textual C=1 (one result) is encoded as c=2 and is followed
        // by an injected NOP before the MOVE into the FORGPREP base register.
        let instructions = vec![
            Instruction::BC {
                op_code: OpCode::LOP_CALLFB,
                a: 4,
                b: 1,
                c: 2,
                aux: 0,
            },
            Instruction::BC {
                op_code: OpCode::LOP_NOP,
                a: 0,
                b: 0,
                c: 0,
                aux: 0,
            },
            Instruction::BC {
                op_code: OpCode::LOP_MOVE,
                a: 1,
                b: 4,
                c: 0,
                aux: 0,
            },
            Instruction::BC {
                op_code: OpCode::LOP_LOADNIL,
                a: 2,
                b: 0,
                c: 0,
                aux: 0,
            },
            Instruction::BC {
                op_code: OpCode::LOP_LOADNIL,
                a: 3,
                b: 0,
                c: 0,
                aux: 0,
            },
            Instruction::AD {
                op_code: OpCode::LOP_FORGPREP,
                a: 1,
                d: 0,
                aux: 0,
            },
        ];

        assert!(Lifter::has_explicit_nil_args(&instructions, 5, 1));
        // Short prefixes must fail closed without underflowing while looking
        // behind the prep instruction.
        assert!(!Lifter::has_explicit_nil_args(&instructions, 2, 1));
    }

    #[test]
    fn local_iterated_after_its_call_has_no_explicit_nil_args() {
        // `local t = table.clone(x); for k, v in t do`: the call result lives
        // in a local below the loop base and is copied into it.
        let bc = |op_code, a, b, c| Instruction::BC {
            op_code,
            a,
            b,
            c,
            aux: 0,
        };
        let instructions = vec![
            bc(OpCode::LOP_CALL, 2, 2, 2),
            bc(OpCode::LOP_MOVE, 3, 2, 0),
            bc(OpCode::LOP_LOADNIL, 4, 0, 0),
            bc(OpCode::LOP_LOADNIL, 5, 0, 0),
            Instruction::AD {
                op_code: OpCode::LOP_FORGPREP,
                a: 3,
                d: 0,
                aux: 0,
            },
        ];
        assert!(!Lifter::has_explicit_nil_args(&instructions, 4, 3));
    }
}
