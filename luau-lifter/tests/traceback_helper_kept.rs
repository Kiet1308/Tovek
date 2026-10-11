//! A helper calling `debug.traceback` is never rebuilt from its copies.
//!
//! The fixture `traceback_helper.luau` was compiled with `luau-compile
//! --binary -O2 -g1 --fflags=false`: Luau inlines `stamp` into `run` and into
//! the main chunk, so the trace taken there lists no `stamp` frame. Rebuilt
//! as `stamp(x)`, the trace gained that frame, and the program printed one
//! more line count than the bytecode (checked on the VM at -O2 -g1/-g2: 14/18
//! before). The trace lists every frame, as `debug.info` reads one, so the
//! frame gate refuses the helper. (An error handler, `xpcall(f,
//! debug.traceback)`, traces an error, a diagnostic like the position
//! `error` prefixes, and closes nothing.)

use luau_lifter::{DecompileOptions, try_decompile_bytecode_artifact_with_diagnostics};

const BYTECODE: &[u8] = include_bytes!("fixtures/traceback_helper.luaubc");

#[test]
fn copies_of_a_tracing_helper_stay_written_out() {
    let source = try_decompile_bytecode_artifact_with_diagnostics(BYTECODE, 1, None, DecompileOptions::default())
        .expect("decompiles")
        .source;
    assert!(source.contains("debug.traceback"), "{source}");
    assert!(!source.contains("inferred equivalent call"), "{source}");
}
