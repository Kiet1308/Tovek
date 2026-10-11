//! What Luau decides for each function literal once the output compiles
//! again, one shared object (DUPCLOSURE) or a new one on every run
//! (NEWCLOSURE), read off the literal as printed, and kept up to date while
//! the de-inliners rewrite code, so that no rewrite changes it.
//!
//! Luau shares a literal when every local it captures is never written and
//! is declared in the chunk's top-level code outside loops, or is bound to a
//! literal it shares in turn; a local holding a constant folds away and is
//! no capture, except under `-g2`, which keeps it (`shouldShareClosure`).
//! A rebuilt call changes what the literal around it captures: it drops the
//! locals only the copy read (a helper's upvalue the copy spelled out) and
//! captures the helper's binder, in that literal and in every literal around
//! it up to the binder's declaration. So it could make a literal the
//! bytecode makes anew a shared one (`function() local r = read() return r
//! end`, where the copy read the written `selected`), or one the bytecode
//! shares new on every run (a discarded copy read nothing, the helper `step`
//! captures a parameter); `==` and table keys tell such closures apart
//! (fr-deinline 1). Each de-inliner asks [`admit`] before a rewrite, which
//! refuses one that changes any literal's verdict under either debug level.
//!
//! The census is taken when a de-inline pass starts ([`enter`]). At `-O0`
//! every literal is new in the bytecode, but the output may be compiled at
//! any level, so a rewrite must not change what it prints there either.

use std::cell::RefCell;

use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{Block, Function, LocalRw, RValue, RcLocal, Statement};

type FnPtr = *const Mutex<Function>;

/// One function literal.
struct Literal {
    /// The function around it (`None`: the chunk).
    parent: Option<FnPtr>,
    /// It may run more than once: in a function, or in a loop of the chunk.
    /// One made once is one object whether Luau shares it or not.
    repeats: bool,
    /// The occurrences of each local declared outside it in its code, the
    /// captures of the literals in it included.
    free: FxHashMap<RcLocal, u32>,
}

/// Where and how a local is declared.
#[derive(Clone, Copy)]
struct Declared {
    /// The function declaring it (`None`: the chunk).
    home: Option<FnPtr>,
    /// In the chunk's own code outside any loop.
    top_level: bool,
    /// Bound to a value Luau folds (a literal, an operation on literals).
    constant: bool,
    /// Bound to a function literal (`local function f`).
    literal: Option<FnPtr>,
}

#[derive(Default)]
pub(crate) struct LiteralSharing {
    literals: FxHashMap<FnPtr, Literal>,
    declared: FxHashMap<RcLocal, Declared>,
    written: FxHashSet<RcLocal>,
}

thread_local! {
    static STATE: RefCell<Option<LiteralSharing>> = const { RefCell::new(None) };
}

/// Holds the census of one de-inline pass until it drops.
pub(crate) struct Scope(Option<LiteralSharing>);

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.0.take();
        STATE.with(|state| *state.borrow_mut() = previous);
    }
}

/// Takes the census of `body` for the de-inline pass about to run on it.
pub(crate) fn enter(body: &Block) -> Scope {
    let census = {
        let _span = crate::telemetry::Span::new("LITERAL_SHARING_CENSUS");
        LiteralSharing::of(body)
    };
    Scope(STATE.with(|state| state.replace(Some(census))))
}

/// Whether a rewrite in `function` (`None`: the chunk) that removes the
/// occurrences `removed` and adds `added` keeps every literal's verdict; if
/// so it is recorded. `local_to_rewrite` are locals the rewrite itself
/// declares. [`statement_locals`] and [`value_locals`] give the occurrences.
pub(crate) fn admit(
    function: Option<FnPtr>,
    removed: &[RcLocal],
    added: &[RcLocal],
    local_to_rewrite: &FxHashSet<RcLocal>,
) -> bool {
    STATE.with(|state| match state.borrow_mut().as_mut() {
        Some(census) => {
            let admitted = census.admit(function, removed, added, local_to_rewrite);
            if !admitted {
                crate::telemetry::count("literal_sharing_refused", 1);
                crate::reconstruction_stats::refuse_site("literal_sharing");
            }
            admitted
        }
        None => true,
    })
}

/// The locals `statement` reads or writes, its nested blocks and the
/// captures of its literals included, a function literal's own code aside.
pub(crate) fn statement_locals(statement: &Statement, out: &mut Vec<RcLocal>) {
    statement.visit_local_reads(&mut |local| {
        out.push(local.clone());
        true
    });
    statement.visit_local_writes(&mut |local| {
        out.push(local.clone());
        true
    });
    let mut child = 0;
    while let Some(block) = child_block(statement, child) {
        for statement in block.lock().iter() {
            statement_locals(statement, out);
        }
        child += 1;
    }
}

/// The locals `value` reads, the captures of its literals included.
pub(crate) fn value_locals(value: &RValue, out: &mut Vec<RcLocal>) {
    value.visit_local_reads(&mut |local| {
        out.push(local.clone());
        true
    });
}

/// The locals the statements declare (`local` statements and loop
/// variables), nested blocks included.
pub(crate) fn declared_locals(statements: &[Statement], out: &mut FxHashSet<RcLocal>) {
    for statement in statements {
        match statement {
            Statement::Assign(assign) if assign.prefix => {
                out.extend(assign.left.iter().filter_map(|left| left.as_local().cloned()));
            }
            Statement::GenericFor(node) => out.extend(node.res_locals.iter().cloned()),
            Statement::NumericFor(node) => {
                out.insert(node.counter.clone());
            }
            _ => {}
        }
        let mut child = 0;
        while let Some(block) = child_block(statement, child) {
            declared_locals(&block.lock().0, out);
            child += 1;
        }
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

/// A value Luau's constant folding folds: a literal, or an operator over
/// such values.
fn folds(value: &RValue) -> bool {
    match value {
        RValue::Literal(_) => true,
        RValue::Unary(unary) => folds(&unary.value),
        RValue::Binary(binary) => folds(&binary.left) && folds(&binary.right),
        _ => false,
    }
}

/// The two rules Luau may share a literal under: `-g0`/`-g1` fold a constant
/// local away; `-g2` keeps it as a capture.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Debug {
    FoldsConstants,
    KeepsConstants,
}

/// One walk of the tree: the literals, their parents and code, every
/// declaration and write.
struct Census<'a> {
    sharing: &'a mut LiteralSharing,
    /// Each literal's occurrences of locals, sorted out once every
    /// declaration is known.
    occurrences: Vec<(FnPtr, Vec<RcLocal>)>,
    visited: FxHashSet<FnPtr>,
}

impl Census<'_> {
    fn function(&mut self, body: &Block, pointer: Option<FnPtr>, parameters: &[RcLocal]) {
        for parameter in parameters {
            self.sharing.declared.insert(parameter.clone(), Declared { home: pointer, top_level: false, constant: false, literal: None });
        }
        let mut occurrences = Vec::new();
        self.block(body, pointer, 0, &mut occurrences);
        if let Some(pointer) = pointer {
            self.occurrences.push((pointer, occurrences));
        }
    }

    fn block(&mut self, block: &Block, pointer: Option<FnPtr>, loops: usize, occurrences: &mut Vec<RcLocal>) {
        for statement in block.iter() {
            statement.visit_local_reads(&mut |local| {
                occurrences.push(local.clone());
                true
            });
            statement.visit_local_writes(&mut |local| {
                occurrences.push(local.clone());
                true
            });
            match statement {
                Statement::Assign(assign) if assign.prefix => {
                    for (index, left) in assign.left.iter().enumerate() {
                        let Some(local) = left.as_local() else { continue };
                        let value = assign.right.get(index);
                        let literal = match value {
                            Some(RValue::Closure(closure)) => Some(Arc::as_ptr(&closure.function.0)),
                            _ => None,
                        };
                        let constant = value.is_some_and(folds)
                            || (value.is_none()
                                && !matches!(assign.right.last(),
                                    Some(RValue::Call(_) | RValue::MethodCall(_) | RValue::VarArg(_) | RValue::Select(_))));
                        self.sharing.declared.insert(local.clone(), Declared {
                            home: pointer,
                            top_level: pointer.is_none() && loops == 0,
                            constant,
                            literal,
                        });
                    }
                }
                Statement::GenericFor(node) => {
                    for local in &node.res_locals {
                        self.sharing.declared.insert(local.clone(), Declared { home: pointer, top_level: false, constant: false, literal: None });
                    }
                }
                Statement::NumericFor(node) => {
                    self.sharing.declared.insert(node.counter.clone(), Declared { home: pointer, top_level: false, constant: false, literal: None });
                }
                _ => {
                    statement.visit_local_writes(&mut |local| {
                        self.sharing.written.insert(local.clone());
                        true
                    });
                }
            }
            let mut nested = Vec::new();
            crate::inline_temps::collect_closures_in_statement(statement, &mut |closure| nested.push(closure.function.0.clone()));
            for function in nested {
                let child = Arc::as_ptr(&function);
                if self.visited.insert(child) {
                    let repeats = pointer.is_some() || loops > 0;
                    self.sharing.literals.insert(child, Literal { parent: pointer, repeats, free: FxHashMap::default() });
                    let function = function.lock();
                    self.function(&function.body, Some(child), &function.parameters);
                }
            }
            let inner = loops + usize::from(matches!(
                statement,
                Statement::While(_) | Statement::Repeat(_) | Statement::NumericFor(_) | Statement::GenericFor(_)
            ));
            let mut child = 0;
            while let Some(nested) = child_block(statement, child) {
                self.block(&nested.lock(), pointer, inner, occurrences);
                child += 1;
            }
        }
    }
}

impl LiteralSharing {
    fn of(body: &Block) -> Self {
        let mut sharing = LiteralSharing::default();
        let occurrences = {
            let mut census = Census { sharing: &mut sharing, occurrences: Vec::new(), visited: FxHashSet::default() };
            census.function(body, None, &[]);
            census.occurrences
        };
        for (pointer, occurrences) in occurrences {
            let mut free: FxHashMap<RcLocal, u32> = FxHashMap::default();
            for local in occurrences {
                if sharing.declared.get(&local).is_none_or(|declared| declared.home != Some(pointer)) {
                    *free.entry(local).or_default() += 1;
                }
            }
            sharing.literals.get_mut(&pointer).unwrap().free = free;
        }
        sharing
    }

    /// Whether Luau shares literal `pointer` as printed now, under `debug`.
    fn shared(&self, pointer: FnPtr, debug: Debug, memo: &mut FxHashMap<(FnPtr, Debug), bool>) -> bool {
        if let Some(&shared) = memo.get(&(pointer, debug)) {
            return shared;
        }
        // A literal reached again through its own captures: Luau checks a
        // literal bound to the local it captures as shared.
        memo.insert((pointer, debug), true);
        let Some(literal) = self.literals.get(&pointer) else { return false };
        let shared = literal
            .free
            .iter()
            .filter(|&(_, &count)| count > 0)
            .all(|(local, _)| self.shareable(local, pointer, debug, memo));
        memo.insert((pointer, debug), shared);
        shared
    }

    /// Whether literal `pointer` capturing `local` may still be shared.
    fn shareable(&self, local: &RcLocal, pointer: FnPtr, debug: Debug, memo: &mut FxHashMap<(FnPtr, Debug), bool>) -> bool {
        let Some(declared) = self.declared.get(local) else { return false };
        if self.written.contains(local) {
            return false;
        }
        if declared.top_level || (declared.constant && debug == Debug::FoldsConstants) {
            return true;
        }
        match declared.literal {
            Some(literal) if literal == pointer => true,
            Some(literal) => self.literals.contains_key(&literal) && self.shared(literal, debug, memo),
            None => false,
        }
    }

    fn admit(&mut self, function: Option<FnPtr>, removed: &[RcLocal], added: &[RcLocal], local_to_rewrite: &FxHashSet<RcLocal>) -> bool {
        let Some(function) = function else { return true };
        let mut deltas: FxHashMap<&RcLocal, i64> = FxHashMap::default();
        for local in added {
            *deltas.entry(local).or_default() += 1;
        }
        for local in removed {
            *deltas.entry(local).or_default() -= 1;
        }
        // The occurrence counts each changed local takes in each literal
        // from `function` out to the local's declaration.
        let mut changes: Vec<(FnPtr, RcLocal, u32)> = Vec::new();
        let mut affected: Vec<FnPtr> = Vec::new();
        for (local, delta) in deltas {
            if delta == 0 || local_to_rewrite.contains(local) {
                continue;
            }
            let home = self.declared.get(local).map(|declared| declared.home);
            let mut at = Some(function);
            while let Some(pointer) = at {
                if home == Some(Some(pointer)) {
                    break;
                }
                let Some(literal) = self.literals.get(&pointer) else { break };
                let before = literal.free.get(local).copied().unwrap_or(0);
                let after = u32::try_from((i64::from(before) + delta).max(0)).unwrap_or(u32::MAX);
                if (before == 0) != (after == 0) && literal.repeats && !affected.contains(&pointer) {
                    affected.push(pointer);
                }
                changes.push((pointer, local.clone(), after));
                at = literal.parent;
            }
        }
        if changes.is_empty() {
            return true;
        }
        // Only a capture gained or lost can change a verdict, and only one of
        // a literal made more than once can be seen.
        let verdicts = |sharing: &Self, affected: &[FnPtr]| -> Vec<(bool, bool)> {
            let mut memo = FxHashMap::default();
            affected
                .iter()
                .map(|&pointer| {
                    (sharing.shared(pointer, Debug::FoldsConstants, &mut memo), sharing.shared(pointer, Debug::KeepsConstants, &mut memo))
                })
                .collect()
        };
        let before = (!affected.is_empty()).then(|| verdicts(self, &affected));
        let mut previous = Vec::with_capacity(changes.len());
        for (pointer, local, after) in &changes {
            let free = &mut self.literals.get_mut(pointer).unwrap().free;
            previous.push((*pointer, local.clone(), free.get(local).copied().unwrap_or(0)));
            free.insert(local.clone(), *after);
        }
        if let Some(before) = before
            && verdicts(self, &affected) != before
        {
            for (pointer, local, count) in previous.into_iter().rev() {
                self.literals.get_mut(&pointer).unwrap().free.insert(local, count);
            }
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Closure, Literal as Lit, Local, Return, Upvalue};
    use by_address::ByAddress;

    fn local(name: &str) -> RcLocal {
        RcLocal::new(Local::new(Some(name.to_owned())))
    }

    fn literal(body: Vec<Statement>, upvalues: Vec<Upvalue>) -> (RValue, FnPtr) {
        with_parameters(body, upvalues, Vec::new())
    }

    fn with_parameters(body: Vec<Statement>, upvalues: Vec<Upvalue>, parameters: Vec<RcLocal>) -> (RValue, FnPtr) {
        let function = Arc::new(Mutex::new(Function { body: Block(body), parameters, ..Function::default() }));
        let pointer = Arc::as_ptr(&function);
        (Closure { node_origin: Default::default(), function: ByAddress(function), upvalues }.into(), pointer)
    }

    fn declare(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![local.clone().into()], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    /// `local selected; local function read() return selected end;
    /// selected = {}; local function nested() return function() return
    /// selected end end`: the inner literal captures the written `selected`
    /// (new on every run). Rebuilt as `return read()` it would capture only
    /// `read`, a top-level local never written: shared. Refused.
    #[test]
    fn a_rewrite_making_a_fresh_literal_shared_is_refused() {
        let selected = local("selected");
        let read = local("read");
        let (read_literal, _) = literal(vec![Return::new(vec![selected.clone().into()]).into()], vec![Upvalue::Ref(selected.clone())]);
        let (inner, inner_ptr) = literal(vec![Return::new(vec![selected.clone().into()]).into()], vec![Upvalue::Ref(selected.clone())]);
        let (outer, outer_ptr) = literal(vec![Return::new(vec![inner]).into()], vec![Upvalue::Ref(selected.clone())]);
        let nested = local("nested");
        let body = Block(vec![
            declare(&selected, Lit::Nil.into()),
            declare(&read, read_literal),
            Assign::new(vec![selected.clone().into()], vec![RValue::Table(Default::default())]).into(),
            declare(&nested, outer),
        ]);
        let mut sharing = LiteralSharing::of(&body);
        let none = FxHashSet::default();
        assert!(!sharing.admit(Some(inner_ptr), &[selected.clone()], &[read.clone()], &none));
        // A rewrite keeping a written capture keeps it new.
        assert!(sharing.admit(Some(inner_ptr), &[], &[read.clone()], &none));
        let _ = outer_ptr;
        let call = Call::new(read.clone().into(), vec![]);
        let _ = call;
    }

    /// `local function outer(k) local function step(t) ... return k end;
    /// return function(t) <copy of step without `return k`> end end`: the
    /// literal captures nothing (shared); rebuilt as `step(t)` it would
    /// capture `step`, bound to a literal capturing the parameter `k`: new on
    /// every run. Refused; a top-level helper is shareable and admitted.
    #[test]
    fn a_rewrite_making_a_shared_literal_fresh_is_refused() {
        let k = local("k");
        let step = local("step");
        let t = local("t");
        let (step_literal, _) = literal(vec![Return::new(vec![k.clone().into()]).into()], vec![Upvalue::Copy(k.clone())]);
        let (inner, inner_ptr) = with_parameters(vec![Return::new(vec![t.clone().into()]).into()], vec![], vec![t.clone()]);
        let (outer, _) = with_parameters(vec![declare(&step, step_literal), Return::new(vec![inner]).into()], vec![], vec![k.clone()]);
        let top = local("top");
        let (top_literal, _) = literal(vec![], vec![]);
        let outer_binder = local("outer");
        let body = Block(vec![declare(&top, top_literal), declare(&outer_binder, outer)]);
        let mut sharing = LiteralSharing::of(&body);
        let none = FxHashSet::default();
        assert!(!sharing.admit(Some(inner_ptr), &[], &[step.clone()], &none));
        assert!(sharing.admit(Some(inner_ptr), &[], &[top.clone()], &none));
    }
}
