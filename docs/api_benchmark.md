# API, CLI and allocation measurements

`benchmark_api` is a native measurement example, independent of the folder
CLI's optional artifact cache. Its ordinary build uses the same mimalloc
allocator as the CLI. The `allocation-counts` feature wraps that allocator
with allocation-free atomic counters in this example only. Neither build
changes normal decompiler options or output.

```powershell
cargo +nightly-2024-12-15 build --release -p luau-lifter --example benchmark_api
Copy-Item target/release/examples/benchmark_api.exe out/api-timing.exe
cargo +nightly-2024-12-15 build --release -p luau-lifter --example benchmark_api --features allocation-counts
Copy-Item target/release/examples/benchmark_api.exe out/api-counts.exe
python scripts/benchmark_api.py pin --corpus INPUT --source-root ACCEPTED_OUTPUT --manifest out/api-manifest.json --workloads out/api-workloads.json
python scripts/benchmark_api.py run --corpus INPUT --manifest out/api-manifest.json --api out/api-timing.exe --counts out/api-counts.exe --cli ACCEPTED_LIFTER --keep out/api-run --report out/api-report.json
```

Create `out` before copying binaries. The pin command requires the four
representative paths in the RobloxProject workload; other datasets can supply
a manifest directly. The run command accepts a subset of the seven group
names with `--groups`. Input consists of saved base64 `.lua` wrappers, with
comment-only lines removed, and the manifest stores the decode key (203 by
default). Empty wrappers remain in the output hash but do not call the API.

The manifest fixes input wrapper hashes, accepted source hashes, groups and
source-tree ordering before measurement. Every result in every process and
round is checked against its per-file source hash. The source-tree digest
uses the manifest order and length-prefixed UTF-8 relative `.luau` paths and
source bytes; it does not silently use a different host's path sort order.
Input/context changes must pass fresh output review before repinning.

The private workloads contain all 3,978 files, 1,968 small files, 395 large
files and one file each for Write, LightningCore, Promise and UI. The size
groups use the nonempty decoded input distribution: index `floor((n-1)*q)`
at q=0.5 and q=0.9 gives thresholds 1,616 and 8,556 bytes. Ties are retained.
The UI representative is the largest decoded input within
`FusionPackage/Components`, Game/GameUnitView/init.lua. This selection was
locked before timing; the reproducible generator matches the earlier locked
manifest exactly.

API timing surrounds `decompile_batch_with_options` within an explicit Rayon
pool. Loading, wrapper decoding, pool creation, output hashing and caller-side
result disposal lie outside that interval. Each process makes one first call
and seven further calls by default. These repeated samples share a process,
allocator and preloaded inputs; the first call is not an OS cold-cache run.
Nested prototype work uses the same pool as the batch. Source options are the
default options plus strict control flow (bits=8).

CLI timing includes process startup, folder discovery, reading, decoding,
decompilation and writing. Each thread count gets a warm-up, then group/thread
order alternates between rounds. Output directories are reused. Subgroups
retain their full original relative paths, preserving module naming context.
The monitor sleeps in 5 ms increments; that granularity can dominate very
small CLI measurements. Output validation is outside the timed interval.

RSS is the Windows process-lifetime `PeakWorkingSetSize`, sampled at 5 ms
and after exit while the process handle is still open. It includes input
loading, hashing, reporting and allocator retention. Other operating systems
report null. It is not a separate peak for each API round. API results retain
all source strings until the batch returns; the CLI writes and releases
individual results. Their memory ownership contracts differ.

Allocation measurement runs after timing workloads, at one thread, in the
separate instrumented executable. It records successful alloc/alloc_zeroed,
realloc and dealloc calls, failed requests and cumulative requested payload
bytes. Reallocation counts the whole new size, even if the allocator grows
in place. Live requested payload adjusts by allocation/free sizes and the
successful realloc size delta. The wrapper preserves pointers, layouts,
alignment and the inner allocator's ownership contract.

Counters stay enabled from process startup, so freeing a preloaded object
cannot underflow a counter that started at the API boundary. Each call reports
counter deltas, live payload before/after, and the largest observed live
payload since the call began. Snapshot and report construction are outside
the counted interval. Concurrent bookkeeping is not a stop-the-world heap
snapshot; the accepted allocation runs use one decompiler worker. Allocation
counts describe Rust's global allocator calls and requested payloads, not
arena sizes, native allocations, bytes actually copied or physical memory.
Instrumented timings are retained as diagnostics and excluded from speed
summaries. A high count alone does not prove allocation-bound execution.

The example refuses diagnostics, duplicate or escaping selected paths, input
hash drift, output hash drift, empty workloads and invalid budgets. Limits
are 10,000 manifest scripts, 16 MiB per manifest/wrapper, 128 MiB total decoded
input, 64 threads and 50 repeated rounds. Reports pin both executable hashes,
the CLI hash, manifest, options, CPU/platform, counts and individual samples.
No OS cache flushing is performed; cold/warm *artifact-cache* results remain
in the separate [cache experiment](artifact_cache.md).

## Accepted Windows workload

Measurements use core commit `61e578c`, strict options and the locked corpus.
Seven repeated samples per API process and seven measured CLI processes per
group/thread count all reproduce the accepted per-file source hashes. Fourteen
instrumented API calls also agree. The ordinary default and instrumented
examples build successfully; 80 Python tests and six validity/refusal controls
pass. [Validation](roadmap_v2_acceptance/api_validation.json),
[every sample and input hash](roadmap_v2_acceptance/api_benchmark.json),
[workload selection](roadmap_v2_acceptance/api_workloads.json).

| Workload | API 1 thread (s) | API 16 (s) | CLI 1 (s) | CLI 16 (s) |
|---|---:|---:|---:|---:|
| all | 16.283149 | 1.276003 | 18.086257 | 1.666657 |
| small | 1.289217 | 0.092034 | 1.974563 | 0.384838 |
| large | 8.322797 | 0.823665 | 8.803690 | 0.907168 |
| write | 0.382051 | 0.382598 | 0.407802 | 0.406201 |
| lightning | 0.212949 | 0.153700 | 0.236285 | 0.179742 |
| promise | 0.043688 | 0.023887 | 0.064045 | 0.047949 |
| ui | 0.030646 | 0.027443 | 0.052860 | 0.048100 |

API peak process RSS for the whole corpus is 64,835,584 / 164,122,624 bytes
at 1/16 threads; median CLI peak RSS is 33,345,536 / 117,129,216 bytes.
The API retains the complete batch of source strings. This is a measurement
of different API/CLI execution contracts, not a before/after speedup claim.
Write shows no useful median gain from 16 API threads in this experiment.

The following counters are the second call in each separate one-thread
instrumented process; first calls and every RSS sample remain in the report.

| Workload | Allocations | Reallocations | Requested bytes | Peak live requested bytes |
|---|---:|---:|---:|---:|
| all | 253,686,740 | 9,632,072 | 12,466,967,133 | 41,084,060 |
| small | 18,039,456 | 616,531 | 892,889,177 | 6,470,716 |
| large | 129,738,996 | 4,673,268 | 6,455,000,702 | 23,266,992 |
| write | 1,898,836 | 37,359 | 123,328,257 | 10,375,150 |
| lightning | 2,953,030 | 72,540 | 173,997,119 | 8,430,934 |
| promise | 706,346 | 18,825 | 36,516,599 | 4,061,132 |
| ui | 618,345 | 39,549 | 26,082,578 | 3,696,222 |

The whole-corpus 12.47 billion requested bytes are cumulative, while the
observed peak live requested payload is about 41.08 million bytes. No request
failed. This establishes a large amount of allocation activity, but does not
isolate allocator CPU cost or prove that allocation is the bottleneck.
