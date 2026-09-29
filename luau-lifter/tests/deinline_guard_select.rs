//! A helper that kept its two returns still matches the select Luau inlined.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_guard_select.luau`.
//! After SSA stopped fusing a helper's two returns (`if dist > 0.2 then
//! return ... end return 0`), its inlined copy - a value flowing into
//! `* math.sign(...)`, still the fused select `not (d > 0.2) and 0 or ...` -
//! no longer matched it (Roblox `ClickToMoveController` `computeThrottle`).

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_guard_select.luaubc");

#[test]
fn guard_return_helper_matches_its_inlined_select() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("computeThrottle((math.min(1,"), "{source}");
    assert!(!source.contains("and 0 or 0.5"), "{source}");
}
