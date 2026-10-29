//! Helpers that kept their returns still match the selects Luau inlined.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_guard_select.luau`.
//! After SSA stopped fusing a helper's two returns, its inlined copy - a
//! value landing in a register, still a fused `c and a or b` select - no
//! longer matched it (Roblox `ClickToMoveController` `computeThrottle`). The
//! matcher's canonical form fuses the helper's return diamonds the same way,
//! inside out (a chain of guards: SkyGarden `FloorFxTuner` `formatBool`),
//! whichever polarity guard folding gave them, and folds a temp the fused
//! select reads once (`local head = ...; if head then return head end`).

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_guard_select.luaubc");

#[test]
fn guard_return_helpers_match_their_inlined_selects() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("computeThrottle(math.min(1,"), "{source}");
    assert!(!source.contains("and 0 or 0.5"), "{source}");
    assert_eq!(source.matches(".. formatBool(").count(), 2, "{source}");
    assert!(!source.contains("\"-\" or"), "{source}");
    assert!(source.contains("= charToHead("), "{source}");
}
