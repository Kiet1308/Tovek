//! One uncached single-script API call per sample. Loading, decoding, pool
//! creation, output verification and result disposal are outside the timer.
//! Use scripts/benchmark_single.py for a matched, alternating comparison.
use base64::Engine;
use clap::Parser;
use luau_lifter::DecompileOptions;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    input_root: PathBuf,
    #[arg(long)]
    report: PathBuf,
    #[arg(long)]
    output_root: Option<PathBuf>,
    #[arg(long, default_value = "all")]
    group: String,
    #[arg(long, default_value_t = 1)]
    threads: usize,
    #[arg(long, default_value_t = 7)]
    rounds: usize,
    /// Known DecompileOptions bits; 8 is strict structured source.
    #[arg(long, default_value_t = 8)]
    option_bits: u32,
    /// Baseline-only lock. Candidate differences require a separate quality gate.
    #[arg(long)]
    require_expected_hashes: bool,
    /// Balance which file receives the first process call in independent runs.
    #[arg(long, default_value_t = 0)]
    start_index: usize,
    #[arg(long)]
    reverse: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u8,
    decode_key: u8,
    scripts: Vec<Script>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Script {
    path: String,
    encoding: Encoding,
    input_sha256: String,
    #[serde(default)]
    script_name: Option<String>,
    #[serde(default)]
    expected_output_sha256: Option<String>,
    groups: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Encoding {
    Raw,
    Base64,
}

struct Loaded {
    spec: Script,
    bytecode: Vec<u8>,
    decoded_sha256: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bounded_read(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= limit, "input byte limit exceeded");
    Ok(bytes)
}

fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '\0'])
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(path).components().all(|part| matches!(part, Component::Normal(_)))
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn decode(saved: &[u8], encoding: &Encoding) -> anyhow::Result<Vec<u8>> {
    match encoding {
        Encoding::Raw => Ok(saved.to_vec()),
        Encoding::Base64 => {
            let compact: Vec<_> = saved.split(|&c| c == b'\n')
                .filter(|line| !line.starts_with(b"--"))
                .flat_map(|line| line.iter().copied().filter(|c| !c.is_ascii_whitespace()))
                .collect();
            Ok(base64::prelude::BASE64_STANDARD.decode(compact)?)
        }
    }
}

fn executable_path() -> anyhow::Result<(PathBuf, &'static str)> {
    match std::env::current_exe() {
        Ok(path) => Ok((std::fs::canonicalize(path)?, "current_exe")),
        Err(error) => {
            // Minimal runners may omit /proc/self/exe. Never search PATH or
            // resolve a relative argv[0]; the Python runner launches and hashes
            // this exact absolute file independently.
            let path = PathBuf::from(std::env::args_os().next()
                .ok_or_else(|| anyhow::anyhow!("missing executable identity: {error}"))?);
            anyhow::ensure!(path.is_absolute(), "current_exe failed and argv[0] is not absolute: {error}");
            Ok((std::fs::canonicalize(path)?, "absolute_argv0"))
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let instrumented = cfg!(any(feature = "allocation-counts", feature = "dhat-heap",
        feature = "byte-storage-trace", feature = "phase-allocation-trace"));
    anyhow::ensure!(!instrumented, "instrumented builds are not speed evidence; build without diagnostic features");
    anyhow::ensure!((1..=64).contains(&args.threads) && (1..=1000).contains(&args.rounds), "invalid sample/thread budget");
    anyhow::ensure!(!std::env::vars_os().any(|(key, _)| {
        let key = key.to_string_lossy().to_ascii_uppercase();
        key.starts_with("MEDAL_") || key == "DEINLINE_ANCHOR_TRACE"
    }), "clear diagnostic environment before measuring");
    let options = DecompileOptions::from_flag_bits(args.option_bits)
        .ok_or_else(|| anyhow::anyhow!("unsupported option bits"))?;
    let (executable, executable_identity_method) = executable_path()?;
    let manifest_bytes = bounded_read(&args.manifest, 16 * 1024 * 1024)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    anyhow::ensure!(manifest.schema_version == 1 && !manifest.scripts.is_empty()
        && manifest.scripts.len() <= 10_000, "invalid manifest");
    let input_root = std::fs::canonicalize(&args.input_root)?;
    let mut seen = BTreeSet::new();
    let mut loaded = Vec::new();
    let mut total_bytes = 0usize;
    for spec in manifest.scripts {
        anyhow::ensure!(valid_relative(&spec.path) && seen.insert(spec.path.clone()), "invalid/duplicate path");
        anyhow::ensure!(valid_hash(&spec.input_sha256)
            && spec.expected_output_sha256.as_ref().is_none_or(|v| valid_hash(v)), "invalid SHA-256");
        if !spec.groups.contains(&args.group) { continue; }
        let path = std::fs::canonicalize(input_root.join(&spec.path))?;
        anyhow::ensure!(path.starts_with(&input_root), "input escapes root");
        let saved = bounded_read(&path, 16 * 1024 * 1024)?;
        anyhow::ensure!(hash(&saved) == spec.input_sha256, "input hash mismatch: {}", spec.path);
        let bytecode = decode(&saved, &spec.encoding)?;
        total_bytes += bytecode.len();
        anyhow::ensure!(total_bytes <= 128 * 1024 * 1024, "corpus byte limit exceeded");
        anyhow::ensure!(!args.require_expected_hashes || spec.expected_output_sha256.is_some(),
            "missing expected output hash: {}", spec.path);
        loaded.push(Loaded { decoded_sha256: hash(&bytecode), spec, bytecode });
    }
    anyhow::ensure!(!loaded.is_empty(), "empty selected workload");
    anyhow::ensure!(loaded.len() * (args.rounds + 1) <= 1_000_000, "sample row budget exceeded");
    let output_root = if let Some(path) = &args.output_root {
        // A new tree prevents accidentally overwriting caller-owned files.
        anyhow::ensure!(!path.exists(), "output root must not exist");
        std::fs::create_dir_all(path)?;
        Some(std::fs::canonicalize(path)?)
    } else { None };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(args.threads).build()?;
    luau_lifter::install_quiet_panic_hook();
    let mut rows = Vec::with_capacity(loaded.len() * (args.rounds + 1));
    let mut identities = BTreeMap::new();
    let mut valid = true;
    let mut invocation = 0usize;
    let count = loaded.len();
    for round in 0..=args.rounds {
        let mut order: Vec<_> = (0..count).map(|i| (i + args.start_index) % count).collect();
        if args.reverse ^ (round % 2 == 1) { order.reverse(); }
        for index in order {
            let input = &loaded[index];
            // This is deliberately neither a batch API nor an artifact-cache API.
            let started = Instant::now();
            let result = pool.install(|| luau_lifter::try_decompile_bytecode_with_options(
                &input.bytecode, manifest.decode_key, input.spec.script_name.as_deref(), options));
            let seconds = started.elapsed().as_secs_f64();
            let mut row = serde_json::json!({
                "path": input.spec.path, "input_sha256": input.spec.input_sha256,
                "decoded_input_sha256": input.decoded_sha256, "decoded_input_bytes": input.bytecode.len(),
                "script_name": input.spec.script_name, "round": round, "invocation": invocation,
                "first_for_file": round == 0, "first_in_process": invocation == 0,
                "threads": args.threads, "option_bits": options.bits(), "seconds": seconds,
                "status": "failed", "output_sha256": null, "cli_output_sha256": null,
                "expected_output_matches": null, "output_bytes": null,
                "fallback_count": null, "retry_count": null,
                "counter_status": "unavailable_in_ordinary_single_script_api"
            });
            match result {
                Ok(source) => {
                    let digest = hash(source.as_bytes());
                    let mut cli_digest = Sha256::new();
                    cli_digest.update(source.as_bytes());
                    cli_digest.update(b"\n");
                    let expected_match = input.spec.expected_output_sha256.as_ref().map(|value| value == &digest);
                    let deterministic = identities.get(&input.spec.path).is_none_or(|previous| previous == &digest);
                    identities.insert(input.spec.path.clone(), digest.clone());
                    row["status"] = serde_json::json!(if input.bytecode.first() == Some(&0) { "input_compile_error" }
                        else if source.trim().is_empty() { "empty_output" }
                        else if !deterministic { "nondeterministic" }
                        else if args.require_expected_hashes && expected_match != Some(true) { "expected_hash_mismatch" }
                        else { "passed" });
                    row["output_sha256"] = serde_json::json!(digest);
                    row["cli_output_sha256"] = serde_json::json!(format!("{:x}", cli_digest.finalize()));
                    row["output_bytes"] = serde_json::json!(source.len());
                    row["expected_output_matches"] = serde_json::json!(expected_match);
                    if round == 0 {
                        if let Some(root) = &output_root {
                            let path = root.join(format!("{}.luau", input.spec.path));
                            std::fs::create_dir_all(path.parent().unwrap())?;
                            std::fs::write(path, source.as_bytes())?;
                        }
                    }
                }
                Err(error) => { row["error"] = serde_json::json!(error); }
            }
            valid &= row["status"] == "passed";
            rows.push(row);
            invocation += 1;
        }
    }
    let report = serde_json::json!({
        "schema_version": 1, "kind": "tovek-single-script-samples-v1",
        "executable_sha256": hash(&std::fs::read(executable)?),
        "executable_identity_method": executable_identity_method,
        "manifest_sha256": hash(&manifest_bytes), "api": "try_decompile_bytecode_with_options",
        "threads": args.threads, "rounds": args.rounds, "option_bits": options.bits(),
        "decode_key": manifest.decode_key, "group": args.group, "scripts": loaded.len(),
        "options": { "dont_reuse_var": options.dont_reuse_var, "no_synth_helpers": options.no_synth_helpers,
            "assume_no_nan": options.assume_no_nan, "control_flow_policy": format!("{:?}", options.control_flow_policy),
            "emit_binding_provenance": options.emit_binding_provenance,
            "synthesize_arithmetic_loops": options.synthesize_arithmetic_loops,
            "compact_annotations": options.compact_annotations, "compact_style": options.compact_style,
            "assume_standard_libraries": options.assume_standard_libraries },
        "instrumented": instrumented, "features": {
            "allocation_counts": cfg!(feature = "allocation-counts"), "dhat_heap": cfg!(feature = "dhat-heap"),
            "byte_storage_trace": cfg!(feature = "byte-storage-trace"),
            "phase_allocation_trace": cfg!(feature = "phase-allocation-trace") },
        "build": { "debug_assertions": cfg!(debug_assertions), "target_arch": std::env::consts::ARCH,
            "target_os": std::env::consts::OS, "panic_unwind": cfg!(panic = "unwind"),
            "crate_version": env!("CARGO_PKG_VERSION") },
        "output_root": output_root, "rows": rows, "complete": true, "valid": valid,
        "require_expected_hashes": args.require_expected_hashes,
        "contract": "Each sample runs the full uncached single-script API; only one script is active. Input I/O/base64 decode, Rayon pool creation, source hashing, output writes and result disposal are outside the timer. Round zero is first-for-file; exactly one row is first-in-process. Later calls reuse machine caches but never a decompiled result. No filesystem-cold claim. Source hashes cover exact API bytes; cli_output_sha256 adds the CLI newline. Optional expected hashes do not certify changed candidate output. Fallback/retry counts are unavailable, not zero."
    });
    if let Some(parent) = args.report.parent() { std::fs::create_dir_all(parent)?; }
    std::fs::write(&args.report, serde_json::to_vec_pretty(&report)?)?;
    anyhow::ensure!(valid, "one or more calls failed; all observations retained in report");
    Ok(())
}
