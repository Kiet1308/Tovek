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
const WRITTEN_OUT_BESIDE_COPY: &[u8] = include_bytes!("fixtures/deinline_written_out_beside_copy.luaubc");
const SELECT_IN_EXPRESSION: &[u8] = include_bytes!("fixtures/deinline_select_helper_in_expression.luaubc");
const CLOSURE_FACTORY: &[u8] = include_bytes!("fixtures/deinline_closure_factory_in_constructor.luaubc");
const FIELDS_SIZED_BY_STORES: &[u8] = include_bytes!("fixtures/table_fields_sized_by_stores.luaubc");
const SELECT_TRUTHY_ARGUMENTS: &[u8] = include_bytes!("fixtures/deinline_select_with_truthy_arguments.luaubc");
const RETURNS_WRITTEN_PARAMETER: &[u8] = include_bytes!("fixtures/deinline_returns_written_parameter.luaubc");
const SELECT_FOLDED_FOR_TRUTH: &[u8] = include_bytes!("fixtures/deinline_select_folded_for_truth.luaubc");
const ENTRY_STORE: &[u8] = include_bytes!("fixtures/deinline_entry_store_in_argument_constructor.luaubc");
const DEFAULT_ARGUMENT: &[u8] = include_bytes!("fixtures/deinline_default_argument_folded.luaubc");
const CALL_TAIL: &[u8] = include_bytes!("fixtures/terminal_call_tail_and_chain_head.luaubc");
const RETURNED_LOCALS: &[u8] = include_bytes!("fixtures/deinline_returned_locals_share_result.luaubc");
const CLOSURE_ON_ONE_LINE: &[u8] = include_bytes!("fixtures/deinline_copy_creating_closure_on_one_line.luaubc");
const ONE_LINE_HELPER: &[u8] = include_bytes!("fixtures/store_address_read_at_store.luaubc");
const NESTED_COPIES: &[u8] = include_bytes!("fixtures/deinline_nested_and_adjacent_copies.luaubc");
const RETURNED_LOCAL_LEAVES: &[u8] = include_bytes!("fixtures/deinline_returned_local_leaves.luaubc");
const TYPEOF_DEFAULT: &[u8] = include_bytes!("fixtures/deinline_typeof_default_left_out.luaubc");
const TRUTHY_ARMS: &[u8] = include_bytes!("fixtures/deinline_select_with_truthy_arms.luaubc");

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
fn fields_the_stores_size_stay_stores_without_nil_padding() {
    let source = decompile(FIELDS_SIZED_BY_STORES);
    // Luau sizes `local v = {}` from all the field stores after it.
    assert!(source.starts_with("local v = {}\nv.DisplayName = \"Premium\"\n"), "{source}");
    assert!(!source.contains("] = nil,"), "{source}");
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
fn only_the_copy_line_info_shows_becomes_a_call() {
    let source = decompile(WRITTEN_OUT_BESIDE_COPY);
    // The copy keeps `lerp`'s lines; the same arithmetic written out does not.
    assert!(source.contains("\t\twavelength = lerp(wavelength, data.EndWavelength, p) -- inferred equivalent call\n"), "{source}");
    assert!(source.contains("((data.Amplitude or 0) - startAmplitude) * p) * p2\n"), "{source}");
}

#[test]
fn a_select_returning_a_call_is_a_call_where_one_value_is_taken() {
    let source = decompile(SELECT_IN_EXPRESSION);
    // `not c and 1 or part:GetScale()` is the copy of `if c then return
    // part:GetScale() end return 1` taking one value.
    assert!(source.contains("\tlocal v2 = p / getScale() -- inferred equivalent call\n"), "{source}");
}

#[test]
fn returned_locals_sharing_the_result_are_a_call() {
    let source = decompile(RETURNED_LOCALS);
    // `local cached = ...; return cached` / `return found`, each computed in
    // the result's register by the copy.
    for call in ["function torsoOf(player)
	local humanoid = findHumanoid(player) -- inferred equivalent call
",
        "function healthOf(player)
	local humanoid = findHumanoid(player) -- inferred equivalent call
"]
    {
        assert!(source.contains(call), "{call}
{source}");
    }
}

#[test]
fn line_info_shows_copies_of_one_line_helpers() {
    // A copy making the helper's inner closure on its one line is the
    // helper's; a one-line helper copied right below its definition is too.
    let closure = decompile(CLOSURE_ON_ONE_LINE);
    assert!(closure.contains("local function setScale(p, p2) -- 2 equivalent calls inferred from this helper
"), "{closure}");
    let swap = decompile(ONE_LINE_HELPER);
    assert_eq!(swap.matches("= swap() -- inferred equivalent call").count(), 3, "{swap}");
}

#[test]
fn a_call_tail_stays_one_return_after_the_if() {
    let source = decompile(CALL_TAIL);
    // The shared tail is not duplicated into a guard.
    assert_eq!(source.matches("return self.Hooks:After(self) or self.Response").count(), 1, "{source}");
    assert!(source.contains("	if self.Response == nil then
"), "{source}");
    assert!(source.contains("	return tonumber(value:match(\"(%d+)$\")) or 1e999
"), "{source}");
}

#[test]
fn a_default_folded_for_a_constant_or_a_left_out_argument_is_a_call() {
    let source = decompile(DEFAULT_ARGUMENT);
    // `(radius or 8) * 2` folded to `12` and to `16`.
    for call in ["\nnewCorner(frame, 6) -- inferred equivalent call\n", "\nnewCorner(frame) -- inferred equivalent call\n",
        "\nlocal corner = newCorner(frame, 3) -- inferred equivalent call\n"]
    {
        assert!(source.contains(call), "{call}\n{source}");
    }
}

#[test]
fn a_store_folded_into_the_argument_constructor_is_a_call() {
    let source = decompile(ENTRY_STORE);
    for call in ["\npushSet(\"Honey\", { -- inferred equivalent call\n", "\npushSet(\"Quest\", { -- inferred equivalent call\n"] {
        assert!(source.contains(call), "{call}\n{source}");
    }
    // The constructor no call took keeps its entries; the store after it stays.
    assert!(source.contains("local plain = { moving = false }\nplain.Name = \"plain\"\n"), "{source}");
}

#[test]
fn a_select_a_left_out_flag_folded_is_a_call() {
    let source = decompile(SELECT_FOLDED_FOR_TRUTH);
    // `failed and rgb(...) or rgb(...)` kept as its second arm.
    assert!(source.contains("\t\tsetStatus(\"ok\") -- inferred equivalent call\n"), "{source}");
    assert!(source.contains("\t\tsetStatus(\"bad\", true) -- inferred equivalent call\n"), "{source}");
}

#[test]
fn a_helper_returning_a_parameter_it_writes_is_a_call() {
    let source = decompile(RETURNS_WRITTEN_PARAMETER);
    for call in ["safeLookup(v, data.Mode, \"none\"), -- inferred equivalent call\n",
        "Speed = safeLookup(v, data.Speed, v.Slow), -- inferred equivalent call\n",
        "Owner = getUserId(data.Owner) -- inferred equivalent call\n"]
    {
        assert!(source.contains(call), "{call}\n{source}");
    }
}

#[test]
fn a_select_on_parameters_is_a_call_where_its_arguments_are_truthy() {
    let source = decompile(SELECT_TRUTHY_ARGUMENTS);
    // `v < 0 and 0 or v > 1 and 1 or v` is `clamp(v, 0, 1)`; after a
    // prefix the select folds into the store using it.
    for call in ["return clamp(value or 1, 0, 1) * 2 -- inferred equivalent call\n",
        "return 1 - clamp(value or 0, 0, 1) -- inferred equivalent call\n",
        "Spin = clampUnit(p.Spin, 0, 2, 0.5) -- inferred equivalent call\n"]
    {
        assert!(source.contains(call), "{call}\n{source}");
    }
    // Line info shows no copy of `clamp` in `clampUnit`: its own bounds
    // checks stay as written.
    assert!(source.contains("local function clamp(p, p2, p3) -- 2 equivalent calls"), "{source}");
}

#[test]
fn a_function_literal_writing_captured_parameters_is_its_factory_call() {
    let source = decompile(CLOSURE_FACTORY);
    // Each copy declared a local per captured parameter; the calls end in
    // no `nil`.
    for call in ["Cutscene = makeCutscene(\"House\") } -- inferred equivalent call\n",
        "Cutscene = makeCutscene(\"Tree\", 2, nil, \"Pos2\") -- inferred equivalent call\n"]
    {
        assert!(source.contains(call), "{call}\n{source}");
    }
    // The module table is called through, not inlined: no `({ ... }).Init()`.
    assert!(source.contains("\nlocal v3 = v2.Init()\n"), "{source}");
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

#[test]
fn nested_and_adjacent_copies_are_all_calls() {
    let source = decompile(NESTED_COPIES);
    // Copies side by side on the helper's line, their constant arguments
    // between them, and the last one an argument itself.
    assert!(source.contains("local v2 = vec(nextNumber(-2, 2), nextNumber(-2, 2), nextNumber(-2, 2)) -- inferred equivalent call
"), "{source}");
    assert!(source.contains("local v3 = vec(nextNumber(-4, 4), nextNumber(-4, 4), nextNumber(-4, 4)) -- inferred equivalent call
"), "{source}");
    // A copy opening with copies inside it, its own first line folded away.
    assert!(source.contains("return p + v2.x + v3.z, wobble(p, 3) -- inferred equivalent call
"), "{source}");
    assert!(source.contains("local function nextNumber(p, p2) -- 7 equivalent calls inferred from this helper
"), "{source}");
}

#[test]
fn a_returned_local_stored_at_every_leaf_is_a_value_helper() {
    let source = decompile(RETURNED_LOCAL_LEAVES);
    // Each copy stores the helper's leaves into its result: declared, then
    // a local the caller has, down an `and` chain.
    assert!(source.contains("	return IsFinite(data.x) and IsFinite(data.y) and IsFinite(data.z) -- inferred equivalent call
"), "{source}");
    assert!(source.contains("local function IsFinite(p): boolean -- 3 equivalent calls inferred from this helper
"), "{source}");
    assert!(source.contains("		if IsFiniteVector3(v) then -- inferred equivalent call
"), "{source}");
}

#[test]
fn copies_folding_a_type_tested_default_are_calls() {
    let source = decompile(TYPEOF_DEFAULT);
    // The default left out (`typeof(nil)` folded), and one passed, which
    // the copy shows where the helper returns it.
    assert!(source.contains("	Board = pickAsset(\"login\", \"board\"), -- inferred equivalent call
"), "{source}");
    assert!(source.contains("	Missing = pickAsset(\"shop\", \"icon\"), -- inferred equivalent call
"), "{source}");
    assert!(source.contains("	Named = pickAsset(\"shop\", \"icon\", \"fallback\") -- inferred equivalent call
"), "{source}");
}

#[test]
fn a_select_with_truthy_arms_keeps_its_condition_and_is_a_call() {
    let source = decompile(TRUTHY_ARMS);
    assert!(source.contains("return p + (p2 + getRandom(p3)) * 2, p - (p2 + getRandom(p3)), (p2 + getRandom(p3)) / 2 -- inferred equivalent call
"), "{source}");
    assert!(!source.contains("not p3 and 0"), "{source}");
}
