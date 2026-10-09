//! Statement helpers rebuild around an argument that runs code, too.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_first_read_statement.luau`.
//! Once FASTCALL argument folding put `toCurveSpace(math.abs(axis))` inside
//! the inlined `math.clamp`, Roblox `CameraUtils` `SCurveTranform` stopped
//! matching: statement sites took only locals and literals. Its parameter is
//! read once, as the body's first observable step, so the argument may move
//! back into the call. The same holds for a captured local (`syncTo(total)`
//! in the cutscene scripts) and for a value stored through a local address
//! (`part.Size = point / 2`).

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_first_read_statement.luaubc");

#[test]
fn statement_helpers_take_a_first_read_argument() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    // Rebuilt where the source called it, the first value it returns.
    assert!(source.contains("return sCurve(toCurveSpace(math.abs("), "{source}");
    assert!(source.contains("syncTo(total) --"), "{source}");
    assert!(source.contains("keep(Vector2.new("), "{source}");
}
