//! A helper pinned down by the outer locals it uses rebuilds, and the helper
//! covering the most statements wins where several match.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_outer_anchor.luau`.
//! `emit` has one fixed name (`AbsoluteEmit`), below the two a pattern
//! needs, but `particles` matches only itself, as a global would (Roblox
//! cutscenes, ~400 sites). `cancel` matches the first statement of every
//! inlined `purchase`, which made both refuse the site (Roblox
//! `ShopPromptPurchase`).

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_outer_anchor.luaubc");

#[test]
fn outer_locals_anchor_helpers_and_the_longest_match_wins() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("emit(instance.Smoke)"), "{source}");
    assert!(source.contains(":FindFirstChild(\"Beams\"))"), "{source}");
    assert!(source.contains("purchase(nil)"), "{source}");
}
