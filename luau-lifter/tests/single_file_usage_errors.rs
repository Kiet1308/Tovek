//! The single-file CLI reports a bad invocation instead of exiting silently.
//!
//! Its argument errors were `expect`/`panic!` calls, and the process-wide
//! quiet panic hook (which keeps caught per-function panics off stderr)
//! swallowed their messages: no arguments, an unknown flag, a missing flag
//! value or an unreadable file all exited 101 with nothing printed
//! (fr-dynamic F6). They now print the error and the usage line and exit 2.

use std::process::Command;

fn run(args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_luau-lifter")).args(args).output().expect("run luau-lifter");
    (output.status.code(), String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn bad_invocations_say_why() {
    let bytecode = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/duptable_template.luaubc");
    for (args, message) in [
        (&[][..], "expected a bytecode file"),
        (&[bytecode, "--bogus"][..], "unexpected argument: --bogus"),
        (&[bytecode, "--style", "bogus"][..], "--style must be `default` or `compact`"),
        (&[bytecode, "--stats-json"][..], "--stats-json requires a path"),
        (&["no-such-file.luaubc"][..], "cannot read no-such-file.luaubc"),
    ] {
        let (code, stderr) = run(args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }
    let (code, _) = run(&[bytecode]);
    assert_eq!(code, Some(0), "a good invocation still decompiles");
}
