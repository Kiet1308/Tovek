# Optional cache for repeated folder decompilation

```sh
luau-lifter decompile-folder ./dump ./out --cache-dir ./tovek-cache
```

The cache is opt-in and belongs outside both the input and output trees.
`--cache-max-mib` sets its content-byte budget, defaulting to 512 MiB. The normal
folder path still decodes every input and writes every output. Hits reuse a
successful source/analysis artifact; source fallbacks, empty inputs and failed
decompilations keep their existing handling. No cache is used by the single-file,
validation, web or worker APIs.

The key includes the SHA-256 of the executing binary and decoded bytecode,
decode key, every `DecompileOptions` bit, whether an analysis artifact was
requested, the presence of `MEDAL_NO_SHARED_TAIL`, and the complete current
script-name projection used by the naming pipeline. The projection is the same
`script_module_hint` function used by both naming and named-module recovery.
It handles parent names for `init` modules, suffixes and normalized spelling.
Future uses of script context must update this contract; changing the binary
already invalidates all old entries.

Script path, source extension, export ID, full name and Volt export metadata
are applied by the normal output path after a hit. Sidecars and generation
manifests are rebuilt for the current file. Cached function occurrences, names
and storage lineage are not reused across different naming contexts or option
sets. All analysis types round-trip through typed serde data; diagnostic codes
use `Cow<str>` so cached strings can be owned without changing serialized text.
This is an additive serialization API; Rust callers constructing an
`AnalysisDiagnostic` now use `code: "code".into()`.

Each entry repeats the complete key and carries a checksum of the serialized
artifact. Missing, malformed, oversized or checksum-mismatched entries are
misses. Failed computations are never stored. Checksums detect accidental
corruption; a cache is a trusted local build artifact store, not an authenticated
source or semantic proof. Input/output trees must be disjoint from the cache.
An exclusive process lock protects its inventory. Unknown marker formats,
unmarked preexisting entry files and nonregular cache paths are refused. Cache
publication uses the existing contained atomic-write implementation.

Limits are 16 MiB per entry/input, 4 KiB per script context input, 20,000 entries
and 100,000 directory entries inspected at startup. Serialization and marker
reads are bounded. The quota counts file contents; it is not a process RSS or
filesystem-allocation limit. Reducing the quota evicts excess contents at
startup, including runs that would otherwise contain only hits. Old entries
are evicted by recency, with a best
effort timestamp update on reads. Cache write failures preserve the newly
computed result and appear in counters. A marker or lock error aborts the
explicit cache request before workers start.

No cache lock is held while the decompiler runs. Nested Rayon tasks can execute
other folder work on the same worker, so waiting on an in-flight duplicate would
risk deadlock. Concurrent misses may compute the same key more than once; a
second lookup serializes publication and checks both artifacts agree. The
`raced_hits` counter reports this case separately from work avoided by an
ordinary hit. Existing entries deduplicate across paths only when the complete
key agrees.

Profiling or dump environments bypass the cache to preserve fresh diagnostics.
This includes `MEDAL_*` except the keyed `MEDAL_NO_SHARED_TAIL`, and
`DEINLINE_ANCHOR_TRACE`; name matching is case-insensitive for Windows. Cache
counters are printed as a `TOVEK_CACHE` JSON line on stderr. They report hits,
misses, writes, raced hits, corrupt reads, bypasses, evictions and I/O errors.
They do not alter source or sidecar schemas.

The acceptance checks compare every source byte on the 3,978-file private
corpus, retain the two known same-bytecode/different-module counterexamples,
and compare source plus full sidecar hashes on all 144 runtime and 513 public
configurations. The public set includes the 45 held-out Rodux configurations.
`provenance_fixtures.py --cache` checks cold/warm reuse against uncached artifacts,
then the existing independent parser checks final binding locations.
`cache_controls.py` adds real CLI checks for option and input invalidation,
wrapper-only changes, corruption, repeated failures and diagnostic bypass.
Both checks run in CI. [Validation inventory](roadmap_v2_acceptance/cache_validation.json).

The shared context projection gains one documentation line in `name_locals.rs`.
Its tracked Rust naming-rule locations therefore advance by one line in 116
runtime and 474 public sidecars. Every other prior metadata field is unchanged;
this is distinct from the exact cold/warm cache comparison within one binary.

The benchmark compares the preceding equality release, the current release
without cache, and the current release with an already populated cache. Seven
interleaved CLI rounds at one/16 threads record source hashes, timings and peak
RSS. Cache population is reported separately from warm hits. Filesystem cache
is warm throughout; these results do not measure cold OS cache, in-memory API
latency, allocation counts, or unchanged work in an external application.
[Paired measurements](roadmap_v2_acceptance/cache_benchmark.json).
