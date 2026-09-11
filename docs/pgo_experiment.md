# Isolated PGO experiment

This experiment builds the reviewed `9b7d1a134edb3dcf3faf545eb867d5ffb4fc3a41` Rust source with and without LLVM profile-guided optimization. Normal Cargo/release settings and shipped executables are unchanged. Both builds explicitly retain `-Cpanic=unwind`. AI remains disabled; compiler execution counters do not involve an AI model or service. Raw `.profraw`/`.profdata`, compiled binaries and private source remain local.

## Frozen data and training

`scripts/pgo_corpus.py` freezes inputs before instrumentation or evaluation. Training contains 648 development configurations: 468 public library profiles and 180 runtime fixtures. The Rodux repository holdout retains 42 of 45 profiles after excluding three exact training execution images. The external private workload retains 3,922 of 3,978 files after excluding 42 empty inputs and 14 exact training execution images. Known source hashes cannot cross the public train/holdout boundary either.

Execution-image matching retains registers, operands, constants, types, names and prototype topology, normalizing opcode encoding and debug line/local data with the existing version-specific fingerprint. The private wrapper trailer allowance is explicitly 24 bytes. This rejects known exact leakage; it does not prove that the private workload has independent source ancestry. It is an external workload, not a certified independent source-family holdout.

Windows CLI instrumentation produced no profile files on its `process::exit` path. Before collecting any usable counters, the plan was amended to train through the existing `benchmark_api` example on both platforms. Its normal `main` return writes the LLVM profiles without changing the production CLI. At each of 1 and 16 threads, the trainer performs a first call followed by two repeated calls: **six complete training passes in two processes**, not four. Every call checks all input and source hashes against a locked, uninstrumented CLI run. Neither evaluation workload executes in the instrumented trainer.

Each platform builds and trains its own profile in an isolated target directory, using Rust `nightly-2024-12-15` and matching `llvm-tools`. Profile merging is non-sparse; the optimized build enables missing-function warnings. The workflow follows the [Rust PGO documentation](https://doc.rust-lang.org/rustc/profile-guided-optimization.html). Profile files use process/module identifiers supported by [LLVM's profile runtime](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html).

The Windows and Linux corpus copies have identical portable path/content hashes. Windows native path ordering is case-insensitive while POSIX ordering is case-sensitive; native benchmark tree hashes therefore cannot be compared directly across hosts. The original Windows manifest remains frozen. The Linux copy records a portable UTF-8-relative-path ordering and the original manifest hash. A preliminary Linux hash check stopped before training when it detected this ordering difference.

## Acceptance policy and scope

The manifest locks seven interleaved measured rounds at 1 and 16 threads, with one warm-up per binary. Each primary-workload thread count must reduce median wall time by at least 10%, with at most 5% regression in nearest-rank p95 and median per-process peak RSS. Each repository-holdout thread count permits at most 5% median regression. Maximum RSS and all secondary-workload tail/memory samples are also reported. Missing primary RSS fails its gate. With seven rounds, nearest-rank p95 is the maximum observation.

`scripts/pgo_evaluate.py` recomputes statistics from the complete sample inventory, validates input/build/training/quality identities, and requires the same source tree across both binaries and thread counts. A failed performance gate remains a valid negative experimental result. No result automatically promotes PGO to the default build.

Both native Windows and Debian 13.2 under WSL2 run on the same Core Ultra 9 275HX machine with 24 logical processors. WSL uses native Linux executables, the GNU target and an ext4 corpus/output tree. This is a second deployment environment on the same physical machine, not a second CPU or a general production-Linux claim. Platform timings are measured separately, without concurrent builds or another timed benchmark. Input filesystem caches are warm; a true OS-cold-cache experiment remains open.

Windows samples `PeakWorkingSetSize`; Linux samples `/proc/PID/status` `VmHWM`, converting KiB to bytes. Sampling is every 10 ms, included in CLI wall time, and can miss a final unsampled peak. Linux RSS describes the child process, not total WSL VM memory or host filesystem cache. Separate 64 MiB resident-allocation controls check units and process scope.

## Quality checks

Each platform checks all 3,964 selected evaluation files at 1/16 threads against its uninstrumented build. Full source bytes agree, as do full detailed provenance sidecars at 16 threads. The sidecar comparison validates actual invocation and executable hashes, then excludes only the expected top-level command/tool identity fields. Existing facts and unknown/different proof statuses are retained.

A malformed version-99 input exercises the actual deserializer panic. Both builds must return batch failure exit code 1 while still producing the exact clean output for a valid item, at 1 and 16 threads. The serial case also checks state after a preceding panic. Portable source trees and valid-after-panic output agree across platforms. This experiment adds no new semantic transformation; the reviewed source's 981 Rust test executions and runtime/compiler-witness results remain its baseline. It does not claim that instrumented release builds reran the Rust unit suite.

## Measured results

All quality configurations pass. Windows misses the 10% primary median target at both thread counts. Linux meets every frozen performance gate in this run. PGO remains experimental on both platforms; there is no default promotion or promised speedup on another workload.

| Environment / workload | Threads | Baseline median (s) | PGO median (s) | Change |
|---|---:|---:|---:|---:|
| Windows / private | 1 | 24.073524 | 21.900740 | -9.03% |
| Windows / private | 16 | 1.832854 | 1.736282 | -5.27% |
| Linux WSL2 / private | 1 | 16.049884 | 14.362121 | -10.52% |
| Linux WSL2 / private | 16 | 1.491669 | 1.266776 | -15.08% |
| Windows / Rodux holdout | 1 | 0.078056 | 0.077173 | -1.13% |
| Windows / Rodux holdout | 16 | 0.035364 | 0.035267 | -0.28% |
| Linux WSL2 / Rodux holdout | 1 | 0.061920 | 0.051662 | -16.57% |
| Linux WSL2 / Rodux holdout | 16 | 0.021193 | 0.021404 | +0.99% |

Private p95 changes are -8.35%/-3.71% on Windows and -9.00%/-9.35% on Linux at 1/16 threads. Median peak RSS changes are -5.83%/-1.91% and -2.45%/+1.93%, respectively. Maximum private RSS at 16 threads increases 3.26% on Windows and 2.90% on Linux. The Windows holdout's 16-thread maximum/p95 rises from 0.035901 to 0.052452 s (+46.10%); this secondary tail statistic was not a frozen gate and is retained as a limitation. These short samples are close to process startup and the monitor's polling interval.

The optimized compilers emit 763 Windows and 750 Linux missing-function warnings, with zero reported hash-mismatch/out-of-date warnings. Untrained CLI/cache paths remain visible in the logs; these warning counts are not execution coverage. LLVM's profile summaries list 6,998 and 6,732 total functions. Exact platform source hashes differ because of source line endings/path ordering; all 140 selected Rust/build files match after CRLF-to-LF normalization. Profiles remain specific to their original platform/build.

One incomplete Windows private benchmark ended before all seven rounds and produced no final report. Its partial log was retained locally and excluded before running a fresh complete measurement. All completed samples, including slower samples, remain in the accepted reports. The Python suite passes 111 tests, including evaluation corruption/leakage controls. Both resident-memory controls pass. [Acceptance inventory and all 20 reports](roadmap_v2_acceptance/pgo_validation.json).

## Reproduction

Use an explicit native toolchain with `llvm-tools`, a reviewed source checkout or `git archive` of the commit, and the public/runtime reports whose retained `.luaubc` files remain available. Do not archive an entire workspace or the `out` directory. The private input directory is a local prerequisite for reproducing that workload; the committed reports contain inventories and hashes, not its source or bytecode.

```text
python scripts/pgo_corpus.py --public-report PUBLIC.json --runtime-report RUNTIME.json --private PRIVATE_INPUT --keep LOCAL_PGO --report corpus.json
python scripts/pgo_prepare.py --source SOURCE --corpus corpus.json --keep LOCAL_PGO --report prepared.json --target NATIVE_TARGET --commit REVIEWED_COMMIT
python scripts/pgo_finish.py collect --prepared prepared.json --source SOURCE --corpus corpus.json --api-manifest WORK/training-api.json
python scripts/pgo_finish.py build --prepared prepared.json --source SOURCE --corpus corpus.json --api-manifest WORK/training-api.json
python scripts/pgo_quality.py --baseline WORK/baseline --optimized WORK/optimized --corpus corpus.json --keep WORK/quality --report quality.json
python scripts/rss_controls.py --report rss-control.json
python scripts/benchmark_v2.py --lifter baseline=WORK/baseline --lifter pgo=WORK/optimized --corpus CORPUS/holdout --key 1 --threads 1 16 --rounds 7 --keep WORK/benchmark-holdout --report holdout.json
python scripts/benchmark_v2.py --lifter baseline=WORK/baseline --lifter pgo=WORK/optimized --corpus CORPUS/private --key 203 --threads 1 16 --rounds 7 --keep WORK/benchmark-private --report private.json
python scripts/pgo_evaluate.py --corpus corpus.json --build WORK/optimized.json --quality quality.json --holdout holdout.json --private private.json --report evaluation.json
```

`WORK` is the new directory in `prepared.json`; `CORPUS` is the directory in `corpus.json`. On Windows, use `.exe` filenames and `x86_64-pc-windows-msvc`; on Linux, use `x86_64-unknown-linux-gnu`. The profile/target directory must have no whitespace because `RUSTFLAGS` splits on whitespace. The preparation wrapper uses the same baseline/API build commands as the recorded manual preparation; its config additionally locks the source and manifest hashes. A fresh directory is required to collect profiles again. The scripts do not download, run or publish AI models, and do not publish compiler profiles.
