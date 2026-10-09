//! A declarative UI tree rebuilds around a captured local assigned on both
//! paths of an `if` before its capture.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false ui_tree_branch_scope.luau`.
//! Moving each child's construction past the reads of `scope` needs proof
//! that no call changes `scope`. Only a captured local written once counted
//! as such, so a Fusion story's `local scope; if outer then scope = ... else
//! scope = ... end` kept the whole tree in temps (`local frame =
//! scope:New("Frame")`, `local v4 = ...`, `v2[children] = { v4, ... }`). Every
//! write precedes the capture, so the cell never changes once captured.

const BYTECODE: &[u8] = include_bytes!("fixtures/ui_tree_branch_scope.luaubc");

#[test]
fn a_tree_rebuilds_around_a_scope_settled_before_its_capture() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    assert!(source.contains(":New(\"Frame\")({"), "{source}");
    let children = source.find("[Children] = {").expect("children field");
    let first = source[children..].trim_start_matches("[Children] = {").trim_start();
    assert!(first.starts_with("scope:New(\"UIScale\")"), "{source}");
}
