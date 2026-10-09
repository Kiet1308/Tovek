use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[path = "reference.rs"]
mod reference;

fn actual_report() -> (Vec<(usize, usize, usize, usize)>, bool) {
    let (regions, truncated) = report();
    (regions.into_iter().map(|r| (r.caller_prototype, r.helper_prototype, r.start_pc, r.end_pc_exclusive)).collect(), truncated)
}
fn expected_report() -> (Vec<(usize, usize, usize, usize)>, bool) {
    let (regions, truncated) = reference::report();
    (regions.into_iter().map(|r| (r.caller_prototype, r.helper_prototype, r.start_pc, r.end_pc_exclusive)).collect(), truncated)
}
fn pending() -> bool { STATE.with(|s| s.borrow().pending_lines.is_some()) }
fn identity(prototype: usize) -> usize { prototype * 17 + 101 }
fn register(identity: usize, prototype: usize) {
    register_function(identity, prototype);
    reference::register_function(identity, prototype);
}
fn assert_order(candidates: &[usize], caller: Option<usize>) {
    let actual_calls = RefCell::new(Vec::new());
    let expected_calls = RefCell::new(Vec::new());
    let actual = prioritize(candidates, caller, |i| { actual_calls.borrow_mut().push(i); identity(i) });
    let expected = reference::prioritize(candidates, caller, |i| { expected_calls.borrow_mut().push(i); identity(i) });
    assert_eq!(actual, expected);
    assert_eq!(actual_calls.into_inner(), expected_calls.into_inner(), "callback invocation order");
}
/// `priority_keys` is the key `prioritize` sorts on: a stable partition by
/// it gives the same order, from any candidate subset.
fn assert_keys(candidates: &[usize], caller: Option<usize>) {
    let helpers: Vec<usize> = candidates.iter().map(|&i| identity(i)).collect();
    let partitioned = match priority_keys(caller, &helpers) {
        Some(keys) => {
            let (mut first, rest): (Vec<_>, Vec<_>) = candidates.iter().zip(&keys).partition(|&(_, &key)| key);
            first.extend(rest);
            first.into_iter().map(|(&i, _)| i).collect()
        }
        None => candidates.to_vec(),
    };
    assert_eq!(partitioned, reference::prioritize(candidates, caller, identity));
}
fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13; *seed ^= *seed >> 7; *seed ^= *seed << 17; *seed
}

#[test]
fn random_eager_oracle_preserves_regions_priority_duplicates_and_callback_order() {
    for seed in 1..=256u64 {
        let mut random = seed;
        let count = (next(&mut random) % 25) as usize;
        let lines: Vec<_> = (0..count).map(|_| {
            (0..next(&mut random) % 48).map(|_| {
                let word = next(&mut random);
                match word % 12 {
                    0 | 1 => None,
                    2 => Some(0),
                    3 => Some(u32::MAX),
                    _ => Some(((word >> 9) % 13 + 1) as u32),
                }
            }).collect::<Vec<_>>()
        }).collect();
        let _actual = enter(lines.clone());
        let _expected = reference::enter(lines);
        for proto in 0..count { register(identity(proto), proto); }
        // Unknown identities, duplicate candidate indices and overwritten mappings
        // exercise the same stable sort and function callback behavior.
        if count > 2 { register(identity(1), 2); }
        let candidates: Vec<_> = (0..count + 3).rev().chain([0, 1, 1]).collect();
        assert_order(&candidates, None);
        assert_order(&candidates, Some(usize::MAX));
        assert_order(&[], Some(identity(0)));
        assert_order(&[1], Some(identity(0)));
        assert_order(&candidates, Some(identity(0)));
        assert_keys(&candidates, None);
        assert_keys(&candidates, Some(usize::MAX));
        assert_keys(&candidates, Some(identity(0)));
        assert_keys(&candidates[..candidates.len() / 2], Some(identity(count.saturating_sub(1))));
        assert_order(&candidates, Some(identity(count.saturating_sub(1))));
        assert_eq!(actual_report(), expected_report(), "seed {seed}");
    }
}

#[test]
fn only_registered_multi_candidate_query_or_report_forces_small_lines() {
    let lines = vec![vec![Some(8), Some(9), None, Some(8)], vec![Some(9)], vec![Some(30)]];
    let _actual = enter(lines.clone());
    let _expected = reference::enter(lines);
    for proto in 0..3 { register(identity(proto), proto); }
    assert!(pending());
    for (candidates, caller) in [(&[2, 1][..], None), (&[2, 1][..], Some(999)), (&[][..], Some(identity(0))), (&[1][..], Some(identity(0)))] {
        assert_order(candidates, caller);
        assert!(pending());
    }
    assert_order(&[2, 1, 2, 1], Some(identity(0)));
    assert!(!pending());
    assert_eq!(actual_report(), expected_report());
    let _nested_actual = enter(vec![vec![Some(4)], vec![Some(4)]]);
    let _nested_expected = reference::enter(vec![vec![Some(4)], vec![Some(4)]]);
    assert!(pending());
    assert_eq!(actual_report(), expected_report());
    assert!(!pending());
}

#[test]
fn budgets_single_prototype_and_missing_lines_match_eager_reference() {
    for lines in [
        vec![], vec![vec![]], vec![vec![None, Some(0), Some(u32::MAX)]],
        vec![vec![None; PC_LIMIT]], vec![vec![None; PC_LIMIT + 1]],
        vec![vec![]; 4096], vec![vec![]; 4097],
    ] {
        let _actual = enter(lines.clone());
        let _expected = reference::enter(lines);
        assert!(!pending());
        assert_eq!(actual_report(), expected_report());
    }
    for pcs in [0, 1, PC_LIMIT, PC_LIMIT + 1] {
        let _actual = enter_single_prototype(pcs);
        let _expected = reference::enter(vec![vec![Some(7); pcs]]);
        register(identity(0), 0);
        assert_order(&[0, 0, 1], Some(identity(0)));
        assert_eq!(actual_report(), expected_report());
    }
    let _actual = enter_truncated();
    let _expected = reference::enter_truncated();
    assert_eq!(actual_report(), expected_report());
}

#[test]
fn retained_capacity_limit_includes_spare_inner_and_outer_capacity() {
    let outer = 2 * std::mem::size_of::<Vec<Option<u32>>>();
    let slots = (DEFERRED_LINE_BYTES - outer) / std::mem::size_of::<Option<u32>>();
    for (extra, expected_pending) in [(0, true), (1, false)] {
        let mut first = Vec::with_capacity(slots + extra);
        first.push(Some(5));
        let mut lines = Vec::with_capacity(2);
        lines.push(first); lines.push(vec![]);
        let _scope = enter(lines);
        assert_eq!(pending(), expected_pending);
    }
    let mut lines = Vec::with_capacity(DEFERRED_LINE_BYTES / std::mem::size_of::<Vec<Option<u32>>>() + 1);
    lines.push(vec![]); lines.push(vec![]);
    let _scope = enter(lines);
    assert!(!pending(), "spare outer slots also consume retained payload");
}

#[test]
fn exact_region_limit_and_seventeenth_owner_sentinel_match_old_replay() {
    let alternating: Vec<_> = (0..9000).map(|pc| (pc % 2 == 0).then_some(7)).collect();
    let cases = [
        vec![alternating.clone(), alternating],
        vec![vec![Some(7); 10]; 16],
        vec![vec![Some(7); 10]; 17],
        vec![vec![Some(7); 10]; 18],
        vec![vec![Some(1), None, Some(1), Some(0), Some(1), Some(2), Some(1)]; 20],
    ];
    for lines in cases {
        let _actual = enter(lines.clone());
        let _expected = reference::enter(lines.clone());
        for proto in 0..lines.len() { register(identity(proto), proto); }
        assert_order(&(0..lines.len()).rev().collect::<Vec<_>>(), Some(identity(0)));
        assert_eq!(actual_report(), expected_report());
    }
}

#[test]
fn pending_scope_restores_after_nested_materialization_and_unwind() {
    let lines = vec![vec![Some(8)], vec![Some(8)]];
    let _actual = enter(lines.clone());
    let _expected = reference::enter(lines);
    register(identity(0), 0); register(identity(1), 1);
    assert!(pending());
    assert!(catch_unwind(AssertUnwindSafe(|| {
        let _inner_actual = enter(vec![vec![Some(99)], vec![None]]);
        let _inner_expected = reference::enter(vec![vec![Some(99)], vec![None]]);
        register(identity(0), 1);
        assert_eq!(actual_report(), expected_report());
        panic!("scope restoration");
    })).is_err());
    assert!(pending(), "restore pending ownership, not inner ready state");
    assert_order(&[0, 1], Some(identity(0)));
    assert_eq!(actual_report(), expected_report());
}

#[test]
fn comparator_reentrant_reads_and_borrow_conflicts_match_old_behavior() {
    let lines = vec![vec![Some(8)], vec![Some(8)], vec![Some(9)]];
    let _actual = enter(lines.clone());
    let _expected = reference::enter(lines);
    for proto in 0..3 { register(identity(proto), proto); }
    let expected_regions = expected_report();
    let actual_calls = RefCell::new(Vec::new());
    let expected_calls = RefCell::new(Vec::new());
    let actual = prioritize(&[2, 1, 0], Some(identity(0)), |i| {
        actual_calls.borrow_mut().push(i);
        assert_eq!(actual_report(), expected_regions);
        assert_eq!(prioritize(&[1], None, identity), vec![1]);
        identity(i)
    });
    let expected = reference::prioritize(&[2, 1, 0], Some(identity(0)), |i| {
        expected_calls.borrow_mut().push(i);
        assert_eq!(expected_report(), expected_regions);
        assert_eq!(reference::prioritize(&[1], None, identity), vec![1]);
        identity(i)
    });
    assert_eq!(actual, expected);
    assert_eq!(actual_calls.into_inner(), expected_calls.into_inner());
    assert!(catch_unwind(AssertUnwindSafe(|| prioritize(&[2, 1], Some(identity(0)), |i| {
        register_function(1234, 0); identity(i)
    }))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| reference::prioritize(&[2, 1], Some(identity(0)), |i| {
        reference::register_function(1234, 0); identity(i)
    }))).is_err());
    assert_eq!(actual_report(), expected_report());
}

#[test]
fn registration_capacity_and_duplicate_replacement_match_reference() {
    let _actual = enter(vec![vec![Some(8)], vec![Some(8)], vec![Some(9)]]);
    let _expected = reference::enter(vec![vec![Some(8)], vec![Some(8)], vec![Some(9)]]);
    for index in 0..50_000 { register(identity(index), index % 3); }
    // Original registration refuses even an existing-key update once full.
    register(identity(0), 2);
    register(identity(50_001), 0);
    assert_order(&[0, 1, 2, 50_001], Some(identity(0)));
    assert_eq!(actual_report(), expected_report());
}

#[test]
fn diagnostic_counters_capture_demand_without_per_query_map_mutation() {
    let lines = vec![vec![Some(7), None, Some(0), Some(7), Some(8), Some(7)], vec![Some(7)], vec![Some(9)]];
    let _scope = enter(lines);
    STATE.with(|s| s.borrow_mut().statistics = Statistics::new_if(true));
    for proto in 0..3 { register_function(identity(proto), proto); }
    assert_eq!(prioritize(&[2, 1], None, identity), vec![2, 1]);
    assert_eq!(prioritize(&[1], Some(identity(0)), identity), vec![1]);
    assert!(pending());
    assert_eq!(prioritize(&[2, 1], Some(identity(0)), identity), vec![1, 2]);
    let _ = report();
    STATE.with(|s| {
        let state = s.borrow();
        let counters = state.statistics.0.as_ref().unwrap().get();
        assert_eq!(counters.priority_calls, 3);
        assert_eq!(counters.registered_caller_calls, 2);
        assert_eq!(counters.eligible_priority_calls, 1);
        assert_eq!(counters.changed_priority_calls, 1);
        assert_eq!(counters.force_prioritize, 1);
        assert_eq!(counters.force_report, 0);
        assert_eq!(counters.report_calls, 1);
        assert_eq!(counters.owner_builds, 1);
        assert_eq!(counters.owner_lines, 6);
        assert_eq!(counters.owner_duplicate_skips, 2);
    });
}

#[cfg(feature = "phase-allocation-trace")]
#[test]
fn diagnostic_counter_box_allocation_and_drop_are_suppressed() {
    use crate::telemetry::allocation::{Counts, snapshot};
    let before = snapshot();
    let statistics = Statistics::new_if(true);
    statistics.update(|c| c.priority_calls += 1);
    std::hint::black_box(statistics.0.as_ref().unwrap().get());
    drop(statistics);
    let disabled = Statistics::new_if(false);
    drop(disabled);
    assert_eq!(snapshot().difference(before), Counts::default());
}
