use anyhow::Result;

use by_address::ByAddress;

use itertools::Itertools;
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
    string_table: &'a Vec<Vec<u8>>,
    typed_locals: &'a [TypedLocalHint],
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
    register_map: FxHashMap<usize, ast::RcLocal>,
    constant_map: FxHashMap<usize, ast::Literal>,
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
        str_list: &'a Vec<Vec<u8>>,
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
            bytecode_version,
            blocks: FxHashMap::default(),
            function: Function::new(function_id),
            child_functions: Vec::new(),
            static_function_id,
            register_map: FxHashMap::default(),
            constant_map: FxHashMap::default(),
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

        context.lift_function();
        (context.function, context.upvalues, context.child_functions)
    }

    fn lift_function(&mut self) {
        self.discover_blocks().unwrap();
        self.build_for_origin_map();

        let mut blocks = self.blocks.keys().cloned().collect::<Vec<_>>();

        blocks.sort_unstable();

        // TODO: code_ranges in lua51-lifter
        let block_ranges = blocks
            .iter()
            .rev()
            .fold(
                (
                    self.function_list[self.function.id].instructions.len(),
                    Vec::new(),
                ),
                |(block_end, mut accumulator), &block_start| {
                    accumulator.push((block_start, block_end - 1));

                    (
                        if block_start != 0 {
                            block_start
                        } else {
                            block_end
                        },
                        accumulator,
                    )
                },
            )
            .1;

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
            let names = self.function_list[self.function.id].debug_locals.iter()
                .filter(|local| local.register == i && local.start_pc == 0)
                .filter_map(|local| self.debug_binding(local)).collect::<Vec<_>>();
            if let [binding] = names.as_slice() {
                parameter.0.lock().add_source_binding(binding.clone());
            }
            self.function.parameters.push(parameter.clone());
            if let Some(trace) = &mut self.function.provenance { trace.register(&parameter, i as usize, "parameter"); }
            self.register_map.insert(i as usize, parameter);
        }

        self.function.is_variadic = self.function_list[self.function.id].is_vararg;

        for (start_pc, end_pc) in block_ranges {
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
        for debug in debug_locals {
            if debug.start_pc <= block_start && block_start < debug.end_pc {
                if debug_locals.iter().filter(|other| other.register == debug.register
                    && other.start_pc <= block_start && block_start < other.end_pc).count() != 1 { continue; }
                if let (Some(local), Some(binding)) = (self.register_map.get(&(debug.register as usize)), self.debug_binding(debug)) {
                    self.function.entry_source_bindings.insert((node, local.clone()), binding);
                }
            }
        }
        let register_of: FxHashMap<ast::RcLocal, u8> = self
            .register_map
            .iter()
            .filter_map(|(&register, local)| u8::try_from(register).ok().map(|r| (local.clone(), r)))
            .collect();
        let mut definitions: FxHashMap<u8, Vec<(usize, usize, usize)>> = FxHashMap::default();
        if !debug_locals.is_empty() {
            for (index, (statement, &pc)) in statements.iter().zip(pcs).enumerate() {
                for (written, local) in statement.values_written().iter().enumerate() {
                    if let Some(&register) = register_of.get(*local) {
                        definitions.entry(register).or_default().push((pc, index, written));
                    }
                }
            }
        }
        let block_end = self.function.block_pc_range(node).unwrap().end + 1;
        for (index, (statement, &pc)) in statements.iter().zip(pcs).enumerate() {
            for (written_index, local) in statement.values_written().into_iter().enumerate() {
                let Some(&register) = register_of.get(local) else {
                    continue;
                };
                // A debug interval starts AFTER initialization. Use the last
                // reaching write within this block; never guess across CFG edges.
                let bindings = debug_locals.iter().filter(|debug| {
                    if debug.register != register { return false; }
                    if debug.start_pc <= pc && pc < debug.end_pc { return true; }
                    if debug.start_pc > block_end { return false; }
                    let defs = &definitions[&register];
                    let count = defs.partition_point(|(def_pc, _, _)| *def_pc < debug.start_pc);
                    count > 0 && defs[count - 1] == (pc, index, written_index)
                }).filter_map(|debug| self.debug_binding(debug)).collect::<Vec<_>>();
                if let [binding] = bindings.as_slice() {
                    self.function.local_source_bindings.entry((node, base + index, written_index)).or_default().push(binding.clone());
                }
                let Some(hint) = self.typed_locals.iter().find(|typed| {
                    typed.register == register && typed.start_pc <= pc && pc < typed.end_pc
                }) else {
                    continue;
                };
                self.function
                    .local_type_hints
                    .insert((node, base + index, written_index), hint.name.clone());
            }
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
            let step_pc = ((prep_pc + 1) as isize + *d as isize) as usize;
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
                body_pc: ((step_pc + 1) as isize + step_d as isize) as usize,
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
            }) if *a == base => {
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
        let mut statement_pcs: Vec<usize> = Vec::with_capacity(statements.capacity());

        let mut top: Option<(ast::RValue, u8)> = None;
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
                            (a..a + (b - 1))
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
                    OpCode::LOP_FASTCALL
                    | OpCode::LOP_FASTCALL1
                    | OpCode::LOP_FASTCALL2
                    | OpCode::LOP_FASTCALL2K
                    | OpCode::LOP_FASTCALL3
                    // NATIVECALL is a JIT dispatch hint; the actual call is the
                    // following CALL, so (like FASTCALL) it lifts to nothing (L5).
                    | OpCode::LOP_NATIVECALL => {}
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
                            ast::Literal::String(string) => String::from_utf8(string).unwrap(),
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

                                // TODO: make sure `a:method with space()` doesnt happen
                                let call = ast::MethodCall::new(
                                    namecall_object.into(),
                                    namecall_method,
                                    arguments,
                                );

                                if c != 0 {
                                    if c == 1 {
                                        statements.push(call.into());
                                    } else {
                                        statements.push(
                                            ast::Assign::new(
                                                (a..a + c - 1)
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

                        let call = ast::Call::new(self.register(a as _).into(), arguments);

                        if c != 0 {
                            if c == 1 {
                                statements.push(call.into());
                            } else {
                                statements.push(
                                    ast::Assign::new(
                                        (a..a + c - 1)
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
                                (b..b + c - 1)
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
                                    (a..a + b - 1)
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
                        let counter_lvalue: ast::LValue = counter.clone().into();
                        statements.push(ast::NumForInit::new(counter, limit, step).into());

                        // The loop header is the block that ends in the matching
                        // `NumForNext` (the FORNLOOP), which FORNPREP jumps to. Normally
                        // it is found as the predecessor of the body that loops back to
                        // it. A degenerate `for i = 1, n do break end` body never loops
                        // back, so the FORNLOOP is NOT a predecessor of the body — the
                        // predecessor lookup then finds nothing and the old
                        // `exactly_one().unwrap()` panicked the whole function out (C8 at
                        // O1). Fall back to the unique `NumForNext` block over the *same*
                        // counter, scanning the whole function.
                        let body_node = self.block_to_node(block_start + index + 1);
                        let loop_node = self
                            .function
                            .predecessor_blocks(body_node)
                            .filter(|&p| {
                                self.function
                                    .block(p)
                                    .unwrap()
                                    .last()
                                    .is_some_and(|s| matches!(s, ast::Statement::NumForNext(_)))
                            })
                            .unique()
                            .exactly_one()
                            .ok()
                            .or_else(|| {
                                self.function
                                    .graph()
                                    .node_indices()
                                    .filter(|&n| {
                                        self.function.block(n).and_then(|b| b.last()).is_some_and(
                                            |s| {
                                                matches!(s, ast::Statement::NumForNext(nfn)
                                                if nfn.counter.0 == counter_lvalue)
                                            },
                                        )
                                    })
                                    .exactly_one()
                                    .ok()
                            })
                            .expect("FORNPREP: no matching NumForNext (FORNLOOP) block");
                        edges.push((loop_node, BlockEdge::new(BranchType::Unconditional)));
                    }
                    OpCode::LOP_FORNLOOP => {
                        let limit = self.register(a as _);
                        let step = self.register((a + 1) as _);
                        let counter = self.register((a + 2) as _);
                        statements
                            .push(ast::NumForNext::new(counter, limit.into(), step.into()).into());
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
                    }
                    OpCode::LOP_FORGPREP
                    | OpCode::LOP_FORGPREP_INEXT
                    | OpCode::LOP_FORGPREP_NEXT => {
                        let prep_pc = block_start + index;
                        let generator = self.register(a as _);
                        let state = self.register((a + 1) as _);
                        let counter = self.register((a + 2) as _);
                        let loop_index = ((prep_pc + 1) as isize + d as isize) as usize;
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
                    }
                    OpCode::LOP_DUPTABLE => {
                        // DUPTABLE duplicates a constant template table. For
                        // `TABLE_WITH_CONSTANTS` the field values are baked into
                        // the constant (no following SETTABLE), so reconstruct
                        // them; a plain `TABLE` only carries keys whose values
                        // are filled by subsequent SETTABLEKS, so start empty.
                        let template = match self.function_list[self.function.id]
                            .constants
                            .get(d as usize)
                        {
                            Some(BytecodeConstant::TableWithConstants(_)) => {
                                self.constant_to_rvalue(d as usize)
                            }
                            _ => ast::Table::default().into(),
                        };
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
                        let closure_site_id = format!("p{}@pc{constructor_pc}", self.function.id);
                        let child_function_id = self.static_function_id.as_ref().map(|parent_id| {
                            format!("{parent_id}/{closure_site_id}:p{func_index}")
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
            statement_pcs.resize(statements.len(), pc);
            debug_assert!(statement_pcs.len() >= lifted_before);
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
        statement_pcs.resize(statements.len(), block_end);
        // A generated warning has no source instruction; preserve that unknown.
        if trace_origins { statement_origins.resize(statements.len(), Vec::new()); }
        (statements, edges, statement_pcs, statement_origins)
    }

    fn register(&mut self, index: usize) -> ast::RcLocal {
        let local = self.register_map.entry(index).or_default().clone();
        if let Some(trace) = &mut self.function.provenance { trace.register(&local, index, "register"); }
        local
    }

    fn constant(&mut self, index: usize) -> ast::Literal {
        let converted_constant = match self.function_list[self.function.id]
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
                if *v == 0 {
                    ast::Literal::String(Vec::new())
                } else {
                    ast::Literal::String(self.string_table[*v - 1].clone())
                }
            }
            BytecodeConstant::Vector(x, y, z, _) => ast::Literal::Vector(*x, *y, *z),
            BytecodeConstant::VectorD(x, y, z, _) => ast::Literal::VectorD(*x, *y, *z),
            BytecodeConstant::Integer(v) => ast::Literal::Number(*v as f64),
            _ => unimplemented!(),
        };
        self.constant_map
            .entry(index)
            .or_insert(converted_constant)
            .clone()
    }

    // Reconstructs an arbitrary constant (including nested constant tables) as an
    // rvalue. Used to materialize DUPTABLE templates whose field values are baked
    // into the bytecode constant pool.
    fn constant_to_rvalue(&mut self, index: usize) -> ast::RValue {
        enum Shape {
            Literal(ast::Literal),
            Table(Vec<(usize, i32)>),
        }
        let shape = match self.function_list[self.function.id].constants.get(index) {
            Some(BytecodeConstant::Boolean(v)) => Shape::Literal(ast::Literal::Boolean(*v)),
            Some(BytecodeConstant::Number(v)) => Shape::Literal(ast::Literal::Number(*v)),
            Some(BytecodeConstant::Integer(v)) => Shape::Literal(ast::Literal::Number(*v as f64)),
            Some(BytecodeConstant::String(v)) => Shape::Literal(if *v == 0 {
                ast::Literal::String(Vec::new())
            } else {
                ast::Literal::String(self.string_table[*v - 1].clone())
            }),
            Some(BytecodeConstant::Vector(x, y, z, _)) => {
                Shape::Literal(ast::Literal::Vector(*x, *y, *z))
            }
            Some(BytecodeConstant::VectorD(x, y, z, _)) => {
                Shape::Literal(ast::Literal::VectorD(*x, *y, *z))
            }
            Some(BytecodeConstant::TableWithConstants(pairs)) => Shape::Table(pairs.clone()),
            // Nil, plain Table (keys only, values set later), Import, Closure
            _ => Shape::Literal(ast::Literal::Nil),
        };
        match shape {
            Shape::Literal(literal) => literal.into(),
            Shape::Table(pairs) => {
                let entries = pairs
                    .into_iter()
                    .map(|(key, value)| {
                        let key = self.constant_to_rvalue(key);
                        let value = self.constant_to_rvalue(value as usize);
                        (Some(key), value)
                    })
                    .collect();
                ast::Table::new(entries).into()
            }
        }
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
        let (function, _, _) = Lifter::lift(&vec![proto], &vec![b"method".to_vec()], 9, 0, Some("root:p0".into()), &[], true);
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
}
