//! A helper whose return diamonds canon fuses into one select matches a site
//! SSA already fused into the result's declaration.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_declared_select.luau`.
//! With a truthy literal arm, SSA writes the inlined copy as
//! `local r = c and K or B`, one declaration, while the value matcher looked
//! for `local r` followed by the region assigning it. The expression
//! de-inliner could not take over: it runs after constant rehoisting, which
//! had rewritten the copy's `1e-6` into the caller's `DISTANCE_EPSILON`
//! (Roblox `HandleDash` `horizontalUnit`, 6 sites below V2.1.1).

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_declared_select.luaubc");

#[test]
fn declared_selects_rebuild_their_helper_call() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert_eq!(source.matches("= horizontal(").count(), 3, "{source}");
    assert!(!source.contains("and \"flat\" or"), "{source}");
}
