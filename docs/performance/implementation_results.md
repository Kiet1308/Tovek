# Cold-script engine experiment — implementation and measured results

## Outcome

The branch implements the analysis, reconstruction, naming, admission, and
measurement changes described below. **It does not achieve the requested 10×
latency improvement.** The ordinary public-source cohort remains close to the
baseline, including a small observed slowdown in repeated uncached calls with
four threads. These results do not justify merging the entire change as a 10×
performance release.

The measured engine comparison is:

- Baseline: `55c5bcd71b01f5ab630bc7102c0162235d534722`.
- Candidate: `4642b2c53c880ed404384d2872d44b7c856df7a9`.
- Branch: `codex/cold-script-engine-rewrite`.
- Review: [PR #7](https://github.com/Kiet1308/Tovek/pull/7).
- Date: 2026-10-04 UTC.

Later evidence-only commits do not change the measured engine. A full ValueIR
replacement, reusable mutable SSA snapshots across retries, and a general
region planner are **not implemented**. The numeric def/use index and bounded
region planner below are real consumers in the production pipeline, with a
deliberately limited scope.

## Latency measurements

Each ratio is baseline latency divided by candidate latency. Above 1 means
faster. The table reports the geometric mean of each script/profile's median
ratio, giving each selected input equal weight.

| Cohort | Threads | First call for each file in a process | Repeated uncached calls | Ratio of summed repeated medians |
| --- | ---: | ---: | ---: | ---: |
| 33 repository bytecode fixtures | 1 | 1.025× | 1.002× | 1.045× |
| 33 repository bytecode fixtures | 4 | 1.024× | 1.009× | 1.065× |
| 513 public source/compiler profiles | 1 | 1.039× | 1.031× | 1.017× |
| 513 public source/compiler profiles | 4 | 1.053× | 0.987× | 0.986× |

The public cohort contains 171 source files compiled at O0/O1/O2, including a
45-profile independent-family holdout. These are 513 bytecode inputs, not 513
independent source projects. The holdout's repeated-call geometric means are
1.074× at one thread and 0.963× at four threads. No measured script/profile
reaches a 10× median improvement.

For scale, the public cohort's median of per-input repeated-call medians changes
from 0.573 ms to 0.559 ms at one thread and from 0.582 ms to 0.596 ms at four
threads. The corresponding sum-of-medians changes are 519.30 ms to 510.82 ms and
444.65 ms to 451.01 ms. These aggregate measures answer different questions;
neither should be substituted for an HTTP batch-throughput number.

### Measurement contract

Both variants were ordinary optimized release builds, with the same
`nightly-2026-06-15` compiler, locked dependencies, workspace release profile,
mimalloc allocator, and target flags. The release profile uses opt-level 3,
fat LTO, one codegen unit, and unwind panics. The local toolchain used GNU
linking with `-C linker-features=-lld` for both variants. No allocation-tracing
or profiling feature was enabled. The same new benchmark harness was copied
into the detached baseline worktree without modifying the baseline engine.

The runner invokes `try_decompile_bytecode_with_options` once per sample, with
strict structured output (`option_bits=8`) and the manifest's exact decode key
and script-name context. Every call decompiles anew. It never invokes a result
cache or the batch API. Input reading/base64 decoding, pool creation, output
hashing, and disposal of the returned source string are outside the timer;
the decompiler's own preparation, transformations, and source formatting are
inside it. This is API latency, not process launch or HTTP round-trip latency.

The process order alternates AB/BA. Each cohort has four independent process
rounds per variant/thread count, with 20 repeated calls after one first call
per input. Thus each input/thread/variant has four first-for-file observations
and 80 repeated uncached observations. All processes inherit CPU affinity
0–3; no concurrent compiler builds run during measurement. Physical CPU model,
frequency control, and isolated-core guarantees were unavailable in this
workspace. The narrow timing differences are observations on this host, not
a statistical significance claim. Four first-for-file samples do not establish
a service p95, and most files are not the first API call in their process.

There are **183,456 timed API invocations** across both cohorts. Every call
succeeded; outputs were stable across repetitions and thread counts. The
public comparison initially remained quality-pending for four source changes.
The exact reviewed gate was attached to the same archived observations without
rerunning or selecting favorable timing samples.

## Implemented architecture

### Immutable preparation across retries

`PreparedChunk` parses and validates the serialized input once per public API
call. It retains immutable prototype data, globals/capture facts, raw upvalue
analysis, and lazy per-prototype type/name metadata. Output-budget retries
reuse this preparation but allocate fresh mutable locals, CFGs, ASTs, local-ID
state, reconstruction state, and call-origin state.

Mutable AST/SSA snapshots are not reused: existing deep-clone operations share
local and closure identities, so treating those objects as independent retry
checkpoints would be unsound. This implementation removes a specific repeated
front-end cost; it does not eliminate the expensive reconstruction work in
every retry.

### Revision-scoped facts and numeric indexes

- Dominators are keyed by CFG topology revision. Operand-only inlining edits
  do not rebuild them. `JumpChanges` separately reports legacy convergence and
  topology changes, including unreachable adapter deletion.
- The SSA inliner consumes a numeric definition/eligible-use ledger that does
  not keep one `RcLocal` owner per operand.
- Liveness uses exact dense or sparse word rows with coalesced pending deltas.
  Sparse rows promote when their payload cost exceeds dense storage, and the
  selector accounts for mandatory row headers on tall, narrow graphs.
- Adjacent copy and nil cleanup share an `AnalysisSession` containing bounded
  numeric capture membership. Known edits update or conservatively preserve
  those facts; unknown edits invalidate them. Depth, work-budget, and lock
  refusal skip optional cleanup rather than starting an unbounded fallback.

These changes retain existing outer pass schedules and proof guards. They do
not constitute a complete new semantic IR or whole-pipeline pass manager.

### Reconstruction and cleanup work

Expression reconstruction caches each caller/root candidate order lazily and
uses constant-time active membership. Canonical statement windows share length
prefix summaries. Helper matching reuses compiled pattern plans only while
their structural assumptions remain valid; capture, effect, motion, and
occurrence-dependent checks are recomputed.

Tail-guard cleanup indexes real statements once and works backward. Nil cleanup
marks removals and compacts once. Operation-count regressions verify specific
reductions: 2,050 candidate queries require two priority builds, and 4,096
overlapping canonical-window queries use a 256-statement prefix with at most
64 directly scanned statements. Those are operation counts on constructed
cases, not measured end-to-end 10× speedups.

### Presentation separated from ownership

Bindings now carry explicit temporary/parameter/named/discard intent and a
method-receiver role. Transformation eligibility no longer changes merely
because the displayed name changes. Unused-binding decisions use actual
occurrences, parameters, and captures rather than incidental retained Arc
owners. Recorded names and meaningful inferred table/function names remain
protected. An unread fixed parameter can be displayed as `_` while still
occupying its original argument position.

### Bounded RegionPlan

Before full region analysis, the new planner can prove linear CFGs or one
conditional with linear arms and a shared continuation or separate terminal
arms. It admits at most 64 nodes. It records node ownership and coverage before
materializing AST content once.

It refuses cycles, nested branching, phi transfers, declarations, CLOSE events,
unlowered VM markers, unmet capture obligations, and incomplete graph coverage.
Every refusal reaches the existing typed prover/fallback. Its tests include
72 graph/outcome trace comparisons, boundary and terminal cases, provenance
and owner checks, and ten refusal classes. This is a narrow initial planner,
not a general replacement of restructuring.

### Expansion and Lua 5.1 boundaries

Weighted prototype expansion is admitted before constructing occurrence trees:
65,536 expanded instances, 8,000,000 expanded instruction words, and constructor
depth 256. Constructor cycles and invalid references are rejected with typed
diagnostics. A syntactically valid but excessively expanding input may now be
refused; this changes the admission boundary and should be considered when
evaluating unusually large production scripts.

Lua 5.1 parsing is iterative and bounded, mandatory debug-section counts remain
mandatory even when stripped, and validation checks operands, captures, jump
targets, open-result protocols, and weighted expansion before lifting. These
are reliability changes, not ordinary-script speedup claims.

### Native, Worker, and client adapters

Native ingress/response admission permits eight reservations with at most two
batches. CPU admission permits four jobs with at most two batch quanta, each
of up to eight scripts. Batch queue quotas preserve interactive waiting
capacity. Detached CPU work and unread response bodies retain their resource
reservations.

Whole-request deduplication survives quantum boundaries with the process cache
disabled. Ordinal plans use the core's normalized context equality. Retained
payloads are bounded and released at their last use. A full memo skips storage;
it never converts a valid current output into an error. The actual serialized
response writer alone enforces the final JSON budget.

The optional process source cache and async singleflight are off by default
(`TOVEK_SOURCE_CACHE_MIB=0`), include complete semantic context, bypass diagnostics,
and retain no errors. Worker bodies/messages and clients have explicit bounds;
malformed WebSocket messages do not prevent later valid messages from working.
The batch client leaves failed rows eligible for bounded single-script fallback.
See [native server contracts](../../web-server/README.md) and
[Worker contracts](../../luau-worker/README.md).

## Quality evidence

| Check | Result |
| --- | --- |
| AST unit tests | 989 passed, 1 ignored |
| CFG unit tests | 126 passed, 1 ignored |
| Region restructuring tests | 137 passed |
| Lua 5.1 parser and CLI tests | 14 passed |
| Luau library tests | 90 passed |
| Luau CLI unit tests in normal Linux CI | 62 passed |
| Native server / Worker unit tests | 40 / 7 passed |
| Python harness tests | 180 passed |
| Deep-review semantic matrix | 253/253 passed |
| Semantic round-trip matrix | 444/444 passed: 148 fixtures × O0/O1/O2 |
| Runtime/determinism cohort | 264/264 passed; 9 negative controls passed |
| Public-source cohort | 513/513 compiled and passed existing checks |
| Comparative quality gate | 777/777 accepted; 4 exact reviewed transitions |
| Benchmark fixture quality gate | 33/33 accepted; all outputs identical |
| Real unwind-enabled workerd recovery | Passed on CI |

These matrices overlap; their counts should not be added as unique scripts.
The local environment lacks `/proc/self/exe`, blocking 12 pre-existing CLI
cache/process-identity tests. They all pass as part of the 62-test CLI suite
on the branch's normal Linux CI. Rust checks and real Worker recovery passed
for engine commit `4642b2c` in
[run 37190640727](https://github.com/Kiet1308/Tovek/actions/runs/37190640727).
The complete fixture audit and subsequent evidence commits are tracked by the
PR's latest checks.

Of 777 comparative cases, 773 sources are byte-identical. Three ClientComm
profiles only rename an unread fixed parameter from `p3` to `_`; the binding
canonical AST matches and the recompiled outputs are byte-identical at the
actual g1 profile. Gamepad O2 moves a single-use, nonrecursive callback into its
sole call argument. Independent source and register audits verify its captures,
argument arity, opcode/effect order, and CLOSE event. Its bounded proof oracle
remains unknown.

The gate's final original-input proof counts are 282 `proved`, 388 `unknown`,
and 107 `different`, exactly as before. `different` here means the bounded model
did not match that compiler transformation, not a newly observed runtime failure.
The four reviews do not upgrade those proof labels. Hash-bound justifications
are in [reviewed_changes.json](../quality/reviewed_changes.json), with independent
transition evidence in [transition_review.json](../quality/transition_review.json).
Compilation, runtime, input identity, and coverage failures cannot be waived.

## Evidence and reproduction

- [results.json](evidence/results.json): every input/thread summary, cohort/group
  aggregates, build hashes, protocol, and archive checksum.
- [samples.json.gz](evidence/samples.json.gz): complete comparison records,
  including all 183,456 observations and process metadata. Repeated identity
  fields use a lossless row catalog to keep the archive small; every reconstructed
  sample was checked against the original dictionary. Embedded local paths are
  provenance from this run, not portable dependencies.
- [public_manifest.json](evidence/public_manifest.json) and
  [fixture_manifest.json](evidence/fixture_manifest.json): frozen inputs, decode
  key, naming context, and group membership.
- [quality_gate.json](evidence/quality_gate.json) and
  [fixture_quality_gate.json](evidence/fixture_quality_gate.json): exact accepted
  comparisons and proof-status limitations.
- [semantic_validation.json](evidence/semantic_validation.json),
  [vm_validation.json](evidence/vm_validation.json), and
  [release_binaries.json](evidence/release_binaries.json): tool identities,
  validation commands, and release binary hashes.

Use [cold_script_protocol.md](cold_script_protocol.md) for the complete command
contract. Build the baseline in a separate worktree, copying only the current
`benchmark_single.rs` harness into its examples directory. Generate the public
raw inputs with `public_source_roundtrip.py` using the committed source manifest
and pinned Luau compiler. Then point `benchmark_single.py run` at the before and
after release examples, the matching frozen manifest, and generated input root:

```sh
python3 scripts/benchmark_single.py run \
  --before /absolute/baseline-target/release/examples/benchmark_single \
  --after /absolute/candidate-target/release/examples/benchmark_single \
  --manifest docs/performance/evidence/public_manifest.json \
  --input-root /absolute/public-raw-input-root \
  --threads 1 4 --rounds 20 --process-rounds 4 --option-bits 8 \
  --cpus 0 1 2 3 --keep /absolute/fresh-runs \
  --report /absolute/comparison.json
```

The CPU list must be available on the reproduction host. Collect quality
evidence separately and attach it with `benchmark_single.py review`; changed
outputs stay pending until their exact transition is accepted. Cache-hit rates,
batch deduplication, profiling builds, and microbenchmark operation counts are
excluded from the latency result.

To inspect all archived sample dictionaries without the original workspace:

```python
import gzip, json
with gzip.open("docs/performance/evidence/samples.json.gz", "rt") as stream:
    archive = json.load(stream)
for cohort in archive["cohorts"]:
    comparison = cohort["comparison"]
    comparison["rows"] = [
        dict(cohort["row_catalog"][values[0]],
             **dict(zip(cohort["observation_fields"], values[1:])))
        for values in cohort["observations"]
    ]
    print(cohort["name"], len(comparison["rows"]), comparison["status"])
```

## What remains for a 10× target

The measured outcome rejects the hypothesis that this set of bounded caches,
indexes, and limited planning changes is sufficient for a broad 10× gain on
ordinary cold inputs. It does not identify one dominant remaining bottleneck:
no phase timing or allocation attribution was used as evidence in this run.

A complete ValueIR design would need stable numeric value/block/function IDs,
explicit effects and tuple/capture semantics, revision-aware analysis ownership,
and a general structure plan that delays AST materialization until the chosen
result is known. It would also need safe retry snapshots and differential
validation of each migrated operation. Those changes cannot be described as
already shipped in this branch.

The next architectural experiment should select the largest measured costs on
the intended production input distribution, attribute their work in a separate
profile run, migrate one complete path, and require a clear uncached latency
gain with this same quality gate before widening the rewrite. The user's
private production corpus was not available in this run, so these results do
not establish a production-wide speedup or a production-wide quality guarantee.
