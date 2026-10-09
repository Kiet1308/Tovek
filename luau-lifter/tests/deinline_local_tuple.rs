//! Helpers that return their own locals rebuild as a multi-result call.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_local_tuple.luau`.
//! `local set, key = makeTrack(x)` inlines as the helper's body alone, the
//! caller's locals taking the place of the returned `set` and `key` (Roblox
//! cutscene scripts, ~30 sites). A returned local that a closure captures
//! (`counter`) is only a snapshot after a call, so its copy stays. A returned
//! parameter is the caller's argument (`toNames` hands its list back). And a
//! helper whose only distinctive statement is a closure (`FrameClock`) is
//! identified by that closure's prototype.

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_local_tuple.luaubc");

#[test]
fn helpers_returning_their_locals_rebuild() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("local set, key = makeTrack(CurrentCamera)"), "{source}");
    assert!(source.contains("= makeTrack(p)"), "{source}");
    assert!(source.contains("= makeTrack(instance)"), "{source}");
    assert!(!source.contains("= counter()"), "{source}");
    assert!(source.contains("local frameClock = FrameClock()"), "{source}");
    assert!(source.contains("= toNames(children)"), "{source}");
}
