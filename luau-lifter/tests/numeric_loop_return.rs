//! A value returned from inside a loop, inlined, leaves through a jump past
//! the rest of the helper.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false numeric_loop_return.luau`.
//! For a numeric `for` the call's result register coalesces with the counter,
//! which the post-loop code reads, and the exhaustion path stores the
//! helper's tail value; a helper's own `break` lands in that tail. Every
//! such script failed whole ("residual goto/label would be invalid Luau").
//! Now the counter is exported past the loop and the tail runs under the
//! exhaustion flag, so the loop-return matcher rebuilds the calls.

const BYTECODE: &[u8] = include_bytes!("fixtures/numeric_loop_return.luaubc");

#[test]
fn numeric_loop_returns_structure_and_rebuild() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(!source.contains("goto"), "{source}");
    for call in ["= indexOf(", "= lastIndexOf(", "= firstOver(", "= indexOrCount(", "if contains(", "classify(list, p), classify(list, p2)"] {
        assert!(source.contains(call), "{call} not rebuilt:\n{source}");
    }
}
