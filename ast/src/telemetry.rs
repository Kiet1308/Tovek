//! Bounded, opt-in pass measurements. This module never retains AST owners.
//! Times are nested per-thread wall durations, not CPU time or cross-worker
//! exclusive time. Instrumented runs must not be used as speed benchmarks.
#[cfg(feature = "phase-allocation-trace")]
pub mod allocation;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use crate::{Block, RValue, Statement, Traverse};
use parking_lot::Mutex;
use serde::Serialize;

pub const ROW_LIMIT: usize = 1_000_000;
const STACK_LIMIT: usize = 256;
pub const NODE_LIMIT: usize = 1_000_000;
const DEPTH_LIMIT: usize = 256;

#[inline]
pub fn enabled() -> bool {
    #[cfg(feature = "phase-allocation-trace")]
    let _allocations = allocation::Suppress::new();
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("MEDAL_PROFILE_JSON").is_some_and(|s| !s.is_empty()))
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Context {
    pub script: Arc<str>,
    pub prototype: Option<usize>,
}

impl Context {
    pub fn prototype(&self, prototype: usize) -> Self {
        Self {
            script: self.script.clone(),
            prototype: Some(prototype),
        }
    }
}

#[derive(Default)]
struct State {
    context: Option<Context>,
    sequence: u64,
    stack: Vec<Frame>,
}
thread_local! { static STATE: RefCell<State> = RefCell::default(); }

/// !Send: a scope must restore the same worker's context, including on unwind.
pub struct ContextGuard(Option<Option<Context>>, PhantomData<Rc<()>>);
pub fn enter(context: Option<Context>) -> ContextGuard {
    #[cfg(feature = "phase-allocation-trace")]
    let _allocations = allocation::Suppress::new();
    let old =
        context.map(|context| STATE.with(|state| state.borrow_mut().context.replace(context)));
    ContextGuard(old, PhantomData)
}
impl Drop for ContextGuard {
    fn drop(&mut self) {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        if let Some(old) = self.0.take() {
            STATE.with(|state| state.borrow_mut().context = old);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
struct Key {
    #[serde(flatten)]
    context: Context,
    pass: &'static str,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Nodes {
    pub statements: u64,
    pub values: u64,
    pub incomplete: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Statistics {
    calls: u64,
    inclusive_ns: u64,
    exclusive_ns: u64,
    max_inclusive_ns: u64,
    node_samples: u64,
    nodes_before_sum: Nodes,
    nodes_after_sum: Nodes,
    node_samples_incomplete: u64,
    counters: BTreeMap<&'static str, u64>,
    #[cfg(feature = "phase-allocation-trace")]
    allocation_inclusive: allocation::Counts,
    #[cfg(feature = "phase-allocation-trace")]
    allocation_exclusive: allocation::Counts,
}

#[derive(Default, Serialize)]
pub struct Report {
    #[serde(skip)]
    rows: BTreeMap<Key, Statistics>,
    pub dropped_records: u64,
    pub misnested_spans: u64,
}

static REPORT: OnceLock<Mutex<Report>> = OnceLock::new();
fn report() -> &'static Mutex<Report> {
    REPORT.get_or_init(Default::default)
}

/// Drain after all workers finish. The caller can serialize rows one at a time.
pub fn take_report() -> Report {
    #[cfg(feature = "phase-allocation-trace")]
    let _allocations = allocation::Suppress::new();
    std::mem::take(&mut *report().lock())
}
impl Report {
    pub fn rows_len(&self) -> usize {
        self.rows.len()
    }
    pub fn rows(&self) -> impl Iterator<Item = impl Serialize + '_> {
        #[derive(Serialize)]
        struct Row<'a> {
            #[serde(flatten)]
            key: &'a Key,
            #[serde(flatten)]
            stats: &'a Statistics,
        }
        self.rows.iter().map(|(key, stats)| Row { key, stats })
    }

    fn add(&mut self, completed: Completed, before: Option<Nodes>, after: Option<Nodes>) {
        self.add_bounded(completed, before, after, ROW_LIMIT);
    }

    fn add_bounded(
        &mut self,
        completed: Completed,
        before: Option<Nodes>,
        after: Option<Nodes>,
        limit: usize,
    ) {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        if self.rows.len() >= limit && !self.rows.contains_key(&completed.key) {
            self.dropped_records += 1;
            return;
        }
        self.misnested_spans += u64::from(completed.misnested);
        let stats = self.rows.entry(completed.key).or_default();
        stats.calls += 1;
        stats.inclusive_ns = stats.inclusive_ns.saturating_add(completed.inclusive);
        stats.exclusive_ns = stats.exclusive_ns.saturating_add(completed.exclusive);
        stats.max_inclusive_ns = stats.max_inclusive_ns.max(completed.inclusive);
        #[cfg(feature = "phase-allocation-trace")]
        {
            stats.allocation_inclusive.add(completed.allocation_inclusive);
            stats.allocation_exclusive.add(completed.allocation_exclusive);
        }
        for (key, value) in completed.counters {
            let entry = stats.counters.entry(key).or_default();
            *entry = entry.saturating_add(value);
        }
        if let (Some(before), Some(after)) = (before, after) {
            stats.node_samples += 1;
            stats.nodes_before_sum.statements += before.statements;
            stats.nodes_before_sum.values += before.values;
            stats.nodes_after_sum.statements += after.statements;
            stats.nodes_after_sum.values += after.values;
            stats.nodes_before_sum.incomplete |= before.incomplete;
            stats.nodes_after_sum.incomplete |= after.incomplete;
            stats.node_samples_incomplete += u64::from(before.incomplete || after.incomplete);
        }
    }
}

struct Frame {
    token: u64,
    key: Key,
    start: Instant,
    children: Duration,
    counters: BTreeMap<&'static str, u64>,
    #[cfg(feature = "phase-allocation-trace")]
    allocation_start: allocation::Counts,
    #[cfg(feature = "phase-allocation-trace")]
    allocation_children: allocation::Counts,
}
struct Completed {
    key: Key,
    inclusive: u64,
    exclusive: u64,
    counters: BTreeMap<&'static str, u64>,
    misnested: bool,
    #[cfg(feature = "phase-allocation-trace")]
    allocation_inclusive: allocation::Counts,
    #[cfg(feature = "phase-allocation-trace")]
    allocation_exclusive: allocation::Counts,
}

impl State {
    fn begin(&mut self, name: &'static str, now: Instant) -> Option<u64> {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        let context = self.context.clone()?;
        if self.stack.len() >= STACK_LIMIT {
            report().lock().dropped_records += 1;
            return None;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.stack.push(Frame {
            token: self.sequence,
            key: Key {
                context,
                pass: name,
            },
            start: now,
            children: Duration::ZERO,
            counters: BTreeMap::new(),
            #[cfg(feature = "phase-allocation-trace")]
            allocation_start: allocation::snapshot(),
            #[cfg(feature = "phase-allocation-trace")]
            allocation_children: allocation::Counts::default(),
        });
        Some(self.sequence)
    }

    fn finish(&mut self, token: u64, now: Instant) -> Option<Completed> {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        #[cfg(feature = "phase-allocation-trace")]
        let allocation_end = allocation::snapshot();
        let index = self.stack.iter().rposition(|frame| frame.token == token)?;
        let misnested = index + 1 != self.stack.len();
        let frame = self.stack.remove(index);
        let elapsed = now.saturating_duration_since(frame.start);
        #[cfg(feature = "phase-allocation-trace")]
        let (allocation_inclusive, allocation_exclusive) = {
            let mut inclusive = allocation_end.difference(frame.allocation_start);
            inclusive.incomplete |= misnested;
            // Live descendants now cross a removed parent. Their deltas still
            // count events, but are no longer a disjoint nested attribution.
            if misnested {
                for descendant in &mut self.stack[index..] {
                    descendant.allocation_start.incomplete = true;
                }
            }
            let exclusive = inclusive.difference(frame.allocation_children);
            if index > 0 { self.stack[index - 1].allocation_children.add(inclusive); }
            (inclusive, exclusive)
        };
        if index > 0 {
            self.stack[index - 1].children += elapsed;
        }
        Some(Completed {
            key: frame.key,
            inclusive: nanos(elapsed),
            exclusive: nanos(elapsed.saturating_sub(frame.children)),
            counters: frame.counters,
            misnested,
            #[cfg(feature = "phase-allocation-trace")]
            allocation_inclusive,
            #[cfg(feature = "phase-allocation-trace")]
            allocation_exclusive,
        })
    }
}
fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// A phase span carries no reference to the measured AST.
pub struct Span {
    token: Option<u64>,
    before: Option<Nodes>,
    _thread: PhantomData<Rc<()>>,
}
impl Span {
    pub fn new(name: &'static str) -> Self {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        let token = if enabled() {
            STATE.with(|state| state.borrow_mut().begin(name, Instant::now()))
        } else {
            None
        };
        Self {
            token,
            before: None,
            _thread: PhantomData,
        }
    }
    pub fn ast(name: &'static str, body: &Block, include_closures: bool) -> Self {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        let before = enabled().then(|| count_nodes(body, include_closures));
        let mut span = Self::new(name);
        span.before = before;
        span
    }
    fn stop(&mut self) -> Option<Completed> {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        self.token
            .take()
            .and_then(|token| STATE.with(|state| state.borrow_mut().finish(token, Instant::now())))
    }
    pub fn finish_ast(mut self, body: &Block, include_closures: bool) {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        if let Some(completed) = self.stop() {
            // The after-census is outside this phase's elapsed interval.
            let after = count_nodes(body, include_closures);
            report().lock().add(completed, self.before, Some(after));
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = allocation::Suppress::new();
        if let Some(completed) = self.stop() {
            report().lock().add(completed, self.before, None);
        }
    }
}

/// Metrics belong to the innermost phase. Names are static and never input data.
#[inline]
pub fn count(name: &'static str, amount: u64) {
    #[cfg(feature = "phase-allocation-trace")]
    let _allocations = allocation::Suppress::new();
    if enabled() {
        STATE.with(|state| {
            if let Some(frame) = state.borrow_mut().stack.last_mut() {
                let entry = frame.counters.entry(name).or_default();
                *entry = entry.saturating_add(amount);
            }
        });
    }
}

/// Statement/rvalue census, including indexed assignment operands. Binder
/// declarations, types and implicit storage references are not value nodes.
/// Child closure bodies are visited once only when the caller owns them.
pub fn count_nodes(body: &Block, include_closures: bool) -> Nodes {
    #[cfg(feature = "phase-allocation-trace")]
    let _allocations = allocation::Suppress::new();
    struct Census {
        nodes: Nodes,
        include_closures: bool,
        closures: BTreeSet<usize>,
        blocks: BTreeSet<usize>,
    }
    impl Census {
        fn room(&mut self, depth: usize) -> bool {
            if depth > DEPTH_LIMIT || self.nodes.statements + self.nodes.values >= NODE_LIMIT as u64
            {
                self.nodes.incomplete = true;
                false
            } else {
                true
            }
        }
        fn block(&mut self, body: &Block, depth: usize) {
            if !self.room(depth) {
                return;
            }
            let address = body as *const Block as usize;
            if !self.blocks.insert(address) {
                self.nodes.incomplete = true;
                return;
            }
            for statement in &body.0 {
                if !self.room(depth) {
                    break;
                }
                self.nodes.statements += 1;
                for value in crate::deinline::stmt_rvalues(statement) {
                    self.value(value, depth + 1);
                }
                match statement {
                    Statement::If(node) => {
                        self.locked_block(&node.then_block, depth + 1);
                        self.locked_block(&node.else_block, depth + 1);
                    }
                    Statement::While(node) => self.locked_block(&node.block, depth + 1),
                    Statement::Repeat(node) => self.locked_block(&node.block, depth + 1),
                    Statement::NumericFor(node) => self.locked_block(&node.block, depth + 1),
                    Statement::GenericFor(node) => self.locked_block(&node.block, depth + 1),
                    _ => {}
                }
            }
            self.blocks.remove(&address);
        }
        fn locked_block(&mut self, block: &triomphe::Arc<Mutex<Block>>, depth: usize) {
            if let Some(block) = block.try_lock() {
                self.block(&block, depth);
            } else {
                self.nodes.incomplete = true;
            }
        }
        fn value(&mut self, value: &RValue, depth: usize) {
            if !self.room(depth) {
                return;
            }
            self.nodes.values += 1;
            if let RValue::Closure(closure) = value {
                if self.include_closures
                    && self
                        .closures
                        .insert(triomphe::Arc::as_ptr(&closure.function) as usize)
                {
                    if let Some(function) = closure.function.try_lock() {
                        self.block(&function.body, depth + 1);
                    } else {
                        self.nodes.incomplete = true;
                    }
                }
            }
            for child in value.rvalues() {
                self.value(child, depth + 1);
            }
        }
    }
    let mut census = Census {
        nodes: Nodes::default(),
        include_closures,
        closures: BTreeSet::new(),
        blocks: BTreeSet::new(),
    };
    census.block(body, 0);
    census.nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "phase-allocation-trace")]
    #[test]
    fn nested_allocation_events_subtract_children_once() {
        let mut state = State { context: Some(context("allocations")), ..State::default() };
        let now = Instant::now();
        let parent = state.begin("parent", now).unwrap();
        allocation::allocated(11);
        let child = state.begin("child", now).unwrap();
        allocation::allocated(17);
        allocation::reallocated(17, 31);
        let child = state.finish(child, now).unwrap();
        allocation::deallocated(31);
        let parent = state.finish(parent, now).unwrap();
        assert_eq!(child.allocation_inclusive, allocation::Counts {
            allocation_calls: 1, reallocation_calls: 1, requested_bytes: 48,
            reallocated_old_bytes: 17, ..allocation::Counts::default()
        });
        assert_eq!(child.allocation_exclusive, child.allocation_inclusive);
        assert_eq!(parent.allocation_exclusive, allocation::Counts {
            allocation_calls: 1, deallocation_calls: 1, requested_bytes: 11,
            deallocated_bytes: 31, ..allocation::Counts::default()
        });
        let mut reconstructed = parent.allocation_exclusive;
        reconstructed.add(child.allocation_inclusive);
        assert_eq!(parent.allocation_inclusive, reconstructed);
    }

    #[cfg(feature = "phase-allocation-trace")]
    #[test]
    fn profiler_census_stack_and_report_allocations_are_suppressed() {
        let mut state = State { context: Some(context("bookkeeping")), ..State::default() };
        let body = Block::from(vec![crate::Return::new(vec![crate::Literal::from("payload").into()]).into()]);
        let mut report = Report::default();
        let now = Instant::now();
        let before = allocation::snapshot();
        let token = state.begin("census", now).unwrap();
        let nodes = count_nodes(&body, false);
        let completed = state.finish(token, now).unwrap();
        assert_eq!(completed.allocation_inclusive, allocation::Counts::default());
        report.add(completed, Some(nodes), Some(nodes));
        assert_eq!(allocation::snapshot(), before);
        // Profiler data destruction is excluded by the exporting caller too.
        let _suppressed = allocation::Suppress::new();
        drop(report);
        drop(state);
    }

    #[cfg(feature = "phase-allocation-trace")]
    #[test]
    fn allocation_events_stay_on_the_executing_thread() {
        let mut state = State { context: Some(context("waiting-parent")), ..State::default() };
        let now = Instant::now();
        let parent = state.begin("parent", now).unwrap();
        allocation::allocated(5);
        let child = {
            let _suppressed = allocation::Suppress::new();
            let worker = std::thread::spawn(|| {
                let mut state = {
                    let _suppressed = allocation::Suppress::new();
                    State { context: Some(context("worker")), ..State::default() }
                };
                let token = state.begin("worker", Instant::now()).unwrap();
                allocation::allocated(19);
                state.finish(token, Instant::now()).unwrap().allocation_inclusive
            });
            worker.join().unwrap()
        };
        let parent = state.finish(parent, now).unwrap();
        assert_eq!(child.requested_bytes, 19);
        assert_eq!(parent.allocation_inclusive.requested_bytes, 5);
        assert_eq!(parent.allocation_exclusive, parent.allocation_inclusive);
    }

    #[cfg(feature = "phase-allocation-trace")]
    #[test]
    fn allocation_context_and_span_restore_after_unwind() {
        let _outer_context = enter(Some(context("allocation-outer")));
        let _outer = Span::new("allocation-outer");
        let before = allocation::snapshot();
        {
            // Unwinding itself allocates; exclude that runtime work here so
            // the assertion isolates profiler restoration and bookkeeping.
            let _suppressed = allocation::Suppress::new();
            let _ = std::panic::catch_unwind(|| {
                let _inner_context = enter(Some(context("allocation-inner")));
                let _inner = Span::new("allocation-inner");
                panic!("allocation context probe");
            });
        }
        assert_eq!(allocation::snapshot(), before);
        STATE.with(|state| {
            let state = state.borrow();
            assert_eq!(state.context.as_ref().unwrap().script.as_ref(), "allocation-outer");
            assert_eq!(state.stack.len(), usize::from(enabled()));
        });
        allocation::allocated(13);
        assert_eq!(allocation::snapshot().difference(before).requested_bytes, 13);
    }
    fn context(name: &str) -> Context {
        Context {
            script: name.into(),
            prototype: None,
        }
    }

    #[test]
    fn nested_thread_wall_time_subtracts_children_once() {
        let mut state = State {
            context: Some(context("file")),
            ..State::default()
        };
        let start = Instant::now();
        let parent = state.begin("parent", start).unwrap();
        let child = state
            .begin("child", start + Duration::from_nanos(3))
            .unwrap();
        let child = state
            .finish(child, start + Duration::from_nanos(8))
            .unwrap();
        let parent = state
            .finish(parent, start + Duration::from_nanos(12))
            .unwrap();
        assert_eq!((child.inclusive, child.exclusive), (5, 5));
        assert_eq!((parent.inclusive, parent.exclusive), (12, 7));
        assert!(!parent.misnested);
    }

    #[test]
    fn context_restores_after_unwind_and_does_not_cross_threads() {
        let outer = enter(Some(context("outer")));
        let _ = std::panic::catch_unwind(|| {
            let _inner = enter(Some(context("inner")));
            panic!("probe");
        });
        STATE.with(|s| {
            assert_eq!(
                s.borrow().context.as_ref().unwrap().script.as_ref(),
                "outer"
            )
        });
        std::thread::spawn(|| STATE.with(|s| assert!(s.borrow().context.is_none())))
            .join()
            .unwrap();
        drop(outer);
        STATE.with(|s| assert!(s.borrow().context.is_none()));
    }

    #[test]
    fn an_out_of_order_span_is_reported() {
        let mut state = State {
            context: Some(context("file")),
            ..State::default()
        };
        let start = Instant::now();
        let outer = state.begin("outer", start).unwrap();
        let inner = state.begin("inner", start).unwrap();
        let outer = state.finish(outer, start).unwrap();
        let inner = state.finish(inner, start).unwrap();
        assert!(outer.misnested);
        assert!(!inner.misnested);
        #[cfg(feature = "phase-allocation-trace")]
        {
            assert!(outer.allocation_inclusive.incomplete);
            assert!(inner.allocation_inclusive.incomplete);
        }
    }

    #[test]
    fn node_census_respects_function_ownership_without_retaining_locals() {
        let local = crate::RcLocal::default();
        let mut function = crate::Function::default();
        function
            .body
            .push(crate::Return::new(vec![local.clone().into()]).into());
        let closure = crate::Closure {
            node_origin: Default::default(),
            function: by_address::ByAddress(triomphe::Arc::new(Mutex::new(function))),
            upvalues: vec![],
        };
        let body = Block::from(vec![crate::Return::new(vec![closure.into()]).into()]);
        let owners = triomphe::Arc::count(&local.0 .0);
        assert_eq!(count_nodes(&body, false).statements, 1);
        assert_eq!(count_nodes(&body, true).statements, 2);
        assert_eq!(triomphe::Arc::count(&local.0 .0), owners);
    }

    #[test]
    fn row_budget_preserves_existing_aggregates_and_reports_drops() {
        let mut report = Report::default();
        let mut state = State {
            context: Some(context("file")),
            ..State::default()
        };
        let now = Instant::now();
        for pass in ["a", "b", "a"] {
            let token = state.begin(pass, now).unwrap();
            report.add_bounded(state.finish(token, now).unwrap(), None, None, 1);
        }
        assert_eq!(report.rows_len(), 1);
        assert_eq!(report.rows.values().next().unwrap().calls, 2);
        assert_eq!(report.dropped_records, 1);
    }

    #[test]
    fn census_depth_and_unavailable_lock_are_explicitly_partial() {
        let mut value: RValue = crate::Literal::Boolean(true).into();
        for _ in 0..300 {
            value = crate::Unary::new(value, crate::UnaryOperation::Not).into();
        }
        let body = Block::from(vec![crate::Return::new(vec![value]).into()]);
        assert!(count_nodes(&body, true).incomplete);
        let node = crate::If::new(
            crate::Literal::Boolean(true).into(),
            Block::default(),
            Block::default(),
        );
        let block = node.then_block.clone();
        let _guard = block.lock();
        assert!(count_nodes(&Block::from(vec![node.into()]), true).incomplete);
    }
}
