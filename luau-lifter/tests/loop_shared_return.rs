//! A loop's early return can share its bytecode block with an outer branch.
//! Such a return terminates the function; it is not a second loop continuation.
//! The reported Events payload is Roblox-encoded v13 bytecode, extracted
//! byte-for-byte from the user's failed-export wrapper (the leading 0x0d is
//! the version byte, not whitespace).
//!
//! Its processEvents loop shares an early RETURN with an outer branch, while
//! exhaustion has a different RETURN. Treating both as continuing exits made
//! loop discovery require a nonexistent common post-dominator. Only terminal
//! single-return blocks may now be emitted per incoming path; the exhaustion
//! path, phi transfers and closure/capture checks retain their usual proof.

const EVENTS: &[u8] = include_bytes!("fixtures/events_shared_return.luaubc");
// Source: docs/failure_fixtures/semantic_roundtrip/loop_shared_return.luau,
// compiled with --binary -O2 -g1 --fflags=false. The semantic runner also
// compiles and executes all three optimization levels against the original.
const LOOPS: &[u8] = include_bytes!("fixtures/loop_shared_return.luaubc");

#[test]
fn events_shared_loop_return_is_source_shaped() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        EVENTS, 203, Some("Events"), Default::default(),
    ).expect("Events decompiles without a synthetic dispatcher");
    for marker in ["goto ", "::", "controlFlowState", "GenericFor", "NumFor"] {
        assert!(!source.contains(marker), "{marker}:\n{source}");
    }
    assert!(source.contains("sendEventsInArray("), "{source}");
    assert!(source.contains("for ") && source.contains(" in pairs("), "{source}");
}

#[test]
fn shared_returns_in_loop_variants_are_source_shaped() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        LOOPS, 1, None, Default::default(),
    ).expect("shared returns in generic/numeric/while/repeat and nested/sequential loops");
    for marker in ["goto ", "controlFlowState", "GenericFor", "NumFor"] {
        assert!(!source.contains(marker), "{marker}:\n{source}");
    }
 }
