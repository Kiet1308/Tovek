//! Remaining helper shapes from the semantic corpus. Regenerate each fixture
//! with Luau c2ec0d4: `luau-compile --binary -O2 -g1 --fflags=false SOURCE`.
//! Sources under docs/failure_fixtures/semantic_roundtrip are respectively
//! deinline_closure_factories.luau, deinline_returns_distributed_and_short_circuit.luau,
//! and exact_fraction_constants.luau. Runtime comparisons live in the cross-opt audit.

use luau_lifter::{DecompileOptions, try_decompile_bytecode_with_options};

fn inferred_calls(source: &str, callee: &str) -> usize {
    source.lines().filter(|line| line.contains(callee) && line.contains("-- inferred")).count()
}

#[test]
fn readonly_closure_snapshots_recover_both_refresh_calls() {
    let source = try_decompile_bytecode_with_options(include_bytes!("fixtures/helper_shape_closure.luaubc"),
        1, None, Default::default()).unwrap();
    assert_eq!(inferred_calls(&source, "refresh("), 2, "{source}");
    assert_eq!(inferred_calls(&source, "mk("), 0, "{source}");
}

#[test]
fn distributed_guard_recovers_run_without_rewriting_loose_truthiness() {
    let source = try_decompile_bytecode_with_options(include_bytes!("fixtures/helper_shape_guard.luaubc"),
        1, None, Default::default()).unwrap();
    assert_eq!(inferred_calls(&source, "isCallable("), 4, "{source}");
    let (_, run) = source.split_once("\n\trun = function(").unwrap();
    let (run, loose) = run.split_once("\n\tloose = function(").unwrap();
    let (loose, _) = loose.split_once("\n\tcount = function(").unwrap();
    assert_eq!(inferred_calls(run, "isCallable("), 1, "{source}");
    assert!(!loose.contains("isCallable("), "{source}");
}

#[test]
fn pi_argument_recovers_only_with_exact_numeric_spelling() {
    for library_spelling in [false, true] {
        let options = DecompileOptions { assume_standard_libraries: library_spelling, ..Default::default() };
        let source = try_decompile_bytecode_with_options(include_bytes!("fixtures/helper_shape_number.luaubc"),
            1, None, options).unwrap();
        assert_eq!(inferred_calls(&source, "show("), if library_spelling { 24 } else { 25 }, "{source}");
        assert_eq!(source.contains("show(\"pi\","), !library_spelling, "{source}");
    }
}
