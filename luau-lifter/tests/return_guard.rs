//! Two source `return`s stay two returns.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g0 --fflags=false return_guard.luau`.
//! Once the FASTCALL argument fold left nothing between the guard and the
//! final `return`, SSA structuring fused the two RETURNs into
//! `return v <= p2 and 1 or 1 + math.clamp(...)` (SkyGarden
//! `PlantReplicator` `calculateScaleFactor`), and guards returning `true`
//! then `false` into `return c and true or false` (SkyGarden
//! `ItemDropFXClient`). `return c and a or b` compiles to a single RETURN,
//! so two RETURNs are the guard the source wrote.

const BYTECODE: &[u8] = include_bytes!("fixtures/return_guard.luaubc");

fn decompile() -> String {
    luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles")
}

#[test]
fn guard_return_is_not_fused_into_and_or() {
    let source = decompile();
    assert!(!source.contains(" and 1 or "), "{source}");
    assert!(source.contains("\t\treturn 1\n\tend\n\n\treturn 1 + math.clamp("), "{source}");
}

#[test]
fn boolean_returns_fold_only_on_a_boolean_condition() {
    let source = decompile();
    assert!(!source.contains("and true or false"), "{source}");
    assert!(source.contains("\t\treturn true\n\tend\n\n\treturn false"), "{source}");
    assert!(source.contains("return p < 1"), "{source}");
}
