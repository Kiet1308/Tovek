//! Separate diagnostic driver: phase allocation events, never speed samples.
//! Set MEDAL_PROFILE_JSON for phase rows and --summary for pinned result metadata.
use base64::Engine;
use clap::Parser;
use luau_lifter::{BatchInput, ControlFlowOutputPolicy, DecompileOptions};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[cfg(feature = "allocation-counts")]
#[path = "support/allocation_counts.rs"]
mod allocation_counts;

#[cfg(feature = "allocation-counts")]
#[global_allocator]
static ALLOCATOR: allocation_counts::Counting<mimalloc::MiMalloc> =
    allocation_counts::Counting(mimalloc::MiMalloc);
#[cfg(not(feature = "allocation-counts"))]
#[global_allocator]
static ALLOCATOR: ast::telemetry::allocation::Tracing<mimalloc::MiMalloc> =
    ast::telemetry::allocation::Tracing(mimalloc::MiMalloc);

#[derive(Parser)]
struct Args {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    input_root: PathBuf,
    #[arg(long)]
    summary: PathBuf,
    #[arg(long, default_value = "all")]
    group: String,
    #[arg(long, default_value_t = 1)]
    threads: usize,
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
    anyhow::ensure!((1..=64).contains(&args.threads), "invalid thread budget");
    let phase_report = std::env::var_os("MEDAL_PROFILE_JSON")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow::anyhow!("set MEDAL_PROFILE_JSON to the phase report path before launching")
        })?;
    anyhow::ensure!(
        phase_report != args.summary,
        "phase report and summary paths must differ"
    );
    anyhow::ensure!(ast::telemetry::enabled(), "phase telemetry is disabled");
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
    #[cfg(feature = "allocation-counts")]
    let global_start = allocation_counts::begin();
    let results = pool.install(|| luau_lifter::decompile_batch_with_options(&inputs, options));
    #[cfg(feature = "allocation-counts")]
    let global_allocations = allocation_counts::finish(global_start);
    #[cfg(not(feature = "allocation-counts"))]
    let global_allocations = serde_json::Value::Null;
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
                .map_err(|error| anyhow::anyhow!("{}: {error}", script.spec.path))?;
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
    let output_tree_hash = format!("{:x}", tree.finalize());
    luau_lifter::profile::write_json()?;
    let summary = serde_json::json!({
        "schema_version": 1, "kind": "phase-allocation-diagnostic-not-speed-benchmark",
        "executable_sha256": hash(&std::fs::read(std::env::current_exe()?)?),
        "manifest_sha256": hash(&manifest_bytes), "phase_report": phase_report,
        "group": args.group, "threads": args.threads, "option_bits": options.bits(),
        "decode_key": manifest.decode_key, "scripts": loaded.len(), "nonempty_scripts": inputs.len(),
        "decoded_input_bytes": input_bytes, "output_tree_hash": output_tree_hash,
        "output_bytes": output_bytes, "global_allocations": global_allocations,
        "global_allocator_counts_include_profiler_bookkeeping": cfg!(feature = "allocation-counts"),
        "contract": "One fresh diagnostic API pass after loading/hash-checking inputs and creating the Rayon pool. Exact locked source hashes verified. Fixed per-phase allocation fields exclude profiler context/census/bookkeeping/report traffic and describe same-thread allocation events, not retained heap, allocator CPU cost or performance. Optional process-global counts include profiler traffic and are not directly comparable to ordinary unprofiled batch counts. No per-allocation global atomics unless allocation-counts is also enabled."
    });
    if let Some(parent) = args.summary.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.summary, serde_json::to_vec_pretty(&summary)?)?;
    println!("phase allocation report: {}", phase_report.display());
    Ok(())
}
