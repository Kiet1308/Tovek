//! A 700-arm `if`/`elseif` chain in a loop of the main chunk.
//!
//! The fixture was compiled with `luau-compile --binary -O2 -g1
//! --fflags=false` from
//!
//! ```lua
//! local out = 0
//! for i = 0, 3 do
//!     if i == 0 then out += 0
//!     elseif i == 1 then out += 1
//!     -- ... one arm per i up to 699
//!     else continue end
//! end
//! print(out)
//! ```
//!
//! Each `elseif` nests the next `if` in the previous `else`, and the passes
//! recurse once per level. On default stacks (1 MiB main thread, 2 MiB
//! workers) such a chain overflowed and aborted the process past any
//! `catch_unwind` (fr-dynamic F2: a 250-arm chain killed the CLI, a 900-arm
//! one the web-server). Every decompiling thread now gets
//! `DECOMPILE_STACK_BYTES`.

use luau_lifter::{DecompileOptions, try_decompile_bytecode_artifact_with_diagnostics};

const BYTECODE: &[u8] = include_bytes!("fixtures/long_elseif_chain.luaubc");

#[test]
fn long_elseif_chain_decompiles_on_the_decompile_stack() {
    let _ = luau_lifter::configure_thread_pool(0);
    let source = luau_lifter::with_decompile_stack(|| {
        try_decompile_bytecode_artifact_with_diagnostics(BYTECODE, 1, None, DecompileOptions::default())
            .map(|artifact| artifact.source)
    })
    .expect("the chain decompiles");
    assert_eq!(source.matches("elseif").count(), 699, "{}", &source[..source.len().min(400)]);
}
