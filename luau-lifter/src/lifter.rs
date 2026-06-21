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
use ast::{self};
use cfg::{
    block::{BlockEdge, BranchType},
    function::Function,
};

pub struct Lifter<'a> {
    function_list: &'a Vec<BytecodeFunction>,
    string_table: &'a Vec<Vec<u8>>,
    blocks: FxHashMap<usize, NodeIndex>,
    function: Function,
    // Insertion-ordered (bytecode/PC order, deterministic) rather than a hash map:
    // the consumer sorts by `func_index` (a bytecode-proto index that is NOT unique
    // — one proto can be instantiated by several closure sites), and a stable sort
    // over PC order then breaks ties deterministically. A hash map keyed by the
    // ASLR-randomized `ByAddress` would leave tied entries in a run-dependent order.
    child_functions: Vec<(ByAddress<Arc<Mutex<ast::Function>>>, usize)>,
    register_map: FxHashMap<usize, ast::RcLocal>,
    constant_map: FxHashMap<usize, ast::Literal>,
    current_node: Option<NodeIndex>,
    upvalues: Vec<ast::RcLocal>,
}

impl<'a> Lifter<'a> {
    pub fn lift(
        f_list: &'a Vec<BytecodeFunction>,
        str_list: &'a Vec<Vec<u8>>,
        function_id: usize,
    ) -> (
        Function,
        Vec<ast::RcLocal>,
        Vec<(ByAddress<Arc<Mutex<ast::Function>>>, usize)>,
    ) {
        let mut context = Self {
            function_list: f_list,
            string_table: str_list,
            blocks: FxHashMap::default(),
            function: Function::new(function_id),
            child_functions: Vec::new(),
            register_map: FxHashMap::default(),
            constant_map: FxHashMap::default(),
            current_node: None,
            upvalues: Vec::new(),
        };

        context.lift_function();
        (context.function, context.upvalues, context.child_functions)
    }

    fn lift_function(&mut self) {
        self.discover_blocks().unwrap();

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

        for _ in 0..self.function_list[self.function.id].num_upvalues {
            self.upvalues.push(ast::RcLocal::default());
        }

        for i in 0..self.function_list[self.function.id].num_parameters {
            let parameter = ast::RcLocal::default();
            self.function.parameters.push(parameter.clone());
            self.register_map.insert(i as usize, parameter);
        }

        self.function.is_variadic = self.function_list[self.function.id].is_vararg;

        for (start_pc, end_pc) in block_ranges {
            self.current_node = Some(self.block_to_node(start_pc));
            let (statements, edges) = self.lift_block(start_pc, end_pc);
            let block = self.function.block_mut(self.current_node.unwrap()).unwrap();
            block.0.extend(statements);
            self.function.set_edges(self.current_node.unwrap(), edges);
        }

        let entry_node = self.function.new_block();
        self.function.set_edges(
            entry_node,
            vec![(
                self.block_to_node(0),
                BlockEdge::new(BranchType::Unconditional),
            )],
        );
        self.function.set_entry(entry_node);
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
                    // CMPPROTO is a jump-D opcode (runtime proto guard). The
                    // fall-through is insn_index+2 (the op plus its injected aux NOP);
                    // when D != 0 it ALSO branches to (insn_index+1)+D, so create that
                    // block too — dropping the D edge silently lost a successor (L3).
                    OpCode::LOP_CMPPROTO => {
                        self.blocks
                            .entry(insn_index + 2)
                            .or_insert_with(|| self.function.new_block());
                        if *d != 0 {
                            let dest_index =
                                (insn_index + 1).checked_add_signed((*d).into()).unwrap();
                            self.blocks
                                .entry(dest_index)
                                .or_insert_with(|| self.function.new_block());
                        }
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
    ) -> (Vec<ast::Statement>, Vec<(NodeIndex, BlockEdge)>) {
        let mut statements = Vec::with_capacity((block_start..=block_end).count());
        let mut edges = Vec::new();

        let mut top: Option<(ast::RValue, u8)> = None;

        let mut iter = self.function_list[self.function.id].instructions[block_start..=block_end]
            .iter()
            .enumerate();

        while let Some((index, instruction)) = iter.next() {
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
                            let (tail, end) = top.take().unwrap();
                            (a..end)
                                .map(|r| self.register(r as _).into())
                                .chain(std::iter::once(tail))
                                .collect()
                        };
                        statements.push(ast::Return::new(values).into());
                        break;
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
                                    let top = top.take().unwrap();
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
                            let top = top.take().unwrap();
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
                            let top = top.take().unwrap();
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
                        let statement = ast::Assign::new(
                            vec![target.into()],
                            vec![ast::Literal::Number(d as _).into()],
                        );
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
                                            |s| matches!(s, ast::Statement::NumForNext(nfn)
                                                if nfn.counter.0 == counter_lvalue),
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
                        let generator = self.register(a as _);
                        let state = self.register((a + 1) as _);
                        let counter = self.register((a + 2) as _);
                        statements.push(ast::GenericForInit::new(generator, state, counter).into());
                        let loop_index = ((block_start + index + 1) as isize + d as isize) as usize;
                        assert!(matches!(
                            self.function_list[self.function.id].instructions[loop_index],
                            Instruction::AD {
                                op_code: OpCode::LOP_FORGLOOP,
                                ..
                            }
                        ));
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
                        let generator = self.register(a as _);
                        let state = self.register((a + 1) as _);
                        let _counter = self.register((a + 2) as _);
                        statements.push(
                            ast::GenericForNext::new(
                                (a as usize + 3..a as usize + 3 + (aux & 0xff) as usize)
                                    .map(|r| self.register(r))
                                    .collect::<Vec<_>>(),
                                generator.into(),
                                state,
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

                        let function = Arc::<Mutex<_>>::default();
                        self.child_functions
                            .push((ByAddress(function.clone()), func_index));
                        function.lock().name = func_name;
                        statements.push(
                            ast::Assign::new(
                                vec![dest_local.into()],
                                vec![ast::Closure {
                                    function: ByAddress(function),
                                    upvalues: upvalues_passed,
                                }
                                .into()],
                            )
                            .into(),
                        );
                    }
                    OpCode::LOP_CMPPROTO => {
                        // Runtime proto-identity guard (no source form; never emitted by
                        // the stock compiler). The fall-through is block_start+index+2
                        // (the op plus its injected aux NOP). When D == 0 there is no
                        // jump, so a plain fall-through is exact. When D != 0 the guard
                        // branches to (block_start+index+1)+D, so emit a genuine two-way
                        // conditional rather than dropping that edge (L3) — the guard
                        // predicate is opaque, so R[A] (the value being proto-checked)
                        // stands in as the condition.
                        if d != 0 {
                            statements.push(
                                ast::If::new(
                                    self.register(a as _).into(),
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
                        } else {
                            edges.push((
                                self.block_to_node(block_start + index + 2),
                                BlockEdge::new(BranchType::Unconditional),
                            ));
                        }
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

        (statements, edges)
    }

    fn register(&mut self, index: usize) -> ast::RcLocal {
        self.register_map.entry(index).or_default().clone()
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
                ast::Table(entries).into()
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
