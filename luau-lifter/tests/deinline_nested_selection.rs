//! Compact selects retain equivalent helper calls beneath arithmetic wrappers.
//!
//! The accompanying source was compiled with Luau c2ec0d4e5ca50796ba174a7565298f59aa572268:
//! `luau-compile --binary -O2 -g1 --fflags=false deinline_nested_selection.luau`.
//! The helper's unknown-truthiness bias stays an if-expression in compact
//! output. Its copied site specializes that bias to 2, so SSA writes and/or
//! instead. Exact unary/arithmetic wrappers must recurse into the selection
//! proof. Keeping both parameters live prevents the compiler from reusing a
//! parameter register for the selection's result.

const BYTECODE: &[u8] = include_bytes!("fixtures/deinline_nested_selection.luaubc");

fn decompile(compact_style: bool) -> String {
    luau_lifter::try_decompile_bytecode_with_options(
        BYTECODE,
        1,
        None,
        luau_lifter::DecompileOptions {
            compact_style,
            control_flow_policy: luau_lifter::ControlFlowOutputPolicy::StrictNoSyntheticControl,
            ..Default::default()
        },
    )
    .expect("nested selection fixture decompiles")
}

#[test]
fn compact_nested_selections_rebuild_the_helper_and_copied_calls() {
    let source = decompile(true);
    // Both sites are inline arithmetic in the pinned -O2 bytecode. Baseline
    // leaves them expanded: neither the marker alone nor successful emission
    // establishes that this additional reconstruction actually happened.
    assert!(source.contains("\"nested-copy\", nestedCurve("), "{source}");
    assert!(source.contains("\"nested-helper\", nestedCurve("), "{source}");
    assert_eq!(source.matches("nestedCurve(").count(), 3, "{source}");
    assert!(source.contains("equivalent arithmetic calls inferred"), "{source}");
}

#[test]
fn default_style_keeps_the_nested_selection_fixture_decompilable() {
    let source = decompile(false);
    assert!(source.contains("local function nestedCurve("), "{source}");
    assert!(source.contains("\"nested-copy\""), "{source}");
}
