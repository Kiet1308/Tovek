//! A helper with a leading statement matches a site whose value branch SSA
//! fused into the result's declaration.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_declared_prefix.luau`.
//! `local n = tonumber(value); if not n then return 4 end; return
//! math.clamp(...)` inlines as `local n = tonumber(x); local r = not n and 4
//! or math.clamp(...)`. The matcher expected `<prefix>; local r; <branch>`,
//! and canon kept the helper's diamond: its `return (math.clamp(...))` is a
//! one-result call, which the select fusion refused although an operand takes
//! one value anyway (Roblox `EventsSchema` `clampDepthValue`). Where SSA also
//! folded the value into its one use (`obj:SetAttribute("K", not n and 4 or
//! ...)`), the call is rebuilt in place: only local reads and the method
//! lookup precede it there, which `tonumber` cannot change.

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_declared_prefix.luaubc");

#[test]
fn declared_selects_after_a_prefix_rebuild_their_helper_call() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert_eq!(source.matches("= clampDepthValue(").count(), 2, "{source}");
    assert!(source.contains(":SetAttribute(\"ChainDepthLimit\", clampDepthValue("), "{source}");
}
