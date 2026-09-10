# Pass profiling

Set `MEDAL_PROFILE_JSON` to an output filename before starting the native CLI:

```powershell
$env:MEDAL_PROFILE_JSON = 'out/pass-profile.json'
target/release/luau-lifter.exe decompile-folder INPUT OUTPUT --key 203 --threads 1
Remove-Item Env:MEDAL_PROFILE_JSON
```

The parent directory must exist. The CLI writes the report after workers join,
including folder modes that terminate through `process::exit`; a write error
produces a nonzero exit status. Library callers that enable profiling must call
`luau_lifter::profile::write_json()` after their workers finish. Profiling is
disabled by default and is independent of the legacy `MEDAL_PROF` counters.

Each row aggregates a file, optional original prototype ID and pass. Whole
module AST passes have `prototype: null`; they are not arbitrarily assigned to
one child prototype. Repeated lifted instances of the same prototype aggregate
their call counts. Input without a script name uses a bytecode hash identity.
Workers carry explicit file/prototype context; nested work and unwinding
restore the previous worker context. Records retain strings and numbers, with
no additional AST/local owners.

`inclusive_ns` is elapsed wall time inside a span. `exclusive_ns` subtracts
nested measured spans **on the same thread**. Both can include waits; Rayon
workers overlap. Adding rows does not produce CPU time or process wall time,
and parent/child inclusive totals overlap. Timing and aggregation overhead
can appear in an ancestor's exclusive interval. Use uninstrumented,
interleaved `benchmark_v2.py` runs to establish performance improvements.

The profiler separates these phases without changing their execution order:

- `S_FACTOR_INITIAL`: common-tail factoring before statement de-inline.
- `S_DEINLINE`: statement de-inline, including its own fixed point.
- `S_FACTOR_FIXEDPOINT`: common-tail factoring after each de-inline invocation.
- `D_WRITE_CENSUS`, `D_COLLECT_TARGETS`, `D_SCAN`, `D_COLLAPSE_RESULTS`: selected
  de-inline subphases. Target candidates, accepted/refused targets, canonical
  length scans, width candidates, matches and unification calls have counters.
- `TAIL_UNSHARE`, `TAIL_SCAN` and their function-only variants: ownership
  preparation and scanning within common-tail factoring.

Existing `F_*` and `S_*` timed pipeline stages are also recorded. Counts are
attached to the innermost active span. A missing counter means unmeasured or
inapplicable. The existing write-once census is measured once per de-inline
invocation; this change does not introduce or claim a new cache.

Before/after node census currently covers the three `S_FACTOR_*`/`S_DEINLINE`
phases. It counts statement and rvalue occurrences, including indexed
assignment operands, and visits each owned closure body once. Binder/type
syntax and implicit storage references are excluded. The census occurs
outside the measured phase interval. `node_samples: 0` means unmeasured,
including other timed passes; zero node sums there do not mean an empty AST.
Broader node/effect/cache accounting and allocation instrumentation remain
open work in R7.

There are limits of one million aggregate rows, 256 nested spans, one million
nodes per census and census depth 256. Overflow or an unavailable AST lock is
reported explicitly. Existing aggregate keys continue collecting after the
row limit; new keys increment `dropped_records`. Incomplete node samples and
misnested spans invalidate a complete-profile claim.

`scripts/profile_v2.py` compares a baseline binary, the new binary without
profiling, and the new binary with profiling at requested thread counts. It
requires exact source-tree hashes and equal counters/node census after removing
timing fields, checks candidate accounting and fixed-point call counts, and
saves the raw profiles losslessly as `.json.gz`. These are diagnostic runs,
separate from release performance benchmarks. The
[acceptance record](roadmap_v2_implementation.md) links measured results.
