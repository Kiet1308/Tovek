//! Arguments fold back into builtin calls.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false fastcall_arguments.luau`.
//! A builtin call compiles to FASTCALL: its arguments are evaluated first and
//! the callee is imported after them, for the fallback CALL. Treating that
//! import as an ordering barrier left every argument in a temporary
//! (`local v3 = depth + 1; table.insert(out, ("  "):rep(v3))`).

const BYTECODE: &[u8] = include_bytes!("fixtures/fastcall_arguments.luaubc");

#[test]
fn builtin_call_arguments_are_inlined() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    // `local out = {}` is the only local: no argument needs a temporary.
    assert_eq!(source.matches("local ").count(), 1, "{source}");
    assert!(source.contains("table.insert(v, (tostring(k)))"), "{source}");
    assert!(source.contains("string.format(\"%q\", (tostring(item)))"), "{source}");
}
