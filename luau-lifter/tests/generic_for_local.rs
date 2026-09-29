//! A local iterated by generalized iteration prints without `, nil, nil`, and
//! a single-result call stays bound to its local instead of moving into the
//! iterator slot.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false generic_for_local.luau`.
//! `local t = table.clone(x); for k, v in t do` copies the call's local into
//! the loop base right before the two nil loads, which the explicit-padding
//! detector mistook for `for k, v in table.clone(x), nil, nil do` (found in
//! RbxUtil `silo/Util.luau`). Once the padding was dropped, an inlined
//! `local items = make()` printed as `for _ in make() do`, spreading every
//! result of `make` into the iterator triple.

const BYTECODE: &[u8] = include_bytes!("fixtures/generic_for_local.luaubc");

fn decompile() -> String {
    luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles")
}

#[test]
fn iterated_local_drops_protocol_nils() {
    let source = decompile();
    assert!(source.contains(" in clone do"), "{source}");
    // An explicitly truncated call keeps its padding: dropping it would let a
    // multi-value return drive the iterator protocol.
    assert!(source.contains("callback(), nil, nil do"), "{source}");
    assert_eq!(source.matches(", nil, nil").count(), 1, "{source}");
}

#[test]
fn single_result_call_stays_out_of_the_iterator_slot() {
    let source = decompile();
    assert!(!source.contains(" in callback() do"), "{source}");
    assert_eq!(source.matches("= callback()").count(), 1, "{source}");
}
