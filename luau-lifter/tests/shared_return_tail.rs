//! A `return` shared by a nested conditional and its enclosing one is emitted
//! once, after the outer `if`, as the source wrote it.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false shared_return_tail.luau`.
//! The structurer never let a nested conditional guard to a one-statement
//! terminal stop, even when the enclosing conditional had already chosen that
//! block as its shared tail, so the outer attempt failed and the tail was
//! cloned into every arm (Roblox PlayerModule `VehicleCamera`).

const BYTECODE: &[u8] = include_bytes!("fixtures/shared_return_tail.luaubc");

#[test]
fn shared_return_is_emitted_once_after_the_nested_conditional() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    let first_person = &source[source.find("firstPersonOffset").unwrap()..source.find("nameOf").unwrap()];
    assert_eq!(first_person.matches("return callback()").count(), 1, "{source}");
    assert!(first_person.contains("if instance and instance.Parent then"), "{source}");
    // The inner conditional falls through to the shared tail: no `else`.
    assert!(!first_person.contains("else"), "{source}");
}
