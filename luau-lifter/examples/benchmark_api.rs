//! Reproducible in-process API measurements with pinned input/output hashes.
//! Timing excludes input loading/decoding, pool creation, source hashing and
//! caller-side result disposal. Allocation instrumentation is a separate build.
use base64::Engine;
use clap::Parser;
use luau_lifter::{BatchInput, ControlFlowOutputPolicy, DecompileOptions};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

#[cfg(feature = "allocation-counts")]
#[path = "support/allocation_counts.rs"]
mod allocation_counts;

#[cfg(feature = "allocation-counts")]
#[global_allocator]
static ALLOCATOR: allocation_counts::Counting<mimalloc::MiMalloc> =
    allocation_counts::Counting(mimalloc::MiMalloc);
#[cfg(not(feature = "allocation-counts"))]
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
    #[arg(long, default_value = "all")]
    group: String,
    #[arg(long, default_value_t = 1)]
    threads: usize,
    #[arg(long, default_value_t = 7)]
    rounds: usize,
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
    input_sha256: String,
    source_sha256: String,
    groups: Vec<String>,
}

struct Loaded {
    spec: Script,
    bytecode: Vec<u8>,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn bounded_read(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= limit, "input byte limit exceeded");
    Ok(bytes)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        (1..=64).contains(&args.threads) && (1..=50).contains(&args.rounds),
        "invalid sample/thread budget"
    );
    anyhow::ensure!(
        !std::env::vars_os().any(|(key, _)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            key.starts_with("MEDAL_") || key == "DEINLINE_ANCHOR_TRACE"
        }),
        "diagnostic environment must be cleared for this benchmark"
    );
    let manifest_bytes = bounded_read(&args.manifest, 16 * 1024 * 1024)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    anyhow::ensure!(
        manifest.schema_version == 1 && manifest.scripts.len() <= 10_000,
        "invalid manifest"
    );
    let input_root = std::fs::canonicalize(&args.input_root)?;
    let mut loaded = Vec::new();
    let mut input_bytes = 0usize;
    let mut seen = std::collections::BTreeSet::new();
    for spec in manifest.scripts {
        if !spec.groups.contains(&args.group) {
            continue;
        }
        let relative = Path::new(&spec.path);
        anyhow::ensure!(
            !spec.path.is_empty()
                && relative
                    .components()
                    .all(|p| matches!(p, Component::Normal(_)))
                && seen.insert(spec.path.clone()),
            "invalid/duplicate input path"
        );
        let path = std::fs::canonicalize(input_root.join(relative))?;
        anyhow::ensure!(path.starts_with(&input_root), "input escapes corpus");
        let saved = bounded_read(&path, 16 * 1024 * 1024)?;
        anyhow::ensure!(
            hash(&saved) == spec.input_sha256,
            "input hash mismatch: {}",
            spec.path
        );
        let compact = saved
            .split(|&b| b == b'\n')
            .filter(|line| !line.starts_with(b"--"))
            .flat_map(|line| {
                line.iter()
                    .copied()
                    .filter(|b| !matches!(b, b' ' | b'\t' | b'\r'))
            })
            .collect::<Vec<_>>();
        let bytecode = base64::prelude::BASE64_STANDARD.decode(compact)?;
        input_bytes += bytecode.len();
        anyhow::ensure!(
            input_bytes <= 128 * 1024 * 1024,
            "project byte limit exceeded"
        );
        loaded.push(Loaded { spec, bytecode });
    }
    anyhow::ensure!(!loaded.is_empty(), "empty workload");
    // The manifest fixes the source-tree hashing order across host platforms.
    let inputs = loaded
        .iter()
        .filter(|s| !s.bytecode.is_empty())
        .map(|s| BatchInput {
            bytecode: &s.bytecode,
            encode_key: manifest.decode_key,
            script_name: Some(&s.spec.path),
        })
        .collect::<Vec<_>>();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build()?;
    luau_lifter::install_quiet_panic_hook();
    let options = DecompileOptions {
        control_flow_policy: ControlFlowOutputPolicy::StrictNoSyntheticControl,
        ..DecompileOptions::default()
    };
    let mut rows = Vec::with_capacity(args.rounds + 1);
    let mut expected_tree = None;
    for round in 0..=args.rounds {
        #[cfg(feature = "allocation-counts")]
        let allocation_start = allocation_counts::begin();
        let started = Instant::now();
        let results = pool.install(|| luau_lifter::decompile_batch_with_options(&inputs, options));
        let seconds = started.elapsed().as_secs_f64();
        #[cfg(feature = "allocation-counts")]
        let allocations = allocation_counts::finish(allocation_start);
        #[cfg(not(feature = "allocation-counts"))]
        let allocations = serde_json::Value::Null;
        let mut results = results.into_iter();
        let mut tree = Sha256::new();
        let mut output_bytes = 0usize;
        for script in &loaded {
            let source = if script.bytecode.is_empty() {
                String::new()
            } else {
                let mut source = results
                    .next()
                    .expect("one result per nonempty script")
                    .map_err(|e| anyhow::anyhow!("{}: {e}", script.spec.path))?;
                source.push('\n');
                source
            };
            anyhow::ensure!(
                hash(source.as_bytes()) == script.spec.source_sha256,
                "source differs from locked CLI output: {}",
                script.spec.path
            );
            output_bytes += source.len();
            let name = Path::new(&script.spec.path)
                .with_extension("luau")
                .to_string_lossy()
                .replace('\\', "/");
            for bytes in [name.as_bytes(), source.as_bytes()] {
                tree.update((bytes.len() as u64).to_le_bytes());
                tree.update(bytes);
            }
        }
        let tree = format!("{:x}", tree.finalize());
        if let Some(expected) = &expected_tree {
            anyhow::ensure!(expected == &tree, "nondeterministic API result");
        }
        expected_tree = Some(tree.clone());
        rows.push(
            serde_json::json!({"round": round, "first_call": round == 0, "seconds": seconds,
            "output_tree_hash": tree, "output_bytes": output_bytes, "allocations": allocations}),
        );
        eprintln!(
            "API group={} threads={} round={} seconds={seconds:.6}",
            args.group, args.threads, round
        );
    }
    let executable_sha256 = hash(&std::fs::read(std::env::current_exe()?)?);
    let report = serde_json::json!({"schema_version": 1, "executable_sha256": executable_sha256,
        "manifest_sha256": hash(&manifest_bytes), "option_bits": options.bits(), "decode_key": manifest.decode_key,
        "group": args.group, "threads": args.threads, "scripts": loaded.len(), "nonempty_scripts": inputs.len(),
        "decoded_input_bytes": input_bytes, "allocation_instrumented": cfg!(feature = "allocation-counts"),
        "api": "decompile_batch_with_options", "rows": rows,
        "contract": "Input loading/base64 decode, Rayon pool creation, hashing and caller result disposal are outside timing. Round zero is the first call, followed by repeated calls in the same process. All source bytes match the locked CLI hashes. This is not cold filesystem-cache timing.",
        "allocation_contract": "Separate instrumented build: successful Rust global allocator calls and requested payload bytes, including full new sizes for reallocations. Live/peak requested payloads include preloaded inputs and retained API results. Allocator arenas, native allocations and OS working set are not inferred from these counts. Instrumented timings are not performance samples."});
    if let Some(parent) = args.report.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.report, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
