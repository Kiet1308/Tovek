//! Independent canonical-length oracle and deterministic physical-work probes.
use super::*;
use std::cell::Cell;

thread_local! {
    static DIRECT: Cell<usize> = const { Cell::new(0) };
    static INDEXED: Cell<usize> = const { Cell::new(0) };
}
pub(super) fn record_direct(width: usize) { DIRECT.with(|count| count.set(count.get() + width)); }
pub(super) fn record_indexed(width: usize) { INDEXED.with(|count| count.set(count.get() + width)); }

fn call() -> Statement { Call::new(crate::Global::from("print").into(), vec![Literal::Number(1.0).into()]).into() }
fn symbol(index: usize) -> Statement {
    let guard = |then: Vec<Statement>, other: Vec<Statement>| -> Statement {
        If::new(Literal::Boolean(true).into(), Block(then), Block(other)).into()
    };
    match index {
        0 => crate::Empty {}.into(),
        1 => Comment::trailing(CALL_MARKER.into()).into(),
        2 => Comment::new("source comment".into()).into(),
        3 => call(),
        4 => Return::new(vec![]).into(),
        5 => Return::new(vec![Literal::Number(-0.0).into()]).into(),
        6 => guard(vec![Return::new(vec![]).into()], vec![]),
        7 => guard(vec![call(), Return::new(vec![]).into()], vec![]),
        8 => guard(vec![call()], vec![]),
        9 => guard(vec![Return::new(vec![Literal::Number(1.0).into(), Literal::Number(2.0).into()]).into()], vec![]),
        _ => guard(vec![Return::new(vec![Literal::Number(1.0).into()]).into()], vec![guard(vec![Return::new(vec![]).into()], vec![])]),
    }
}

#[test]
fn prefix_lengths_match_owned_canonicalization_for_every_short_shape() {
    const ALPHABET: usize = 11;
    for length in 0..=4u32 {
        for encoded in 0..ALPHABET.pow(length) {
            let mut code = encoded;
            let statements: Vec<_> = (0..length).map(|_| {
                let statement = symbol(code % ALPHABET);
                code /= ALPHABET;
                statement
            }).collect();
            for start in 0..=statements.len() {
                let mut lengths = PrefixLengths::default();
                let remaining = &statements[start..];
                for width in 0..=remaining.len() {
                    assert_eq!(lengths.get(remaining, width), canon_top(&remaining[..width], true).len(),
                        "shape {encoded}, length {length}, start {start}, width {width}");
                }
                for width in (0..=remaining.len()).rev() {
                    assert_eq!(lengths.get(remaining, width), canon_top(&remaining[..width], true).len());
                }
            }
        }
    }
}

#[test]
fn growing_window_queries_visit_each_prefix_statement_once_after_small_scan_budget() {
    let statements: Vec<_> = (0..256).map(|index| symbol(if index % 7 == 0 { 0 } else { 3 })).collect();
    let mut lengths = WindowLengths::default();
    DIRECT.with(|count| count.set(0));
    INDEXED.with(|count| count.set(0));
    for _ in 0..16 {
        for width in 1..=statements.len() {
            assert_eq!(lengths.get(&statements, 0, width), canon_top(&statements[..width], true).len());
        }
    }
    assert!(DIRECT.with(Cell::get) <= 64);
    assert_eq!(INDEXED.with(Cell::get), statements.len());
    // 4,096 overlapping logical windows used 256 summary statements, plus the
    // bounded small scans; source semantics and logical search fuel are intact.
}

#[test]
fn position_reset_discards_lengths_after_splicing_or_child_edits() {
    let mut statements = vec![symbol(6), call(), call(), Return::new(vec![]).into()];
    let mut cache = CanonCache::default();
    cache.lengths.remaining_scan = 0;
    assert_eq!(cache.top_len(&statements, 0, 4), 1);
    let Statement::If(branch) = &statements[0] else { unreachable!() };
    branch.then_block.lock().0 = vec![call()];
    cache.clear();
    cache.lengths.remaining_scan = 0;
    assert_eq!(cache.top_len(&statements, 0, 4), 3);
    statements.remove(0);
    cache.clear();
    assert_eq!(cache.top_len(&statements, 0, 3), 2);
}
