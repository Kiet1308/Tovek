//! A captured cell swapped in a loop beside an inlined copy of it.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O2 -g1 --fflags=false cell_swap_beside_copy.luau`.
//! All versions of a cell are one variable and a read names an earlier
//! version even after a later write. Destruction took the inlined `D(a)`
//! copy's value as the cell's for good, so value-based copy coalescing
//! joined `b` to the cell: `a, b = b, a` printed as `v = v`.

const BYTECODE: &[u8] = include_bytes!("fixtures/cell_swap_beside_copy.luaubc");

#[test]
fn cell_swap_keeps_both_variables() {
    let source = luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles");
    let self_assignment = source.lines().map(str::trim).find(|line| {
        line.split_once(" = ").is_some_and(|(left, right)| left == right && !left.contains(' '))
    });
    assert!(self_assignment.is_none(), "{source}");
}
