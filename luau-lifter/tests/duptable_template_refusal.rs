//! A DUPTABLE whose template a hand-made chunk corrupted.
//!
//! The accompanying source was compiled with
//! `luau-compile --binary -O2 -g1 --fflags=false duptable_template.luau`.
//! Its DUPTABLE (byte offset 26) copies constant 4, the template
//! `{[k0] = k1, [k2] = k3}` whose first key index sits at byte 72. A template
//! naming a later constant as a key, or a DUPTABLE copying a constant that is
//! no template, loads in the VM but has no constructor that spells it: such a
//! chunk is refused with a message, never by a panic in the lifter (which the
//! web-server and the fuzzer reported as `panicked: ...`).

use luau_lifter::{DecompileOptions, try_decompile_bytecode_artifact_with_diagnostics};

const BYTECODE: &[u8] = include_bytes!("fixtures/duptable_template.luaubc");
const DUPTABLE_OFFSET: usize = 26;
const FIRST_KEY_OFFSET: usize = 72;

fn refusal(bytecode: &[u8]) -> String {
    let failure = std::panic::catch_unwind(|| {
        try_decompile_bytecode_artifact_with_diagnostics(bytecode, 1, None, DecompileOptions::default())
    })
    .expect("a corrupted template is refused, not a panic")
    .expect_err("no constructor spells the corrupted template");
    failure.to_string()
}

#[test]
fn compiler_template_decompiles() {
    assert_eq!(BYTECODE[DUPTABLE_OFFSET], 54, "LOP_DUPTABLE");
    assert_eq!(BYTECODE[DUPTABLE_OFFSET + 2], 4, "its template constant");
    assert_eq!(BYTECODE[FIRST_KEY_OFFSET - 2], 8, "a template with constants");
    let artifact =
        try_decompile_bytecode_artifact_with_diagnostics(BYTECODE, 1, None, DecompileOptions::default())
            .expect("a compiler DUPTABLE decompiles");
    assert!(artifact.source.contains("x = 1"), "{}", artifact.source);
}

#[test]
fn later_key_constant_is_refused() {
    let mut bytecode = BYTECODE.to_vec();
    bytecode[FIRST_KEY_OFFSET] = 5;
    let message = refusal(&bytecode);
    assert!(message.contains("template key is not an earlier scalar constant"), "{message}");
}

#[test]
fn non_template_constant_is_refused() {
    let mut bytecode = BYTECODE.to_vec();
    bytecode[DUPTABLE_OFFSET + 2] = 0;
    let message = refusal(&bytecode);
    assert!(message.contains("constant is not a table template"), "{message}");
}
