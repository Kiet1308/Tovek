//! A dead self-update keeps its variable (C13 self-update).
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false dead_self_update.luau`.
//! The last increment on a branch is dead (nothing reads it, no loop phi
//! rescues it), so out-of-SSA left it a class of its own and it printed as
//! `local _ = count + 1`, hiding the source `count += 1`. The same shape was
//! found in a real dump (SkyGarden `MeteorShower`).

const BYTECODE: &[u8] = include_bytes!("fixtures/dead_self_update.luaubc");

#[test]
fn dead_self_update_stays_a_compound_increment() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(!source.contains("local _ ="), "a dead increment became a throwaway local:\n{source}");
    // Both dead-on-path increments and the loop increment stay writes.
    assert_eq!(source.matches("+= 1").count(), 3, "{source}");
}
