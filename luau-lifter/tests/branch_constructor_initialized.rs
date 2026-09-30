//! A branch that overwrites a property the constructor already sets keeps
//! its stores.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false branch_constructor_initialized.luau`.
//! The private-property pass moved the choice into the constructor and
//! appended it, repeating the key: `{ Value = p, ..., Value = v1 }`. The
//! source assigned that property in the branches, so the pass now leaves it
//! there (benchmark `branch_constructor_order`).

const BYTECODE: &[u8] = include_bytes!("fixtures/branch_constructor_initialized.luaubc");

#[test]
fn an_initialized_property_keeps_its_branch_stores() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert_eq!(source.matches("\tValue = ").count(), 1, "{source}");
    assert_eq!(source.matches(".Value = ").count(), 2, "{source}");
}
