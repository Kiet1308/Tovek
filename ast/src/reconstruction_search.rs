//! Input line/PC hints prioritize helper candidates. They never authorize a
//! rewrite, eliminate a rival, or establish the PC of an original source call.
use std::{cell::RefCell, collections::{BTreeMap, BTreeSet}, marker::PhantomData, rc::Rc};
use serde::Serialize;

pub const PC_LIMIT: usize = 200_000;
pub const REGION_LIMIT: usize = 8192;
const OWNERS_PER_LINE: usize = 16;

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
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
pub struct Scope(State, PhantomData<Rc<()>>);
impl Drop for Scope {
    fn drop(&mut self) { STATE.with(|s| { s.replace(std::mem::take(&mut self.0)); }); }
}

pub fn enter(lines: Vec<Vec<Option<u32>>>) -> Scope {
    let mut state = State::default();
    if lines.iter().map(Vec::len).sum::<usize>() <= PC_LIMIT && lines.len() <= 4096 {
        let mut owners: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
        for (proto, pcs) in lines.iter().enumerate() {
            for &line in pcs.iter().flatten().filter(|&&line| line != 0) {
                let entry = owners.entry(line).or_default();
                if entry.len() <= OWNERS_PER_LINE { entry.insert(proto); }
            }
        }
        'callers: for (caller, pcs) in lines.iter().enumerate() {
            let mut last: BTreeMap<usize, usize> = BTreeMap::new();
            for (pc, line) in pcs.iter().enumerate() {
                let Some(helpers) = line.and_then(|line| owners.get(&line)) else { continue; };
                if helpers.len() > OWNERS_PER_LINE { continue; }
                for &helper in helpers {
                    if caller == helper { continue; }
                    if let Some(region) = last.get(&helper).map(|&i| &mut state.regions[i]) {
                        if region.end_pc_exclusive == pc { region.end_pc_exclusive += 1; continue; }
                    }
                    if state.regions.len() >= REGION_LIMIT { state.truncated = true; break 'callers; }
                    last.insert(helper, state.regions.len());
                    state.pairs.insert((caller, helper));
                    state.regions.push(Region { caller_prototype: caller, helper_prototype: helper, start_pc: pc, end_pc_exclusive: pc + 1 });
                }
            }
        }
    } else { state.truncated = true; }
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
        let state = s.borrow();
        let Some(caller) = caller.and_then(|c| state.functions.get(&c)) else { return; };
        ordered.sort_by_key(|&i| !state.functions.get(&function(i)).is_some_and(|callee| state.pairs.contains(&(*caller, *callee))));
    });
    ordered
}

pub fn report() -> (Vec<Region>, bool) {
    STATE.with(|s| { let state = s.borrow(); (state.regions.clone(), state.truncated) })
}

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
    }
}
