//! Search fuel scales with the work actually done.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_budget.luau`.
//! 300 inlined calls of 30 helpers. The module-wide fuel was charged as if
//! every attempt canonicalized and unified every width, so large modules ran
//! dry after a few dozen sites and silently stopped reconstructing - a third
//! of a real game's scripts (`captureLightingSnapshot` in SkyGarden `Setting`).

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_budget.luaubc");

#[test]
fn every_site_of_a_large_module_is_reconstructed() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    let calls = source
        .lines()
        .filter(|line| line.trim_start().starts_with("apply") && line.contains("(v)"))
        .count();
    assert_eq!(calls, 300, "{source}");
}
