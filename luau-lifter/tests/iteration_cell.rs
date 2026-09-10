//! CLOSEUPVALS provenance must reach the source-structuring decision.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O0 -g0 --fflags=false iteration_cell.luau`.
//! The root prototype's PC 22 (byte offset 128) closes register 5, captured
//! by reference at PC 20. Replacing that one instruction leaves valid loop
//! bytecode whose callbacks share an open cell across iterations.

use luau_lifter::{
    ControlFlowOutputPolicy, DecompileOptions, try_decompile_bytecode_artifact_with_diagnostics,
};

const BYTECODE: &[u8] = include_bytes!("fixtures/iteration_cell.luaubc");
const CLOSE_OFFSET: usize = 128;

fn strict() -> DecompileOptions {
    DecompileOptions {
        control_flow_policy: ControlFlowOutputPolicy::StrictNoSyntheticControl,
        ..Default::default()
    }
}

#[test]
fn compiler_closed_iteration_cell_structures() {
    assert_eq!(&BYTECODE[CLOSE_OFFSET..CLOSE_OFFSET + 4], &[11, 5, 0, 0]);
    let artifact = try_decompile_bytecode_artifact_with_diagnostics(BYTECODE, 1, None, strict())
        .expect("the compiler closes the captured result at every iteration boundary");
    assert!(
        artifact.source.contains(" in ipairs("),
        "{}",
        artifact.source
    );
    assert!(!artifact.source.contains("controlFlowState"));
}

#[test]
fn missing_or_wrong_close_never_reaches_legacy() {
    for replacement in [[0, 0, 0, 0], [11, 6, 0, 0]] {
        let mut bytecode = BYTECODE.to_vec();
        bytecode[CLOSE_OFFSET..CLOSE_OFFSET + 4].copy_from_slice(&replacement);
        let failure =
            try_decompile_bytecode_artifact_with_diagnostics(&bytecode, 1, None, strict())
                .expect_err("an open loop-result cell cannot become a fresh source binding");
        assert!(
            failure
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "source_like_unsafe_CapturedLoopResultRef"),
            "{failure:?}"
        );
    }
}
