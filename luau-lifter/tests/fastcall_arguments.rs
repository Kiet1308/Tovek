//! Open builtin arguments keep their evaluation order or are refused.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false fastcall_arguments.luau`.
//! Its spreading `:rep` call executes before the table.insert fallback lookup.
//! Moving it into a call with an earlier callee snapshot changes errors/effects;
//! truncating it to one result assumes the external callee's return arity.

const BYTECODE: &[u8] = include_bytes!("fixtures/fastcall_arguments.luaubc");

#[test]
fn spreading_builtin_arguments_require_a_tuple_order_proof() {
    let error = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect_err("an open call cannot cross the observable import lookup");
    assert!(error.contains("cannot preserve open call results across an operation without a tuple-order proof"), "{error}");
}
