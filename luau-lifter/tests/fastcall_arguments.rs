//! Open builtin arguments print as the source wrote them.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false fastcall_arguments.luau`.
//! Its spreading `:rep` call runs before the FASTCALL fallback loads
//! `table.insert`, which is how Luau compiles `table.insert(out, s:rep(n))`
//! itself, so that spelling recompiles to the same order at this level.

const BYTECODE: &[u8] = include_bytes!("fixtures/fastcall_arguments.luaubc");

#[test]
fn spreading_builtin_arguments_keep_the_source_spelling() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("a FASTCALL fallback only loads its callee");
    assert!(source.contains(r#"table.insert(v, ("  "):rep(count + 1))"#), "{source}");
    assert!(source.contains(r#"table.insert(v, string.format("%q", (tostring(item))))"#), "{source}");
    assert!(!source.contains("local insert"), "{source}");
}
