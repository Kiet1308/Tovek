//! Fully folded copies of pure helpers (plan E2, rule (b) of the evidence
//! layer: the constant ledger).
//!
//! `task.wait(frames(13))`, for `local function frames(n) return n / 60
//! end`, compiles at `-O2` to `task.wait(0.21666666666666667)`: the copy is
//! one constant, with no structure left to unify. Line info still tells it
//! apart: Luau keeps the helper's line on the constant's load, and the
//! census counts such loads per function and value
//! ([`evidence::Copies::constant_copies`]). A literal becomes the call only
//! where three counts agree, all or nothing for each value in each
//! function:
//! - every reference to that number constant in the function's bytecode is
//!   such a copy of one helper (a `task.wait(0.1)` written by hand next to a
//!   `frames(6)` copy refuses 0.1 there);
//! - the function's tree holds exactly as many literals of it, each where
//!   the helper is in scope: the copies outside other copies, or those and
//!   the ones inside copies of other helpers not rebuilt yet;
//! - an argument folds back to the value bit for bit ([`fold::solve`]),
//!   with the helper's one parameter read once.
//!
//! The helper is pure ([`fold::PureHelpers`]): its call has no effect and,
//! with a constant argument, gives what the compiler folded, so the call is
//! exact wherever the literal stands. Line info only says where to look;
//! without it nothing here applies.

use rustc_hash::{FxHashMap, FxHashSet};

use super::{evidence, fold, FnPtr};
use crate::{Block, Call, LValue, Literal, RValue, RcLocal, Statement, Traverse};

/// What [`rebuild`] rewrote: the helpers it rebuilt calls of, and the
/// function bodies holding them (`None`: the chunk).
#[derive(Default)]
pub(super) struct Rebuilt {
    pub(super) binders: FxHashSet<RcLocal>,
    pub(super) bodies: FxHashSet<Option<FnPtr>>,
}

/// One value in one function's prototype whose every reference is a fully
/// folded copy of one helper: the helper, its copies there, and the
/// arguments of its call once solved.
struct Planned {
    binder: RcLocal,
    copies: evidence::ConstantCopies,
    arguments: Option<Option<Vec<RValue>>>,
}

/// The literals of one value in one function body, and those of them
/// where the planned helper is in scope.
#[derive(Default)]
struct Counted {
    literals: usize,
    in_scope: usize,
}

/// Rebuilds the fully folded copies of pure helpers in `body` that the
/// census `copies` admits (see the module documentation). `write_counts`
/// is the module's write-once census of binders.
pub(super) fn rebuild(body: &mut Block, copies: &evidence::Copies, write_counts: &FxHashMap<RcLocal, usize>) -> Rebuilt {
    if copies.constant_copies.is_empty() {
        return Rebuilt::default();
    }
    let mut declarations = Vec::new();
    super::each_closure_decl(&body.0, &mut |binder, function| declarations.push((binder.clone(), function.clone())));
    let helpers = fold::PureHelpers::of(&declarations, write_counts);
    // The helpers whose copies may fold whole, by prototype: one parameter
    // read once, by an operation (the copy is no argument passed through).
    let mut by_proto: FxHashMap<usize, Option<RcLocal>> = FxHashMap::default();
    for (binder, helper) in helpers.iter() {
        let Some(proto) = helper.proto else { continue };
        let read: Vec<&RcLocal> = helper.params.iter().filter(|param| fold::reads(&helper.value, param) > 0).collect();
        let usable = matches!(read.as_slice(), [param] if fold::reads(&helper.value, param) == 1)
            && !matches!(helper.value, RValue::Local(_) | RValue::Literal(_));
        if usable {
            by_proto.entry(proto).and_modify(|other| *other = None).or_insert(Some(binder.clone()));
        }
    }
    // Each value in each prototype that every reference of is a fully
    // folded copy of one helper.
    let mut plan: FxHashMap<(usize, u64), Option<Planned>> = FxHashMap::default();
    for (&(caller, helper, bits), &counted) in &copies.constant_copies {
        let Some(Some(binder)) = by_proto.get(&(helper as usize)) else { continue };
        let references = copies.constant_refs.get(&(caller, bits)).copied().unwrap_or(0);
        if references != counted.outermost + counted.nested || !f64::from_bits(bits).is_finite() {
            continue;
        }
        let planned = Planned { binder: binder.clone(), copies: counted, arguments: None };
        // Two helpers for one value cannot both have every reference.
        plan.entry((caller as usize, bits)).and_modify(|other| *other = None).or_insert(Some(planned));
    }
    let mut plan: FxHashMap<(usize, u64), Planned> = plan.into_iter().filter_map(|(key, planned)| Some((key, planned?))).collect();
    if plan.is_empty() {
        return Rebuilt::default();
    }
    // Count the literals in each function body (each instance of a
    // prototype has its own: the same code, lifted again), then decide.
    let mut walk = Walk { plan: &plan, decided: None, counted: FxHashMap::default(), shared: FxHashSet::default(), visited: FxHashSet::default(), rebuilt: Rebuilt::default() };
    walk.function(&mut body.0, Some(copies.main), None, &mut Vec::new());
    let (counted, shared) = (std::mem::take(&mut walk.counted), std::mem::take(&mut walk.shared));
    let mut decided: FxHashMap<(Option<FnPtr>, u64), Vec<RValue>> = FxHashMap::default();
    for (&(function, bits, proto), found) in &counted {
        let Some(planned) = plan.get_mut(&(proto, bits)) else { continue };
        let expected = (planned.copies.outermost + planned.copies.nested) as usize;
        let agrees = found.literals == expected || (planned.copies.outermost > 0 && found.literals == planned.copies.outermost as usize);
        if !agrees || found.in_scope != found.literals || function.is_some_and(|pointer| shared.contains(&pointer)) {
            crate::reconstruction_stats::refuse_site("constant_copy_count_mismatch");
            continue;
        }
        let arguments = planned.arguments.get_or_insert_with(|| {
            let helper = helpers.get(&planned.binder)?;
            let at = helper.params.iter().position(|param| fold::reads(&helper.value, param) > 0)?;
            let goal = fold::Value::Number(f64::from_bits(bits));
            match fold::solve(&helper.value, &goal, &helper.params[at], &|_| None, &helpers) {
                Some(fold::Value::Number(argument)) if argument.is_finite() => {
                    let mut arguments = vec![RValue::Literal(Literal::Nil); at];
                    arguments.push(RValue::Literal(Literal::Number(argument)));
                    Some(arguments)
                }
                _ => {
                    crate::reconstruction_stats::refuse_site("constant_copy_unsolved");
                    None
                }
            }
        });
        if let Some(arguments) = arguments {
            decided.insert((function, bits), arguments.clone());
        }
    }
    if decided.is_empty() {
        return Rebuilt::default();
    }
    let mut walk = Walk { plan: &plan, decided: Some(&decided), counted: FxHashMap::default(), shared: FxHashSet::default(), visited: FxHashSet::default(), rebuilt: Rebuilt::default() };
    walk.function(&mut body.0, Some(copies.main), None, &mut Vec::new());
    walk.rebuilt
}

/// One walk over the tree, counting the planned literals or rewriting the
/// decided ones, with the planned helpers in scope at each point.
struct Walk<'a> {
    plan: &'a FxHashMap<(usize, u64), Planned>,
    /// The literals to rewrite, by function body and value, with the
    /// arguments of their calls; `None` while counting.
    decided: Option<&'a FxHashMap<(Option<FnPtr>, u64), Vec<RValue>>>,
    /// While counting: the literals of each planned value in each function
    /// body, with its prototype.
    counted: FxHashMap<(Option<FnPtr>, u64, usize), Counted>,
    /// Function bodies met twice (one body under two literals), whose
    /// literals are refused: each place may see other helpers in scope.
    shared: FxHashSet<FnPtr>,
    visited: FxHashSet<FnPtr>,
    rebuilt: Rebuilt,
}

impl Walk<'_> {
    /// The statements of a function body of prototype `proto`, `pointer`
    /// its function (`None`: the chunk); `active` holds the planned helpers
    /// declared around it.
    fn function(&mut self, stmts: &mut [Statement], proto: Option<usize>, pointer: Option<FnPtr>, active: &mut Vec<RcLocal>) {
        let depth = active.len();
        self.block(stmts, proto, pointer, active);
        active.truncate(depth);
    }

    fn block(&mut self, stmts: &mut [Statement], proto: Option<usize>, pointer: Option<FnPtr>, active: &mut Vec<RcLocal>) {
        let depth = active.len();
        for statement in stmts.iter_mut() {
            let mut visit = |value: &mut RValue| {
                self.value(value, proto, pointer, active);
                true
            };
            match &mut *statement {
                Statement::Assign(assign) => {
                    for value in assign.right.iter_mut() {
                        visit(value);
                    }
                    for left in assign.left.iter_mut() {
                        if let LValue::Index(index) = left {
                            visit(&mut index.left);
                            visit(&mut index.right);
                        }
                    }
                }
                other => {
                    other.visit_rvalues_mut(&mut visit);
                }
            }
            match &mut *statement {
                Statement::If(branch) => {
                    self.block(&mut branch.then_block.lock().0, proto, pointer, active);
                    self.block(&mut branch.else_block.lock().0, proto, pointer, active);
                }
                Statement::While(node) => self.block(&mut node.block.lock().0, proto, pointer, active),
                Statement::Repeat(node) => self.block(&mut node.block.lock().0, proto, pointer, active),
                Statement::NumericFor(node) => self.block(&mut node.block.lock().0, proto, pointer, active),
                Statement::GenericFor(node) => self.block(&mut node.block.lock().0, proto, pointer, active),
                _ => {}
            }
            // A helper is in scope after its declaration.
            if let Statement::Assign(assign) = &*statement
                && assign.prefix
                && let [LValue::Local(binder)] = assign.left.as_slice()
                && matches!(assign.right.as_slice(), [RValue::Closure(_)])
                && self.plan.values().any(|planned| &planned.binder == binder)
            {
                active.push(binder.clone());
            }
        }
        active.truncate(depth);
    }

    fn value(&mut self, value: &mut RValue, proto: Option<usize>, pointer: Option<FnPtr>, active: &mut Vec<RcLocal>) {
        match value {
            RValue::Literal(Literal::Number(number)) => {
                let Some(proto) = proto else { return };
                let bits = number.to_bits();
                let Some(planned) = self.plan.get(&(proto, bits)) else { return };
                let in_scope = active.contains(&planned.binder);
                match self.decided {
                    None => {
                        let counted = self.counted.entry((pointer, bits, proto)).or_default();
                        counted.literals += 1;
                        counted.in_scope += usize::from(in_scope);
                    }
                    Some(decided) => {
                        if let Some(arguments) = decided.get(&(pointer, bits))
                            && in_scope
                        {
                            let mut call = Call::new(RValue::Local(planned.binder.clone()), arguments.clone())
                                .reconstructed(crate::call_origins::Kind::StatementDeinline);
                            call.one_result = true;
                            self.rebuilt.binders.insert(planned.binder.clone());
                            self.rebuilt.bodies.insert(pointer);
                            *value = RValue::Call(call);
                        }
                    }
                }
            }
            RValue::Closure(closure) => {
                let function_pointer = triomphe::Arc::as_ptr(&closure.function.0);
                let mut function = closure.function.0.lock();
                let inner = function.bytecode_proto_id;
                if !self.visited.insert(function_pointer) {
                    self.shared.insert(function_pointer);
                    return;
                }
                let mut active = active.clone();
                self.function(&mut function.body.0, inner, Some(function_pointer), &mut active);
            }
            other => {
                other.visit_rvalues_mut(&mut |child| {
                    self.value(child, proto, pointer, active);
                    true
                });
            }
        }
    }
}
