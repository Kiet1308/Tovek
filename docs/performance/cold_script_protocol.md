# Fresh-script latency and output-quality contract

This protocol targets one new, uncached script relative to the current frozen
baseline. It permits different source text when independent quality evidence
supports the change. It does not promise a particular speedup.

## What a sample measures

`luau-lifter/examples/benchmark_single.rs` calls
`try_decompile_bytecode_with_options` once per sample, with one script active in
an existing Rayon pool. Every invocation runs the pipeline; it does not call the
batch API, folder deduplication or an artifact cache. Report thread counts
separately: one-thread latency and latency with the production pool size are
different measurements.

The timer includes pool dispatch and the complete single-script API, including
its internal retries. It excludes input loading/base64 decoding, creating the
pool, source hashing, writing saved sources and caller-side result disposal.
Returned source bytes are hashed exactly. `cli_output_sha256` additionally
hashes one trailing newline, for matching existing CLI quality reports.

`first_for_file` labels round zero for each input in each process.
`first_in_process` labels exactly one invocation in each process. Repeated calls
have warm instruction/data/allocator caches but no cached decompilation result.
This is not an operating-system cold-cache experiment. A first request after
starting a server should be reported separately from the uncached steady-state
request latency.

The ordinary API does not expose fallback or retry counters. Those report fields
are `null` with an explicit unavailable status, not zero. Run the existing
profiling tools separately if those counts are required. Instrumented feature
builds and debug builds cannot supply accepted performance samples. Diagnostics
must not run concurrently with a timing cohort.

## Freeze inputs and builds before timing

Build both revisions using the same compiler, target, optimization profile,
features, allocator and flags. Preserve the binaries and record their revision
IDs. For a revision predating the benchmark, copy the exact same benchmark
source into its isolated checkout before building; that changes the harness,
not the engine. The JSON report pins executable and input hashes. Self-reported
binary metadata cannot establish that two LTO/toolchain settings were identical;
retain the build commands/toolchain identity as separate evidence.

The manifest schema is:

```json
{
  "schema_version": 1,
  "decode_key": 1,
  "scripts": [
    {
      "path": "module.luaubc",
      "encoding": "raw",
      "input_sha256": "<64 lowercase hex digits>",
      "script_name": "module.luaubc",
      "groups": ["all", "small"],
      "expected_output_sha256": "<optional exact API-source hash>"
    }
  ]
}
```

`encoding` can be `raw` or `base64` per file. Base64 inputs may contain the
existing `--` saved-bytecode header lines. The decode key is shared by a
manifest; put key-1 and key-203 fixtures in different manifests. Paths must be
relative and stay within the input root. Explicit input hashes prevent corpus
drift. Optional expected output hashes are checked only when
`--require-expected-hashes` is explicitly used for a baseline lock; a candidate
is not rejected merely because its text changes.

For the repository fixture cohort:

```sh
python3 scripts/benchmark_single.py pin \
  --input-root luau-lifter/tests/fixtures --pattern '*.luaubc' \
  --encoding raw --decode-key 1 --manifest /work/raw-manifest.json

python3 scripts/benchmark_single.py pin \
  --input-root docs/failure_fixtures/residual_control_flow --pattern '*.lua' \
  --encoding base64 --decode-key 203 --manifest /work/residual-manifest.json
```

These fixtures are a regression cohort, not a representative sample of users'
scripts. Include every predeclared fixture, and report malformed/compiler-error
inputs as a separate robustness cohort; never remove an input because the
candidate is slow or fails. Compiler profiles of one source are related samples,
not independent programs. A private production corpus, when available, needs a
separate frozen source-family split and the same protocol.

For source-known scenarios, compile the same frozen source set at the declared
optimization/debug/bytecode profiles before creating a raw-input manifest.
Keep source-family identity in the reporting denominator. Do not average
unrelated strata such as tiny commands, one large function and many-prototype
modules into an unexplained headline.

## Run matched A/B processes

```sh
python3 scripts/benchmark_single.py run \
  --before /work/baseline/benchmark_single --after /work/candidate/benchmark_single \
  --before-revision BASE_SHA --after-revision CANDIDATE_SHA \
  --manifest /work/raw-manifest.json --input-root luau-lifter/tests/fixtures \
  --threads 1 4 --rounds 20 --process-rounds 4 --option-bits 8 \
  --keep /work/single-runs --report /work/single-comparison.json
```

On Linux, add `--cpus N` for an explicitly chosen equivalent core set. On other
hosts pin the parent process with the platform tool. The runner inherits its
affinity into both variants, alternates AB/BA order and rotates the first input
between process pairs. Each process report, log and first-process source tree
is retained. The minimal execution environment may lack `/proc/self/exe`; the
Rust harness then accepts only canonical absolute `argv[0]` for executable
identity. The runner launches that absolute path and independently checks its
hash.

All attempts stay in the report. Missing/duplicate rows, changed inputs,
instrumentation, wrong options, failures and nondeterministic output prevent an
accepted gain. First samples and repeated samples have separate summaries.
Small-sample p95 is only an observed order statistic, not an established service
tail. Compare the same current baseline and candidate in one cohort; never
multiply ratios from earlier optimization rounds.

## Evaluate changed source outside the timer

For arbitrary saved-bytecode fixtures, `capture` evaluates the source trees
saved by the benchmark's first independent process:

```sh
python3 scripts/quality_gate.py capture \
  --manifest /work/raw-manifest.json --input-root luau-lifter/tests/fixtures \
  --output-root /work/single-runs/RUN/0-1-before-sources \
  --compiler /work/luau-compile --ast /work/luau-ast \
  --report /work/before-quality.json

python3 scripts/quality_gate.py capture \
  --manifest /work/raw-manifest.json --input-root luau-lifter/tests/fixtures \
  --output-root /work/single-runs/RUN/0-1-after-sources \
  --compiler /work/luau-compile --ast /work/luau-ast \
  --report /work/after-quality.json
```

Capture recompiles source, runs the independent bounded bytecode/dataflow
oracles, and collects presentation metrics. It has no runtime driver for
arbitrary bytecode and does not certify unknown cases. Use the existing VM,
binding/capture, effect-order, negative/mutant and generated controls as well.

For the stronger source-known runtime suite, run the **same current harness and
fixtures** against both frozen CLI binaries using the same pinned compiler and
VM. Freeze their reports with identical labels:

```sh
python3 scripts/quality_gate.py freeze \
  --input runtime-v9=/work/before-roadmap.json \
  --input public-v9=/work/before-public.json --report /work/before-quality.json
python3 scripts/quality_gate.py freeze \
  --input runtime-v9=/work/after-roadmap.json \
  --input public-v9=/work/after-public.json --report /work/after-quality.json
```

Roadmap and public-source runs need `--ast`; generated runs now optionally
accept `--ast` too. Without measured presentation data the presentation gate
refuses to pass. Reports record raw decoded input hashes and successful
recompilation explicitly, so an approval binds to the actual timed input.

Compare snapshots:

```sh
python3 scripts/quality_gate.py compare \
  --before /work/before-quality.json --after /work/after-quality.json \
  --allowlist docs/quality/reviewed_changes.json \
  --metric discard_locals --metric require_field_relays \
  --metric generated_single_use_AstExprLocal \
  --report /work/quality-gate.json
```

The gate enforces coverage, input/compiler/parser context, successful output and
compilation, runtime observations, proof transitions and each selected
presentation metric **per case**. Aggregate improvements cannot cancel a
regression. `--minimum-metric exact_names` or `alignment_coverage` can additionally
gate measured source-fidelity values for suitable source-known cohorts.

Unknown stays unknown. Changed output without a proof needs an explicit review,
even when finite VM vectors pass. Losing a prior proof is a separate review
decision. New proved output can pass without preserving exact text, provided
the other gates pass. Compilation alone does not prove semantic equivalence;
opcode-multiset similarity alone does not preserve effects or capture lifetime.

## Reviewed exceptions and CI

`docs/quality/reviewed_changes.json` starts empty. Each exception is the exact
failure object plus `reason`: `case_id`, `check`, `before`, `after`, both output
hashes, and a concrete explanation of the independent evidence reviewed.
Wildcards and status/compile/runtime/input failures cannot be waived. Duplicate,
stale or hash-mismatched reviews fail. An exception acknowledges a limitation;
it never turns `unknown` into `proved`. Do not auto-generate accepted exceptions
from all reported failures.

Reviews belong to one before/after transition. When the comparison base advances,
remove reviews that no longer match a current violation; retaining them fails the
gate instead of carrying an old approval forward silently.

CI should build the PR base in a separate worktree, run the same pinned fixture
runner against both binaries, then run `freeze` and `compare` above. Comparing a
binary with itself tests gate plumbing but cannot establish non-regression for
a PR. Keep the current Rust, VM, bytecode, compact, provenance, cache and Wasm
gates. The existing Python unittest discovery also runs the new gate/harness
tests.

The fixture workflow compares the PR base (or previous main commit on pushes)
against the candidate with the current runners and pinned tools. Its `runtime-v9`
label covers 44 fixtures at six optimization/debug profiles, and `public-v9`
covers 171 sources at three optimization levels: 777 rows in total. The existing
v12/v14 runtime checks remain separate; this comparative presentation gate does
not claim to cover those additional bytecode profiles.

Readability needs an additional blinded review on source-family holdouts. Check
control structure, naming, declaration locality, captures/effects and uncertain
reconstruction annotations. Lower line count or fewer temporaries is not by
itself better output; some temporaries preserve observable evaluation order.

After a quality gate passes, attach it to the original cohort without rerunning
timing:

```sh
python3 scripts/benchmark_single.py review \
  --comparison /work/single-comparison.json --quality-report /work/quality-gate.json \
  --report /work/single-reviewed.json
```

This rechecks archived raw-report hashes and sample coverage before binding the
exact before/after output and decoded-input hashes. Until then changed-text
ratios remain observations with `measured_quality_pending`, not accepted gains.

## Compact bytecode-baseline summaries

`scripts/refresh_bytecode_baseline.py` checks that summary counts match the
immutable per-file threshold rows. `--write` splices only the top-level summary;
all bytes outside it, including thresholds and provenance, remain unchanged.
Compact rows retain original prototype and non-equivalent counts, but not all
legacy tier splits, so the regenerated summary does not invent an exact/equiv
breakdown or ratio.

```sh
python3 scripts/refresh_bytecode_baseline.py \
  docs/bytecode_roundtrip/baseline_semantic_roundtrip.json \
  docs/bytecode_roundtrip/baseline_residual_control_flow.json
```
