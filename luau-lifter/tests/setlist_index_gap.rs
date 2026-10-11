//! A SETLIST whose start index skips past the array its constructor fills.
//!
//! The accompanying source was compiled with
//! `luau-compile --binary -O2 -g1 --fflags=false setlist_index_gap.luau`.
//! Its SETLIST (byte offset 42) stores the two list items at index 1; the
//! AUX word at byte 46 holds that index. Luau's compiler writes a
//! constructor's list batches from index 1 on, each continuing the last, so
//! only a hand-made chunk moves it. The `nil` items that hold a constructor's
//! slots must stay bounded by the stores themselves: an index of 2^29 + 1
//! once made the decompiler pad 2^29 items (a 72 GiB allocation that aborts
//! the process, past any `catch_unwind`).

use luau_lifter::{DecompileOptions, try_decompile_bytecode_artifact_with_diagnostics};

const BYTECODE: &[u8] = include_bytes!("fixtures/setlist_index_gap.luaubc");
const SETLIST_OFFSET: usize = 42;

fn with_index(index: u32) -> Vec<u8> {
    let mut bytecode = BYTECODE.to_vec();
    bytecode[SETLIST_OFFSET + 4..SETLIST_OFFSET + 8].copy_from_slice(&index.to_le_bytes());
    bytecode
}

#[test]
fn compiler_index_keeps_the_constructor() {
    assert_eq!(BYTECODE[SETLIST_OFFSET], 55, "LOP_SETLIST");
    assert_eq!(&BYTECODE[SETLIST_OFFSET + 4..SETLIST_OFFSET + 8], &1u32.to_le_bytes());
    let artifact =
        try_decompile_bytecode_artifact_with_diagnostics(BYTECODE, 1, None, DecompileOptions::default())
            .expect("a compiler SETLIST decompiles");
    assert!(artifact.source.contains("{ \"a\", \"b\" }"), "{}", artifact.source);
}

#[test]
fn short_gap_pads_the_array_part() {
    // The VM sizes the array part to the store's last index (`#t == 4`).
    let artifact =
        try_decompile_bytecode_artifact_with_diagnostics(&with_index(3), 1, None, DecompileOptions::default())
            .expect("a short hand-made gap still decompiles");
    assert!(artifact.source.contains("nil, nil"), "{}", artifact.source);
}

#[test]
fn index_far_past_the_stored_slots_is_refused_not_padded() {
    for index in [(1u32 << 29) + 1, 50_001, 259] {
        let failure = try_decompile_bytecode_artifact_with_diagnostics(
            &with_index(index),
            1,
            None,
            DecompileOptions::default(),
        )
        .expect_err("no bounded constructor stands for a list store past its array");
        assert!(
            failure.to_string().contains("SETLIST index far past the array"),
            "index {index}: {failure}"
        );
    }
}

#[test]
fn index_zero_does_not_underflow() {
    // Index 0 stores below the array: whatever the output, no panic.
    let _ = try_decompile_bytecode_artifact_with_diagnostics(&with_index(0), 1, None, DecompileOptions::default());
}
