# Bytecode v12 readiness and performance review

Reviewed 2026-09-19 against source `c8eb88995e3c4619d37526198f5a65e6e5bc413e` and the unchanged F8 native executable. This is an audit, not a claim that the findings below have been fixed.

## Result

The native x64 decompiler handles the v12 serialization used by the complete user samples and the tested compiler-generated fixtures. It is **not correct for every accepted opcode**: a runtime `CMPPROTO` guard has a reproduced semantic mismatch. A separate portability issue affects large cost-model varints on 32-bit targets, and the independent Python comparison tooling still rejects v12 directly.

The large sample's slowdown is real. The measured bottleneck is out-of-SSA processing, not the v12 reader. Further optimization remains separate from this audit.

## Same-machine comparison with v0.9 beta

Tools:

- Beta release tag: `v0.9.0-beta`, source `27422b9f8e0aeca0e61db55992930f17a98f4099`, executable SHA-256 `eb39366eb2428bbdda725376d6b205604d06296e4cc21b0d97eb1612676a6a87`.
- V2 executable SHA-256 `c78895cd804593dfcf35c7bd711af87bf1ea7a4730898a75b4262dcb822bfa77`, matching final F8 acceptance.
- Sample bytecode SHA-256 `27af9e72520901ac54a27f9d2d32db86b35f2741e9fcb8a3d9961760e81f6799`: 478,363 decoded bytes, 228 prototypes, 48,158 instructions / 56,430 words, including 3,882 CALLFB instructions and no CMPPROTO. Prototype bodies contain no unknown extensions. An uninterpreted 24-byte trailer follows the main ID.

Beta rejects the original v12 input before decompilation. Its approximately 0.033-second error response is **not a performance result**.

For a useful diagnostic comparison, an additional v11 serialization view removes the v12 prototype-size prefixes and changes the version header. This particular sample has no cost-model fields. All 228 prototype bodies (instructions, constants, type/debug information and feedback), the main ID and the trailer are copied byte-for-byte. No source is recompiled to prepare the input. This does not add native v12 support to beta.

| Run | Beta, v11 metadata view | V2, original v12 |
|---|---:|---:|
| Warm-up, excluded | 0.838529 s | 18.120992 s |
| Measured 1 | 0.829476 s | 18.180171 s |
| Measured 2 | 0.817479 s | 18.181588 s |
| Measured 3 | 0.793151 s | 18.374725 s |
| Median | **0.817479 s** | **18.181588 s** |

Process wall time includes launch, file read, decompilation, output write and process exit. Runs are sequential, with alternating order, the same machine/input content, default single-file options and decode key 203. Profiling is disabled during these measurements. No compilation or CPU-heavy audit workload ran concurrently. Three runs are a small local sample, not a general benchmark or a claim about all workloads.

On this sample V2 takes **22.24×** as long. V2 also processes the v11 metadata view in **18.263160 s**, emitting byte-identical source to the original v12 run. Thus changing the serialization version does not remove the slowdown. The earlier one-shot result of 15.4 s was not a repeated benchmark; timing varies between runs, while all recorded V2 output hashes match.

Both versions' output is deterministic across their three measured runs. V2's default-mode output also matches the separately tested strict-mode output exactly.

## Where the time goes

A separate instrumented V2 run takes 15.77 s wall time and emits the same output hash. Do not combine or compare its instrumentation timings as if they were uninstrumented benchmark samples.

| Instrumented phase | Observed time |
|---|---:|
| Deserialization and initial lifting, whole file | 0.02331 s |
| Parallel per-function phase, wall time | 15.22544 s |
| Out-of-SSA for prototype 220 alone | **14.80330 s** |
| Whole decompile span | 15.72343 s |

Prototype 220 has 9,150 instructions. Worker spans overlap, so summing all worker times would overcount process wall time. This one function explains most of the elapsed time without such a sum.

The R2 change in `cfg/src/ssa/destruct.rs::try_coalesce_copy_by_value` (commit `a3a6444`) checks every member of one congruence class against every member of the other before checking whether the classes are already equal. Each compatibility check can lock two locals. This cross-product scan is a strong code-level suspect within the measured out-of-SSA hotspot. **No subphase instrumentation or ablation has yet proved how much of the 14.8 seconds belongs to that scan**, so the audit does not attribute an exact speedup to a proposed replacement. Optimizing it must retain the source-binding constraints.

## Output delivery and quality limits

Both outputs are saved locally alongside the user's samples:

- `output/V2/test3.luau`: actual v12 decompilation, 564,140 bytes / 14,041 lines.
- `output/beta-v0.9/test3.compat-v11.luau`: beta decompilation of the documented v11 serialization view, 534,169 bytes / 11,962 lines.
- `output/benchmark.json` and `output/profile-summary.json`: measured evidence.

Both parse and compile successfully at O0/O1/O2. V2 contains no `controlFlowState` occurrences; beta contains 60 such occurrences. V2 has three lines over 180 characters versus beta's eight. V2 is longer overall; line count and successful compilation are not proofs of semantic quality. The complete user script was not executed in Roblox, and neither output has been established as fully equivalent to that script.

Private bytecode, source, outputs and raw profiling data remain local and are not committed.

## v12 coverage actually exercised

The v12 definition adds per-prototype serialized size and an inlinable-prototype cost model. Its basic instruction set carries forward the v11 feedback opcodes. Reader behavior was checked against the [official bytecode definition](https://github.com/luau-lang/luau/blob/master/Common/include/Luau/Bytecode.h) and [VM loader](https://github.com/luau-lang/luau/blob/master/VM/src/lvmload.cpp). Local compiler and VM source are pinned at `c2ec0d4e5ca50796ba174a7565298f59aa572268`.

- Existing v10/v11/v12/v13 and prototype-graph test group: **23/23 passed**. This group includes the two existing v12-specific tests.
- 15 self-contained source fixtures at O0/O1/O2, cost-model flag enabled: **45/45 passed**.
- The same 15 sources at O0/O1/O2 with `-g2 -t1`, cost model and call feedback enabled: **45/45 passed**. The resulting 45 chunks contain 219 prototypes, 133 feedback slots / CALLFB instructions and 115 cost-bearing prototypes; no compiler warnings. Every input and recompiled output header is checked to be v12. Both original source and decompiled source run with exit 0 and equal stdout in the standalone VM. These are 90 profiles of 15 sources, not 90 independent programs.
- Additional locally generated structural cases: **26/26 passed**. Both decode keys 1 and 203; cost values through `u64::MAX` on x64; next-prototype alignment; unknown per-prototype extensions; feedback slots; trailing bytes; rejection of zero/short/oversized sizes, truncated bodies/cost, missing or invalid main ID, and unknown feedback kinds.
- A separate CMPPROTO semantic probe **fails** and is described below. It is not included in the 26 passing structural cases.

The two complete user samples decompile and recompile successfully. The incomplete sample remains blocked by missing input bytes, not by a demonstrated v12 incompatibility.

## Open findings

### 1. CMPPROTO guard semantics are wrong — confirmed runtime mismatch

`luau-lifter/src/lifter.rs` substitutes the truthiness of register A for the predicate of CMPPROTO. The actual VM checks whether A contains a Luau function with the specified runtime prototype ID; a non-function always takes the mismatch branch. Truthiness cannot represent this predicate.

Minimal generated v12 prototype, unencoded instruction stream, no constants or children:

```text
pc0: LOADNIL R0
pc1: CMPPROTO R0 D=3
pc2: AUX proto-id=0
pc3: LOADN R1 111
pc4: JUMP D=1
pc5: LOADN R1 222
pc6: RETURN R1 B=2
```

An isolated host linked to the pinned official `Luau.VM` loads and executes the original fixture: **222**. Current Tovek emits `return 111`; recompiling and executing that output in the same VM returns **111**. Both VM runs exit successfully. This is a concrete wrong-output case, not merely an AST or opcode difference.

CMPPROTO is a runtime guard inherited from v11, not a new v12 opcode, and is not normally emitted by the stock source compiler. The large user sample has zero occurrences. This limits the impact on the tested input, but does not make silently accepting it correct. The existing CMPPROTO unit test only checks that a D=0 fixture does not panic; it does not validate the guard semantics.

### 2. Cost-model reader depends on pointer width — static portability finding

The upstream loader reads cost with `readVarInt64`; `Function::parse` currently uses `leb128_usize`. This works for all tested values on the x64 binary. On wasm32 / 32-bit targets, the dependency instantiates a narrower reader and rejects sufficiently long valid 64-bit varints (for example, a cost with the 64th bit set). Since cost is metadata, it should be consumed with an explicitly 64-bit reader regardless of pointer width. A wasm/32-bit executable was not run in this audit.

### 3. Independent validation tooling still stops at v11

`scripts/bytecode_roundtrip.py::parse_chunk` rejects versions above 11. Consequently existing bytecode/dataflow corpus checks cannot directly certify arbitrary v12 inputs. This audit uses an explicit independent field walk for structure and controlled v11 metadata views where applicable; it does not represent these as full v12 support in the shipped oracle. Update the independent parser and its regression tests before extending those corpus claims.

### 4. Documentation and performance need follow-up

README still advertises bytecode coverage through v11, while the Rust reader accepts through v13. Documentation needs to distinguish implemented serialization support from semantic coverage and known runtime-only opcode limitations. The out-of-SSA performance regression on this sample also remains unresolved.

## Follow-up work

- [ ] Replace the inaccurate CMPPROTO lowering with proven semantics, or explicitly reject unsupported guards before emitting output; add the VM-backed counterexample as a regression.
- [ ] Read v12 cost metadata as u64 and cover wide values on a 32-bit target.
- [ ] Extend the independent Python reader and comparison tests for v12 boundaries and cost fields.
- [ ] Profile out-of-SSA subphases and optimize binding compatibility without removing its correctness constraints; repeat the exact-hash benchmark and semantic gates.
- [ ] Update compatibility documentation after the above fixes and distinguish standard compiler output from runtime-mutated bytecode.

These items are findings of this review. No decompiler behavior was changed while measuring them.
