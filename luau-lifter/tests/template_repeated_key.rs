//! A template placeholder filled through a temp key is not listed twice.
//!
//! The accompanying source was compiled with Luau c2ec0d4 at -O0:
//! `luau-compile --binary -O0 -g1 --fflags=false template_repeated_key.luau`.
//! DUPTABLE lists every key with a `0` placeholder, and -O0 stores through a
//! key register (`local v2 = "Velocity"; v[v2] = velocity`). The constructor
//! took that store while its key was still the temp, so once the temp folded
//! it read `{ Velocity = 0, ..., Velocity = velocity }`, and the leftover
//! entry kept the next store (`MaxSpeed`) out of the constructor (RbxUtil
//! `Spring.new`).

const BYTECODE: &[u8] = include_bytes!("fixtures/template_repeated_key.luaubc");

#[test]
fn a_placeholder_takes_the_value_stored_through_a_temp_key() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert_eq!(source.matches("Velocity = ").count(), 1, "{source}");
    assert!(source.contains("MaxSpeed = huge"), "{source}");
    assert!(!source.contains("= 0\n"), "{source}");
}
