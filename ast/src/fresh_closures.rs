//! Keep a closure the bytecode makes anew on every run of its literal
//! (NEWCLOSURE) from becoming one shared object once the output compiles again.
//!
//! Luau shares one closure object per literal (DUPCLOSURE) when every local
//! the literal captures is never written and is declared in the chunk's
//! top-level code outside loops, or is bound to a literal it shares in turn;
//! a local holding a constant folds into the literal's body and is not
//! captured at all (`shouldShareClosure`, `compileExprFunction`). Where the
//! bytecode made a new object, the printed literal can still meet that rule.
//! An `-O2` copy of a helper returning `function() ... tag ... end`, called
//! with a constant, captures the constant through a temporary register and
//! prints as
//!
//! ```lua
//! local tag = "y"
//! return function() return tag end
//! ```
//!
//! Compiled again, `tag` folds away, the literal captures nothing, and every
//! run returns one object where the bytecode returned a new one (`==` and
//! table keys tell them apart). One local such a literal captures is then
//! declared first and assigned after (`local tag` / `tag = "y"`): written, it
//! is captured by reference, and Luau never shares a literal capturing a
//! written local. Its value and every read stay as they were.
//!
//! Only a literal that can run more than once matters (in a function, or in a
//! loop of the chunk). The caller runs this only for a chunk whose bytecode
//! shares some closure: at `-O0` every literal is new, and a recompiled one
//! capturing nothing is shared whatever the spelling.

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Assign, Block, Function, LValue, LocalRw, RValue, RcLocal, Statement, Upvalue};

type FnPtr = *const Mutex<Function>;

/// What a `local` declaration binds, as Luau's constant folding and closure
/// sharing read it.
#[derive(Clone)]
enum Init {
    /// A value Luau folds when the local is never written: a literal, `nil`
    /// for a declaration without one, an operator over such values.
    Constant,
    /// Another local: a constant when that one is.
    Alias(RcLocal),
    /// A function literal: shared when that literal is.
    Closure(FnPtr),
    Other,
}

struct Declaration {
    /// In the chunk's own code, outside any loop.
    top_level: bool,
    init: Init,
}

#[derive(Default)]
struct Facts {
    declarations: FxHashMap<RcLocal, Declaration>,
    /// Locals written after their declaration, or captured by reference.
    written: FxHashSet<RcLocal>,
    /// The captures of each function literal bound by a declaration.
    bound_captures: FxHashMap<FnPtr, Vec<Upvalue>>,
    /// Literals of NEWCLOSURE prototypes that can run more than once.
    fresh: Vec<(FnPtr, Vec<Upvalue>)>,
    visited: FxHashSet<FnPtr>,
}

/// Split the declaration of one captured local of every fresh literal the
/// output would share (module documentation).
pub fn keep_fresh_closures(block: &mut Block) {
    let mut facts = Facts::default();
    census(block, 0, 0, &mut facts);
    if facts.fresh.is_empty() {
        return;
    }
    let mut split = FxHashSet::default();
    for (function, upvalues) in &facts.fresh {
        if !facts.shares(upvalues, *function, &mut FxHashSet::default()) {
            continue;
        }
        // A constant first (folded away, it hides the capture), else a
        // top-level local; a local bound to a literal keeps its `local function`.
        let chosen = upvalues
            .iter()
            .filter_map(|upvalue| match upvalue {
                Upvalue::Copy(local) => Some(local),
                Upvalue::Ref(_) => None,
            })
            .filter(|local| !matches!(facts.declarations.get(*local), Some(Declaration { init: Init::Closure(_), .. })))
            .min_by_key(|local| !facts.is_constant(local, 0));
        match chosen {
            Some(local) => {
                crate::telemetry::count("fresh_closure_kept", 1);
                split.insert(local.clone());
            }
            None => crate::telemetry::count("fresh_closure_unkept", 1),
        }
    }
    if !split.is_empty() {
        split_declarations(block, &split, &mut FxHashSet::default());
    }
}

fn census(block: &Block, function_depth: usize, loop_depth: usize, facts: &mut Facts) {
    for statement in &block.0 {
        let top_level = function_depth == 0 && loop_depth == 0;
        let mut declared = Vec::new();
        match statement {
            Statement::Assign(assign) if assign.prefix => {
                // A call or `...` last fills the remaining locals (a `Select` is a
                // call whose results the statement adjusts).
                let multiple_tail = matches!(
                    assign.right.last(),
                    Some(RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_))
                );
                for (index, left) in assign.left.iter().enumerate() {
                    let LValue::Local(local) = left else { continue };
                    let init = match assign.right.get(index) {
                        Some(value) => init_of(value),
                        None if multiple_tail => Init::Other,
                        None => Init::Constant,
                    };
                    if let Init::Closure(function) = init
                        && let Some(RValue::Closure(closure)) = assign.right.get(index)
                    {
                        facts.bound_captures.insert(function, closure.upvalues.clone());
                    }
                    facts.declarations.insert(local.clone(), Declaration { top_level, init });
                    declared.push(local.clone());
                }
            }
            Statement::NumericFor(node) => declared.push(node.counter.clone()),
            Statement::GenericFor(node) => declared.extend(node.res_locals.iter().cloned()),
            _ => {}
        }
        for local in statement.values_written() {
            if !declared.contains(local) {
                facts.written.insert(local.clone());
            } else if !facts.declarations.contains_key(local) {
                // A loop variable: never shared, never folded.
                facts.declarations.insert(local.clone(), Declaration { top_level: false, init: Init::Other });
            }
        }
        let repeats = function_depth > 0 || loop_depth > 0;
        let mut nested = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            for upvalue in &closure.upvalues {
                if let Upvalue::Ref(local) = upvalue {
                    facts.written.insert(local.clone());
                }
            }
            let pointer = Arc::as_ptr(&closure.function.0);
            {
                let function = closure.function.lock();
                if repeats && !closure.upvalues.is_empty() && function.bytecode_proto_id.is_some() && function.closure_constant.is_none() {
                    facts.fresh.push((pointer, closure.upvalues.clone()));
                }
            }
            nested.push(closure.function.0.clone());
        });
        for function in nested {
            if facts.visited.insert(Arc::as_ptr(&function)) {
                census(&function.lock().body, function_depth + 1, 0, facts);
            }
        }
        let inner_loop = loop_depth + usize::from(matches!(
            statement,
            Statement::While(_) | Statement::Repeat(_) | Statement::NumericFor(_) | Statement::GenericFor(_)
        ));
        for child in child_blocks(statement) {
            census(&child.lock(), function_depth, inner_loop, facts);
        }
    }
}

fn init_of(value: &RValue) -> Init {
    match value {
        RValue::Local(local) => Init::Alias(local.clone()),
        RValue::Closure(closure) => Init::Closure(Arc::as_ptr(&closure.function.0)),
        value if folds(value) => Init::Constant,
        _ => Init::Other,
    }
}

/// A literal, or an operator over literals: what Luau's constant folding may
/// fold. Reading more as constant than Luau does only splits a declaration
/// that needed none.
fn folds(value: &RValue) -> bool {
    match value {
        RValue::Literal(_) => true,
        RValue::Unary(unary) => folds(&unary.value),
        RValue::Binary(binary) => folds(&binary.left) && folds(&binary.right),
        _ => false,
    }
}

impl Facts {
    /// Whether Luau folds `local` into a constant: declared with one and
    /// never written.
    fn is_constant(&self, local: &RcLocal, depth: usize) -> bool {
        if depth > 32 || self.written.contains(local) {
            return false;
        }
        match self.declarations.get(local).map(|declaration| &declaration.init) {
            Some(Init::Constant) => true,
            Some(Init::Alias(other)) => self.is_constant(other, depth + 1),
            _ => false,
        }
    }

    /// Luau's `shouldShareClosure` for a literal of `function` capturing
    /// `upvalues`, as the output declares and writes them.
    fn shares(&self, upvalues: &[Upvalue], function: FnPtr, visiting: &mut FxHashSet<FnPtr>) -> bool {
        upvalues.iter().all(|upvalue| {
            let Upvalue::Copy(local) = upvalue else { return false };
            if self.written.contains(local) {
                return false;
            }
            if self.is_constant(local, 0) {
                return true;
            }
            match self.declarations.get(local) {
                Some(Declaration { top_level: true, .. }) => true,
                Some(Declaration { init: Init::Closure(bound), .. }) => {
                    *bound == function
                        || visiting.insert(*bound)
                            && self.bound_captures.get(bound).is_some_and(|captures| self.shares(captures, *bound, visiting))
                }
                _ => false,
            }
        })
    }
}

fn child_blocks(statement: &Statement) -> Vec<&Arc<Mutex<Block>>> {
    match statement {
        Statement::If(node) => vec![&node.then_block, &node.else_block],
        Statement::While(node) => vec![&node.block],
        Statement::Repeat(node) => vec![&node.block],
        Statement::NumericFor(node) => vec![&node.block],
        Statement::GenericFor(node) => vec![&node.block],
        _ => Vec::new(),
    }
}

/// `local a, b = values` declaring a local of `split` becomes `local a, b`
/// then `a, b = values` (`a, b = nil` without values).
fn split_declarations(block: &mut Block, split: &FxHashSet<RcLocal>, visited: &mut FxHashSet<FnPtr>) {
    let mut index = 0;
    while index < block.0.len() {
        let mut nested = Vec::new();
        crate::inline_temps::collect_closures_in_statement(&block.0[index], &mut |closure| nested.push(closure.function.0.clone()));
        for function in nested {
            if visited.insert(Arc::as_ptr(&function)) {
                split_declarations(&mut function.lock().body, split, visited);
            }
        }
        for child in child_blocks(&block.0[index]) {
            split_declarations(&mut child.lock(), split, visited);
        }
        let declares_split = matches!(&block.0[index], Statement::Assign(assign) if assign.prefix
            && assign.left.iter().any(|left| matches!(left, LValue::Local(local) if split.contains(local))));
        if declares_split {
            let Statement::Assign(declaration) = &mut block.0[index] else { unreachable!() };
            let mut right = std::mem::take(&mut declaration.right);
            if right.is_empty() {
                right = vec![RValue::Literal(crate::Literal::Nil); declaration.left.len()];
            }
            let mut assignment = Assign::new(declaration.left.clone(), right);
            assignment.node_origin = declaration.node_origin.clone();
            block.0.insert(index + 1, assignment.into());
            index += 1;
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Closure, Literal, Return};

    fn closure(proto: usize, constant: Option<usize>, upvalues: Vec<Upvalue>) -> RValue {
        let function = Arc::new(Mutex::new(Function {
            bytecode_proto_id: Some(proto),
            closure_constant: constant,
            ..Default::default()
        }));
        Closure { node_origin: Default::default(), function: by_address::ByAddress(function), upvalues }.into()
    }

    fn declare(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    /// `function() local tag = "y"; return function() return tag end end`.
    fn factory(constant: Option<usize>) -> (Block, RcLocal) {
        let tag = RcLocal::default();
        let inner = closure(1, constant, vec![Upvalue::Copy(tag.clone())]);
        let body = Block(vec![declare(&tag, Literal::String(b"y".to_vec()).into()), Return::new(vec![inner]).into()]);
        let outer = closure(0, Some(0), vec![]);
        if let RValue::Closure(closure) = &outer {
            closure.function.lock().body = body;
        }
        (Block(vec![declare(&RcLocal::default(), outer)]), tag)
    }

    fn inner_body(block: &Block) -> Block {
        let Statement::Assign(assign) = &block.0[0] else { panic!() };
        let RValue::Closure(closure) = &assign.right[0] else { panic!() };
        closure.function.lock().body.clone()
    }

    #[test]
    fn a_fresh_literal_capturing_a_constant_gets_a_written_local() {
        let (mut block, tag) = factory(None);
        keep_fresh_closures(&mut block);
        let body = inner_body(&block);
        assert_eq!(body.0.len(), 3);
        let Statement::Assign(declaration) = &body.0[0] else { panic!() };
        assert!(declaration.prefix && declaration.right.is_empty());
        let Statement::Assign(assignment) = &body.0[1] else { panic!() };
        assert!(!assignment.prefix);
        assert_eq!(assignment.left, vec![LValue::Local(tag)]);
        assert_eq!(assignment.right, vec![RValue::Literal(Literal::String(b"y".to_vec()))]);
    }

    #[test]
    fn a_shared_literal_or_a_written_capture_stays() {
        let (mut block, _) = factory(Some(3));
        keep_fresh_closures(&mut block);
        assert_eq!(inner_body(&block).0.len(), 2);

        // Captured by reference: Luau never shares it.
        let tag = RcLocal::default();
        let inner = closure(1, None, vec![Upvalue::Ref(tag.clone())]);
        let outer = closure(0, Some(0), vec![]);
        if let RValue::Closure(closure) = &outer {
            closure.function.lock().body =
                Block(vec![declare(&tag, Literal::Number(1.0).into()), Return::new(vec![inner]).into()]);
        }
        let mut block = Block(vec![declare(&RcLocal::default(), outer)]);
        keep_fresh_closures(&mut block);
        assert_eq!(inner_body(&block).0.len(), 2);
    }

    #[test]
    fn a_literal_run_once_in_the_chunk_stays() {
        let tag = RcLocal::default();
        let mut block = Block(vec![
            declare(&tag, Literal::Boolean(true).into()),
            Return::new(vec![closure(1, None, vec![Upvalue::Copy(tag)])]).into(),
        ]);
        keep_fresh_closures(&mut block);
        assert_eq!(block.0.len(), 2);
    }
}
