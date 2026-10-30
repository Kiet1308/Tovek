//! Source locals of `do` blocks the output flattens stay within Luau's 200
//! local limit when debug information names them.
//!
//! The accompanying sources were compiled with Luau c2ec0d4:
//! `luau-compile --binary -O1 -g2 --fflags=false locals_over_limit_*.luau`.
//! Each repeats `do local a = f(i); t[i] = g(i) + a end` 220 times, with one
//! name (`same_name`) or a name per block (`distinct_names`). At -g2 every
//! `a` carries its own debug interval, which kept it out of the shared
//! storage temporaries get: the output declared 220 locals and did not
//! compile ("Out of local registers"). The output puts the `do` blocks back,
//! each local in its own scope under its own name.

const SAME_NAME: &[u8] = include_bytes!("fixtures/locals_over_limit_same_name.luaubc");
const DISTINCT_NAMES: &[u8] = include_bytes!("fixtures/locals_over_limit_distinct_names.luaubc");

fn decompile(bytecode: &[u8]) -> String {
    luau_lifter::try_decompile_bytecode_with_options(bytecode, 1, None, Default::default())
        .expect("fixture decompiles")
}

/// The top-level declarations: those of the `do` blocks are scoped.
fn top_level_declarations(source: &str) -> usize {
    source.lines().filter(|line| line.starts_with("local ")).count()
}

#[test]
fn locals_of_one_name_keep_that_name_in_do_blocks() {
    let source = decompile(SAME_NAME);
    assert!(top_level_declarations(&source) < 200, "{source}");
    assert!(source.contains("\ndo\n\tlocal a = f(1)\n\tt[1] = g(1) + a\nend\n"), "{source}");
    assert!(source.contains("\ndo\n\tlocal a = f(219)\n"), "{source}");
}

#[test]
fn locals_of_many_names_keep_their_names_in_do_blocks() {
    let source = decompile(DISTINCT_NAMES);
    assert!(top_level_declarations(&source) < 200, "{source}");
    assert!(source.contains("\ndo\n\tlocal item1 = f(1)\n\tt[1] = g(1) + item1\nend\n"), "{source}");
}

#[test]
fn retry_analysis_is_deterministic_and_does_not_leak_between_calls() {
    let options = luau_lifter::DecompileOptions {
        emit_binding_provenance: true,
        ..Default::default()
    };
    for bytecode in [SAME_NAME, DISTINCT_NAMES] {
        let first = luau_lifter::try_decompile_bytecode_artifact_with_options(
            bytecode, 1, None, options,
        ).expect("retry with provenance succeeds");
        assert!(first.upvalue_analysis.is_some());
        let plain = decompile(bytecode);
        let second = luau_lifter::try_decompile_bytecode_artifact_with_options(
            bytecode, 1, None, options,
        ).expect("later retry with provenance succeeds");
        assert_eq!(first.source, plain);
        assert_eq!(first.source, second.source);
        assert_eq!(
            serde_json::to_value(first.upvalue_analysis).unwrap(),
            serde_json::to_value(second.upvalue_analysis).unwrap(),
        );
    }
}
