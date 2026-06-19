//! Native `validate-folder` — the in-process replacement for `validate_all.sh`.
//!
//! Pipeline:
//!   A. discover + decompile every `*.lua` in parallel (shared decompile core),
//!      writing `.luau` outputs and capturing per-file line-count + `goto` flag;
//!   B. one batched `luau-analyze --formatter=plain <all OK outputs>` (chunked
//!      only if the Windows command line would overflow);
//!   C. attribute each diagnostic line back to its file and classify it
//!      (parse-error / local-scope) entirely in-process;
//!   D. run the backgroundMusic regression check, print the byte-for-byte
//!      summary + dump sections, and return the exit code.
//!
//! This is a *Luau gate*, not Lua 5.x: `continue` is valid, `goto`/`::label::`
//! are flagged. Parse errors are detected via `luau-analyze`'s own
//! `SyntaxError` diagnostics (verified equivalent to `luau-ast`'s parse gate),
//! so `luau-ast` — which only ever processes its first argument and cannot be
//! batched — is not used here.

use crate::decompile_core::{build_work, precreate_dirs, process_one_capture, size_pool, Outcome};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(PartialEq, Clone, Copy)]
enum Status {
    Ok,
    Skipped,
    DecodeFail,
    DecompileFail,
}

/// Which luau-analyze typechecker to use. `New` is the default and the parity
/// baseline (what `validate_all.sh` runs); `Old` is an opt-in fast path (~8x
/// faster) whose diagnostics can differ on dirty corpora — see `--solver`.
#[derive(Clone, Copy)]
enum Solver {
    New,
    Old,
}

struct FileRecord {
    /// Relative path including `.lua` (matches the bash `rel`).
    rel: String,
    /// Absolute `.luau` output path (also the analyze argument + map key source).
    output: PathBuf,
    status: Status,
    /// First line of the decode/decompile error (for the DECOMPILE-FAIL line).
    fail_detail: String,
    line_count: usize,
    has_goto: bool,
    // Filled in phase C (only meaningful for `Status::Ok`):
    parse_error: bool,
    parse_lines: Vec<String>,
    scope_lines: Vec<String>,
    leaked_v: bool,
}

/// Run the native validator. `analyze_arg`/`tool_dir` come from CLI flags.
pub fn run(
    src: &Path,
    out: &Path,
    key: u8,
    threads: usize,
    verbose: bool,
    analyze_arg: Option<&Path>,
    tool_dir: Option<&Path>,
    old_solver: bool,
) -> i32 {
    let start = Instant::now();
    let solver = if old_solver { Solver::Old } else { Solver::New };

    // Resolve + existence-check luau-analyze up front (mirror the shell's `-x`
    // precondition: exit 2 if the validator binary is missing).
    let analyze = resolve_analyze(analyze_arg, tool_dir);
    if !analyze.is_file() {
        eprintln!(
            "luau-analyze executable not found: {} (set LUAU_ANALYZE or pass --analyze/--tool-dir)",
            analyze.display()
        );
        return 2;
    }

    // ---- Phase A: discover + decompile in parallel --------------------------
    let (_src_root, _out_root, mut work) = match build_work(src, out) {
        Ok(t) => t,
        Err(code) => return code,
    };
    // The bash baseline iterates `find | sort`; with `LC_ALL=C` that is a byte
    // sort of the full path, which (since every path shares the SRC prefix) is a
    // byte sort of the forward-slashed `rel`. Sort by `rel` bytes explicitly so
    // the ordered dump sections match regardless of `build_work`'s internal sort.
    work.sort_by(|a, b| a.rel.as_bytes().cmp(b.rel.as_bytes()));

    if work.is_empty() {
        eprintln!("no .lua files found under {}", src.display());
    }

    if let Err(code) = precreate_dirs(&work) {
        return code;
    }
    size_pool(threads);

    let mut records: Vec<FileRecord> = work
        .par_iter()
        .map_init(Vec::<u8>::new, |b64, w| {
            let (outcome, source) = process_one_capture(w, key, b64, false);
            let mut rec = FileRecord {
                rel: w.rel.clone(),
                output: w.output.clone(),
                status: Status::Ok,
                fail_detail: String::new(),
                line_count: 0,
                has_goto: false,
                parse_error: false,
                parse_lines: Vec::new(),
                scope_lines: Vec::new(),
                leaked_v: false,
            };
            match outcome {
                Outcome::Ok => {
                    let s = source.unwrap_or_default();
                    // `wc -l` counts newline bytes; the on-disk file is
                    // `source + "\n"`, so this equals (newlines in source) + 1.
                    rec.line_count = s.bytes().filter(|&b| b == b'\n').count() + 1;
                    rec.has_goto = has_goto(&s);
                    rec.status = Status::Ok;
                }
                Outcome::Skipped => rec.status = Status::Skipped,
                Outcome::Fail(reason) => {
                    if reason.starts_with("base64:") {
                        rec.status = Status::DecodeFail;
                    } else {
                        rec.status = Status::DecompileFail;
                        rec.fail_detail = reason.lines().next().unwrap_or("").to_string();
                    }
                }
            }
            rec
        })
        .collect();

    // ---- Phase B: batched luau-analyze over every OK output -----------------
    // Two concurrent processes: the single heaviest output (by line count — a
    // strong predictor of analyze cost; one pathological file like AuraUI.luau
    // alone dominates the new solver's ~2.5s floor) in isolation, and all the
    // rest in one batch. They overlap, so the wall time is the heavy file's
    // analyze, not the serial sum. Diagnostics are per-file independent, so the
    // split is output-neutral. A spawn failure here is FATAL (exit 2): otherwise
    // a validator whose checker failed to run would silently report all-valid.
    let ok: Vec<(&Path, usize)> = records
        .iter()
        .filter(|r| r.status == Status::Ok)
        .map(|r| (r.output.as_path(), r.line_count))
        .collect();
    let analyze_lines = match run_analyze_phase(&analyze, &ok, solver) {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    drop(ok);

    // ---- Phase C: attribute diagnostics back to files, classify ------------
    // Map the (forward-slashed) absolute output path -> record index. We key on
    // the SAME PathBuf we passed to analyze, so analyze's `\`->`/`-normalized
    // echo matches by construction (no drive-case / separator drift).
    let mut path_map: FxHashMap<String, usize> = FxHashMap::default();
    for (i, r) in records.iter().enumerate() {
        if r.status == Status::Ok {
            path_map.insert(r.output.to_string_lossy().replace('\\', "/"), i);
        }
    }

    for line in &analyze_lines {
        let Some(path) = attribute_path(line) else {
            continue;
        };
        let Some(&idx) = path_map.get(path) else {
            continue;
        };
        let rec = &mut records[idx];
        if line.contains("SyntaxError") {
            rec.parse_error = true;
            if rec.parse_lines.len() < 6 {
                rec.parse_lines.push(line.clone());
            }
        }
        if scope_matches(line) {
            rec.scope_lines.push(line.clone());
        }
        // Regression check (c): the analyzer still seeing the leaked temp `v`
        // (bare `v`, NOT `v3` — the trailing quote pins it). Not in SCOPE_RE.
        if line.contains("Unknown global 'v'") {
            rec.leaked_v = true;
        }
    }

    // ---- Phase D: regression check, counters, dumps, exit code -------------
    let regression_msg = check_bgm_regression(&records);

    let mut valid = 0usize;
    let mut invalid = 0usize;
    let mut skipped_no_bytecode = 0usize;
    let mut decode_fail = 0usize;
    let mut decompile_fail = 0usize;
    let mut goto_files = 0usize;
    let mut scope_files = 0usize;
    let mut total_lines = 0usize;

    for r in &records {
        match r.status {
            Status::Skipped => skipped_no_bytecode += 1,
            Status::DecodeFail => decode_fail += 1,
            Status::DecompileFail => decompile_fail += 1,
            Status::Ok => {
                total_lines += r.line_count;
                if r.has_goto {
                    goto_files += 1;
                }
                if r.parse_error {
                    invalid += 1;
                } else {
                    valid += 1;
                }
                if !r.scope_lines.is_empty() {
                    scope_files += 1;
                }
            }
        }
    }
    let regression_fail = usize::from(regression_msg.is_some());

    // Build the ordered dump sections (records are already in rel-byte order).
    let mut problem_lines: Vec<String> = Vec::new();
    let mut goto_lines: Vec<String> = Vec::new();
    let mut scope_report: Vec<String> = Vec::new();
    for r in &records {
        match r.status {
            Status::DecodeFail => problem_lines.push(format!("DECODE-FAIL {}", r.rel)),
            Status::DecompileFail => {
                problem_lines.push(format!("DECOMPILE-FAIL {} {}", r.rel, r.fail_detail));
            }
            Status::Ok => {
                if r.parse_error {
                    problem_lines.push(format!("INVALID {}", r.rel));
                    for l in r.parse_lines.iter().take(6) {
                        problem_lines.push(format!("  {l}"));
                    }
                }
                if r.has_goto {
                    goto_lines.push(r.rel.clone());
                }
                if !r.scope_lines.is_empty() {
                    scope_report.push(r.rel.clone());
                    for l in r.scope_lines.iter().take(5) {
                        scope_report.push(format!("  {l}"));
                    }
                }
            }
            Status::Skipped => {}
        }
    }

    // Summary line — byte-for-byte identical to validate_all.sh (stdout).
    println!(
        "VALID={valid} INVALID={invalid} SKIP_NO_BYTECODE={skipped_no_bytecode} \
DECODE_FAIL={decode_fail} DECOMPILE_FAIL={decompile_fail} goto-files={goto_files} \
scope-files={scope_files} regression-fail={regression_fail} total-output-lines={total_lines}"
    );

    if !problem_lines.is_empty() {
        println!("--- parse/decompile problems ---");
        for l in problem_lines.iter().take(80) {
            println!("{l}");
        }
    }
    if !goto_lines.is_empty() {
        println!("--- goto files ---");
        for l in goto_lines.iter().take(80) {
            println!("{l}");
        }
    }
    if !scope_report.is_empty() {
        println!("--- generic local scope warnings ---");
        for l in scope_report.iter().take(120) {
            println!("{l}");
        }
    }
    if let Some(msg) = &regression_msg {
        println!("--- regressions ---");
        println!("{msg}");
    }

    if verbose {
        let secs = start.elapsed().as_secs_f64();
        eprintln!(
            "validate-folder: {} files in {secs:.2}s ({} threads)",
            records.len(),
            rayon::current_num_threads()
        );
    }

    if invalid != 0
        || decode_fail != 0
        || decompile_fail != 0
        || goto_files != 0
        || scope_files != 0
        || regression_fail != 0
    {
        1
    } else {
        0
    }
}

/// Resolve the luau-analyze path: explicit flag > `LUAU_ANALYZE` env >
/// `--tool-dir` > `<ROOT>/luau-tools/luau-analyze.exe`.
fn resolve_analyze(analyze_arg: Option<&Path>, tool_dir: Option<&Path>) -> PathBuf {
    if let Some(p) = analyze_arg {
        return p.to_path_buf();
    }
    if let Ok(p) = std::env::var("LUAU_ANALYZE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(d) = tool_dir {
        return d.join("luau-analyze.exe");
    }
    resolve_root().join("luau-tools").join("luau-analyze.exe")
}

/// Minimal native port of the shell `resolve_root` (no WSL `/mnt/d` branch):
/// `MEDAL_ROOT` env > cwd-if-it-looks-like-the-repo > `D:/Medal`.
fn resolve_root() -> PathBuf {
    if let Ok(r) = std::env::var("MEDAL_ROOT") {
        let p = PathBuf::from(r);
        if p.is_dir() {
            return p;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("BytecodeTest").is_dir() && cwd.join("medal-decompiler").is_dir() {
            return cwd;
        }
    }
    PathBuf::from("D:/Medal")
}

/// Analyze every OK output. Splits the single heaviest file into its own process
/// and runs it concurrently with one batch of all the rest, so the wall time is
/// bounded by the heavy file's analyze rather than the serial sum. Returns `Err`
/// (fatal) if any analyze process fails to spawn.
fn run_analyze_phase(
    analyze: &Path,
    ok: &[(&Path, usize)],
    solver: Solver,
) -> Result<Vec<String>, String> {
    if ok.is_empty() {
        return Ok(Vec::new());
    }
    if ok.len() < 2 {
        let paths: Vec<&Path> = ok.iter().map(|&(p, _)| p).collect();
        return run_analyze_chunked(analyze, &paths, solver);
    }

    // Heaviest by line count (ties broken arbitrarily — the choice only affects
    // the split, which is output-neutral). One process for it, one for
    // everything else, run concurrently.
    let heavy_idx = ok
        .iter()
        .enumerate()
        .max_by_key(|(_, &(_, lc))| lc)
        .map(|(i, _)| i)
        .unwrap();
    let heavy: [&Path; 1] = [ok[heavy_idx].0];
    let rest: Vec<&Path> = ok
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != heavy_idx)
        .map(|(_, &(p, _))| p)
        .collect();

    let (r_heavy, r_rest) = rayon::join(
        || run_analyze_chunked(analyze, &heavy, solver),
        || run_analyze_chunked(analyze, &rest, solver),
    );
    let mut lines = r_heavy?;
    lines.extend(r_rest?);
    Ok(lines)
}

/// Run `luau-analyze --formatter=plain` over `files`, splitting into chunks only
/// if a single command line would approach the Windows 32767-char limit.
/// Diagnostics are independent per file, so chunk boundaries don't affect output.
fn run_analyze_chunked(
    analyze: &Path,
    files: &[&Path],
    solver: Solver,
) -> Result<Vec<String>, String> {
    const LIMIT: usize = 30_000;
    // Budget the heaviest fixed prefix (exe + both flags + separators).
    let base = analyze.as_os_str().len() + "--formatter=plain --solver=old".len() + 8;

    let mut lines = Vec::new();
    let mut chunk: Vec<&Path> = Vec::new();
    let mut len = base;
    for &f in files {
        // +3 ≈ surrounding quotes + separating space added by the OS quoting.
        let flen = f.as_os_str().len() + 3;
        if !chunk.is_empty() && len + flen > LIMIT {
            invoke_analyze(analyze, &chunk, solver, &mut lines)?;
            chunk.clear();
            len = base;
        }
        chunk.push(f);
        len += flen;
    }
    if !chunk.is_empty() {
        invoke_analyze(analyze, &chunk, solver, &mut lines)?;
    }
    Ok(lines)
}

fn invoke_analyze(
    analyze: &Path,
    files: &[&Path],
    solver: Solver,
    out: &mut Vec<String>,
) -> Result<(), String> {
    let mut cmd = Command::new(analyze);
    cmd.arg("--formatter=plain");
    if let Solver::Old = solver {
        cmd.arg("--solver=old");
    }
    let result = cmd
        .args(files.iter().map(|p| p.as_os_str()))
        .stdin(Stdio::null()) // mirror the shell's `< /dev/null`
        .output();
    match result {
        Ok(o) => {
            // analyze writes diagnostics to stdout under --formatter=plain; the
            // shell merges stderr too (`2>&1`), so scan both.
            for buf in [&o.stdout, &o.stderr] {
                for l in String::from_utf8_lossy(buf).lines() {
                    out.push(l.to_string());
                }
            }
            Ok(())
        }
        Err(e) => Err(format!(
            "failed to run luau-analyze {}: {e}",
            analyze.display()
        )),
    }
}

/// Return the `<...>.luau` path prefix of an analyze diagnostic line, or `None`
/// if the line isn't a file-tied diagnostic. The path always ends in `.luau`,
/// so the first `.luau:` disambiguates it from the drive colon / location colons.
fn attribute_path(line: &str) -> Option<&str> {
    let idx = line.find(".luau:")?;
    Some(&line[..idx + ".luau".len()])
}

// ---------------------------------------------------------------------------
// Manual pattern matchers (no `regex` crate — these reproduce the bash greps).
// ---------------------------------------------------------------------------

#[inline]
fn is_word_byte(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

/// `grep '\bgoto\b\|::[A-Za-z_][A-Za-z0-9_]*::'` — word-bounded `goto` OR a
/// `::label::`. Scans raw text (matches inside strings/comments too, like grep).
fn has_goto(src: &str) -> bool {
    let b = src.as_bytes();
    let mut i = 0;
    while let Some(rel) = src[i..].find("goto") {
        let pos = i + rel;
        let before_ok = pos == 0 || !is_word_byte(b[pos - 1]);
        let after = pos + 4;
        let after_ok = after >= b.len() || !is_word_byte(b[after]);
        if before_ok && after_ok {
            return true;
        }
        i = pos + 4;
    }
    has_label(b)
}

/// `::[A-Za-z_][A-Za-z0-9_]*::` — two colons, an identifier, two colons.
fn has_label(b: &[u8]) -> bool {
    let n = b.len();
    let mut i = 0;
    while i + 1 < n {
        if b[i] == b':' && b[i + 1] == b':' {
            let mut j = i + 2;
            if j < n && (b[j] == b'_' || b[j].is_ascii_alphabetic()) {
                j += 1;
                while j < n && is_word_byte(b[j]) {
                    j += 1;
                }
                if j + 1 < n && b[j] == b':' && b[j + 1] == b':' {
                    return true;
                }
            }
        }
        // Advance by one so overlapping forms like `:::a:::` are still found.
        i += 1;
    }
    false
}

/// `Unknown global '(v[0-9]*|p[0-9]*|_)'|LocalShadow: Variable '(v[0-9]*|p[0-9]*)'`
fn scope_matches(line: &str) -> bool {
    contains_temp(line, "Unknown global '", true)
        || contains_temp(line, "LocalShadow: Variable '", false)
}

/// True if `line` contains `needle` immediately followed by a decompiler temp
/// name `v<digits>` / `p<digits>` (zero-or-more digits, so bare `v`/`p` count),
/// or — when `allow_underscore` — exactly `_`, then a closing `'`.
fn contains_temp(line: &str, needle: &str, allow_underscore: bool) -> bool {
    let mut start = 0;
    while let Some(rel) = line[start..].find(needle) {
        let after = line[start + rel + needle.len()..].as_bytes();
        if temp_name_then_quote(after, allow_underscore) {
            return true;
        }
        start += rel + needle.len();
    }
    false
}

fn temp_name_then_quote(b: &[u8], allow_underscore: bool) -> bool {
    if b.is_empty() {
        return false;
    }
    if allow_underscore && b[0] == b'_' {
        return b.len() >= 2 && b[1] == b'\'';
    }
    if b[0] == b'v' || b[0] == b'p' {
        let mut i = 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        return i < b.len() && b[i] == b'\'';
    }
    false
}

// ---------------------------------------------------------------------------
// Regression check for the one known scope-bug file.
// ---------------------------------------------------------------------------

/// Port of `check_background_music_scope_regression`. Only runs for
/// `Client/backgroundMusic.client.lua`; returns the `REGRESSION ...` message on
/// the first failing sub-check, else `None`.
fn check_bgm_regression(records: &[FileRecord]) -> Option<String> {
    const TARGET: &str = "Client/backgroundMusic.client.lua";
    let rec = records
        .iter()
        .find(|r| r.rel == TARGET && r.status == Status::Ok)?;

    let out_text = std::fs::read_to_string(&rec.output).ok()?;

    // 1) Must contain a hoisted loop local: `^\tlocal <ident>$`.
    if !out_text.lines().any(is_hoisted_local) {
        return Some(format!("REGRESSION {TARGET} missing hoisted loop local declaration"));
    }
    // 2) Must NOT declare the random sound inside the inner loop:
    //    `^\t\tlocal <ident> = children[math.random`.
    if out_text.lines().any(is_inner_random_decl) {
        return Some(format!(
            "REGRESSION {TARGET} still declares random sound inside inner loop"
        ));
    }
    // 3) Analyzer must not still see the leaked `v`.
    if rec.leaked_v {
        return Some(format!("REGRESSION {TARGET} analyzer still sees leaked v"));
    }
    None
}

fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    if !(b[0] == b'_' || b[0].is_ascii_alphabetic()) {
        return false;
    }
    b[1..].iter().all(|&c| is_word_byte(c))
}

/// `^\tlocal [A-Za-z_][A-Za-z0-9_]*$` (one leading tab, end-anchored).
fn is_hoisted_local(line: &str) -> bool {
    let l = line.strip_suffix('\r').unwrap_or(line);
    match l.strip_prefix("\tlocal ") {
        Some(rest) => is_ident(rest),
        None => false,
    }
}

/// `^\t\tlocal [A-Za-z_][A-Za-z0-9_]* = children\[math.random` (two leading
/// tabs, NOT end-anchored — a prefix into the rest of the expression).
fn is_inner_random_decl(line: &str) -> bool {
    let l = line.strip_suffix('\r').unwrap_or(line);
    let Some(rest) = l.strip_prefix("\t\tlocal ") else {
        return false;
    };
    match rest.find(" = children[math.random") {
        Some(eq) => is_ident(&rest[..eq]),
        None => false,
    }
}
