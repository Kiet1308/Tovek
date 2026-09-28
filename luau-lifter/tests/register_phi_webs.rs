//! Out-of-SSA coalescing keeps each bytecode register's phi web together.
//!
//! The accompanying source was compiled with Luau c2ec0d4:
//! `luau-compile --binary -O1 -g0 --fflags=false register_phi_webs.luau`.
//! Merging a copy between two registers first (`lastRescan = now`) used to pull
//! the whole `lastRescan` web into `now`, leaving compensating copies in the
//! `else` arm and inside the `for` body; `clamp` reassigned the wrong parameter.

const BYTECODE: &[u8] = include_bytes!("fixtures/register_phi_webs.luaubc");

fn source() -> String {
    luau_lifter::try_decompile_bytecode_with_options(BYTECODE, 1, None, Default::default())
        .expect("fixture decompiles")
}

/// Plain `a = b` copies between locals, ignoring declarations.
fn local_copies(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter(|line| {
            let Some((left, right)) = line.split_once(" = ") else { return false };
            !line.starts_with("local ")
                && [left, right].iter().all(|side| side.chars().all(|c| c.is_alphanumeric() || c == '_'))
                && !right.chars().all(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_rescan_timestamp_is_one_variable_with_one_copy() {
    let source = source();
    let rescan = &source[source.find("rescan").unwrap()..source.find("clamp").unwrap()];
    assert_eq!(local_copies(rescan), ["v = now"], "{rescan}");
    let body = &rescan[rescan.find("for i").unwrap()..];
    assert!(!body.contains("now"), "no copy survives inside the loop:\n{rescan}");
}

#[test]
fn a_clamped_parameter_is_reassigned_in_place() {
    let source = source();
    let clamp = &source[source.find("clamp").unwrap()..];
    assert_eq!(local_copies(clamp), ["p = p2", "p = p3"], "{clamp}");
    assert!(!clamp.contains("else\n"), "{clamp}");
}
