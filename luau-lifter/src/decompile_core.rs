//! Shared decompile core used by both `decompile-folder` (`batch.rs`) and
//! `validate-folder` (`validate.rs`).
//!
//! This is the single byte-for-byte decode/decompile/write path: the bash
//! pipeline `grep -v '^--' | tr -d ' \t\r\n' | base64 -d` followed by an
//! in-process decompile that appends one trailing `\n` (to match the legacy
//! single-file `println!` and the `decompile_folder.sh` baseline tree).
//!
//! Both drivers MUST go through here so they never drift apart.

use base64::prelude::*;
use luau_lifter::DecompileOptions;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// One unit of work, fully resolved up front so the parallel closure never
/// touches `strip_prefix`/path math (and never allocates the output path twice).
pub(crate) struct Work {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Path relative to SRC, forward-slashed, *including* the `.lua` extension —
    /// passed verbatim as `--script-name` to match the bash baseline.
    pub rel: String,
}

pub(crate) enum Outcome {
    Ok,
    /// Input had no base64 payload (the "Failed to get bytecode" files).
    Skipped,
    Fail(String),
}

/// Discover every `*.lua` file under SRC (recursive, sorted), resolve the SRC/OUT
/// roots, and build the fully-resolved [`Work`] list. Returns a process exit code
/// (`Err`) on a fatal path error, mirroring the original `batch::run` behavior.
pub(crate) fn build_work(src: &Path, out: &Path) -> Result<(PathBuf, PathBuf, Vec<Work>), i32> {
    // canonicalize SRC (must exist) so the walk + strip_prefix share one verbatim
    // form; OUT may not exist yet, so use `absolute` (no existence requirement,
    // and keeps long paths Windows-safe). This asymmetry is load-bearing — do not
    // "tidy" it to canonicalize both.
    let src_root = match std::fs::canonicalize(src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot access SRC {}: {e}", src.display());
            return Err(2);
        }
    };
    let out_root = match std::path::absolute(out) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: invalid OUT {}: {e}", out.display());
            return Err(2);
        }
    };

    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&src_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lua")))
        .collect();
    files.sort();

    let work: Vec<Work> = files
        .iter()
        .map(|input| {
            let rel_path = input.strip_prefix(&src_root).unwrap_or(input);
            let output = out_root.join(rel_path).with_extension("luau");
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            Work {
                input: input.clone(),
                output,
                rel,
            }
        })
        .collect();

    Ok((src_root, out_root, work))
}

/// Pre-create every unique parent directory single-threaded, so the parallel
/// phase only ever writes files (no concurrent `create_dir_all` race).
pub(crate) fn precreate_dirs(work: &[Work]) -> Result<(), i32> {
    let mut dirs: HashSet<&Path> = HashSet::new();
    for w in work {
        if let Some(parent) = w.output.parent() {
            dirs.insert(parent);
        }
    }
    for dir in dirs {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("error: create dir {}: {e}", dir.display());
            return Err(2);
        }
    }
    Ok(())
}

/// Size the global rayon pool once, before the first parallel call. `threads == 0`
/// leaves rayon's default (= logical CPU count).
pub(crate) fn size_pool(threads: usize) {
    if threads != 0 {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
        {
            eprintln!("warning: could not set thread count ({e}); using default pool");
        }
    }
}

/// Decode + decompile + write one file. Never panics out: a panic anywhere in
/// the decompile pipeline is caught and reported as a `Fail`. Thin wrapper used
/// by `decompile-folder`, which discards the source string.
pub(crate) fn process_one(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    verbose: bool,
    options: DecompileOptions,
) -> Outcome {
    decode_and_decompile(w, key, b64, true, false, verbose, options).0
}

/// Like [`process_one`] but returns the decompiled source on success so the
/// caller can compute line counts / `goto` flags without re-reading the file.
///
/// `write_skipped` controls whether an empty-payload input writes a zero-byte
/// output: `decompile-folder` passes `true` (mirror its baseline tree), while
/// `validate-folder` passes `false` (the bash validator `continue`s before any
/// write, so no file exists for skipped inputs).
pub(crate) fn process_one_capture(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    write_skipped: bool,
    options: DecompileOptions,
) -> (Outcome, Option<String>) {
    decode_and_decompile(w, key, b64, write_skipped, true, false, options)
}

/// The actual decode/decompile/write logic shared by both entry points.
///
/// * `write_skipped` — write a zero-byte file for empty-payload inputs.
/// * `capture` — return `Some(source)` on success (otherwise `None`, and the
///   source is moved straight into the on-disk bytes with no clone).
/// * `verbose` — print `ok <rel>` to stderr on success (the `decompile-folder`
///   `-v` behavior; kept inside the closure so its timing/threading is
///   byte-for-byte unchanged).
fn decode_and_decompile(
    w: &Work,
    key: u8,
    b64: &mut Vec<u8>,
    write_skipped: bool,
    capture: bool,
    verbose: bool,
    options: DecompileOptions,
) -> (Outcome, Option<String>) {
    let text = match std::fs::read(&w.input) {
        Ok(t) => t,
        Err(e) => return (Outcome::Fail(format!("read: {e}")), None),
    };

    // Replicate `grep -v '^--' | tr -d ' \t\r\n'`: drop lines starting with
    // "--" (start-of-line anchor — no trim), keep all non-whitespace bytes.
    b64.clear();
    for line in text.split(|&b| b == b'\n') {
        if line.starts_with(b"--") {
            continue;
        }
        b64.extend(
            line.iter()
                .copied()
                .filter(|&b| b != b' ' && b != b'\t' && b != b'\r'),
        );
    }

    // Empty payload => a "Failed to get bytecode" file.
    if b64.is_empty() {
        if write_skipped {
            if let Err(e) = std::fs::write(&w.output, b"") {
                return (Outcome::Fail(format!("write: {e}")), None);
            }
        }
        return (Outcome::Skipped, None);
    }

    let bytecode = match BASE64_STANDARD.decode(b64.as_slice()) {
        Ok(b) => b,
        Err(e) => return (Outcome::Fail(format!("base64: {e}")), None),
    };
    if bytecode.is_empty() {
        if write_skipped {
            if let Err(e) = std::fs::write(&w.output, b"") {
                return (Outcome::Fail(format!("write: {e}")), None);
            }
        }
        return (Outcome::Skipped, None);
    }

    // catch_unwind is the backstop for deep panics in the lifter/ssa/restructure
    // passes. The common deserialize-failure path already comes back as Err.
    let result = catch_unwind(AssertUnwindSafe(|| {
        luau_lifter::try_decompile_bytecode_with_options(&bytecode, key, Some(&w.rel), options)
    }));

    let source = match result {
        Ok(Ok(s)) => s,
        Ok(Err(reason)) => return (Outcome::Fail(reason), None),
        Err(payload) => return (Outcome::Fail(panic_message(payload)), None),
    };

    // Append a trailing newline so output is byte-identical to the single-file
    // mode (which prints via `println!`). When `capture` is set we keep the
    // source string for the caller, so we must clone before consuming it.
    let captured = if capture {
        let mut bytes = source.clone().into_bytes();
        bytes.push(b'\n');
        if let Err(e) = std::fs::write(&w.output, &bytes) {
            return (Outcome::Fail(format!("write: {e}")), None);
        }
        Some(source)
    } else {
        let mut bytes = source.into_bytes();
        bytes.push(b'\n');
        if let Err(e) = std::fs::write(&w.output, &bytes) {
            return (Outcome::Fail(format!("write: {e}")), None);
        }
        None
    };

    if verbose {
        eprintln!("ok {}", w.rel);
    }
    (Outcome::Ok, captured)
}

/// Extract a human-readable message from a caught panic payload.
pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => format!("panicked: {s}"),
        Err(p) => match p.downcast::<&'static str>() {
            Ok(s) => format!("panicked: {s}"),
            Err(_) => "panicked: <non-string payload>".to_string(),
        },
    }
}
