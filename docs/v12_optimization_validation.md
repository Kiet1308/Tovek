# v12 correctness and performance acceptance

Validated on 2026-09-19. Native executable SHA-256: `216f2b833e6927b4708c7bed341be3a39d1c8883adc26f0375411a26acd2c6e8`. The original [readiness audit](v12_readiness_review.md) retains the pre-fix failures and measurements.

## Changes

- Reject runtime `CMPPROTO` with prototype/PC diagnostics before lifting in strict and permissive modes. Its predicate is a runtime Luau-closure prototype identity, not register truthiness. The original nil-guard bytecode returns 222 in the pinned official VM; the old decompiler emitted 111. The new decompiler returns an error and no source. Regression cases cover v11/v12/v13, nil/false/number operands and D=0/1/3.
- Read inlinable v12 cost metadata with `leb128_u64`, independent of pointer width. Costs through `u64::MAX`, following-prototype alignment and extension skipping execute successfully on both native x64 and wasm32.
- Extend the independent Python parser through v12: bounded prototype bodies, u64 cost, feedback/AUX alignment, retained opaque extension/trailer bytes, explicit varint bounds and invalid main/child rejection.
- Replace the out-of-SSA binding cross-product with cached exact class summaries. Recorded debug-local sequences, wildcard evidence, parameter separation and the same-local identity exception are retained. Mandatory classes with internal conflicts still fail the same checks, including self-comparisons. Every insert, replacement and class extension invalidates the summary. Local metadata is stable during this phase; subsequent local-map application still inherits it normally.
- Add opt-in SSA subphase telemetry. Expand the existing semantic runner with `--bytecode-version 12`, checking genuine input/recompiled headers and enabling feedback/type metadata. The pinned compiler's `LuauEmitCallFeedback` requires the separate VM flag `LuauCallFeedback` when executing that emitted code; the runner now sets both appropriately.

## Performance

Same machine, sequential interleaved single-file CLI runs, one warm-up per executable and five measured rounds each. Profiling is disabled; hashing is outside the timed interval. Default thread count, identical v12 bytes and strict output mode for V2 before/after. Windows peak working set is sampled at 5 ms; monitoring overhead is included. No compiler or CPU-heavy audit workload ran concurrently.

| Executable | Median | Min–max | Median peak working set |
|---|---:|---:|---:|
| V2 before | 15.295765 s | 15.273137–15.334267 s | 172.94 MiB |
| V2 after | **1.017918 s** | 1.007424–1.039378 s | 164.50 MiB |
| Beta v0.9, metadata-only v11 view | 0.787879 s | 0.773457–0.795445 s | 143.96 MiB |

V2 improves **15.03×**, reducing elapsed time by **93.35%** and sampled peak working set by about **4.9%**. The earlier audit's 18.182 s baseline was a different session; it is not substituted into this paired measurement. V2 remains about 29% slower than beta on this diagnostic comparison while retaining its current output. Beta cannot consume native v12: its comparison input only changes the version header and removes size prefixes; every prototype body is preserved. This sample contains no cost fields requiring removal.

Separate instrumented runs identify prototype 220's copy-coalescing phase: **15.312641 s → 0.022286 s**. Its complete out-of-SSA phase changes from 15.460 s to 0.161 s. Remaining time is spread across liveness, source structuring and source cleanup; none of these correctness/readability passes is disabled. Worker spans overlap and must not be summed as wall time.

All five V2 outputs retain SHA-256 `207f8d8e894cc77004e896a074f8b79b7c21d4bb6f3b624d3717cad99cfd9e71` (564,140 bytes, 14,041 lines). Beta's output is different; comparing its latency does not establish equivalent reconstruction quality.

## Validation

- Workspace Rust tests: 1,053 primary tests pass; an additional cache-invalidation test passes after being added (1,054 distinct primary tests total). One subprocess test also repeats an existing test. AST's 761 tests are included in the workspace count.
- Python: 131 tests pass.
- Aggregate compatibility oracle: 24,576 deterministic pairs of classes agree with the former pairwise predicate, including overlapping identities, roles, unknown evidence and conflicting classes. Separate tests cover multiple debug origins and cache invalidation on same-size replacement/merge.
- Genuine v12: **246/246** compiler/runtime profiles from 41 fixtures at O0/O1/O2 and g1/g2, with type metadata and call feedback; **9/9** controls pass. Inputs and rebuilt output headers are v12. Source/output observations match locked expectations, and source output is deterministic at 1/4 threads. The source-based runtime harness executes compiled source/output; it does not execute every original serialized input directly.
- Structural cases: **27/27**, including both decode keys, wide costs, extensions, feedback, trailers, malformed sizes/cost/main and the now-rejected CMPPROTO counterexample. The isolated original-bytecode VM probe still returns 222.
- Executed wasm32 reader: **6/6** cost/alignment cases, pointer width asserted as 32. The probe includes the production deserializer/opcode modules and runs them in Node, rather than substituting a separate parser.
- Private corpus: **3,978/3,978** output paths unchanged byte-for-byte (3,936 decompiled, 42 empty inputs skipped, zero failures).
- Public corpus: **513/513** existing compiled inputs replayed with the same script-name context, all output hashes unchanged. These equality gates preserve earlier parser/compiler acceptance; they are not new whole-game runtime proofs.
- Both complete user samples parse and compile at O0/O1/O2. Their output bytes remain unchanged.

Core reproduction commands (substitute local executable paths):

```text
cargo +nightly-2024-12-15 test --workspace
python -m unittest discover -s scripts -p test_*.py
python scripts/check_v12_wasm.py --keep out/v12-wasm
python scripts/roadmap_v2.py --compiler <luau-compile> --luau <luau> --ast <luau-ast> --lifter <tovek> --bytecode-version 12 --determinism --report out/v12-runtime.json --keep out/v12-runtime
```

Compiler/VM source is pinned at `c2ec0d4e5ca50796ba174a7565298f59aa572268`. Private benchmark inputs and raw results are intentionally local; their hashes, aggregate results and validation report are sufficient to identify this acceptance run without publishing source.

## Boundaries

The complete v12 samples work; this is not a proof of all possible bytecode or full Roblox-game behavior. Runtime-mutated CMPPROTO remains explicitly unsupported. v13 has serialization/targeted vector coverage, while v14 is rejected. The user's second sample is still truncated by at least 866 bytes and is rejected without fabricated output. AI remains disabled, with no model assets added or uploaded.
