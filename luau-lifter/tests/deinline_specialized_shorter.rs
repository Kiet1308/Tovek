//! A helper whose copy lost a branch to a constant argument rebuilds.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_specialized_shorter.luau`.
//! `helper(false)` inlines as `print("start", false); print("end")`, shorter
//! than the helper's three statements, so the window scan, which started at
//! the body's length, never offered it to the specializing matcher. Where the
//! removed branch leads the body (`leading(false)`), the scan's first-statement
//! filter and the argument seeding, which aligned statements from the start,
//! missed it too.

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_specialized_shorter.luaubc");

#[test]
fn a_copy_shortened_by_a_constant_argument_rebuilds() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("helper(false)"), "{source}");
    assert!(source.contains("helper(true)"), "{source}");
    assert!(source.contains("leading(false)"), "{source}");
    assert!(source.contains("leading(true)"), "{source}");
}
