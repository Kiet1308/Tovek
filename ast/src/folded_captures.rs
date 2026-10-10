//! Spell the reads of a capture Luau folded away.
//!
//! From `-O1` on, a local declared with a constant and never written folds
//! into its reads: `local tag = 7` ... `return tag` compiles as `return 7`.
//! With `-g2` the local still gets a register, and a function literal that
//! read it still captures it (`gatherConstUpvals`: "constant folding may
//! remove some upvalue refs from bytecode, so this puts them back"). The
//! prototype then lists an upvalue its code never touches. Luau shares a
//! literal capturing a local of a function or a loop only when that local is
//! bound to a shared literal itself, so this one is a new closure on every
//! run (NEWCLOSURE). Printed as `function() return 7 end` it captures
//! nothing, and the output's literal would be one shared object.
//!
//! Such a literal reads the local again wherever its body holds the local's
//! constant: `function() return tag end`. The local holds that constant at
//! every read (declared with it, never written), so each read yields the
//! value the body had. [`crate::fresh_closures`] then keeps the literal new
//! where the output would share it, and gives it a read of its own where the
//! body holds the constant nowhere (`if DEBUG then` folded away, or
//! `size * 2` folded to `8`).
//!
//! Where the constant stands as a value, those places read the local. Where
//! it stands only as an index key, `t.Name` reads `t[key]`. Never rewritten:
//! the exponent of `^` (Luau computes `x ^ 0.5` and `x ^ 3` from a constant
//! exponent with `sqrt` and multiplications, not `pow`), table constructor
//! keys and DUPTABLE placeholders, and nested function literals, which
//! capture through slots of their own.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    Binary, BinaryOperation, Block, Function, LValue, Literal, LocalRw, RValue, RcLocal, Statement, Traverse, Upvalue,
    inline_temps::collect_closures_in_statement, is_template_placeholder,
};
use parking_lot::Mutex;
use triomphe::Arc;

type FnPtr = *const Mutex<Function>;

/// The upvalue slots of each prototype (`bytecode_proto_id`) its code never
/// reads, writes or passes on.
pub type FoldedSlots = FxHashMap<usize, Vec<u8>>;

/// Where a literal stands, as far as reading a local there instead goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Value,
    /// An index key: `t.Name`, `t[1]`.
    Key,
    Fixed,
}

/// What the tree says of one local a folded slot captured.
#[derive(Default)]
struct Binding {
    declarations: usize,
    /// The constant of its one declaration, when that declaration gives it one.
    constant: Option<Literal>,
    written: bool,
}

/// Rewrite each literal capturing a folded slot to read the captured local
/// (module documentation). The local takes the name evidence of the slot
/// (`inputs`, the debug name of the upvalue), as linking gives it to a local
/// a slot's reads were renamed to. Returns how many literals now read a
/// local again.
pub fn read_folded_captures(block: &mut Block, folded: &FoldedSlots, inputs: &crate::link_upvalues::Inputs) -> usize {
    if folded.is_empty() {
        return 0;
    }
    // Every literal of a folded prototype, with the locals its folded slots
    // capture by value.
    let mut sites: Vec<(Arc<Mutex<Function>>, Vec<(u8, RcLocal)>)> = Vec::new();
    let mut visited = FxHashSet::default();
    collect_sites(block, folded, &mut sites, &mut visited);
    if sites.is_empty() {
        return 0;
    }
    let mut bindings: FxHashMap<RcLocal, Binding> =
        sites.iter().flat_map(|(_, locals)| locals.iter().map(|(_, local)| (local.clone(), Binding::default()))).collect();
    visited.clear();
    census(block, &mut bindings, &mut visited);
    let mut rebuilt = 0;
    for (function, locals) in sites {
        let slot_inputs = inputs.get(&by_address::ByAddress(function.clone())).map_or(&[][..], Vec::as_slice);
        let mut function = function.lock();
        let mut read = false;
        for (slot, local) in locals {
            let Some(Binding { declarations: 1, constant: Some(constant), written: false }) = bindings.get(&local) else {
                continue;
            };
            let mut counts = [0usize; 2];
            for_each_literal(&mut function.body, &mut |value, position| {
                if let RValue::Literal(literal) = value
                    && literal == constant
                {
                    match position {
                        Position::Value => counts[0] += 1,
                        Position::Key => counts[1] += 1,
                        Position::Fixed => {}
                    }
                }
            });
            let target = if counts[0] > 0 {
                Position::Value
            } else if counts[1] > 0 {
                Position::Key
            } else {
                continue;
            };
            for_each_literal(&mut function.body, &mut |value, position| {
                if position == target
                    && let RValue::Literal(literal) = value
                    && literal == constant
                {
                    *value = RValue::Local(local.clone());
                }
            });
            if let Some(input) = slot_inputs.get(usize::from(slot)) {
                local.inherit_source_bindings(input);
            }
            read = true;
        }
        rebuilt += usize::from(read);
    }
    crate::telemetry::count("folded_capture_reads", rebuilt as u64);
    rebuilt
}

/// The literals of folded prototypes anywhere in the tree, each function once.
fn collect_sites(
    block: &Block,
    folded: &FoldedSlots,
    sites: &mut Vec<(Arc<Mutex<Function>>, Vec<(u8, RcLocal)>)>,
    visited: &mut FxHashSet<FnPtr>,
) {
    for statement in &block.0 {
        let mut nested = Vec::new();
        collect_closures_in_statement(statement, &mut |closure| {
            let function = &closure.function.0;
            if !visited.insert(Arc::as_ptr(function)) {
                return;
            }
            // A shared literal (DUPCLOSURE) captures only top-level locals:
            // printed without the capture it is shared all the same, while a
            // read of a local the output declares in a function would make
            // it new on every run once compiled with `-g2`.
            let slots = {
                let function = function.lock();
                match (function.closure_constant, function.bytecode_proto_id) {
                    (None, Some(proto)) => folded.get(&proto),
                    _ => None,
                }
            };
            if let Some(slots) = slots {
                let locals = slots
                    .iter()
                    .filter_map(|&slot| match closure.upvalues.get(usize::from(slot)) {
                        Some(Upvalue::Copy(local)) => Some((slot, local.clone())),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !locals.is_empty() {
                    sites.push((function.clone(), locals));
                }
            }
            nested.push(function.clone());
        });
        for function in nested {
            collect_sites(&function.lock().body, folded, sites, visited);
        }
        for_each_child_block(statement, |child| collect_sites(child, folded, sites, visited));
    }
}

/// The declarations and writes of the recorded locals, in every function.
/// A capture by reference counts as a write: Luau captures a local so only
/// when something assigns it.
fn census(block: &Block, bindings: &mut FxHashMap<RcLocal, Binding>, visited: &mut FxHashSet<FnPtr>) {
    for statement in &block.0 {
        let mut declared = Vec::new();
        match statement {
            Statement::Assign(assign) if assign.prefix => {
                // A call or `...` last fills the locals past the values.
                let multiple_tail = matches!(
                    assign.right.last(),
                    Some(RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_))
                );
                for (index, left) in assign.left.iter().enumerate() {
                    let LValue::Local(local) = left else { continue };
                    declared.push(local.clone());
                    let Some(binding) = bindings.get_mut(local) else { continue };
                    binding.declarations += 1;
                    binding.constant = match assign.right.get(index) {
                        Some(RValue::Literal(literal)) => Some(literal.clone()),
                        None if !multiple_tail => Some(Literal::Nil),
                        _ => None,
                    };
                }
            }
            // A loop variable: a declaration without a constant.
            Statement::NumericFor(_) | Statement::GenericFor(_) => {
                statement.visit_local_writes(&mut |local| {
                    declared.push(local.clone());
                    if let Some(binding) = bindings.get_mut(local) {
                        binding.declarations += 1;
                    }
                    true
                });
            }
            _ => {}
        }
        statement.visit_local_writes(&mut |local| {
            if !declared.contains(local)
                && let Some(binding) = bindings.get_mut(local)
            {
                binding.written = true;
            }
            true
        });
        let mut nested = Vec::new();
        collect_closures_in_statement(statement, &mut |closure| {
            for upvalue in &closure.upvalues {
                if let Upvalue::Ref(local) = upvalue
                    && let Some(binding) = bindings.get_mut(local)
                {
                    binding.written = true;
                }
            }
            nested.push(closure.function.0.clone());
        });
        for function in nested {
            if visited.insert(Arc::as_ptr(&function)) {
                census(&function.lock().body, bindings, visited);
            }
        }
        for_each_child_block(statement, |child| census(child, bindings, visited));
    }
}

/// Every literal of a function body (nested function literals aside), with
/// where it stands.
fn for_each_literal(block: &mut Block, visit: &mut impl FnMut(&mut RValue, Position)) {
    for statement in &mut block.0 {
        statement.visit_lvalues_mut(&mut |lvalue| {
            if let LValue::Index(index) = lvalue {
                literals_in(&mut index.left, Position::Value, visit);
                literals_in(&mut index.right, Position::Key, visit);
            }
            true
        });
        statement.visit_rvalues_mut(&mut |value| {
            literals_in(value, Position::Value, visit);
            true
        });
        for_each_child_block_mut(statement, |child| for_each_literal(child, visit));
    }
}

fn literals_in(value: &mut RValue, position: Position, visit: &mut impl FnMut(&mut RValue, Position)) {
    match value {
        RValue::Literal(_) => visit(value, position),
        RValue::Closure(_) => {}
        RValue::Index(index) => {
            literals_in(&mut index.left, Position::Value, visit);
            literals_in(&mut index.right, Position::Key, visit);
        }
        RValue::Table(table) => {
            for (key, value) in &mut table.0 {
                if let Some(key) = key {
                    literals_in(key, Position::Fixed, visit);
                }
                let position = if is_template_placeholder(value) { Position::Fixed } else { Position::Value };
                literals_in(value, position, visit);
            }
        }
        RValue::Binary(Binary { left, right, operation: BinaryOperation::Pow, .. }) => {
            literals_in(left, Position::Value, visit);
            literals_in(right, Position::Fixed, visit);
        }
        _ => {
            value.visit_rvalues_mut(&mut |child| {
                literals_in(child, Position::Value, visit);
                true
            });
        }
    }
}

fn for_each_child_block(statement: &Statement, mut visit: impl FnMut(&Block)) {
    match statement {
        Statement::If(node) => {
            visit(&node.then_block.lock());
            visit(&node.else_block.lock());
        }
        Statement::While(node) => visit(&node.block.lock()),
        Statement::Repeat(node) => visit(&node.block.lock()),
        Statement::NumericFor(node) => visit(&node.block.lock()),
        Statement::GenericFor(node) => visit(&node.block.lock()),
        _ => {}
    }
}

fn for_each_child_block_mut(statement: &mut Statement, mut visit: impl FnMut(&mut Block)) {
    match statement {
        Statement::If(node) => {
            visit(&mut node.then_block.lock());
            visit(&mut node.else_block.lock());
        }
        Statement::While(node) => visit(&mut node.block.lock()),
        Statement::Repeat(node) => visit(&mut node.block.lock()),
        Statement::NumericFor(node) => visit(&mut node.block.lock()),
        Statement::GenericFor(node) => visit(&mut node.block.lock()),
        _ => {}
    }
}

/// Whether the body of a literal reads `local`: some statement reads it, or
/// a nested literal capturing it reads it in turn.
pub(crate) fn body_reads(block: &Block, local: &RcLocal) -> bool {
    block.0.iter().any(|statement| {
        let mut reads = 0usize;
        statement.visit_local_reads(&mut |read| {
            reads += usize::from(read == local);
            true
        });
        // Each capture is one of the statement's reads; whether the capture
        // is spelled is the nested literal's matter.
        let mut capturing = Vec::new();
        collect_closures_in_statement(statement, &mut |closure| {
            for upvalue in &closure.upvalues {
                let (Upvalue::Copy(captured) | Upvalue::Ref(captured)) = upvalue;
                if captured == local {
                    capturing.push(closure.function.0.clone());
                }
            }
        });
        let mut in_child = false;
        for_each_child_block(statement, |child| in_child = in_child || body_reads(child, local));
        reads > capturing.len() || in_child || capturing.iter().any(|function| body_reads(&function.lock().body, local))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Closure, Return};

    fn declare(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    fn literal_closure(proto: usize, upvalues: Vec<Upvalue>, body: Block) -> (RValue, Arc<Mutex<Function>>) {
        let function = Arc::new(Mutex::new(Function { bytecode_proto_id: Some(proto), body, ..Default::default() }));
        (Closure { node_origin: Default::default(), function: by_address::ByAddress(function.clone()), upvalues }.into(), function)
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    /// `local tag = 7; return function() return 7, x ^ 7, t[7] end`.
    #[test]
    fn a_folded_capture_reads_its_local_where_the_body_holds_the_constant() {
        let tag = RcLocal::default();
        let x = RcLocal::default();
        let t = RcLocal::default();
        let pow = Binary::new(RValue::Local(x.clone()), number(7.0), BinaryOperation::Pow);
        let index = crate::Index::new(RValue::Local(t.clone()), number(7.0));
        let (literal, function) = literal_closure(
            1,
            vec![Upvalue::Copy(tag.clone())],
            Block(vec![Return::new(vec![number(7.0), pow.into(), index.into()]).into()]),
        );
        let mut block = Block(vec![declare(&tag, number(7.0)), Return::new(vec![literal]).into()]);
        let folded = FoldedSlots::from_iter([(1, vec![0])]);
        assert_eq!(read_folded_captures(&mut block, &folded, &Default::default()), 1);
        let body = function.lock().body.clone();
        let Statement::Return(ret) = &body.0[0] else { panic!() };
        assert_eq!(ret.values[0], RValue::Local(tag.clone()));
        let RValue::Binary(pow) = &ret.values[1] else { panic!() };
        assert_eq!(*pow.right, number(7.0), "the exponent keeps its constant");
        let RValue::Index(index) = &ret.values[2] else { panic!() };
        assert_eq!(*index.right, number(7.0), "a value place took the read");
        assert!(body_reads(&body, &tag));
    }

    /// `local key = "Name"; return function() return t.Name end`: only an
    /// index key holds the constant.
    #[test]
    fn an_index_key_reads_the_local_when_no_value_holds_the_constant() {
        let key = RcLocal::default();
        let t = RcLocal::default();
        let name = RValue::Literal(Literal::String(b"Name".to_vec()));
        let index = crate::Index::new(RValue::Local(t), name.clone());
        let (literal, function) =
            literal_closure(1, vec![Upvalue::Copy(key.clone())], Block(vec![Return::new(vec![index.into()]).into()]));
        let mut block = Block(vec![declare(&key, name), Return::new(vec![literal]).into()]);
        assert_eq!(read_folded_captures(&mut block, &FoldedSlots::from_iter([(1, vec![0])]), &Default::default()), 1);
        let Statement::Return(ret) = &function.lock().body.0[0] else { panic!() };
        let RValue::Index(index) = &ret.values[0] else { panic!() };
        assert_eq!(*index.right, RValue::Local(key));
    }

    /// A written local, or one whose declaration holds no constant, holds no
    /// one value: the body stays.
    #[test]
    fn a_written_or_computed_local_is_left_alone() {
        for written in [true, false] {
            let tag = RcLocal::default();
            let (literal, function) =
                literal_closure(1, vec![Upvalue::Copy(tag.clone())], Block(vec![Return::new(vec![number(7.0)]).into()]));
            let init = if written { number(7.0) } else { RValue::VarArg(crate::VarArg {}) };
            let mut statements = vec![declare(&tag, init)];
            if written {
                statements.push(Assign::new(vec![tag.clone().into()], vec![number(8.0)]).into());
            }
            statements.push(Return::new(vec![literal]).into());
            let mut block = Block(statements);
            assert_eq!(read_folded_captures(&mut block, &FoldedSlots::from_iter([(1, vec![0])]), &Default::default()), 0);
            let Statement::Return(ret) = &function.lock().body.0[0] else { panic!() };
            assert_eq!(ret.values[0], number(7.0));
        }
    }
}
