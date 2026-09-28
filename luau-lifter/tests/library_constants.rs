//! Folded library constants stay exact unless the caller opts in.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false --vector-lib=vector
//! --vector-ctor=create library_constants.luau`, which folds `math.pi`,
//! `math.huge` and the vector constructor into constants.

use luau_lifter::{DecompileOptions, try_decompile_bytecode_with_options};

const BYTECODE: &[u8] = include_bytes!("fixtures/library_constants.luaubc");

#[test]
fn exact_output_never_reads_the_environment_for_a_constant() {
    let exact = try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default()).unwrap();
    assert!(exact.contains("3.141592653589793 * "), "{exact}");
    assert!(exact.contains("1e999") && !exact.contains("math.") && !exact.contains("Vector3"), "{exact}");
}

#[test]
fn standard_libraries_spell_folded_constants() {
    let options = DecompileOptions { assume_standard_libraries: true, ..Default::default() };
    let friendly = try_decompile_bytecode_with_options(BYTECODE, 1, None, options).unwrap();
    assert!(friendly.contains("math.pi * ") && friendly.contains("math.huge"), "{friendly}");
    assert!(friendly.contains("Vector3.new(1, 2, 3)") && !friendly.contains("createVector"), "{friendly}");
}
