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
    /// Do not reuse generated regular local names across functions in one file.
    ///
    /// Loop header names such as `i`, `k`, and `v` remain reusable between loops.
    #[arg(long)]
    dont_reuse_var: bool,
    /// Disable P1 terminal-helper synthesis (diagnostic/ablation switch).
    #[arg(long)]
    no_synth_helpers: bool,
    /// Permit relational negation flips without proving operands non-NaN.
    /// Faster/cleaner output, but changes behavior when either operand is NaN.
    #[arg(long)]
    assume_no_nan: bool,
    /// Fail closed when source-like structuring cannot avoid synthetic control
    /// (the folder driver is already strict by default).
    #[arg(long)]
    strict_no_synthetic_control: bool,
    /// Permit the certified synthetic dispatcher (diagnostic opt-in).
    ///
    /// Ordinary folder runs are strict by default; this switch is an explicit
    /// compatibility escape hatch for callers that need a semantics-preserving
    /// dispatcher while investigating an otherwise unstructured function.
    #[arg(long, conflicts_with = "strict_no_synthetic_control")]
    allow_certified_dispatcher: bool,
    /// Emit immutable static upvalue metadata under OUT/.tovek-analysis.
    #[arg(long)]
    emit_upvalue_analysis: bool,
    /// Source extension written by the folder decompiler.
    #[arg(long, default_value = "luau", value_parser = ["lua", "luau"])]
    output_extension: String,
    /// Volt exporter manifest used to exclude source fallbacks from bytecode analysis.
    #[arg(long)]
    export_manifest: Option<PathBuf>,
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
    /// Do not reuse generated regular local names across functions in one file.
    ///
    /// Loop header names such as `i`, `k`, and `v` remain reusable between loops.
    #[arg(long)]
    dont_reuse_var: bool,
    /// Disable P1 terminal-helper synthesis (diagnostic/ablation switch).
    #[arg(long)]
    no_synth_helpers: bool,
    /// Permit relational negation flips without proving operands non-NaN.
    /// Default is off to preserve exact NaN semantics.
    #[arg(long)]
    assume_no_nan: bool,
    /// Fail closed when source-like structuring cannot avoid synthetic control
    /// (the folder driver is already strict by default).
    #[arg(long)]
    strict_no_synthetic_control: bool,
    /// Permit the certified synthetic dispatcher (diagnostic opt-in).
    #[arg(long, conflicts_with = "strict_no_synthetic_control")]
    allow_certified_dispatcher: bool,
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
                let options = luau_lifter::DecompileOptions {
                    dont_reuse_var: a.dont_reuse_var,
                    no_synth_helpers: a.no_synth_helpers,
                    assume_no_nan: a.assume_no_nan,
                    control_flow_policy: folder_control_flow_policy(
                        a.strict_no_synthetic_control,
                        a.allow_certified_dispatcher,
                    ),
                    ..luau_lifter::DecompileOptions::default()
                };
                let code = batch::run(
                    &a.src,
                    &a.out,
                    key,
                    a.threads,
                    a.verbose,
                    options,
                    a.emit_upvalue_analysis,
                    &a.output_extension,
                    a.export_manifest.as_deref(),
                );
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
                    luau_lifter::DecompileOptions {
                        dont_reuse_var: a.dont_reuse_var,
                        no_synth_helpers: a.no_synth_helpers,
                        assume_no_nan: a.assume_no_nan,
                        control_flow_policy: folder_control_flow_policy(
                            a.strict_no_synthetic_control,
                            a.allow_certified_dispatcher,
                        ),
                        ..luau_lifter::DecompileOptions::default()
                    },
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

/// Select the folder driver's control-flow policy.  A normal folder run is
/// fail-closed so an `ok` result can never contain the synthetic state-machine
/// dispatcher.  The explicit allow switch is retained as a diagnostic escape
/// hatch, while the existing strict flag remains accepted for CLI/API
/// compatibility.
fn folder_control_flow_policy(
    strict_requested: bool,
    allow_requested: bool,
) -> luau_lifter::ControlFlowOutputPolicy {
    if allow_requested && !strict_requested {
        luau_lifter::ControlFlowOutputPolicy::AllowCertifiedDispatcher
    } else {
        luau_lifter::ControlFlowOutputPolicy::StrictNoSyntheticControl
    }
}

#[cfg(test)]
mod policy_tests {
    use super::{Cli, Command, folder_control_flow_policy};
    use clap::Parser;
    use luau_lifter::ControlFlowOutputPolicy;

    #[test]
    fn ordinary_folder_defaults_to_strict_no_synthetic_control() {
        assert_eq!(
            folder_control_flow_policy(false, false),
            ControlFlowOutputPolicy::StrictNoSyntheticControl
        );
        assert_eq!(
            folder_control_flow_policy(true, false),
            ControlFlowOutputPolicy::StrictNoSyntheticControl
        );
    }

    #[test]
    fn dispatcher_requires_explicit_diagnostic_opt_in() {
        assert_eq!(
            folder_control_flow_policy(false, true),
            ControlFlowOutputPolicy::AllowCertifiedDispatcher
        );
    }

    #[test]
    fn cli_preserves_strict_flag_and_rejects_conflicting_allow() {
        let parsed = Cli::try_parse_from([
            "luau-lifter",
            "decompile-folder",
            "src",
            "out",
            "--strict-no-synthetic-control",
        ])
        .expect("legacy strict flag remains accepted");
        let Command::DecompileFolder(args) = parsed.command else {
            panic!("expected folder command");
        };
        assert!(args.strict_no_synthetic_control);
        assert!(!args.allow_certified_dispatcher);

        assert!(
            Cli::try_parse_from([
                "luau-lifter",
                "decompile-folder",
                "src",
                "out",
                "--strict-no-synthetic-control",
                "--allow-certified-dispatcher",
            ])
            .is_err()
        );
    }
}

/// Legacy single-file mode: `luau-lifter <file> [-e] [--script-name NAME]`.
fn run_single_file() {
    let mut args = std::env::args().skip(1);
    let file_name = args.next().expect("expected exactly one file");
    let mut key = 1;
    let mut script_name: Option<String> = None;
    let mut options = luau_lifter::DecompileOptions::default();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-e" => key = 203,
            "--dont-reuse-var" => options.dont_reuse_var = true,
            "--no-synth-helpers" => options.no_synth_helpers = true,
            "--assume-no-nan" => options.assume_no_nan = true,
            "--allow-certified-dispatcher" => {
                options.control_flow_policy =
                    luau_lifter::ControlFlowOutputPolicy::AllowCertifiedDispatcher;
            }
            "--strict-no-synthetic-control" => {
                options.control_flow_policy =
                    luau_lifter::ControlFlowOutputPolicy::StrictNoSyntheticControl;
            }
            "--script-name" => {
                script_name = Some(args.next().expect("--script-name requires a value"));
            }
            _ => panic!("unexpected argument: {arg}"),
        }
    }

    let bytecode = std::fs::read(&file_name).expect("failed to read file");
    match luau_lifter::try_decompile_bytecode_with_options(
        &bytecode,
        key,
        script_name.as_deref(),
        options,
    ) {
        Ok(source) => println!("{source}"),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
