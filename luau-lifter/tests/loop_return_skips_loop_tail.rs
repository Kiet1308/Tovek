//! A helper's `return` from inside its `while`, inlined, jumps past the code
//! after the loop, which the loop's own exits run.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false loop_return_skips_loop_tail.luau`.
//! The skipped code branches (`if p1 > 2 then print("big") end`, a `for`, a
//! choice of return value), so no `break` can carry a linear copy of it, and
//! every such script failed whole ("residual goto/label would be invalid
//! Luau"). It now runs after the loop under a flag the `return` clears.

const BYTECODE: &[u8] = include_bytes!("fixtures/loop_return_skips_loop_tail.luaubc");

#[test]
fn loop_return_past_a_branching_tail_structures_under_a_flag() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(!source.contains("goto"), "{source}");
    assert!(source.contains("flag = false"), "{source}");
}
