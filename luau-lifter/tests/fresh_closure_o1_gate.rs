//! A closure the bytecode makes anew, in an `-O1` chunk with no shared
//! closure anywhere.
//!
//! The accompanying source was compiled with
//! `luau-compile --binary -O1 -g1 --fflags=false fresh_closure_o1_without_dupclosure.luau`.
//! At -O1 Luau aliases the loop's `local f = print` to the top-level
//! `print` register, so the literal is NEWCLOSURE + CAPTURE VAL R0 and every
//! iteration makes a new closure (`t[1] == t[2]` is false). Printed as
//! `function(p) print(p) end`, it captures only a never-written top-level
//! local, which Luau shares (DUPCLOSURE) once the output compiles at -O1:
//! `true`. `fresh_closures` keeps such literals new, but ran only for chunks
//! with a DUPCLOSURE or a folded capture; this one has neither. Its
//! GETIMPORT and ADDK are -O1 evidence all the same (-O0 emits GETGLOBAL and
//! LOADK + ADD). A source run cannot hold this as a semantic fixture: the
//! -O0 output runs at -O1 there, where nothing in -O0 bytecode tells the
//! literal's capture apart from a top-level local.

use luau_lifter::{DecompileOptions, try_decompile_bytecode_artifact_with_diagnostics};

const BYTECODE: &[u8] = include_bytes!("fixtures/fresh_closure_o1_without_dupclosure.luaubc");

#[test]
fn the_fresh_literal_captures_a_written_local() {
    let artifact =
        try_decompile_bytecode_artifact_with_diagnostics(BYTECODE, 1, None, DecompileOptions::default())
            .expect("decompiles");
    let source = &artifact.source;
    // The captured local is declared first and assigned after: written, Luau
    // captures it by reference and never shares the literal.
    let assigned = source.lines().find_map(|line| {
        let (left, right) = line.trim().split_once(" = ")?;
        (right == "print" && !left.starts_with("local ")).then_some(left)
    });
    let assigned = assigned.unwrap_or_else(|| panic!("no split declaration of the captured local:\n{source}"));
    assert!(source.contains(&format!("local {assigned}\n")), "{source}");
    assert!(source.contains(&format!("\t\t{assigned}(p)")), "{source}");
}
