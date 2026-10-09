//! What the de-inliners rebuilt and refused in one script, for
//! `luau-lifter --stats-json` and `scripts/scorecard.py`.
//!
//! Rebuilt calls are counted in the final tree, from each call's
//! [`crate::Call::rebuilt`] producer, so the count does not depend on the
//! wording or placement of any comment. Refusals are recorded while the
//! de-inliners run, inside a [`Scope`]; outside one, recording a refusal
//! costs one thread-local check.

use std::{cell::RefCell, collections::BTreeMap, marker::PhantomData, rc::Rc};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;

use crate::{Block, LValue, RValue, RcLocal, Select, Statement, Traverse, call_origins::Kind};

#[derive(Default)]
struct State {
    /// The reason each helper (by binder id) was last refused as a target.
    refused_helpers: FxHashMap<u64, &'static str>,
    refused_sites: BTreeMap<&'static str, usize>,
}

thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

/// Records refusals until dropped; restores the enclosing scope (a nested
/// decompilation the worker picked up runs in a scope of its own).
pub struct Scope(Option<State>, PhantomData<Rc<()>>);

pub fn enter(enabled: bool) -> Scope {
    Scope(STATE.with(|state| state.replace(enabled.then(State::default))), PhantomData)
}

impl Drop for Scope {
    fn drop(&mut self) {
        STATE.with(|state| state.replace(self.0.take()));
    }
}

fn with_state(record: impl FnOnce(&mut State)) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            record(state);
        }
    });
}

/// The statement de-inliner refused a helper as a target. Its last reason
/// counts, unless a later round accepts it.
pub(crate) fn refuse_helper(binder: u64, reason: &'static str) {
    with_state(|state| {
        state.refused_helpers.insert(binder, reason);
    });
}

pub(crate) fn accept_helper(binder: u64) {
    with_state(|state| {
        state.refused_helpers.remove(&binder);
    });
}

/// A site whose region matched a helper was refused: a soundness gate after
/// unification, or two helpers matching alike. Each attempt counts.
pub(crate) fn refuse_site(reason: &'static str) {
    with_state(|state| *state.refused_sites.entry(reason).or_default() += 1);
}

/// One script's numbers.
#[derive(Debug, Default, Serialize)]
pub struct Stats {
    /// Calls rebuilt from inlined copies, as printed (a copied function body
    /// counts once per copy).
    pub reconstructed_calls: Calls,
    /// Calls of helpers synthesized for duplicated terminal regions. They
    /// rebuild no source call, so they are not counted above.
    pub synthesized_calls: usize,
    /// Distinct helpers the calls above call.
    pub helpers: Helpers,
    /// The calls above per helper binding, by the bytecode prototype it was
    /// lifted from: helpers that print alike (two local `fn`s) stay apart,
    /// and the census of inlined copies joins on the prototype.
    pub calls_by_helper: Vec<HelperCalls>,
    /// Helpers the statement de-inliner refused as targets (and never
    /// rebuilt a call of), by reason.
    pub refused_helpers: BTreeMap<&'static str, usize>,
    /// Refused site attempts, by reason.
    pub refused_sites: BTreeMap<&'static str, usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct Calls {
    pub total: usize,
    /// Statement sites: `f(x)`, `local r = f(x)`, and a call the statement
    /// de-inliner rebuilt inside another statement.
    pub statement: usize,
    /// Expression sites.
    pub expression: usize,
    /// Expression sites of arithmetic helpers.
    pub arithmetic: usize,
}

/// One helper's rebuilt calls.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct HelperCalls {
    /// The prototype the helper's function was lifted from; `None` when the
    /// tree holds no definition of it with one (a parameter, a synthesized
    /// function).
    pub proto: Option<usize>,
    /// Its printed name (`?` unnamed).
    pub helper: String,
    pub calls: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct Helpers {
    pub reconstructed: usize,
    pub synthesized: usize,
}

impl Scope {
    /// The numbers for `body`, the final tree, with the refusals recorded.
    pub fn finish(self, body: &Block) -> Stats {
        let mut stats = Stats::default();
        // Rebuilt calls per helper binding, with the binding.
        let mut callees: FxHashMap<u64, (RcLocal, usize)> = FxHashMap::default();
        let mut unbound = 0;
        let mut synthesized = FxHashSet::default();
        // The prototype of each binding defined as a lifted function.
        let mut protos: FxHashMap<u64, usize> = FxHashMap::default();
        count_block(body, &mut protos, &mut |call| {
            let callee = match &*call.value {
                RValue::Local(local) => Some(local),
                _ => None,
            };
            let calls = &mut stats.reconstructed_calls;
            match call.rebuilt {
                None => return,
                Some(Kind::StatementDeinline) => calls.statement += 1,
                Some(Kind::ExpressionDeinline) => calls.expression += 1,
                Some(Kind::ArithmeticDeinline) => calls.arithmetic += 1,
                Some(Kind::TerminalSynthesis) => {
                    stats.synthesized_calls += 1;
                    synthesized.extend(callee.map(RcLocal::stable_id));
                    return;
                }
            }
            calls.total += 1;
            match callee {
                Some(local) => callees.entry(local.stable_id()).or_insert_with(|| (local.clone(), 0)).1 += 1,
                None => unbound += 1,
            }
        });
        stats.helpers = Helpers { reconstructed: callees.len(), synthesized: synthesized.len() };
        let name = |local: &RcLocal| local.0.0.lock().0.clone().unwrap_or_else(|| "?".to_string());
        stats.calls_by_helper = callees
            .iter()
            .map(|(id, (local, calls))| HelperCalls { proto: protos.get(id).copied(), helper: name(local), calls: *calls })
            .chain((unbound > 0).then(|| HelperCalls { proto: None, helper: "?".into(), calls: unbound }))
            .collect();
        stats.calls_by_helper.sort_unstable();
        if let Some(state) = STATE.with(|state| state.borrow_mut().take()) {
            for (binder, reason) in state.refused_helpers {
                if !callees.contains_key(&binder) {
                    *stats.refused_helpers.entry(reason).or_default() += 1;
                }
            }
            stats.refused_sites = state.refused_sites;
        }
        // Dropping the scope restores the enclosing one.
        stats
    }
}

/// Every call in `block`: nested blocks, and each closure literal's body as
/// often as it is printed. Records, in `protos`, the prototype of every local
/// defined as a lifted function (`local function f`, `f = function`).
fn count_block(block: &Block, protos: &mut FxHashMap<u64, usize>, visit: &mut impl FnMut(&crate::Call)) {
    for statement in &block.0 {
        match statement {
            Statement::Call(call) => visit(call),
            Statement::Assign(assign) => {
                for (target, value) in assign.left.iter().zip(&assign.right) {
                    if let (LValue::Local(local), RValue::Closure(closure)) = (target, value)
                        && let Some(proto) = closure.function.lock().bytecode_proto_id
                    {
                        protos.insert(local.stable_id(), proto);
                    }
                }
            }
            _ => {}
        }
        statement.traverse_rvalues_ref(&mut |value| match value {
            RValue::Call(call) | RValue::Select(Select::Call(call)) => visit(call),
            RValue::Closure(closure) => count_block(&closure.function.lock().body, protos, &mut *visit),
            _ => {}
        });
        match statement {
            Statement::If(r#if) => {
                count_block(&r#if.then_block.lock(), protos, visit);
                count_block(&r#if.else_block.lock(), protos, visit);
            }
            Statement::While(r#while) => count_block(&r#while.block.lock(), protos, visit),
            Statement::Repeat(repeat) => count_block(&repeat.block.lock(), protos, visit),
            Statement::NumericFor(numeric_for) => count_block(&numeric_for.block.lock(), protos, visit),
            Statement::GenericFor(generic_for) => count_block(&generic_for.block.lock(), protos, visit),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Assign, Call, Closure, Function, If, Literal};
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use triomphe::Arc;

    fn rebuilt(callee: &RcLocal, kind: Kind) -> Call {
        Call::new(callee.clone().into(), vec![]).reconstructed(kind)
    }

    /// Statement, expression and arithmetic sites count; a synthesized
    /// helper's calls and ordinary calls do not; a closure body counts once
    /// per printed copy.
    #[test]
    fn counts_rebuilt_calls_by_producer_in_the_final_tree() {
        let named = |name: &str| RcLocal::new(crate::Local::new(Some(name.into())));
        let (helper, arithmetic, tail, plain) = (named("helper"), RcLocal::default(), named("tail"), named("plain"));
        let body = Function {
            body: Block(vec![Statement::Call(rebuilt(&helper, Kind::StatementDeinline))]),
            ..Function::default()
        };
        let closure = || -> RValue {
            Closure { node_origin: Default::default(), function: ByAddress(Arc::new(Mutex::new(body.clone()))), upvalues: vec![] }
                .into()
        };
        let shared = Closure { node_origin: Default::default(), function: ByAddress(Arc::new(Mutex::new(body.clone()))), upvalues: vec![] };
        let block = Block(vec![
            Statement::Call(rebuilt(&helper, Kind::StatementDeinline)),
            Assign::new(vec![RcLocal::default().into()], vec![
                crate::Binary::new(rebuilt(&helper, Kind::ExpressionDeinline).into(), rebuilt(&arithmetic, Kind::ArithmeticDeinline).into(),
                    crate::BinaryOperation::Add).into(),
            ]).into(),
            If::new(Literal::Boolean(true).into(), Block(vec![Statement::Call(rebuilt(&tail, Kind::TerminalSynthesis))]),
                Block(vec![Statement::Call(Call::new(plain.into(), vec![]))])).into(),
            Statement::Call(Call::new(crate::Global::from("keep").into(), vec![closure(), shared.clone().into(), shared.into()])),
        ]);
        let stats = enter(true).finish(&block);
        let calls = &stats.reconstructed_calls;
        assert_eq!((calls.statement, calls.expression, calls.arithmetic, calls.total), (4, 1, 1, 6));
        assert_eq!(stats.synthesized_calls, 1);
        assert_eq!((stats.helpers.reconstructed, stats.helpers.synthesized), (2, 1));
        assert_eq!(stats.calls_by_helper, vec![
            HelperCalls { proto: None, helper: "?".into(), calls: 1 },
            HelperCalls { proto: None, helper: "helper".into(), calls: 5 },
        ]);
    }

    /// Two helpers that print alike stay two entries, each with the
    /// prototype its definition was lifted from.
    #[test]
    fn helpers_that_print_alike_stay_apart_by_prototype() {
        let named = |name: &str| RcLocal::new(crate::Local::new(Some(name.into())));
        let define = |helper: &RcLocal, proto: usize| -> Statement {
            let function = Function { bytecode_proto_id: Some(proto), ..Function::default() };
            let closure = Closure { node_origin: Default::default(), function: ByAddress(Arc::new(Mutex::new(function))), upvalues: vec![] };
            Assign { prefix: true, ..Assign::new(vec![helper.clone().into()], vec![closure.into()]) }.into()
        };
        let (first, second) = (named("fn"), named("fn"));
        let block = Block(vec![
            define(&first, 3),
            define(&second, 7),
            Statement::Call(rebuilt(&first, Kind::StatementDeinline)),
            Statement::Call(rebuilt(&first, Kind::StatementDeinline)),
            Statement::Call(rebuilt(&second, Kind::ExpressionDeinline)),
        ]);
        let stats = enter(true).finish(&block);
        assert_eq!(stats.calls_by_helper, vec![
            HelperCalls { proto: Some(3), helper: "fn".into(), calls: 2 },
            HelperCalls { proto: Some(7), helper: "fn".into(), calls: 1 },
        ]);
        assert_eq!(stats.helpers.reconstructed, 2);
    }

    /// A helper's last refusal counts, unless it was accepted later or one
    /// of its calls was rebuilt; site refusals count every attempt.
    #[test]
    fn refusals_count_per_helper_and_per_site_attempt() {
        let scope = enter(true);
        refuse_helper(1, "low_anchors");
        refuse_helper(1, "return_shape");
        refuse_helper(2, "variadic");
        refuse_helper(3, "low_anchors");
        accept_helper(3);
        let rebuilt_helper = RcLocal::default();
        refuse_helper(rebuilt_helper.stable_id(), "low_anchors");
        refuse_site("ambiguous");
        refuse_site("ambiguous");
        let stats = scope.finish(&Block(vec![Statement::Call(rebuilt(&rebuilt_helper, Kind::StatementDeinline))]));
        assert_eq!(stats.refused_helpers, BTreeMap::from([("return_shape", 1), ("variadic", 1)]));
        assert_eq!(stats.refused_sites, BTreeMap::from([("ambiguous", 2)]));
    }

    /// Outside a scope nothing is recorded, and a nested scope leaves the
    /// outer one's records alone.
    #[test]
    fn scopes_nest_and_record_nothing_when_disabled() {
        refuse_site("outside");
        let outer = enter(true);
        refuse_site("outer");
        {
            let inner = enter(true);
            refuse_site("inner");
            assert_eq!(inner.finish(&Block::default()).refused_sites, BTreeMap::from([("inner", 1)]));
            let _off = enter(false);
            refuse_site("off");
        }
        assert_eq!(outer.finish(&Block::default()).refused_sites, BTreeMap::from([("outer", 1)]));
    }
}
