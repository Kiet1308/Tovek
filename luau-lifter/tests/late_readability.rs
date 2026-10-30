//! The matching semantic fixtures run at O0/O1/O2 in semantic_roundtrip.py.
//! These bytes pin their positive/negative presentation cases at O2; compile
//! with Luau c2ec0d4e5ca50796ba174a7565298f59aa572268 and
//! `--binary -O2 -g1 --fflags=false` from docs/failure_fixtures/semantic_roundtrip.

const MERGED: &[u8] = include_bytes!("fixtures/deinline_merged_result_branches.luaubc");
const CONTROL: &[u8] = include_bytes!("fixtures/late_control_flow_shapes.luaubc");
const WHILE: &[u8] = include_bytes!("fixtures/while_inlined_condition.luaubc");
const EVENT: &[u8] = include_bytes!("fixtures/event_parameter_roles.luaubc");
const VECTOR: &[u8] = include_bytes!("fixtures/deinline_vector_arguments.luaubc");
const COPY_CAPTURE: &[u8] = include_bytes!("fixtures/deinline_copy_capture_rebound.luaubc");
const COPY_BEFORE_CAPTURE: &[u8] = include_bytes!("fixtures/deinline_copy_written_before_capture.luaubc");
const COPY_PER_SITE: &[u8] = include_bytes!("fixtures/deinline_copy_per_site.luaubc");
const LOOP_EXIT_HELPERS: &[u8] = include_bytes!("fixtures/deinline_loop_exit_helpers.luaubc");
const TABLE_SHAPE: &[u8] = include_bytes!("fixtures/table_shape_from_stores.luaubc");
const OVER_PADDING: &[u8] = include_bytes!("fixtures/table_shape_over_padding_limit.luaubc");
const SCOPED_OVER_LIMIT: &[u8] = include_bytes!("fixtures/scoped_declarations_over_limit.luaubc");
const FOLDED_ARGUMENTS: &[u8] = include_bytes!("fixtures/deinline_folded_constant_arguments.luaubc");
const STORED_AND_FOLDED: &[u8] = include_bytes!("fixtures/deinline_stored_and_folded_sites.luaubc");

fn decompile(bytecode: &[u8]) -> String {
    luau_lifter::try_decompile_bytecode_with_options(bytecode, 1, None, Default::default())
        .expect("fixture decompiles")
}

#[test]
fn a_table_its_later_stores_size_prints_empty() {
    let source = decompile(TABLE_SHAPE);
    // Luau sizes `local t = {}` from the stores after it: no `nil` items.
    assert!(source.contains("local function collect(p, p2, p3) -- 2 equivalent calls inferred from this helper\n\tlocal v = {}\n"), "{source}");
    assert!(source.contains("local function fill(p) -- 1 equivalent call inferred from this helper\n\tlocal v3 = {}\n"), "{source}");
    assert!(source.contains("local function shuffled(p) -- 1 equivalent call inferred from this helper\n\tlocal v4 = {}\n\tv4[2] = p\n"), "{source}");
}

#[test]
fn copies_folded_for_constant_arguments_are_calls_again() {
    let source = decompile(FOLDED_ARGUMENTS);
    // Dropped arms, folded arithmetic and comparisons, stores folded into
    // the constructor, and spread arms: each copy is its call.
    for call in ["local v = collect(1, nil, 3) -- inferred equivalent call",
        "local v2 = collect(\"x\", \"y\", \"z\") -- inferred equivalent call",
        "local v3 = fill(3) -- inferred equivalent call",
        "local v4 = counted(4) -- inferred equivalent call"]
    {
        assert!(source.contains(call), "{call}\n{source}");
    }
}

#[test]
fn stored_results_reference_captures_and_folded_flags_are_calls_again() {
    let source = decompile(STORED_AND_FOLDED);
    // The argument's closure writes the caller's local it captures by
    // reference; `update(false)` folded to `if v then ... end`; results go
    // into locals the caller has, or into one nothing reads after.
    for call in ["\ttrack({ -- inferred equivalent call\n",
        "\t\t\tupdate(false) -- inferred equivalent call\n",
        "\tv3, v2 = basis(-i, i + 1) -- inferred equivalent call\n",
        "\tprint(basis(i, 3), v2) -- inferred equivalent call\n"]
    {
        assert!(source.contains(call), "{call}\n{source}");
    }
}

#[test]
fn an_allocation_past_nil_padding_is_spelled_table_create() {
    let source = decompile(OVER_PADDING);
    assert!(source.starts_with("local function build(p)\n\tlocal v = {}\n"), "{source}");
    // The call Luau inlined with a constant `false` allocates and stores nothing.
    assert!(source.contains("print(#table.create(4100))"), "{source}");
}

#[test]
fn flattened_do_blocks_come_back_over_the_local_limit() {
    let source = decompile(SCOPED_OVER_LIMIT);
    assert_eq!(source.matches("\ndo\n").count(), 210, "{source}");
    assert!(source.contains("\ndo\n\tlocal v3 = v + 209\n\tlocal function c209(p)\n"), "{source}");
}

#[test]
fn helpers_returning_from_a_loop_after_a_guard_are_rebuilt() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        LOOP_EXIT_HELPERS, 1, None, Default::default(),
    ).expect("loop-exit helpers decompile");
    // Each copy stores and breaks (or clears a flag) where the helper returns.
    for line in ["print(repeated(1), repeated(2), repeated(3))", "print(tested(1), tested(2), tested(3))",
        "print(find(v, true), nil, find({}, 1))"]
    {
        assert!(source.contains(line), "{line}
{source}");
    }
    // The helper's loop returns where the source does, not `break` and a return after it.
    assert!(!source.contains("break"), "{source}");
}

#[test]
fn a_value_capture_written_only_before_any_closure_keeps_its_helper_calls() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        COPY_BEFORE_CAPTURE, 1, None, Default::default(),
    ).expect("copy written before capture decompiles");
    // The inlined `ensure` assigns `scale` twice, both before any closure
    // copies it: every copy holds the value `tweenTo` captured.
    assert_eq!(source.matches("tweenTo(").count(), 5, "{source}");
}

#[test]
fn a_site_copying_with_no_write_since_the_helper_rebuilds_its_call() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        COPY_PER_SITE, 1, None, Default::default(),
    ).expect("per-site copy proof decompiles");
    // `cframe` is written after both closures copy it, never between them.
    assert!(source.contains("Origin = originFor("), "{source}");
}

#[test]
fn a_value_capture_is_not_the_current_value_after_register_reuse() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        COPY_CAPTURE, 1, None, Default::default(),
    ).expect("copied capture regression decompiles");
    // The helper returns the creation-time snapshot: its one call is rebuilt
    // where the source makes it. The later inlined region returns the
    // phi-selected input and stays a select, not a second `captured()`.
    assert_eq!(source.matches("captured()").count(), 2, "{source}");
    assert!(source.contains("if p2 then v else p"), "{source}");
}

#[test]
fn vector_constant_arguments_rebuild_reflect_without_folding_library_calls() {
    let source = luau_lifter::try_decompile_bytecode_with_options(
        VECTOR, 1, None, Default::default(),
    ).expect("vector argument regression decompiles");
    let reflected = source.split("local function reflected(").nth(1).unwrap()
        .split("\nend").next().unwrap();
    assert_eq!(reflected.matches("reflect(").count(), 2, "{source}");
    assert!(reflected.contains("-- inferred equivalent call"), "{source}");
    assert!(source.contains("scaled(p.create(1, 2, 3))"), "mutable lookup must stay a call: {source}");
}

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
