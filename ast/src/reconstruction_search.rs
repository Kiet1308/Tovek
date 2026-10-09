//! Input line/PC hints prioritize helper candidates. They never authorize a
//! rewrite, eliminate a rival, or establish the PC of an original source call.
use std::{cell::{Cell, RefCell}, collections::{BTreeMap, BTreeSet}, marker::PhantomData, rc::Rc};
use serde::Serialize;

pub const PC_LIMIT: usize = 200_000;
pub const REGION_LIMIT: usize = 8192;
const OWNERS_PER_LINE: usize = 16;
// Retain only bounded, already-owned payloads. Larger inputs keep the eager
// lifetime, dropping decoded lines before the AST pipeline as before.
const DEFERRED_LINE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Default)]
struct Counters {
    scopes: u64,
    known_inputs: u64,
    prototypes: u64,
    pcs: u64,
    single_prototype: u64,
    budget_refusals: u64,
    deferred: u64,
    deferred_bytes: u64,
    eager_capacity_fallbacks: u64,
    force_prioritize: u64,
    force_report: u64,
    unused_deferred: u64,
    unused_deferred_bytes: u64,
    priority_calls: u64,
    registered_caller_calls: u64,
    eligible_priority_calls: u64,
    changed_priority_calls: u64,
    report_calls: u64,
    owner_builds: u64,
    owner_lines: u64,
    owner_duplicate_skips: u64,
}

// No per-query telemetry map writes or production counter updates. Keeping
// counters behind a diagnostic-only allocation also keeps normal Scope small.
#[derive(Default)]
struct Statistics(Option<Box<Cell<Counters>>>);
impl Statistics {
    fn new() -> Self {
        Self::new_if(crate::telemetry::enabled())
    }
    fn new_if(enabled: bool) -> Self {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = crate::telemetry::allocation::Suppress::new();
        Self(enabled.then(|| Box::new(Cell::new(Counters { scopes: 1, ..Counters::default() }))))
    }
    fn update(&self, update: impl FnOnce(&mut Counters)) {
        if let Some(cell) = &self.0 {
            let mut counters = cell.get();
            update(&mut counters);
            cell.set(counters);
        }
    }
    fn flush(&self) {
        let Some(cell) = &self.0 else { return; };
        let counters = cell.get();
        macro_rules! emit {
            ($($field:ident),* $(,)?) => { $(
                if counters.$field != 0 {
                    crate::telemetry::count(concat!("reconstruction_", stringify!($field)), counters.$field);
                }
            )* };
        }
        emit!(scopes, known_inputs, prototypes, pcs, single_prototype, budget_refusals,
              deferred, deferred_bytes, eager_capacity_fallbacks, force_prioritize,
              force_report, unused_deferred, unused_deferred_bytes, priority_calls,
              registered_caller_calls, eligible_priority_calls, changed_priority_calls,
              report_calls, owner_builds, owner_lines, owner_duplicate_skips);
    }
}
impl Drop for Statistics {
    fn drop(&mut self) {
        #[cfg(feature = "phase-allocation-trace")]
        let _allocations = crate::telemetry::allocation::Suppress::new();
        drop(self.0.take());
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Region {
    pub caller_prototype: usize,
    pub helper_prototype: usize,
    pub start_pc: usize,
    pub end_pc_exclusive: usize,
}
#[derive(Default)]
struct State {
    functions: BTreeMap<usize, usize>,
    pairs: BTreeSet<(usize, usize)>,
    regions: Vec<Region>,
    truncated: bool,
    pending_lines: Option<Vec<Vec<Option<u32>>>>,
    statistics: Statistics,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
pub struct Scope(State, PhantomData<Rc<()>>);
impl Drop for Scope {
    fn drop(&mut self) {
        let current = STATE.with(|s| s.replace(std::mem::take(&mut self.0)));
        if let Some(lines) = &current.pending_lines {
            current.statistics.update(|c| {
                c.unused_deferred += 1;
                c.unused_deferred_bytes += retained_line_bytes(lines).expect("admitted bounded lines") as u64;
            });
        }
        current.statistics.flush();
    }
}

pub fn enter(lines: Vec<Vec<Option<u32>>>) -> Scope {
    let pc_count = lines.iter().map(Vec::len).sum::<usize>();
    let mut state = State { statistics: Statistics::new(), ..State::default() };
    state.statistics.update(|c| { c.known_inputs = 1; c.prototypes = lines.len() as u64; c.pcs = pc_count as u64; });
    if pc_count > PC_LIMIT || lines.len() > 4096 {
        state.truncated = true;
        state.statistics.update(|c| c.budget_refusals = 1);
    } else if lines.len() <= 1 {
        // Every helper would equal its caller, so no pair or region can exist.
        state.statistics.update(|c| c.single_prototype = u64::from(lines.len() == 1));
    } else if let Some(bytes) = retained_line_bytes(&lines).filter(|&bytes| bytes <= DEFERRED_LINE_BYTES) {
        state.statistics.update(|c| { c.deferred = 1; c.deferred_bytes = bytes as u64; });
        state.pending_lines = Some(lines);
    } else {
        state.statistics.update(|c| c.eager_capacity_fallbacks = 1);
        state.build(&lines);
    }
    Scope(STATE.with(|s| s.replace(state)), PhantomData)
}

/// The library can avoid decoding lines when there is exactly one prototype.
/// Keep the same PC-budget refusal even though no cross-prototype pair exists.
pub fn enter_single_prototype(pc_count: usize) -> Scope {
    let state = State {
        truncated: pc_count > PC_LIMIT,
        statistics: Statistics::new(),
        ..State::default()
    };
    state.statistics.update(|c| {
        c.known_inputs = 1; c.prototypes = 1; c.pcs = pc_count as u64;
        if state.truncated { c.budget_refusals = 1; } else { c.single_prototype = 1; }
    });
    Scope(STATE.with(|s| s.replace(state)), PhantomData)
}

fn retained_line_bytes(lines: &Vec<Vec<Option<u32>>>) -> Option<usize> {
    let outer = lines.capacity().checked_mul(std::mem::size_of::<Vec<Option<u32>>>())?;
    lines.iter().try_fold(outer, |bytes, line| {
        bytes.checked_add(line.capacity().checked_mul(std::mem::size_of::<Option<u32>>())?)
    })
}

impl State {
    fn build(&mut self, lines: &[Vec<Option<u32>>]) {
        if self.statistics.0.is_some() {
            self.build_with_counters::<true>(lines);
        } else {
            self.build_with_counters::<false>(lines);
        }
    }

    fn build_with_counters<const TRACE: bool>(&mut self, lines: &[Vec<Option<u32>>]) {
        let mut owner_lines = 0u64;
        let mut owner_duplicate_skips = 0u64;
        let mut owners: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
        for (proto, pcs) in lines.iter().enumerate() {
            let mut previous = None;
            for &line in pcs.iter().flatten().filter(|&&line| line != 0) {
                if TRACE { owner_lines += 1; }
                if previous == Some(line) {
                    if TRACE { owner_duplicate_skips += 1; }
                    continue;
                }
                previous = Some(line);
                let entry = owners.entry(line).or_default();
                if entry.len() <= OWNERS_PER_LINE {
                    // Prototypes arrive in ascending order, including repeated
                    // nonconsecutive lines. Preserve the seventeenth-owner sentinel.
                    if entry.last() == Some(&proto) {
                        if TRACE { owner_duplicate_skips += 1; }
                    } else { entry.insert(proto); }
                }
            }
        }
        'callers: for (caller, pcs) in lines.iter().enumerate() {
            let mut last: BTreeMap<usize, usize> = BTreeMap::new();
            for (pc, line) in pcs.iter().enumerate() {
                let Some(helpers) = line.and_then(|line| owners.get(&line)) else { continue; };
                if helpers.len() > OWNERS_PER_LINE { continue; }
                for &helper in helpers {
                    if caller == helper { continue; }
                    if let Some(region) = last.get(&helper).map(|&i| &mut self.regions[i]) {
                        if region.end_pc_exclusive == pc { region.end_pc_exclusive += 1; continue; }
                    }
                    if self.regions.len() >= REGION_LIMIT { self.truncated = true; break 'callers; }
                    last.insert(helper, self.regions.len());
                    self.pairs.insert((caller, helper));
                    self.regions.push(Region { caller_prototype: caller, helper_prototype: helper, start_pc: pc, end_pc_exclusive: pc + 1 });
                }
            }
        }
        if TRACE {
            self.statistics.update(|c| { c.owner_builds += 1; c.owner_lines += owner_lines; c.owner_duplicate_skips += owner_duplicate_skips; });
        }
    }

    fn keys(&self, caller: Option<usize>, helpers: &[usize]) -> Option<Vec<bool>> {
        let caller = caller.and_then(|c| self.functions.get(&c))?;
        Some(helpers.iter().map(|helper| {
            self.functions.get(helper).is_some_and(|callee| self.pairs.contains(&(*caller, *callee)))
        }).collect())
    }

    fn order(&self, candidates: &[usize], ordered: &mut Vec<usize>, caller: Option<usize>, function: &impl Fn(usize) -> usize) {
        let caller = caller.and_then(|c| self.functions.get(&c));
        self.statistics.update(|c| {
            c.priority_calls += 1;
            c.registered_caller_calls += u64::from(caller.is_some());
            c.eligible_priority_calls += u64::from(caller.is_some() && candidates.len() >= 2);
        });
        let Some(caller) = caller else { return; };
        ordered.sort_by_key(|&i| !self.functions.get(&function(i)).is_some_and(|callee| self.pairs.contains(&(*caller, *callee))));
        if self.statistics.0.is_some() && ordered.as_slice() != candidates {
            self.statistics.update(|c| c.changed_priority_calls += 1);
        }
    }
}

/// Preserve the budget-refusal state without allocating a fake PC table.
pub fn enter_truncated() -> Scope {
    let state = State { truncated: true, statistics: Statistics::new(), ..State::default() };
    state.statistics.update(|c| c.budget_refusals = 1);
    Scope(STATE.with(|s| s.replace(state)), PhantomData)
}

pub fn register_function(identity: usize, prototype: usize) {
    STATE.with(|s| {
        let mut state = s.borrow_mut();
        if state.functions.len() < 50_000 { state.functions.insert(identity, prototype); }
    });
}

pub(crate) fn prioritize(candidates: &[usize], caller: Option<usize>, function: impl Fn(usize) -> usize) -> Vec<usize> {
    let mut ordered = candidates.to_vec();
    STATE.with(|s| {
        {
            let state = s.borrow();
            if state.pending_lines.is_none() || candidates.len() < 2
                || !caller.is_some_and(|c| state.functions.contains_key(&c)) {
                // Normal ready-state calls keep one borrow and one caller lookup.
                state.order(candidates, &mut ordered, caller, &function);
                return;
            }
        }
        // Release this mutable borrow before invoking the caller's function
        // callback under the original immutable sort borrow. Nested read-only
        // queries/report calls keep their previous RefCell behavior.
        {
            let mut state = s.borrow_mut();
            if let Some(lines) = state.pending_lines.take() {
                state.statistics.update(|c| c.force_prioritize += 1);
                state.build(&lines);
            }
        }
        let state = s.borrow();
        state.order(candidates, &mut ordered, caller, &function);
    });
    ordered
}

/// [`prioritize`]'s sort key for each of `helpers` (by identity) under
/// `caller`: whether the helper's code is inlined into the caller's. Read
/// once per helper, so a caller ordering many candidate sets stably
/// partitions each by these instead of sorting it anew. `None` where
/// `prioritize` keeps every order as it is (an unregistered caller).
pub(crate) fn priority_keys(caller: Option<usize>, helpers: &[usize]) -> Option<Vec<bool>> {
    STATE.with(|s| {
        {
            let state = s.borrow();
            if state.pending_lines.is_none() || helpers.len() < 2
                || !caller.is_some_and(|c| state.functions.contains_key(&c)) {
                return state.keys(caller, helpers);
            }
        }
        {
            let mut state = s.borrow_mut();
            if let Some(lines) = state.pending_lines.take() {
                state.statistics.update(|c| c.force_prioritize += 1);
                state.build(&lines);
            }
        }
        s.borrow().keys(caller, helpers)
    })
}

pub fn report() -> (Vec<Region>, bool) {
    let force = STATE.with(|s| s.borrow().pending_lines.is_some());
    if force {
        STATE.with(|s| {
            let mut state = s.borrow_mut();
            if let Some(lines) = state.pending_lines.take() {
                state.statistics.update(|c| c.force_report += 1);
                state.build(&lines);
            }
        });
    }
    STATE.with(|s| {
        let state = s.borrow();
        state.statistics.update(|c| c.report_calls += 1);
        (state.regions.clone(), state.truncated)
    })
}

#[cfg(test)]
mod differential;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_pc_regions_prioritize_but_keep_every_rival_and_restore_scope() {
        let _scope = enter(vec![vec![Some(8), Some(9), Some(9), None], vec![Some(9)], vec![Some(30)]]);
        for (id, proto) in [(101, 0), (201, 1), (301, 2)] { register_function(id, proto); }
        assert_eq!(prioritize(&[2, 1], Some(101), |i| i * 100 + 101), vec![1, 2]);
        let (regions, truncated) = report();
        assert!(!truncated);
        assert!(regions.iter().any(|r| r.caller_prototype == 0 && r.helper_prototype == 1 && r.start_pc == 1 && r.end_pc_exclusive == 3));
        {
            let _nested = enter(vec![]);
            assert_eq!(prioritize(&[2, 1], Some(101), |i| i * 100 + 101), vec![2, 1]);
        }
        assert_eq!(prioritize(&[2, 1], Some(101), |i| i * 100 + 101), vec![1, 2]);
    }
    #[test]
    fn missing_or_over_budget_lines_fall_back_to_structural_order() {
        let _scope = enter(vec![vec![None; PC_LIMIT + 1]]);
        assert!(report().1);
        assert_eq!(prioritize(&[3, 1, 2], None, |i| i), vec![3, 1, 2]);
        let _nested = enter_truncated();
        assert!(report().1);
        assert!(report().0.is_empty());
        assert_eq!(prioritize(&[3, 1, 2], None, |i| i), vec![3, 1, 2]);
    }
}
