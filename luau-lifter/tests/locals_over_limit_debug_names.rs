//! Source locals of `do` blocks the output flattens stay within Luau's 200
//! local limit when debug information names them.
//!
//! The accompanying sources were compiled with Luau c2ec0d4:
//! `luau-compile --binary -O1 -g2 --fflags=false locals_over_limit_*.luau`.
//! Each repeats `do local a = f(i); t[i] = g(i) + a end` 220 times, with one
//! name (`same_name`) or a name per block (`distinct_names`). At -g2 every
//! `a` carries its own debug interval, which kept it out of the shared
//! storage temporaries get: the output declared 220 locals and did not
//! compile ("Out of local registers").

const SAME_NAME: &[u8] = include_bytes!("fixtures/locals_over_limit_same_name.luaubc");
const DISTINCT_NAMES: &[u8] = include_bytes!("fixtures/locals_over_limit_distinct_names.luaubc");

fn decompile(bytecode: &[u8]) -> String {
    luau_lifter::try_decompile_bytecode_with_options(bytecode, 1, None, Default::default())
        .expect("fixture decompiles")
}

fn declarations(source: &str) -> usize {
    source.lines().filter(|line| line.trim_start().starts_with("local ")).count()
}

#[test]
fn locals_of_one_name_share_that_name() {
    let source = decompile(SAME_NAME);
    assert!(declarations(&source) < 200, "{source}");
    assert!(source.contains("\na = f(1)\n"), "{source}");
}

#[test]
fn locals_of_many_names_share_an_inferred_one() {
    let source = decompile(DISTINCT_NAMES);
    assert!(declarations(&source) < 200, "{source}");
    // One slot holding every `itemN` takes none of their names.
    assert!(!source.contains("item1 "), "{source}");
}
