//! A conditional collapsed into an `and`/`or` value keeps SSA form.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O1 -g1 --fflags=false collapsed_conditional_stays_ssa.luau`.
//! `structure_bool_conditional` assigned the collapsed value to the join's phi
//! parameter while other paths still reached the phi: a second definition,
//! across which destruction joined the parameter's old value to another
//! variable. `best` then overwrote its stack count with `nil` and returned
//! `math.min(x, x)`.

const BYTECODE: &[u8] = include_bytes!("fixtures/collapsed_conditional_stays_ssa.luaubc");

#[test]
fn collapsed_conditional_keeps_both_counts() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    let min = source.lines().map(str::trim).find(|line| line.contains("math.min(")).expect(&source);
    let arguments = min.split_once("math.min(").unwrap().1;
    let (first, second) = arguments.split_once(", ").expect(&source);
    assert_ne!(first, second.trim_end_matches(|c| c == ')'), "{source}");
}
