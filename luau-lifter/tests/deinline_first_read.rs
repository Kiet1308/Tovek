//! An argument that runs code still rebuilds its helper call when the body
//! reads the parameter first.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_first_read.luau`.
//! Luau evaluates such an argument into the parameter's register right before
//! the inlined body, so `helper(arg)` keeps the original order whenever the
//! parameter is read once, as the body's first observable step (Roblox
//! `CameraUtils` `toSCurveSpace(math.abs(axis))`, `ClickToMoveDisplay`
//! `getTrailDotScale(distance, TrailDotSize)` with a reassigned upvalue). A
//! helper whose body is another's, inlined (`shiftHue` over `warp`), still
//! wins its sites: stable arguments outrank a reordered one.

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_first_read.luaubc");

#[test]
fn a_first_read_argument_rebuilds_its_call() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("= toCurveSpace(math.abs("), "{source}");
    assert_eq!(source.matches("= shiftHue(").count(), 1, "{source}");
    assert_eq!(source.matches("= warp(").count(), 1, "{source}");
    assert_eq!(source.matches("= trailScale(").count(), 1, "{source}");
}
