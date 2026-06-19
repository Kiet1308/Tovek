//! Parallel folder decompiler — the native replacement for `decompile_folder.sh`.
//!
//! Walks SRC for `*.lua` files (each a UniversalSynSaveInstance text wrapper:
//! `--` comment header lines, then a base64 Luau-bytecode blob), decodes them
//! exactly like the bash pipeline `grep -v '^--' | tr -d ' \t\r\n' | base64 -d`,
//! decompiles in parallel with rayon, and mirrors the tree into OUT with every
//! `.lua` renamed to `.luau`.
//!
//! The decode/decompile/write logic lives in [`crate::decompile_core`] so it is
//! shared byte-for-byte with the `validate-folder` driver.

use crate::decompile_core::{build_work, precreate_dirs, process_one, size_pool, Outcome};
use rayon::prelude::*;
use std::path::Path;
use std::time::Instant;

/// Run the folder decompiler. Returns a process exit code (0 = no failures).
pub fn run(src: &Path, out: &Path, key: u8, threads: usize, verbose: bool) -> i32 {
    let start = Instant::now();

    let (_src_root, out_root, work) = match build_work(src, out) {
        Ok(t) => t,
        Err(code) => return code,
    };

    if work.is_empty() {
        eprintln!("no .lua files found under {}", src.display());
        return 0;
    }

    if let Err(code) = precreate_dirs(&work) {
        return code;
    }

    size_pool(threads);

    // Decompile in parallel. map_init gives each worker a reusable base64 scratch
    // buffer so we don't reallocate it per file.
    let outcomes: Vec<Outcome> = work
        .par_iter()
        .map_init(Vec::<u8>::new, |b64, w| process_one(w, key, b64, verbose))
        .collect();

    // Tally on the main thread (collect() preserves input order, so the FAIL
    // list is deterministic).
    let (mut ok, mut skipped, mut fail) = (0usize, 0usize, 0usize);
    for (w, o) in work.iter().zip(&outcomes) {
        match o {
            Outcome::Ok => ok += 1,
            Outcome::Skipped => skipped += 1,
            Outcome::Fail(reason) => {
                fail += 1;
                eprintln!("FAIL {}\n      {reason}", w.rel);
            }
        }
    }

    let total = work.len();
    let elapsed = start.elapsed();
    let secs = elapsed.as_secs_f64();
    let per_ms = secs * 1000.0 / total as f64;
    let fps = if secs > 0.0 { total as f64 / secs } else { 0.0 };

    eprintln!("----------------------------------------");
    eprintln!("Done: {ok} decompiled, {skipped} skipped (no bytecode), {fail} failed.");
    eprintln!("Output: {}", out_root.display());
    eprintln!(
        "Time: {secs:.2}s  ({total} files, {per_ms:.1} ms/file, {fps:.0} files/s, {} threads)",
        rayon::current_num_threads()
    );

    i32::from(fail > 0)
}
