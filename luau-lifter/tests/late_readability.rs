//! The matching semantic fixtures run at O0/O1/O2 in semantic_roundtrip.py.
//! These bytes pin their positive/negative presentation cases at O2; compile
//! with Luau c2ec0d4e5ca50796ba174a7565298f59aa572268 and
//! `--binary -O2 -g1 --fflags=false` from docs/failure_fixtures/semantic_roundtrip.

const MERGED: &[u8] = include_bytes!("fixtures/deinline_merged_result_branches.luaubc");
const CONTROL: &[u8] = include_bytes!("fixtures/late_control_flow_shapes.luaubc");
const WHILE: &[u8] = include_bytes!("fixtures/while_inlined_condition.luaubc");
const EVENT: &[u8] = include_bytes!("fixtures/event_parameter_roles.luaubc");

#[test]
fn rebuilt_head_test_recovers_while_and_keeps_its_call_comment() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        WHILE, 1, None, Default::default(),
    ).expect("inlined while condition decompiles");
    assert!(source.contains("while alive(p) do -- inferred equivalent call"), "{source}");
    assert!(source.contains("while not done() do -- inferred equivalent call"), "{source}");
}

#[test]
fn character_event_parameter_is_not_named_after_parent_store() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        EVENT, 1, None, Default::default(),
    ).expect("event role regression decompiles");
    assert!(source.contains("Connect(function(character)"), "{source}");
    assert!(source.contains(".Parent = character"), "{source}");
}

#[test]
fn merged_default_rebuilds_only_when_the_result_cell_is_unobserved() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        MERGED, 1, None, Default::default(),
    ).expect("merged-result regression decompiles");
    let stamp = source.split("local function stamp(").nth(1).unwrap()
        .split("local function observed(").next().unwrap();
    assert_eq!(stamp.matches("adjusted(").count(), 1, "{source}");
    // Function bodies are indented; the first unindented end closes observed.
    let observed = source.split("local function observed(").nth(1).unwrap()
        .split("\nend").next().unwrap();
    assert!(!observed.contains("adjusted("), "a captured result must stay inline:\n{source}");
    assert!(observed.contains("observed:"), "{source}");
}

#[test]
fn late_loops_recover_repeat_and_keep_continue_sensitive_while() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        CONTROL, 1, None, Default::default(),
    ).expect("late control-flow regression decompiles");
    let bottom = source.split("local function bottom(").nth(1).unwrap()
        .split("bottom({},").next().unwrap();
    assert!(bottom.contains("repeat") && bottom.contains("until "), "{source}");
    let continued = source.split("while true do").nth(1).unwrap()
        .split("print(\"continue\"").next().unwrap();
    assert!(continued.contains("continue") && continued.contains("if done() then"), "{source}");
    let guard = source.split("local function guarded(").nth(1).unwrap()
        .split("local function bottom(").next().unwrap();
    assert!(!guard.contains("else"), "the small returning guard needs no else:\n{source}");
}
