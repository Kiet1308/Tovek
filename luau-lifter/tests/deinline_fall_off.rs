//! Value helpers that may fall off their end, or return calls, rebuild.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_fall_off.luau`.
//! `if c then return x end` (nothing returned otherwise) inlines as `local r;
//! if c then r = x else r = nil end`, which the value matcher refused as a
//! mixed return shape (Roblox `ClientFishingHandler`
//! `getEquippedRodAssetName`, `ClientTeleportation` `verifyInstance`). And a
//! helper whose arms return calls never matched its sites at all: the store
//! `r = f()` adjusts the call to one result, the pattern's `return f()` does
//! not.

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_fall_off.luaubc");

#[test]
fn falling_off_and_call_returning_helpers_rebuild() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("= getEquippedAsset()"), "{source}");
    assert!(source.contains("if not verify("), "{source}");
    assert!(source.contains("= getAsset("), "{source}");
}
