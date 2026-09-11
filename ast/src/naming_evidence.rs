//! Non-owning audit of legacy naming candidates. Never retains RcLocal: earlier
//! cleanup and unused-local naming deliberately observe its strong count.
use std::{collections::BTreeMap, panic::Location};

pub const BINDING_LIMIT: usize = 50_000;
pub const CANDIDATE_LIMIT: usize = 24;
pub const NAME_BYTE_LIMIT: usize = 256;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct Candidate {
    pub name: String,
    pub priority: u8,
    pub rule: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Default)]
pub struct Binding {
    pub id: u64,
    pub candidates: Vec<Candidate>,
    pub selected_hint: Option<(String, u8)>,
    pub invalidations: Vec<&'static str>,
    pub truncated: bool,
}

#[derive(Debug, Default)]
pub struct Report {
    pub enabled: bool,
    pub candidate_attempts: usize,
    pub omitted_attempts: usize,
    pub binding_budget_exhausted: bool,
    pub bindings: Vec<Binding>,
}

#[derive(Default)]
pub(crate) struct Collector {
    // Raw addresses are ephemeral lookup keys, never exposed in the report.
    pointers: rustc_hash::FxHashMap<usize, u64>,
    rows: BTreeMap<u64, (usize, Binding)>,
    report: Report,
}

impl Collector {
    pub fn register(&mut self, ptr: usize, id: u64) {
        if self.rows.contains_key(&id) { return; }
        if self.rows.len() == BINDING_LIMIT {
            self.report.binding_budget_exhausted = true;
            // Stable-ID selection, independent of the usage hash-map walk.
            if id > *self.rows.last_key_value().unwrap().0 { return; }
            let (_, (old, _)) = self.rows.pop_last().unwrap();
            self.pointers.remove(&old);
        }
        self.pointers.insert(ptr, id);
        self.rows.insert(id, (ptr, Binding { id, ..Default::default() }));
    }

    pub fn record(&mut self, ptr: usize, name: &str, priority: u8,
        rule: &'static str, site: &'static Location<'static>) {
        self.report.candidate_attempts += 1;
        let Some(&id) = self.pointers.get(&ptr) else {
            self.report.omitted_attempts += 1;
            return;
        };
        let row = &mut self.rows.get_mut(&id).unwrap().1;
        if name.len() > NAME_BYTE_LIMIT {
            row.truncated = true;
            self.report.omitted_attempts += 1;
            return;
        }
        let candidate = Candidate { name: name.into(), priority, rule,
            file: site.file(), line: site.line(), column: site.column() };
        if row.candidates.contains(&candidate) { return; }
        row.candidates.push(candidate);
        // Prefer strong candidates, then a deterministic tie order. This order
        // is diagnostic only and does not alter legacy first-winner semantics.
        row.candidates.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.cmp(b)));
        if row.candidates.len() > CANDIDATE_LIMIT {
            row.candidates.pop();
            row.truncated = true;
            self.report.omitted_attempts += 1;
        }
    }

    pub fn invalidate(&mut self, ptr: usize, reason: &'static str) {
        if let Some(id) = self.pointers.get(&ptr) {
            let row = &mut self.rows.get_mut(id).unwrap().1;
            if !row.invalidations.contains(&reason) {
                row.invalidations.push(reason);
                row.invalidations.sort_unstable();
            }
        }
    }

    pub fn finish(mut self, mut selected: impl FnMut(usize) -> Option<(String, u8)>) -> Report {
        self.report.enabled = true;
        self.report.bindings = self.rows.into_values().map(|(ptr, mut row)| {
            row.selected_hint = selected(ptr).filter(|(name, _)| name.len() <= NAME_BYTE_LIMIT);
            row
        }).collect();
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_losing_candidates_and_invalidation() {
        let mut c = Collector::default();
        c.register(0x1234, 7);
        c.record(0x1234, "component", 80, "field", Location::caller());
        c.record(0x1234, "instance", 41, "type_guard", Location::caller());
        c.invalidate(0x1234, "conflicting_classes");
        let r = c.finish(|_| Some(("component".into(), 80)));
        assert_eq!(r.bindings[0].id, 7);
        assert_eq!(r.bindings[0].candidates.len(), 2);
        assert_eq!(r.bindings[0].invalidations, ["conflicting_classes"]);
    }

    #[test]
    fn candidate_budget_is_order_independent_and_does_not_truncate_a_name() {
        let site = Location::caller();
        let build = |reverse| {
            let mut c = Collector::default();
            c.register(1, 9);
            for i in 0..30 {
                let n = if reverse { 29 - i } else { i };
                c.record(1, &format!("role{n}"), n, "role", site);
            }
            c.record(1, &"x".repeat(257), 255, "role", site);
            c.finish(|_| None)
        };
        let (a, b) = (build(false), build(true));
        assert_eq!(a.bindings[0].candidates, b.bindings[0].candidates);
        assert_eq!(a.bindings[0].candidates.len(), CANDIDATE_LIMIT);
        assert!(a.bindings[0].truncated);
        assert_eq!(a.omitted_attempts, 7);
    }

    #[test]
    fn binding_budget_uses_stable_identity() {
        let mut c = Collector::default();
        for id in (0..BINDING_LIMIT + 3).rev() { c.register(id, id as u64); }
        let r = c.finish(|_| None);
        assert!(r.binding_budget_exhausted);
        assert_eq!(r.bindings.len(), BINDING_LIMIT);
        assert_eq!(r.bindings[0].id, 0);
        assert_eq!(r.bindings.last().unwrap().id, BINDING_LIMIT as u64 - 1);
    }
}
