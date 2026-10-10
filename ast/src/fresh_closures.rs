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
//! Every other literal capturing that local is captured by reference too: one
//! the bytecode shares (DUPCLOSURE) would become new on every run. Such a
//! local is never split. Where the fresh literal captures nothing else, it
//! captures a private copy instead, declared and assigned right before the
//! statement making it (`local p` / `p = tag`), its reads renamed: the copy
//! holds the value the literal captured, as the local is never written.
//!
//! Only a literal that can run more than once matters (in a function, or in a
//! loop of the chunk). The caller runs this only for a chunk whose bytecode
//! shares some closure: at `-O0` every literal is new, and a recompiled one
//! capturing nothing is shared whatever the spelling.

use itertools::Either;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Assign, Block, Function, LValue, Local, LocalRw, RValue, RcLocal, Statement, Traverse, Upvalue};

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
    /// Locals written after their declaration: what makes Luau capture a
    /// local by reference once the output compiles again.
    written: FxHashSet<RcLocal>,
    /// The captures of each function literal bound by a declaration.
    bound_captures: FxHashMap<FnPtr, Vec<Upvalue>>,
    /// Literals of NEWCLOSURE prototypes that can run more than once.
    fresh: Vec<(Arc<Mutex<Function>>, Vec<Upvalue>)>,
    /// The locals literals the bytecode shares (DUPCLOSURE) capture by value.
    shared_captures: FxHashSet<RcLocal>,
    /// How many literals make each function.
    literals: FxHashMap<FnPtr, usize>,
    visited: FxHashSet<FnPtr>,
}

/// Keep every fresh literal the output would share fresh (module
/// documentation): split the declaration of one local it captures, else give
/// it a private copy of one.
pub fn keep_fresh_closures(block: &mut Block) {
    let mut facts = Facts::default();
    census(block, 0, 0, &mut facts);
    if facts.fresh.is_empty() {
        return;
    }
    // Rare: only now are the writes of the recorded locals worth a walk.
    facts.visited.clear();
    writes(block, &mut facts);
    let copies = |upvalues: &[Upvalue]| -> Vec<RcLocal> {
        upvalues
            .iter()
            .filter_map(|upvalue| match upvalue {
                Upvalue::Copy(local) => Some(local.clone()),
                Upvalue::Ref(_) => None,
            })
            .collect()
    };
    // A constant first (folded away, it hides the capture), else a top-level
    // local.
    let constant_first = |local: &&RcLocal| !facts.is_constant(local, 0);
    let mut split = FxHashSet::default();
    let mut unsplit = Vec::new();
    for (function, upvalues) in &facts.fresh {
        if !facts.shares(upvalues, Arc::as_ptr(function), &mut FxHashSet::default()) {
            continue;
        }
        // A local bound to a literal keeps its `local function`; a local a
        // shared literal captures keeps that literal shared.
        let captured = copies(upvalues);
        let splittable = captured
            .iter()
            .filter(|local| {
                !matches!(facts.declarations.get(*local), Some(Declaration { init: Init::Closure(_), .. }))
                    && !facts.shared_captures.contains(*local)
            })
            .min_by_key(constant_first);
        match splittable {
            Some(local) => {
                crate::telemetry::count("fresh_closure_kept", 1);
                split.insert(local.clone());
            }
            None => unsplit.push((function, captured)),
        }
    }
    let mut aliased = Vec::new();
    for (function, captured) in unsplit {
        // Another literal's split made this one capture a written local.
        if captured.iter().any(|local| split.contains(local)) {
            crate::telemetry::count("fresh_closure_kept", 1);
            continue;
        }
        let pointer = Arc::as_ptr(function);
        // Never the literal's own binder (`local function f` capturing `f`),
        // not yet declared before the statement making it.
        let own_binder = |local: &&RcLocal| {
            matches!(facts.declarations.get(*local), Some(Declaration { init: Init::Closure(bound), .. }) if *bound == pointer)
        };
        match captured.iter().filter(|local| !own_binder(local)).min_by_key(constant_first) {
            Some(local)
                if facts.literals.get(&pointer) == Some(&1)
                    && only_literals_of_their_functions(&function.lock().body, &facts.literals) =>
            {
                aliased.push((pointer, local.clone()));
            }
            _ => crate::telemetry::count("fresh_closure_unkept", 1),
        }
    }
    if !split.is_empty() {
        split_declarations(block, &split, &mut FxHashSet::default());
    }
    if aliased.is_empty() {
        return;
    }
    // Each copy is named after its local, counted on past every name the
    // chunk spells (`tag2`): it shadows nothing a later read means.
    let Ok(Some(inventory)) = crate::lower_conditionals::prepare_local_rewrite(block, |_| true) else {
        crate::telemetry::count("fresh_closure_unkept", aliased.len() as u64);
        return;
    };
    let mut reserved = inventory.reserved;
    let aliased: FxHashMap<FnPtr, (RcLocal, RcLocal)> = aliased
        .into_iter()
        .map(|(function, local)| {
            let name = local.0.lock().0.clone().unwrap_or_default();
            let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
            let stem = if stem.is_empty() || stem.len() > 32 { "v" } else { stem };
            let name = (2..).map(|counter| format!("{stem}{counter}")).find(|name| !reserved.contains(name)).unwrap();
            reserved.insert(name.clone());
            (function, (local, RcLocal::new(Local::new(Some(name)))))
        })
        .collect();
    crate::telemetry::count("fresh_closure_aliased", aliased.len() as u64);
    alias_captures(block, &aliased, &mut FxHashSet::default());
}

/// One walk of the tree. Only locals that can keep a literal shared are
/// recorded (declared at the top level, or with a constant, another local or
/// a literal that may be shared as value); a literal capturing anything else
/// is never shared, so most literals are dismissed on the spot. Writes are
/// read afterwards, and only when some literal remains.
fn census(block: &Block, function_depth: usize, loop_depth: usize, facts: &mut Facts) {
    let top_level = function_depth == 0 && loop_depth == 0;
    let repeats = !top_level;
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement
            && assign.prefix
        {
            declare(assign, top_level, facts);
        }
        let mut nested = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| {
            *facts.literals.entry(Arc::as_ptr(&closure.function.0)).or_default() += 1;
            if !closure.upvalues.is_empty() {
                let function = closure.function.lock();
                if function.closure_constant.is_some() {
                    facts.shared_captures.extend(closure.upvalues.iter().filter_map(|upvalue| match upvalue {
                        Upvalue::Copy(local) => Some(local.clone()),
                        Upvalue::Ref(_) => None,
                    }));
                } else if repeats && function.bytecode_proto_id.is_some() && facts.may_share(&closure.upvalues, None) {
                    facts.fresh.push((closure.function.0.clone(), closure.upvalues.clone()));
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
        let mut child = 0;
        while let Some(block) = child_block(statement, child) {
            census(&block.lock(), function_depth, inner_loop, facts);
            child += 1;
        }
    }
}

/// The recorded locals written after their declaration anywhere in the
/// tree, function bodies included. A capture the tree marks by reference
/// is no write: a literal passing on an upvalue of its function
/// (`CAPTURE UPVAL`) is marked so, however that function captured it; the
/// output's writes alone decide how Luau captures a local.
fn writes(block: &Block, facts: &mut Facts) {
    for statement in &block.0 {
        // A `local` statement writes only the locals it declares; a loop's
        // variables are recorded nowhere.
        if !matches!(statement, Statement::Assign(assign) if assign.prefix) {
            statement.visit_local_writes(&mut |local| {
                if facts.declarations.contains_key(local) {
                    facts.written.insert(local.clone());
                }
                true
            });
        }
        let mut nested = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| nested.push(closure.function.0.clone()));
        for function in nested {
            if facts.visited.insert(Arc::as_ptr(&function)) {
                writes(&function.lock().body, facts);
            }
        }
        let mut child = 0;
        while let Some(block) = child_block(statement, child) {
            writes(&block.lock(), facts);
            child += 1;
        }
    }
}

/// Records the locals of `local ... = ...` that may keep a literal shared.
fn declare(assign: &Assign, top_level: bool, facts: &mut Facts) {
    // A call or `...` last fills the remaining locals (a `Select` is a call
    // whose results the statement adjusts).
    let multiple_tail =
        matches!(assign.right.last(), Some(RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_)));
    for (index, left) in assign.left.iter().enumerate() {
        let LValue::Local(local) = left else { continue };
        let init = match assign.right.get(index) {
            Some(RValue::Local(other)) if facts.declarations.contains_key(other) => Init::Alias(other.clone()),
            Some(RValue::Closure(closure)) if facts.may_share(&closure.upvalues, Some(local)) => {
                let function = Arc::as_ptr(&closure.function.0);
                facts.bound_captures.insert(function, closure.upvalues.clone());
                Init::Closure(function)
            }
            Some(value) if folds(value) => Init::Constant,
            Some(_) => Init::Other,
            None if multiple_tail => Init::Other,
            None => Init::Constant,
        };
        if top_level || !matches!(init, Init::Other) {
            facts.declarations.insert(local.clone(), Declaration { top_level, init });
        }
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
    /// Whether every local `upvalues` captures by value is recorded (or is
    /// `itself`, a literal bound to the local it captures): only then may the
    /// literal be shared, the writes after it aside.
    fn may_share(&self, upvalues: &[Upvalue], itself: Option<&RcLocal>) -> bool {
        upvalues.iter().all(|upvalue| match upvalue {
            Upvalue::Copy(local) => Some(local) == itself || self.declarations.contains_key(local),
            Upvalue::Ref(_) => false,
        })
    }

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

fn child_block(statement: &Statement, child: u8) -> Option<&Arc<Mutex<Block>>> {
    match (statement, child) {
        (Statement::If(node), 0) => Some(&node.then_block),
        (Statement::If(node), 1) => Some(&node.else_block),
        (Statement::While(node), 0) => Some(&node.block),
        (Statement::Repeat(node), 0) => Some(&node.block),
        (Statement::NumericFor(node), 0) => Some(&node.block),
        (Statement::GenericFor(node), 0) => Some(&node.block),
        _ => None,
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
        let mut child = 0;
        while let Some(nested) = child_block(&block.0[index], child) {
            split_declarations(&mut nested.lock(), split, visited);
            child += 1;
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

/// Whether every function literal in `body`, at any depth, is the only
/// literal of its function: renaming a capture there renames it nowhere
/// else.
fn only_literals_of_their_functions(body: &Block, literals: &FxHashMap<FnPtr, usize>) -> bool {
    body.0.iter().all(|statement| {
        let mut nested = Vec::new();
        crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| nested.push(closure.function.0.clone()));
        let mut blocks = Vec::new();
        let mut child = 0;
        while let Some(block) = child_block(statement, child) {
            blocks.push(block.clone());
            child += 1;
        }
        nested.iter().all(|function| {
            literals.get(&Arc::as_ptr(function)) == Some(&1) && only_literals_of_their_functions(&function.lock().body, literals)
        }) && blocks.iter().all(|block| only_literals_of_their_functions(&block.lock(), literals))
    })
}

/// Gives each literal of `aliased` (its function, mapped to the local it
/// captures by value and the copy) the copy of that local: `local copy` and
/// `copy = local` right before the statement making it, the literal
/// capturing `copy` and its body reading `copy`.
fn alias_captures(block: &mut Block, aliased: &FxHashMap<FnPtr, (RcLocal, RcLocal)>, visited: &mut FxHashSet<FnPtr>) {
    let mut index = 0;
    while index < block.0.len() {
        let mut nested = Vec::new();
        let mut made = Vec::new();
        crate::inline_temps::collect_closures_in_statement(&block.0[index], &mut |closure| {
            let function = Arc::as_ptr(&closure.function.0);
            if let Some(alias) = aliased.get(&function) {
                made.push((function, alias.clone()));
            }
            nested.push(closure.function.0.clone());
        });
        for function in nested {
            if visited.insert(Arc::as_ptr(&function)) {
                alias_captures(&mut function.lock().body, aliased, visited);
            }
        }
        let mut child = 0;
        while let Some(nested) = child_block(&block.0[index], child) {
            alias_captures(&mut nested.lock(), aliased, visited);
            child += 1;
        }
        for (function, (local, copy)) in made {
            block.0[index].post_traverse_values(&mut |value| -> Option<()> {
                if let Either::Right(RValue::Closure(closure)) = value
                    && Arc::as_ptr(&closure.function.0) == function
                {
                    for upvalue in &mut closure.upvalues {
                        if let Upvalue::Copy(captured) = upvalue
                            && *captured == local
                        {
                            *captured = copy.clone();
                        }
                    }
                    rename_reads(&mut closure.function.lock().body, &local, &copy);
                }
                None
            });
            let mut declaration = Assign::new(vec![copy.clone().into()], Vec::new());
            declaration.prefix = true;
            let assignment = Assign::new(vec![copy.into()], vec![RValue::Local(local)]);
            block.0.splice(index..index, [declaration.into(), assignment.into()]);
            index += 2;
        }
        index += 1;
    }
}

/// Every read of `from` in `block`, nested blocks and function literals
/// included (a literal's capture is a read), reads `to` instead.
fn rename_reads(block: &mut Block, from: &RcLocal, to: &RcLocal) {
    for statement in &mut block.0 {
        statement.visit_local_reads_mut(&mut |local| {
            if local == from {
                *local = to.clone();
            }
            true
        });
        statement.post_traverse_values(&mut |value| -> Option<()> {
            if let Either::Right(RValue::Closure(closure)) = value {
                rename_reads(&mut closure.function.lock().body, from, to);
            }
            None
        });
        let mut child = 0;
        while let Some(nested) = child_block(statement, child) {
            rename_reads(&mut nested.lock(), from, to);
            child += 1;
        }
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

    /// `local tag = "y"; local f = function() return function() return tag
    /// end, <DUPCLOSURE>function() return tag end end`.
    #[test]
    fn a_local_a_shared_literal_captures_is_copied_not_split() {
        let tag = RcLocal::new(Local::new(Some("tag".into())));
        let fresh = closure(1, None, vec![Upvalue::Copy(tag.clone())]);
        let shared = closure(2, Some(5), vec![Upvalue::Copy(tag.clone())]);
        for literal in [&fresh, &shared] {
            if let RValue::Closure(closure) = literal {
                closure.function.lock().body = Block(vec![Return::new(vec![tag.clone().into()]).into()]);
            }
        }
        let outer = closure(0, Some(0), vec![]);
        if let RValue::Closure(closure) = &outer {
            closure.function.lock().body = Block(vec![Return::new(vec![fresh, shared]).into()]);
        }
        let mut block = Block(vec![declare(&tag, Literal::String(b"y".to_vec()).into()), declare(&RcLocal::default(), outer)]);
        keep_fresh_closures(&mut block);
        // The declaration stays: `shared` keeps capturing an unwritten local.
        let Statement::Assign(declaration) = &block.0[0] else { panic!() };
        assert!(declaration.prefix && declaration.right.len() == 1);
        let Statement::Assign(assign) = &block.0[1] else { panic!() };
        let RValue::Closure(outer) = &assign.right[0] else { panic!() };
        let body = outer.function.lock().body.clone();
        assert_eq!(body.0.len(), 3, "`local tag2`, `tag2 = tag`, the return");
        let Statement::Assign(copy) = &body.0[1] else { panic!() };
        let LValue::Local(copy_local) = &copy.left[0] else { panic!() };
        assert!(!copy.prefix && copy.right == vec![RValue::Local(tag.clone())]);
        assert_eq!(copy_local.to_string(), "tag2");
        let Statement::Return(ret) = &body.0[2] else { panic!() };
        let (RValue::Closure(fresh), RValue::Closure(shared)) = (&ret.values[0], &ret.values[1]) else { panic!() };
        assert_eq!(fresh.upvalues, vec![Upvalue::Copy(copy_local.clone())]);
        assert_eq!(shared.upvalues, vec![Upvalue::Copy(tag.clone())]);
        let Statement::Return(inner) = &fresh.function.lock().body.0[0] else { panic!() };
        assert_eq!(inner.values, vec![RValue::Local(copy_local.clone())]);
    }

    /// `local tag = "y"; local f = function() return function() return
    /// function() return tag end end end`: the innermost literal passes `tag`
    /// on as an upvalue of the middle one, which the tree marks as a capture by
    /// reference. That is no write: the middle literal, fresh, would print
    /// shared, so `tag` is split.
    #[test]
    fn passing_an_upvalue_on_is_no_write() {
        let tag = RcLocal::new(Local::new(Some("tag".into())));
        let inner = closure(2, None, vec![Upvalue::Ref(tag.clone())]);
        if let RValue::Closure(closure) = &inner {
            closure.function.lock().body = Block(vec![Return::new(vec![tag.clone().into()]).into()]);
        }
        let middle = closure(1, None, vec![Upvalue::Copy(tag.clone())]);
        if let RValue::Closure(closure) = &middle {
            closure.function.lock().body = Block(vec![Return::new(vec![inner]).into()]);
        }
        let outer = closure(0, Some(0), vec![]);
        if let RValue::Closure(closure) = &outer {
            closure.function.lock().body = Block(vec![Return::new(vec![middle]).into()]);
        }
        let mut block = Block(vec![declare(&tag, Literal::String(b"y".to_vec()).into()), declare(&RcLocal::default(), outer)]);
        keep_fresh_closures(&mut block);
        assert_eq!(block.0.len(), 3, "`local tag`, `tag = \"y\"`, `local f = ...`");
    }

    /// `function() local function f() return f end; return f end`: the
    /// literal captures only its own binder, declared by the statement making
    /// it, which no copy before that statement can read.
    #[test]
    fn a_literal_capturing_only_its_own_binder_gets_no_copy() {
        let f = RcLocal::new(Local::new(Some("f".into())));
        let literal = closure(1, None, vec![Upvalue::Copy(f.clone())]);
        if let RValue::Closure(closure) = &literal {
            closure.function.lock().body = Block(vec![Return::new(vec![f.clone().into()]).into()]);
        }
        let outer = closure(0, Some(0), vec![]);
        if let RValue::Closure(closure) = &outer {
            closure.function.lock().body = Block(vec![declare(&f, literal), Return::new(vec![f.clone().into()]).into()]);
        }
        let mut block = Block(vec![declare(&RcLocal::default(), outer)]);
        keep_fresh_closures(&mut block);
        assert_eq!(inner_body(&block).0.len(), 2);
    }
}
