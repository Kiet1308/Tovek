//! A rebuilt arithmetic helper call folds into the expression that uses it.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false arithmetic_result_fold.luau`.
//! The early arithmetic de-inliner declared each result with a plain call,
//! which temp inlining never moves, so Fusion `sRGB.fromLinear` kept
//! `local new = Color3.new; local v = inverse(c.R) ... return new(v, v2, v3)`.
//! Declared as one result, the way the lifter declares any `local r = f()`,
//! it folds under the usual ordering proofs.

const BYTECODE: &[u8] = include_bytes!("fixtures/arithmetic_result_fold.luaubc");

#[test]
fn arithmetic_results_fold_into_their_use() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains("return Color3.new(inverse("), "{source}");
    assert!(source.contains("return outBounce(p * 2 - 1) / 2 + 0.5"), "{source}");
    assert!(!source.contains("local new"), "{source}");
}
