mod batch;
mod decompile_core;
mod validate;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

// Global allocator for the `luau-lifter` binary. It lives here (the binary
// crate), not in the library, so it is NOT inherited by library consumers
// (`web-server`, the `luau-worker` wasm cdylib). The decompiler is
// allocator-bound — 2–4M allocations against a 6–9 MB live heap — so mimalloc's
// per-thread free-lists replace the slow Windows `HeapAlloc` and cut single-file
// wall time ~1.5–1.7× (measured), byte-identical output. The mimalloc dependency
// is target-gated off wasm32 (its C source has no wasm build); this binary is
// never built for wasm, so the static is unconditional there.
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(not(feature = "dhat-heap"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser, Debug)]
#[command(about, version, author)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Decompile every saved-bytecode `.lua` file in a folder, in parallel.
    ///
    /// Each input is a UniversalSynSaveInstance text file (`--` comment header
    /// then a base64 bytecode blob). The output tree mirrors SRC under OUT with
    /// every `.lua` renamed to `.luau`.
    DecompileFolder(FolderArgs),
    /// Decompile a folder, then validate every output with Luau's own parser
    /// (`luau-analyze`): parse gate + local-scope check + the backgroundMusic
    /// regression. The native, parallel replacement for `validate_all.sh`.
    ValidateFolder(ValidateArgs),
}

#[derive(clap::Args, Debug)]
struct FolderArgs {
    /// Source directory containing saved-bytecode `.lua` files.
    src: PathBuf,
    /// Output directory (mirrors SRC; `.lua` -> `.luau`).
    out: PathBuf,
    /// Force the Roblox client key (203). Redundant with the default; kept for
    /// parity with the single-file `-e` flag.
    #[arg(short = 'e', long)]
    encoded: bool,
    /// Decode key: op = op * key % 256. Defaults to 203 (Roblox client
    /// bytecode), since that is the only thing this pipeline decodes. Pass
    /// `--key 1` for unencoded Luau bytecode.
    #[arg(short, long, default_value_t = 203)]
    key: u8,
    /// Worker threads (0 = all logical CPUs).
    #[arg(short, long, default_value_t = 0)]
    threads: usize,
    /// Print one line per decompiled file.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args, Debug)]
struct ValidateArgs {
    /// Source directory containing saved-bytecode `.lua` files.
    src: PathBuf,
    /// Output directory (mirrors SRC; `.lua` -> `.luau`).
    out: PathBuf,
    /// Force the Roblox client key (203).
    #[arg(short = 'e', long)]
    encoded: bool,
    /// Decode key: op = op * key % 256. Defaults to 203 (Roblox client).
    #[arg(short, long, default_value_t = 203)]
    key: u8,
    /// Worker threads (0 = all logical CPUs).
    #[arg(short, long, default_value_t = 0)]
    threads: usize,
    /// Print a timing line to stderr.
    #[arg(short, long)]
    verbose: bool,
    /// Path to `luau-analyze.exe` (overrides LUAU_ANALYZE / --tool-dir / ROOT).
    #[arg(long)]
    analyze: Option<PathBuf>,
    /// Directory holding `luau-analyze.exe` (used if --analyze is unset).
    #[arg(long)]
    tool_dir: Option<PathBuf>,
    /// luau-analyze typechecker: `new` (default, the validate_all.sh parity
    /// baseline) or `old` (~8x faster, opt-in fast path; diagnostics may differ
    /// on dirty corpora, so not for CI gating).
    #[arg(long, default_value = "new")]
    solver: String,
}

fn main() {
    // One global quiet panic hook for the whole process (see lib.rs). Caught
    // per-function panics stay silent and the parallel driver stays race-free.
    luau_lifter::install_quiet_panic_hook();

    // Manual pre-dispatch on argv[1] so the legacy single-file invocation
    // (`luau-lifter <file> [-e] [--script-name X]`) is untouched by clap.
    match std::env::args().nth(1).as_deref() {
        Some("decompile-folder") => match Cli::parse().command {
            Command::DecompileFolder(a) => {
                let key = if a.encoded { 203 } else { a.key };
                let code = batch::run(&a.src, &a.out, key, a.threads, a.verbose);
                std::process::exit(code);
            }
            _ => unreachable!("argv[1] dispatch guarantees the DecompileFolder variant"),
        },
        Some("validate-folder") => match Cli::parse().command {
            Command::ValidateFolder(a) => {
                let key = if a.encoded { 203 } else { a.key };
                let old_solver = match a.solver.as_str() {
                    "old" => true,
                    "new" => false,
                    other => {
                        eprintln!("error: --solver must be 'new' or 'old', got '{other}'");
                        std::process::exit(2);
                    }
                };
                let code = validate::run(
                    &a.src,
                    &a.out,
                    key,
                    a.threads,
                    a.verbose,
                    a.analyze.as_deref(),
                    a.tool_dir.as_deref(),
                    old_solver,
                );
                std::process::exit(code);
            }
            _ => unreachable!("argv[1] dispatch guarantees the ValidateFolder variant"),
        },
        // Route help/version through clap (prints and exits).
        Some("--help") | Some("-h") | Some("--version") | Some("-V") => {
            Cli::parse();
        }
        _ => run_single_file(),
    }
}

/// Legacy single-file mode: `luau-lifter <file> [-e] [--script-name NAME]`.
fn run_single_file() {
    let mut args = std::env::args().skip(1);
    let file_name = args.next().expect("expected exactly one file");
    let mut key = 1;
    let mut script_name: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-e" => key = 203,
            "--script-name" => {
                script_name = Some(args.next().expect("--script-name requires a value"));
            }
            _ => panic!("unexpected argument: {arg}"),
        }
    }

    let bytecode = std::fs::read(&file_name).expect("failed to read file");
    match luau_lifter::try_decompile_bytecode_with_script_name(
        &bytecode,
        key,
        script_name.as_deref(),
    ) {
        Ok(source) => println!("{source}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
