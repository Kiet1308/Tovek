# Roadmap V2 — implementation and acceptance record

## R7: executable/context cache and safe cross-path reuse

The optional folder cache keys decoded bytes, decode key, exact executable, every option, analysis mode, module naming context and the output-affecting shared-tail environment. Per-file path/export metadata is regenerated. It validates complete keys and payload checksums, bounds entry/serialization/disk usage, refuses unknown cache ownership, and applies smaller quotas at startup. Profiling/dump environments bypass it. Nested Rayon computations never hold cache locks; concurrent duplicate misses may recompute, with one checked publication. [Contract and command](artifact_cache.md).

All 942 primary Rust tests, one child-process repeat, 80 Python tests, 14 CLI cache controls, 144 runtime configurations, nine oracle controls, 513 public configurations, 45 legacy semantic configurations and 52 size gates pass. The CI all-targets Rust command also passes. Runtime/public source bytes, dataflow results and source-fidelity measurements are unchanged from the equality release. Every private source file is byte-identical in uncached/cold/warm modes and across benchmark thread counts. Cold/warm metadata matches the current uncached sidecar hashes on all 144 runtime and 513 public cases. Prior metadata differs only in the tracked Rust naming-rule line numbers, which advance by one after documenting the shared context projection; all input/binding/PC/name evidence remains equal. [Validation inventory](roadmap_v2_acceptance/cache_validation.json).

The private store holds 3,706 entries (16,047,452 bytes) for 3,350 bytecode hashes. Forty-two bytecode groups have multiple naming contexts, and two groups produce different source: MockUnits/MockItems/MockSkins and TowerOfGodTitle/TowerOfGodTraitlessTitle. The cache preserves both counterexamples. The cold four-thread validation run records 187 ordinary hits, 3,749 computations and 43 concurrent duplicate publications; the warm run reuses all 3,936 nonempty files. Forty-two empty inputs remain separate. Public provenance has 433 cache entries for 513 configurations and runtime provenance has 144; both warm runs hit every input.

Seven interleaved warm-filesystem CLI rounds compare the equality release, the new uncached release and populated-cache reuse. All output hashes agree. The uncached median changes 18.215 -> 18.615 s (+2.20%) at one thread and 1.686 -> 1.685 s (-0.05%) at 16 threads; this is not a default speedup claim. Relative to the current uncached release, warm cache medians are 1.371 s (-92.63%) and 0.727 s (-56.86%). Median peak RSS falls 33,394,688 -> 22,675,456 bytes and 113,754,112 -> 43,225,088 bytes. Seven-sample p95 equals the maximum; it falls from 19.965 to 1.411 s and from 1.706 to 0.765 s. The single initial cache-population sample costs 6.474 s at 16 threads, versus a 1.866 s uncached warm-up; it is recorded separately, not included among warm-hit samples. These are repeated CLI workload results, not cold OS cache, allocation or in-memory API measurements. [Full samples](roadmap_v2_acceptance/cache_benchmark.json).

## R4: preserve ordered equality metamethod arguments

SSA inline no longer swaps unknown equality operands to move an earlier definition into a comparison. Primitive nil/boolean/number/string literals remain eligible; relational inversion is unchanged and type hints are insufficient. The old release fails the three stripped-debug configurations of the new fixture, while the corrected release passes all six with bounded dataflow certificates. [Contract and examples](equality_order.md).

All 144 runtime configurations, nine controls, 513 public compile/decompile/recompile configurations, 45 legacy semantic configurations and 52 size gates pass. There are 932 primary Rust tests, one child repeat and 80 Python tests. Source/provenance maps pass independent parser checks and remain deterministic at one/four threads. All recorded mappings from the previous 138 runtime and 513 public cases are preserved.

Only two public outputs change: Roact createSpy at O1/O2. Executing its exact source/output bodies with an unused dependency stub confirms that both old outputs reverse __eq arguments; source and corrected output agree on results, trace and caught errors. Of 3,978 private outputs, 3,977 are identical. The single Summon/init change restores the comparison operand order observed in original prototype 20, PC 21. These three complete chunks remain unknown to the bounded validator; none is promoted by the focused runtime/instruction witnesses. [Per-case audit](roadmap_v2_acceptance/equality_corpus.json), [validation inventory](roadmap_v2_acceptance/equality_validation.json).

Seven interleaved warm CLI rounds are deterministic within each release. One-thread median 18.131 -> 18.329 s (+1.09%); 16-thread median 1.673 -> 1.662 s (-0.70%). Median peak RSS 33,284,096 -> 33,280,000 bytes and 114,163,712 -> 114,057,216 bytes. This is a correctness fix with measured cost, not an R7 speedup. Seven-sample p95 equals the maximum. [Benchmark](roadmap_v2_acceptance/equality_benchmark.json).

## R8: exact, license-bearing upstream source registry

The optional offline registry pins source/commit/license/compiler profiles and retains full v9 execution images, including registers, AUX bits, constant bytes, native flags and type payloads. Source-text ambiguity and low-information chunks refuse selection; other versions have no flexible fallback. Accepted source is freshly recompiled. Labelled materialization preserves license artifacts and passes another full-image recompile gate. [Contract and commands](source_registry.md).

Two fresh builds of 171 sources at five profiles yield the same registry index. All 855 independent public recompile/lookup configurations and 28 real-compiler controls pass; 80 Python tests pass. The two nontrivial Rodux source-text collisions correctly refuse selection. These public checks are registry self-consistency, not holdout generalization. Private lookup verifies seven distinct Fusion sources among 3,936 nonempty files (0.18%); 3,633 have no match and 296 fail the low-information threshold. All six historical return-nil collisions refuse selection. Of eleven previous nontrivial candidates, seven verify, two fail the stronger image comparison, and two are too small for admission.

All seven matched outputs were exported separately with commit/license/profile metadata, input hashes rechecked, and the labelled source recompiled. Default decompiler output and the Rust core are unchanged. The observed sum of private per-file lookup time is 45.772 seconds excluding registry startup; the public recompile audit plus materialization took 38.121 seconds. These are single-run costs, not benchmark improvements. This coverage supports retaining the registry as an optional extension. [Validation inventory](roadmap_v2_acceptance/registry_validation.json).

## R2/R6: emitted identifier and annotation locations

Optional binding provenance now records exact final identifier positions and annotation spans. A token links to its final IR binding ID, then to the existing bounded storage lineage and original lifted-statement PC sets. This does not claim a unique value-producing PC. Interpolation sub-rendering and display fallbacks retain explicit opaque regions. [Contract and lookup command](emission_map.md).

The pinned parser independently checks 2,271 runtime, 45,617 public and 495,224 private mapped local tokens. There are 0/251/2,306 respective opaque tokens, with zero unexplained missing tokens. Public source has three cases of one storage ID serving multiple lexical declarations (one Fusion file at three optimization levels); the private corpus has 11. These are reported separately from parser binding identity. No output-map budget is exhausted on runtime or public matrices.

All 3,978 private outputs, 513 public outputs and 138 runtime outputs are byte-identical to the preceding formatter release. Private detailed-trace source has the same tree hash as ordinary source. Prior non-lineage metadata is unchanged on runtime/public, and detailed sidecars are deterministic at one/four threads. All 930 primary Rust tests, one child repeat, 72 Python tests, 138 runtime configurations, nine controls, 45 legacy semantic configurations and 52 size gates pass. CI checks token identity on both runtime and public matrices. [Validation inventory](roadmap_v2_acceptance/emission_validation.json).

Seven interleaved warm CLI rounds preserve the same output tree across both releases and thread counts. One-thread median 24.576 -> 24.111 s (-1.89%); 16-thread median 1.838 -> 1.871 s (+1.76%). Median peak RSS 33,091,584 -> 33,288,192 bytes and 119,984,128 -> 118,710,272 bytes. This records source-only cost, not a speedup claim; metadata mode timings are separate diagnostics, and seven-sample p95 equals the maximum. [Benchmark](roadmap_v2_acceptance/emission_benchmark.json).

## R4: preserve repeated index and operator evaluation in compound assignment

The formatter no longer collapses nested index or computed-key evaluations into compound assignment without proof. Only local/literal base and key leaves qualify. The new runtime fixture fails all six configurations on the previous release and passes all six on the corrected release, with bounded dataflow moving from `different` to `proved`. All 138 runtime configurations and nine negative controls pass; six separate VM mutant controls demonstrate the observable difference. [Contract and counterexamples](formatter_effects.md).

All 513 public outputs are identical to the previous release. The private corpus changes 28 of 3,978 outputs solely by expanding compound assignments; all 32 changed table-read counts in uniquely named prototypes match the original input. Whole-chunk dataflow remains unknown for those files and is not promoted by the shape/count audit. Recorded naming mappings pass for 138 runtime and 513 public configurations, with source and metadata deterministic at one/four threads. All 927 primary Rust tests, one child repeat, 67 Python tests, 45 legacy semantic configurations and 52 size gates pass. [Validation inventory](roadmap_v2_acceptance/compound_validation.json).

Seven interleaved warm CLI rounds: one-thread median 24.002 -> 23.758 s (-1.02%), 16-thread median 1.849 -> 1.866 s (+0.91%). Median peak RSS 33,619,968 -> 33,075,200 bytes and 117,596,160 -> 119,447,552 bytes. All samples are deterministic within each release. These finite samples record cost, not a speedup claim; p95 is the maximum of seven samples. [Paired benchmark](roadmap_v2_acceptance/compound_benchmark.json).

## R3: tuple/diamond roles, exact API slots and type-evidence separation

The final graph now propagates bounded helper result roles and private-diamond consensus, resolves single-write captured helpers without treating capture cells as value aliases, and reports bytecode type evidence separately from inferred names. Buffer API argument roles use an exact, versioned table. [Contract](graph_naming.md).

All 926 primary Rust tests plus one child-process repeat and 67 Python tests pass. The expanded runtime manifest passes 132 configurations and nine negative controls, including field metamethod order/errors, nil/false return arity and buffer bit/count errors. The 513 public configurations still compile/decompile/recompile. Existing 52 semantic/residual outputs pass size gates with zero regressions.

The pinned parser verifies alpha-equivalent binding graphs and unchanged type syntax on all 3,978 private files: 3,887 are text-identical and 91 change only local names (119 renamed bindings). All 513 public outputs pass the same gate: 495 identical, 18 with local renames (90 bindings). Recorded-binding metadata audits pass for all 132 runtime and 513 public cases; analysis/provenance source is unchanged and sidecars are deterministic across one/four threads. No naming graph budget is exhausted.

On the locked source-aligned metric, development exact names rise from 1,039/3,583 to 1,048/3,583. Of 36 changed aligned names, nine match source and none lose an exact match. Those nine represent `processor` in three Fusion files at O0/O1/O2, not nine independent sources. Rodux holdout remains 85/200 aligned exact names; 108 configurations across both splits remain unknown alignment. The three changed buffer modules are in that unknown-alignment group and receive no invented exact-name score. The buffer fixture improves from 1 to 7 exact parameter names in each g1 optimization configuration.

The helper fixture now emits `width, height` and `width2, height2` for fixed returned `.Width, .Height` slots. These roles differ from the author's `leftWidth/rightWidth` spelling. BufferWriter emits `size`, `str`, `count`; source distinguishes `desiredSize/newSize` and calls byte count `length`. The user subsequently reviewed the width/height, processor and size examples and answered “Rõ hơn” (clearer). This [reader review](role_naming_review.md) is qualitative feedback on three examples, not a corpus-wide role-precision score.

Seven warm interleaved CLI rounds: one-thread median 19.018 → 19.510 s (+2.58%), 16-thread median 1.686 → 1.706 s (+1.16%). Median peak RSS 33,435,648 → 33,443,840 bytes and 114,909,184 → 113,086,464 bytes respectively. All samples are deterministic. These are cost observations, not a performance improvement; p95 is the maximum of seven samples. Per-file reports and binary hashes are in the [validation inventory](roadmap_v2_acceptance/roles_validation.json).

## R3: explicit module context, exports and cyclic summaries

An optional analysis tool now resolves static script paths through a versioned manifest and summarizes bounded function/table exports, fixed call positions and scalar/forwarded returns. It builds dependency SCCs and retains unknown for unresolved recursive summaries. It produces naming metadata without executing modules or changing source. [Contract](module_summaries.md).

The 67 Python tests include eight module-analysis controls; nine actual-parser fixture modules pass locked expectations for result-origin forwarding, dependency cycles, dynamic requires, shadowing, import rebinding and observed exports. The public run includes all 171 pinned source files:

| Split | Modules / functions | Resolved / unknown require paths | Resolved call targets | Unknown return summaries |
|---|---:|---:|---:|---:|
| Development | 156 / 1,102 | 162 / 221 | 226 | 993 |
| Rodux holdout | 15 / 39 | 24 / 0 | 7 | 35 |

The public dependency graph has 171 SCCs and no static cycles under the declared filesystem-mirror context; cycles are exercised by the independent fixtures. Seventy-five public modules expose a supported function/forwarding or private-literal export shape; all other export shapes are reported unknown. Both repeat runs are byte-identical for the nine fixtures and 171 public inputs. Diagnostic process measurements are 0.323/0.316 s for fixtures and 3.659/3.713 s for public analysis; they ran alongside release-build work and are not a throughput benchmark. Input totals are 632,781 source bytes and 72,271 AST dictionary nodes. No project budget is exhausted.

The [validation record](roadmap_v2_acceptance/module_validation.json) pins the tests and report hashes. CI runs these fixtures and the public manifest. This closes bounded module-summary infrastructure; automatic inter-module rename application and human assessment of inferred-role quality are not claimed.

## R3: bounded legacy naming candidate evidence

The namer now retains accepted proposals, losing alternatives, selected base hints and invalidation events in optional analysis metadata. It stores stable binding IDs and no local owners; ordinary naming, cleanup and final binding choices remain unchanged. See [the contract](naming_evidence.md).

- 920 primary Rust tests plus one child-process repeat, and 59 Python tests pass. New tests cover reference-count-sensitive cleanup, fill-only parameter type hints, losing candidates, invalidations, bounded deterministic retention and audit mutants.
- All 120 runtime configurations and nine negative controls pass; all 513 public configurations compile/decompile/recompile. Source is byte-identical to the prior release in all 633 configurations and all 3,978 private corpus files. Corpus tree SHA-256 remains `10ae04fbed5296f57e93a45a5821704a5101a3bf9bd771cc0fbb7a2e55a6b844`.
- Runtime evidence records 263 retained candidates over 667 pre-cleanup bindings, with 14 bindings containing alternatives. Public evidence records 9,226 candidates over 13,226 pre-cleanup bindings; 968 have alternatives and 18 have invalidations. No evidence budget overflows. Of the public pre-cleanup identities, 12,934 remain in the final graph; the 292 others are explicitly unmapped.
- All 633 recorded-binding metadata audits pass. Analysis and provenance modes preserve source; provenance sidecars are identical at one and four threads for every fixture/public input. Diagnostic public runs take 3.297 s (analysis, one thread), 4.466 s (provenance, one thread), and 1.296 s (provenance, four threads). These are single diagnostic measurements.
- Seven interleaved warm CLI rounds: one-thread median 19.191 → 19.532 s (+1.78%); 16-thread median 1.686 → 1.706 s (+1.18%). Median peak RSS 33,718,272 → 33,603,584 bytes (one thread) and 114,524,160 → 113,156,096 bytes (16 threads). All samples are deterministic. This adds diagnostics and does not claim a throughput improvement; nearest-rank p95 is a seven-sample maximum.

The [validation inventory](roadmap_v2_acceptance/candidates_validation.json) links per-file coverage, metadata audits, benchmark samples and [public candidate examples](roadmap_v2_acceptance/candidates_examples.json). This closes candidate retention only. Broader role propagation, module summaries and human role-quality acceptance are separate R3 work.

This record distinguishes implemented gates from the research roadmap's wider
acceptance criteria. The baseline is not an overall source-recovery percentage.

## R0: ordered CFG/register validation and capture lifetime

The oracle now falls back from acyclic symbolic execution to a bounded v9
transition-graph certificate. It retains branch successors/polarity, operand
identity, loop register groups, iterator arity, call/multret state, closure
constant sharing, capture mode and CLOSE partitions. FASTCALL success and
fallback paths are both explicit. A must-definition worklist checks joins and
loops. Ref-capture chunks retain physical register/frame layout throughout;
other eligible finite register groups can alpha-rename. Graph mismatch or
budget exhaustion remains `unknown`. See the [model contract](dataflow_graph.md).

The independent parser no longer rounds signed 64-bit integer constants into
floats. The acyclic checker now distinguishes separate closure constant entries
for one prototype, consumes SETLIST's open result state and validates the full
comparison-register AUX operand. Fifteen added tests cover valid graph proofs
and mutants for operands, branch/back edges, missing definitions, capture mode,
CLOSE, closure sharing, integer precision, call/iterator arity, FASTCALL,
malformed targets and budget/version refusal. All 57 Python tests pass.

Immutable re-scoring checks the original report/tool/source/output hashes and
recompiles both sides with the pinned compiler. It produces no new source and
claims no new runtime observations:

| Dataset | Prior proved retained | Unknown now proved | Still unknown | Existing different retained |
|---|---:|---:|---:|---:|
| [120 default runtime configurations](roadmap_v2_acceptance/graph_runtime.json) | 56 | 32 | 18 | 14 |
| [513 public configurations](roadmap_v2_acceptance/graph_public.json) | 88 | 42 | 297 | 86 |
| [120 opt-in loop configurations](roadmap_v2_acceptance/graph_reroll.json) | 56 | 26 | 24 | 14 |

The public holdout contributes five new graph proofs, seven retained acyclic
proofs, 17 unknown and 16 different results across all 45 configurations.
Graph self-controls prove all 120 runtime and 513 public inputs. New proofs
include actual numeric/generic loops and mutable capture fixtures; no prior
proof is lost relative to the recorded checker. The six manually expanded
`unrolled_capture` configurations now prove in default mode, while their
synthesized-loop counterparts remain unknown under the stricter graph shape
criterion. Runtime observations still pass; the synthesis experiment does not
receive a stronger source-origin or graph-equivalence claim from this change.

With four replay workers, summed validator timings are about 0.104 s for the
120 default configurations and 4.630 s for the 513 public configurations; the
largest individual public comparison is about 0.148 s. These are diagnostic
measurements under thread contention, not an end-to-end speedup benchmark.
There is no Rust pipeline/output change. [Validation and code/report hashes](roadmap_v2_acceptance/graph_validation.json).
The original acceptance artifacts and legacy normalization gates remain
historical records; unknown is never counted as equivalence. This completes
R0's stated bounded foundation, not a universal Luau equivalence decision
procedure.

## R5: opt-in bounded arithmetic loop synthesis

The new pass recognizes exactly ordered `+0 + x*1 + ... + x*N` accumulations
for N=4..8, including private chains of scalar declarations. It preserves
each multiplication/addition and re-reads a reference-captured multiplicand
on every iteration. Observed intermediates, captured destinations, conflicting
debug names, gaps and reassociation refuse. Existing named arithmetic helpers
take priority. Each site and the module rewrite count are bounded; see the
[eligibility and equivalence contract](arithmetic_reroll.md).

**The experiment is disabled by default.** The explicit
`--synthesize-arithmetic-loops` option enables it in the CLI, with a matching
Rust API option and transported flag bit. Emitted comments say
`equivalent fixed-count loop synthesized; original loop unknown`. The manually
expanded `unrolled_capture` source is a counterexample to inferring an original
loop from this pattern alone: its behavior is preserved but source similarity
falls. This closes only a bounded experiment, not automatic loop recovery or
the broader R5 precision/recall acceptance criterion.

Both [default](roadmap_v2_acceptance/reroll_default_runtime.json) and
[enabled](roadmap_v2_acceptance/reroll_runtime.json) modes pass all 120 runtime
configurations and nine negative controls, with deterministic source at one
and four threads. Default output, full dataflow and source-fidelity metrics
remain identical to the arithmetic-helper baseline on all 120 cases. Enabling
the option changes ten outputs: `helper_loop` and `unrolled_effects` at O2/g1
and O2/g2 retain `proved`; `unrolled_capture` at all six configurations retains
`unknown` because reference capture is outside the ordered-dataflow checker's
supported model. The other 110 outputs remain byte-identical.

The capture fixture uses `__mul` to replace its captured multiplicand after
each product, checks ordered `__add`, and throws on the third product in a
second run. The successful result is 30 with trace
`mul:1:1,add:0:1,mul:2:2,add:1:4,mul:3:3,add:5:9,mul:4:4,add:14:16`.
An unsafe snapshot would repeat the old operand. The fixture also checks
positive/negative zero, subnormal/large numbers, NaN and infinity. The
`sum_helper` fixture prevents loop synthesis from hiding an existing
`weightedSum` helper or its two recovered calls.

[Twelve compiler witnesses](roadmap_v2_acceptance/reroll_witness.json) preserve
source/output hashes, AST loop/call counts and original/recompiled disassembly.
The pinned O2 compiler confirms the source loops in `helper_loop` and
`unrolled_effects` were unrolled, and the emitted loops unroll again. The manual
expansion has no original source loop; that fact remains explicit in each row.
Source metrics at O2/g2 are reported separately from runtime equivalence:

| Fixture | Raw structural ratio, default → enabled | Aligned / exact names, default → enabled |
|---|---:|---:|
| `helper_loop` | 0.6970 → 0.8816 | 4 / 4 → 7 / 7 |
| `unrolled_effects` | 0.5693 → 1.0000 | 0 / 0 → 3 / 2 |
| `unrolled_capture` (manual expansion) | 1.0000 → 0.7242 | 4 / 4 → 3 / 3 |
| `sum_helper` | 1.0000 → 1.0000 | 5 / 5 → 5 / 5 |

Structural ratio ignores identifier spelling. The generated `i` is inferred
even when it happens to match source; in `unrolled_effects` the source counter
was `index`. The capture fixture still prints `current`, but changing its
occurrence structure affects binding alignment. None of these numbers is
original-loop recovery precision.

All 3,978 private corpus outputs (3,936 nonempty plus 42 empty inputs) remain
byte-identical to the arithmetic baseline in both modes. Both modes also pass
all [513 public configurations](roadmap_v2_acceptance/reroll_public_identity.json),
with identical full dataflow/source-fidelity results, including the 45 Rodux
holdout configurations. There are no eligible loop sites in this holdout; it
provides regression evidence only. The 45 legacy runtime cases pass; all 52
legacy/residual sources remain byte-identical with either option setting, so
their prior oracle gates are inherited without baseline updates. The
[size gate](roadmap_v2_acceptance/reroll_size.json) has zero regressions.

[Provenance validation](roadmap_v2_acceptance/reroll_traces.json) passes for
120 enabled configurations across ordinary analysis and detailed traces at
one/four threads. The 17 unlocated final bindings are the ten fresh counters
and seven fresh accumulators; they receive no fabricated bytecode ancestry.
The [profile audit](roadmap_v2_acceptance/reroll_fixture_profiles.json) checks
plain/profiled output and deterministic counters within the same enabled
version: 120 pass invocations and ten synthesized loops at both thread counts.
Node census is unmeasured for this pass. Compressed raw profiles are archived.

Workspace checks pass: 916 primary Rust tests plus one child-process repeat,
and 42 Python tests. The [validation record](roadmap_v2_acceptance/reroll_validation.json)
retains final binary, report and log hashes and all source tree identities.

The final [seven-round release benchmark](roadmap_v2_acceptance/reroll_benchmark.json)
compares default and enabled settings of the **same binary**, with interleaved
warm-cache CLI runs, profiling off and no concurrent build/test workload:

| Threads | Default median | Enabled median | Default / enabled nearest-rank p95 |
|---:|---:|---:|---:|
| 1 | 17.604 s | 18.194 s | 20.423 / 19.839 s |
| 16 | 1.655 s | 1.676 s | 1.697 / 1.717 s |

Enabling the pass adds 3.35% to the one-thread median and 1.27% at 16
threads on this corpus. The default skips its traversal and usage census.
This is a reconstruction experiment, not a speed improvement. Median peak RSS
is 33,492,992 → 33,558,528 bytes at one thread and
116,006,912 → 112,267,264 bytes at 16 threads. All output hashes are identical
across settings and samples. Nearest-rank p95 is the maximum of seven samples;
timing variation prevents interpreting these finite samples as an isolated
measurement of the pass's cost.

No Roblox runtime, general induction/body reconstruction, independent loop
precision/recall, allocation-count, cold-cache or in-memory performance result
is claimed. Source-origin discovery from line/PC evidence and automatic loop
recovery remain open.

## R5: bounded reconstruction of named arithmetic helpers

The expression de-inliner now has a separate arithmetic family requiring a
named bytecode prototype, a write-once helper binder, exact operator/branch
matching and stable scalar arguments. It preserves statement-style helper
bodies and constructs a return/selection pattern only for matching. Reference
captures, compound arguments, ambiguous helpers and exhausted budgets refuse
the new rewrite. The general anchor threshold is unchanged. See the
[eligibility and proof contract](arithmetic_deinline.md).

Both `adjust` calls in `helper_loop` are restored at O2/g1 and O2/g2, with the
`value + 1` temporary evaluated at its original position. The pinned compiler
confirms two original inlines and one four-iteration unroll. Six
[compiler witnesses](roadmap_v2_acceptance/arithmetic_witness.json) record
original/recompiled disassembly, source hashes, AST call counts and runtime
results. The four fixture configurations whose output changes retain ordered
dataflow `proved`. Literals printed as `math.pi`/`math.huge` are excluded so the
rewrite cannot move those environment lookups into a different function.
Output labels the calls as equivalent-call inference from
the existing helper; it does not claim unique original call sites.

The two new fixtures check arithmetic/comparison metamethod order, mutable
table state, exceptions, NaN/infinity, signed-zero inputs and reference-capture
mutation. In the capture negative case, the comparison changes the variable
before multiplication. The correct result remains 23 with trace
`lt:1,mul:10`; the new matcher refuses to snapshot that variable into a call.
All [108 runtime configurations and nine controls](roadmap_v2_acceptance/arithmetic_runtime.json)
pass, including deterministic output at one/four threads. Of these, 104 outputs
are byte-identical to the prior binary run against the expanded manifest.

All 3,978 corpus files finish (3,936 nonempty, 42 empty); debug and release
output trees agree. [Four files change](roadmap_v2_acceptance/arithmetic_corpus.json),
introducing 13 equivalent arithmetic calls across six definitions:
`multiplyHue`, `cubicBezier`/`cubicBezierDerivative` in two modules,
and `blendChannel`. All four before/after files recompile at O0/O1/O2 (12
configurations per version); their whole-file dataflow remains `unknown`.
The other 3,974 files are byte-identical. Including inference comments, the
changed files add three lines and 43 UTF-8 bytes after LF normalization.

All [513 public-source configurations](roadmap_v2_acceptance/arithmetic_public_identity.json),
including 45 Rodux holdout configurations, retain byte-identical output and
identical dataflow/source-fidelity results. There are no eligible new sites in
this holdout, so it supplies regression evidence, not arithmetic recovery
precision/recall. The 45 legacy runtime fixtures pass and all 52 residual/semantic
sources stay byte-identical; [the size gate](roadmap_v2_acceptance/arithmetic_size.json)
has no regressions. Legacy oracle gates are inherited through that exact source
identity, with no baseline updates. [Provenance checks](roadmap_v2_acceptance/arithmetic_traces.json)
pass for all 108 configurations, including ordinary analysis and detailed
traces at one/four threads. Workspace tests pass: 906 primary Rust tests plus
one child-process repeat, and 42 Python tests. The
[validation record](roadmap_v2_acceptance/arithmetic_validation.json) retains
binary, report and log hashes.

The final [seven-round interleaved release benchmark](roadmap_v2_acceptance/arithmetic_benchmark.json)
uses the same input/build settings and warm-cache CLI protocol as R7, with no
concurrent build/test workload:

| Threads | Before median | After median | Before / after nearest-rank p95 |
|---:|---:|---:|---:|
| 1 | 17.571 s | 18.324 s | 20.238 / 18.512 s |
| 16 | 1.665 s | 1.645 s | 1.736 / 1.704 s |

The one-thread median costs 4.3%; the 16-thread median changes by -1.2%.
This is a reconstruction feature, not a throughput improvement. Median peak
RSS is 33,787,904 → 33,079,296 bytes at one thread and
114,614,272 → 113,225,728 bytes at 16 threads. All measured output hashes are
stable within each version. An earlier
[exploratory benchmark](roadmap_v2_acceptance/arithmetic_preview_benchmark.json)
and [profile audit](roadmap_v2_acceptance/arithmetic_preview_profile_audit.json)
are retained separately: they used the candidate before the literal guard,
whose output included one additional file. Its one-thread CLI samples had a
larger median difference and two timing clusters; its expression pass cost
0.119 → 0.169 s. These diagnostic results do not replace the final measurements
or identify the cause of all CLI timing variation. No cold-cache, allocation
count or in-memory API claim is made.

This completes the scoped arithmetic-helper checkbox, not all R5 acceptance.
At this stage the four-iteration loop stayed unrolled; the later opt-in
experiment above handles its bounded shape. The second `arithmetic_effects` result
has become statement-level control flow and is deliberately retained. General
specialization, line/PC candidate discovery, missing-prototype synthesis and
independent arithmetic precision/recall remain open. No Roblox runtime result
is claimed for the public or private corpus.

## R7: cache statement facts during SSA inline

The SSA inliner's backward scans now reuse read/write group IDs, captured-cell
access and existing effect predicates within one block visit. Modified
consumers and emptied producers invalidate their entries; the cache is
discarded before statement reindexing or cleanup. It retains no AST/local
owners and changes no rewrite condition. Small/oversized blocks compute fresh
facts. See the [cache contract](ssa_inline_cache.md).

The [seven-round interleaved release benchmark](roadmap_v2_acceptance/ssa_cache_benchmark.json)
compares the R4 global-lookup fix with this cache, with profiling disabled.

| Threads | Before median / p95 | Cached median / p95 | Median peak RSS before → after |
|---|---:|---:|---:|
| 1 | 18.190 / 18.532 s | 17.503 / 18.380 s | 33,837,056 → 33,464,320 B |
| 16 | 1.726 / 2.138 s | 1.676 / 1.992 s | 117,760,000 → 110,825,472 B |

Median time falls **3.78% at one thread** and **2.85% at 16 threads**. Six of
seven paired one-thread rounds are faster with the cache. Both versions have a
16-thread timing outlier; nearest-rank p95 is the maximum of seven samples.
Maximum peak RSS is 34,439,168 → 34,144,256 B at one thread and
122,335,232 → 112,775,168 B at 16 threads. This is a modest measured gain on
this machine/corpus, below the roadmap's proposed 10% end-to-end target.
It is not an allocation-count or cold-cache measurement.

All 3,978 corpus outputs remain byte-identical to R4, across binaries, build
modes and the measured thread counts. Debug builds recompute facts on every
cache hit; the entire corpus passes that invariant, with 3,936 nonempty
scripts, 42 empty inputs and zero decompile failures. Three Rust tests cover
invalidation, captured access, emptied statements, size fallback and deliberate
failure of the debug guard. Workspace tests and all 42 Python checks pass.
[Validation record](roadmap_v2_acceptance/ssa_cache_validation.json).

The [complete profile validation](roadmap_v2_acceptance/ssa_cache_corpus_profiles.json)
checks 531,967 rows per profile at one/16 threads. Cache counters are identical:
5,784,537 hits, 1,615,852 stored computations, 200,006 uncached computations,
555,603 invalidations and 1,463,967 allocated statement slots across visits.
Slots are not peak live entries or allocation bytes. Removing the three timing
fields and new cache counters leaves **every prior row field unchanged** from
a fresh profile of the R4 binary. [Comparison and excerpt hashes](roadmap_v2_acceptance/ssa_cache_profile_audit.json).

In the separate one-thread diagnostic runs, `F_SSA_INLINE` takes 2.237 →
1.743 seconds across 62,305 calls, a 22.11% reduction within that phase.
The phase has 26,037 distinct file/prototype rows; these are not the total
prototype or function-visit counts. Other pass timings and profile-export
overhead prevent treating this phase reduction as process speedup. The gzip
artifacts linked by the audit contain the SSA-inline rows only; complete raw
profiles were validated before extraction and their hashes are retained.

All [96 runtime configurations](roadmap_v2_acceptance/ssa_cache_runtime.json),
nine oracle controls, 45 older semantic cases and the 52-file size gate pass.
Every one of these source outputs is identical to R4. All
[513 public configurations](roadmap_v2_acceptance/ssa_cache_public_identity.json),
including the 45 Rodux holdout configurations, compile and preserve exact
source hashes; the prior AST/dataflow results therefore remain unchanged.
The 96-fixture lineage run preserves every full R4 sidecar, and both lineage
and profiler counters remain deterministic across modes and one/four threads.
No baseline was changed. Broader AST/CFG caches, allocation instrumentation
and the remaining R7 benchmark modes remain open.

## R4: preserve evaluation order across global lookup

The SSA inliner treated an already visited global read as reorderable. A missing
global can invoke the environment's `__index`, mutate state or throw. This let
`local value = fetch(); return sink(value)` become `return sink((fetch()))`,
looking up `sink` before the earlier call to `fetch`. Global reads now stop an
observable candidate from moving past them. Total candidates can still cross
the barrier, and an ordinary local callee does not introduce this barrier.
No standard-library name or type hint is assumed to establish a pure lookup.

The [locked fixture](failure_fixtures/roadmap_v2/global_evaluation_order.luau)
checks ordinary lookup, replacement of `fetch` during lookup, nil/false scalar
results, argument count, trailing nil in multret and a throwing lookup. The
preceding binary fails all three `-g1` configurations: the call trace changes,
the replacement function returns 99 instead of 7, and the throwing lookup
prevents the earlier call from executing. Debug binding protection already
keeps this local at `-g2`; all six configurations now pass. Error category and
trace are compared, not stack locations. [Before/after observations and source](roadmap_v2_acceptance/global_order_examples.json).

The [runtime matrix](roadmap_v2_acceptance/global_order_runtime.json) passes
96 configurations and nine oracle controls, with identical source at one/four
threads. All prior 90 fixture outputs remain byte-identical. The 45 older
semantic fixtures and 52-file size gate also pass. Two focused Rust tests cover
the call barrier and allowed total/local-callee cases; workspace tests and all
41 Python checks pass. The 96-fixture lineage and profiler checks preserve
source output and deterministic metadata/counters across modes and threads.
[Validation details](roadmap_v2_acceptance/global_order_validation.json).

All 3,936 nonempty corpus scripts decompile with zero failures; 42 empty inputs
remain separate. [Per-file change audit](roadmap_v2_acceptance/global_order_corpus.json)
records 531 changed outputs and 3,447 byte-identical outputs. Both versions of
every changed file compile at O0/O1/O2: 1,593 configurations per version. The
changed subset gains 16 `proved` comparisons with the original bytecode;
23 remain `different` and 492 `unknown` in the bounded dataflow model. These
unknowns are not a runtime correctness claim. The change adds 2,116 lines and
38,899 bytes LF. Larger examples include field reads passed to `typeof`,
Promise's validation chains and calls passed to `warn`. They now retain the
temporaries needed under an unknown environment, sometimes preventing branch
compression or constructor folding. This is a correctness cost, not a claim
that the output is closer to the source. Size baselines were not changed.

The [public matrix](roadmap_v2_acceptance/global_order_public.json) passes all
513 configurations, including 45 Rodux holdout configurations. Outputs change
in 167 configurations: Fusion 72, RbxUtil 51, Roact 35, Rodux six and Promise
three. At O0, Fusion `lerpType`/`nameOf` and Roact `strict` move from `different`
to `proved`; all 85 prior proofs remain. Holdout proof statuses remain seven
`proved`, 16 `different` and 22 `unknown`. These modules have not been executed
in Roblox. The wider R4 effect model, dependency graph and alias improvements
remain open.

The [seven-round interleaved benchmark](roadmap_v2_acceptance/global_order_benchmark.json)
uses the preceding profiler build as its baseline, with instrumentation off.
All output hashes are stable within each binary across one/16 threads; the
new hash is `ba6b674bf91e12bbcb5782b1d298b7c73f3cc2c708f88815569b9e704b29cc17`.

| Threads | Before median / p95 | After median / p95 | Median peak RSS before → after |
|---|---:|---:|---:|
| 1 | 17.874 / 18.334 s | 18.118 / 18.517 s | 33,964,032 → 33,517,568 B |
| 16 | 1.686 / 2.600 s | 1.698 / 1.988 s | 111,886,336 → 114,053,120 B |

Median time rises 1.37% and 0.71%, respectively. Seven samples make the reported
nearest-rank p95 the maximum; the 16-thread baseline has a 2.600-second outlier.
This is a correctness fix with a measured cost, not a speedup. The maximum
16-thread peak RSS is 117,702,656 → 121,942,016 B. Both committed legacy oracle
gates also pass, with no baseline changes.

## R7: scoped pass profiling and separate factoring measurements

`MEDAL_PROFILE_JSON` enables bounded per-file/prototype/pass diagnostics.
Whole-module AST passes remain module-level rows. Inclusive and exclusive
times are **thread wall intervals**, with only same-thread nested spans
subtracted; worker overlap and waits prevent interpreting their sum as CPU or
process wall time. Before/after statement/rvalue census currently covers
initial factoring, statement de-inline and subsequent factoring. Other passes
retain `node_samples: 0` as unmeasured. No cache/allocation measurement is
invented. See the [profiling contract](pass_profiling.md).

The [full corpus audit](roadmap_v2_acceptance/profile_corpus.json) has
**531,967 aggregate rows**, covering all **3,936 nonempty scripts** and
**26,391 function-processing visits**. There are no dropped records,
misnested spans or incomplete measured node samples. All **3,978 output
files** are byte-identical to the preceding R2 binary, with profiling off/on
and one/16 threads. Removing only timing fields leaves identical counters and
node census across thread counts. Raw one-thread and 16-thread reports are
archived as [JSON gzip](roadmap_v2_acceptance/profile_corpus_t1.json.gz) and
[JSON gzip](roadmap_v2_acceptance/profile_corpus_t16.json.gz), respectively.

One-thread measurements on the current pipeline:

| Phase | Calls | Inclusive wall sum | Measured activity |
|---|---:|---:|---|
| Initial common-tail factoring | 3,936 | 0.096 s | 302 calls change the AST |
| Statement de-inline | 3,936 | 0.523 s | 4,156 internal iterations |
| Subsequent common-tail factoring | 3,936 | 0.081 s | No change on this corpus; the pass remains required for other shapes |
| De-inline write census | 3,936 | 0.046 s | Existing census computed once per invocation |
| Target collection | 4,156 | 0.112 s | 5,194 candidates, 3,685 accepted and 1,509 refused |
| Candidate scanning | 1,236 | 0.240 s | 141,641 width candidates, 123,950 matches attempted, 39,807 unification calls |

Subphase times overlap their parent de-inline/factoring time. Refusals include
862 low-anchor, 425 return-shape, 220 variadic and two empty-pattern cases.
The scan also records 149,131 canonical-length calls, 32,185 canonicalization
calls and 49,897 return scans. These count invocations, not unique recovered
source constructs. No speedup can be inferred by comparing this experiment
with the older `MEDAL_PROF` run on a different pipeline.

The largest measured one-thread exclusive pass totals are SSA construction
(2.400 s), SSA inline (2.257 s), SSA destruction (1.686 s) and restructuring
(1.458 s). Further performance work should investigate these current costs;
the prior aggregate `S_DEINLINE` counter does not establish today's bottleneck.

[Fixture profiling](roadmap_v2_acceptance/profile_fixtures.json) passes 90
configurations with exact source and identical counters at one/four threads.
A separate combined profile/lineage run preserves all 90 full lineage
sidecars from R2. Six Rust tests cover nested timing, context restoration on
unwind, worker isolation, bounded records and partial/ownership-safe census.
Python checks reject invalid timing, counter/context mismatches and incomplete
profiles. Runtime, public-source and additional validation results are recorded
with the [validation summary](roadmap_v2_acceptance/profile_validation.json).

[Seven-round uninstrumented benchmark](roadmap_v2_acceptance/profile_benchmark.json):

| Threads | R2 median / p95 | Profiler build median / p95 | Median peak RSS before → after |
|---|---:|---:|---:|
| 1 | 17.943 / 19.016 s | 17.986 / 19.076 s | 33,185,792 → 33,828,864 B |
| 16 | 1.718 / 1.842 s | 1.697 / 1.719 s | 115,085,312 → 114,454,528 B |

Median differences are +0.24% and −1.20%, with every output hash unchanged.
Maximum peak RSS at 16 threads is 119,345,152 → 121,192,448 B. This introduces
diagnostics, not an accepted performance optimization. Timings in the profile
validation runs include JSON export and are kept separate from this benchmark.
All-pass node/cache accounting, allocation counts, broader benchmark modes and
actual algorithm/data-structure optimizations remain open in R7.

## R2 foundation and R4 conditional-result recognition

`--emit-binding-provenance` adds a bounded diagnostic trace to static-analysis
sidecars. It is an explicit opt-in and implies ordinary upvalue analysis;
the existing artifact APIs leave it disabled unless
`DecompileOptions::emit_binding_provenance` is set. The
[record contract](binding_provenance.md) describes initial statement PC sets,
SSA definition/write slots, debug intervals, local-map history and final
storage ancestry. Open call/vararg packs, NAMECALL/CALL and closure/CAPTURE
instruction clusters retain multiple PCs without inventing AUX origins.

Before initial SSA copy propagation and again before destruction, a read-only
recognizer records distinct scalar phi inputs of diamond/triangle regions.
Each arm is direct or one private block ending at a two-predecessor join.
Shared arms, extra join predecessors, self-phi, equal inputs and nonlocal
inputs are refused. This is a branch-to-phi relation, **not** purity, totality
or permission to evaluate an arm eagerly. Existing statement output and
source/capture/close proof rules govern the emitted program.

The `conditional` O2 probe demonstrates the distinction: at `-g1`, the phi
maps to the final `p2` storage, which also has parameter ancestry; at `-g2`,
the separately protected source local `selected` retains the phi ancestry.
This does not guess a new source name or turn storage reuse into source-local
identity. IDs/strings in the history do not retain extra `RcLocal` owners.

Validation against the preceding R3 binary:

- [Corpus audit](roadmap_v2_acceptance/provenance_corpus.json): all **3,978**
  output files are byte-identical; all **3,936** script sidecars preserve
  every prior metadata field except analysis ID/options. PC bounds, ordered
  write slots, origin uniqueness and both mapping directions pass.
- There are **26,391 lifted static function instances**, **1,021,632** initial
  statements, all with instruction PCs, and **1,163,364 SSA definitions**.
  **468,839 definitions** have final storage ancestry. **38,538** conditional
  records include both recognition phases; they are not a count of distinct
  original source conditionals.
- Of **129,156** final bindings, **290** have no attributed lineage and
  **11** hit the 256-ancestor limit. They remain explicitly unknown/partial.
  No function exhausts the combined 50,000-record budget. An unmapped
  definition is not automatically classified as inlined or dead.
- [Runtime fixture traces](roadmap_v2_acceptance/provenance_fixtures.json):
  **90/90** analysis/trace pairs have identical source and prior metadata;
  detailed sidecars are identical at one/four threads. All 1,906 statement
  sites have PCs; all 454 final bindings have complete attributed lineage.
- [Public identity audit](roadmap_v2_acceptance/provenance_public_identity.json)
  preserves all **513** previous outputs. The separate
  [public trace audit](roadmap_v2_acceptance/provenance_public_traces.json)
  covers all 468 development and 45 Rodux holdout configurations, with equal
  source/metadata at ordinary/trace modes and one/four threads. Its 66
  unattributed final bindings remain in the denominator; there are no partial
  nonempty lineages or exhausted record budgets. Public modules are still not
  executed in Roblox.
- The source/runtime suite passes **90 configurations and nine negative
  controls**, the prior semantic suite passes **45**, and size gates pass
  **52 files**. Rust and Python tests also cover instruction clusters,
  register reuse, source-proof separation, map-order-independent bounds,
  shared-arm refusals, corrupted trace references and option isolation.

These results complete the explicitly scoped diagnostic foundation and R4's
bounded conditional recognition. Arbitrary nested-value provenance,
clone/synthesis attribution, value-level output spans, a complete per-pass
invalidation ledger, stripped-input source splitting, effect dependencies
and using conditional facts for new naming/constructor rewrites remain open.
No output-quality or performance improvement is claimed for a trace-only
change.

[Seven-round interleaved CLI measurements](roadmap_v2_acceptance/provenance_benchmark.json)
with detailed analysis disabled compare archived release binaries on the same
3,978-input corpus. No other build, test or benchmark ran concurrently:

| Threads | R3 median / p95 | Provenance build median / p95 | Median peak RSS before → after |
|---|---:|---:|---:|
| 1 | 16.971 / 17.264 s | 17.008 / 17.071 s | 34,721,792 → 33,030,144 B |
| 16 | 1.769 / 1.820 s | 1.758 / 1.767 s | 112,926,720 → 116,011,008 B |

Median differences are +0.22% and −0.67%; these do not demonstrate a speedup.
The 16-thread maximum peak working set is 119,803,904 → 127,004,672 B. All
runs, thread counts and traced corpus output share source-tree hash
`5914c1f113bd53340ee5b6a0129f3b65f5ecd29bfeab9416dbf2d660495dcb14`.
The after binary SHA-256 is
`5afdfdf637d87d86daeb5211579bdf0c5a88e0d75a609e3229e0e4105edf5327`.

A separate [three-round analysis-mode sample](roadmap_v2_acceptance/provenance_analysis_benchmark.json)
at 16 threads reports ordinary-analysis median 8.605 s (8.533–8.797 s),
versus detailed-trace median 9.670 s (6.137–10.157 s). Median peak working
set rises from 186,400,768 to 328,650,752 B; maximum is 359,874,560 B with
trace. Timing variance and three samples limit any latency conclusion.
Ordinary sidecars retain their previous total size of 282,031,094 B;
detailed sidecars occupy 1,409,056,857 B for 3,936 scripts. This cost is why
trace collection is opt-in. Source hashes remain equal in every mode.

## R3: bounded role inference on the final binding graph

`ast::refine_names` runs after every expression/control-flow cleanup and before
formatting. It changes only the spelling of existing `RcLocal` identities.
Record fields, assertion messages with exactly one backtick identifier and a
guard referring to exactly one parameter, immutable copies, and exact-arity
calls to uniquely defined local closures supply candidates. A static `script`
path used as a table key can supply the module leaf's case, such as `Children`.
This is naming context, not module resolution or an API effect claim.

Each candidate retains an ordinal priority, rule, witness and originating
binding where applicable. Equal-priority conflicts retain the prior name.
Recorded source bindings, `self`, meaningful existing roles, uncertain owners
and overflowing candidate lists are protected. Propagation refuses written
parameters, multiply defined locals and reference capture cells. Scope
constraints reserve globals and unchanged ancestor/descendant bindings before
assigning collision suffixes; sibling reuse follows `dont_reuse_var`.

The pass has limits of 100,000 visited nodes, 50,000 bindings, depth 256,
24 inference candidates per binding and four propagation rounds. Traversal
budget exhaustion retains **all** prior names. Optional `name_inference`
metadata records candidates and refusal status by binding ID. The prior namer's
selected name is explicitly labelled as partial evidence: its discarded
alternatives are not yet recorded. Phi/default-value/result-tuple propagation,
module summaries/SCCs, versioned API metadata and full legacy candidate
collection remain open.

The [corpus alpha-equivalence audit](roadmap_v2_acceptance/naming_corpus_alpha.json)
checks all 3,978 outputs: **899 changed texts and 3,079 identical texts**, with
equal binding graph, expressions, globals/fields/constants and type syntax in
every file. There are **2,513 renamed bindings**, 143 conflicts, ten uncertain
owners and no traversal/candidate budget exhaustion. No long-line category or
conditional-expression count changes. The
[public audit](roadmap_v2_acceptance/naming_public_alpha.json) similarly passes
all **513** O0/O1/O2 outputs, with 382 renamed bindings in 141 changed files.
These exact structural checks are independent of the budgeted source-fidelity
alignment used to assess original names.

[Binding-aligned source comparison](roadmap_v2_acceptance/naming_comparison.json):

| Locked split | Configurations measured / unknown | Aligned bindings | Exact names before → after | Changed aligned names | Changes to exact / from exact |
|---|---:|---:|---:|---:|---:|
| Development | 363 / 105 | 3,526 | 972 → 1,041 | 96 | 69 / 0 |
| Rodux holdout | 42 / 3 | 200 | 79 → 85 | 6 | 6 / 0 |

Exact-name precision on aligned bindings rises from 27.57% to 29.52% on
development and from 39.5% to 42.5% on holdout. Of the changed aligned names,
71.88% and 100% respectively match the original spelling. The six holdout
changes are a small result, not a general accuracy estimate. O-levels are
separate configurations of the same sources. The remaining 280 public renames
are outside complete source alignment and have no exact-name score; unknown
configurations remain reported. Human role-quality review remains open.

Roact `createElement` now has inferred `component, props, children`; `Children`
and `Type` retain their static module spelling when used as keys. Its normalized
props table becomes `props2`, a distinct binding. The gallery also records the
unchanged buffer/math limitations. No source-name/debug recovery is claimed
for these inferred roles. The fixed runtime suite passes 90/90 configurations,
the existing semantic suite passes 45/45, and all 52 size gates pass without a
baseline change. There are 875 passing Rust tests and 32 Python tests.

The [metadata audit](roadmap_v2_acceptance/naming_metadata.json) preserves the
recorded mapping contract and all 3,320 protected bindings across 3,936 scripts.
It archives every inferred rename and candidate witness. Of the corpus's
2,513 selected names, 2,450 come directly from fields, 41 from resolved local
call arguments and 22 from immutable-copy propagation. Shouting underscore
keys such as `EXTREMELY_DANGEROUS_usedAsValue` are deliberately refused as
parameter roles; ordinary private fields such as `_scope` remain eligible.
This refinement was made on development cases, without changing the holdout.

Seven interleaved timed rounds after warm-up compare the archived layout
release with the final naming release, with no concurrent build/test workload.
[Raw benchmark](roadmap_v2_acceptance/naming_benchmark.json):

| Release | Threads | Median | p95 (nearest rank) | Median peak RSS |
|---|---:|---:|---:|---:|
| Layout | 1 | 23.568 s | 23.591 s | 30.77 MiB |
| Final naming | 1 | 23.856 s | 23.894 s | 32.41 MiB |
| Layout | 16 | 1.742 s | 1.753 s | 109.24 MiB |
| Final naming | 16 | 1.732 s | 1.822 s | 104.71 MiB |

Single-thread median cost is about **1.2% higher**. The 16-thread median is
within 0.6% while p95 is higher; this is not a demonstrated performance gain.
Seven-sample p95 equals the maximum and is only a small-sample tail indicator.
Output is byte-identical across all repetitions, both thread counts and
plain/source-map modes within each release. Final naming release SHA-256:
`7a833c173020b7e3641befd13720419ce2a5fd987151a5ddfc1d29272105ae31`.

## M0: bounded dataflow and reproducible fixtures

`scripts/bytecode_dataflow.py` adds a separate, budgeted symbolic execution tree.
Parameters and upvalue slots remain distinct; temporary registers and copies
are normalized by their definitions. Operand order, branch polarity/successors,
table/global/upvalue effects, callee/arguments, fixed/open result packs and
value-captured closure bodies remain significant. Constants use tagged bytes,
including signed zero and non-UTF-8 strings; metadata and pool indices do not.

The initial contract is **acyclic instruction-tree equality**:

- `proved`: equal ordered symbolic trees within the supported model.
- `different`: different trees; this alone is not a concrete runtime counterexample.
- `unknown`: unsupported semantics, invalid input, or budget exhaustion. Never proof.

Loops, reference/upvalue captures, CLOSEUPVALS, fastcalls and newer unmodelled
opcodes return unknown, even on self-comparison. The budget covers child bodies
and both successors. No float rewrites, guessed purity, metamethod suppression,
or instruction-multiset equivalence are used. Debugger observations, stack
locations, resource exhaustion and allocation timing are outside this contract.
It is not a full Luau translation validator. Results remain separate from the
legacy normalization gate and runtime evidence.

`docs/failure_fixtures/roadmap_v2/manifest.json` locks the seven research sources,
their drivers and observed stdout, compiler commit, O0/O1/O2 and g1/g2 matrix.
All are development fixtures, not an independent holdout. The runner records
per-file/group failures and unknowns, binary/source hashes, options, timings,
outputs, runtime observations and optional 1/4-thread determinism. It uses
fresh work directories and subprocess timeouts and never updates expectations.
CI builds the pinned compiler and runs this matrix in addition to existing gates.

Initial baseline, 2026-09-10, decompiler `d313661`:

| Gate | Result |
|---|---:|
| Source → strict decompile → recompile + expected runtime | 42/42 |
| Output identical with 1 and 4 threads | 42/42 |
| Three negative controls × O0/O1/O2 distinguished | 9/9 |
| Positive self-controls | 9/9 |
| Dataflow proved / unknown / different | 20 / 14 / 8 |

The eight different trees are six UI layouts and two O0 conditional cases;
runtime observations still match. They are **not** counted as proofs. Fourteen
unknown cases contain loops. No baseline was changed to make these trees pass.
The full local report is `out/v2-baseline.json`; replay with:

```powershell
python scripts/roadmap_v2.py --compiler D:/Medal/luau-tools-src/build/luau-compile.exe --luau D:/Medal/luau-tools-src/build/luau.exe --lifter target/release/luau-lifter.exe --report out/v2-report.json --keep out/v2-fixtures --determinism
python -m unittest discover -s scripts -p 'test_*.py'
```

## M0 additions: binding metrics, public manifest and generation

`source_fidelity.py` uses JSON from `Luau.Ast.CLI` at the same compiler commit.
An `AstLocal` declaration location identifies each binding, including shadowed
names. Matching requires every declaration/reference occurrence of a binding to
align bijectively. It reports raw structural similarity, alignment coverage,
exact names on aligned bindings and an exact-name lower bound over **all** source
bindings. Globals, field names, constants, operators and grouped call/return
arity remain significant. Type-only syntax and trivia are separate. Non-UTF-8
constant bytes in the CLI's JSON are preserved with surrogate escapes, not
replacement characters.

The supplementary style score normalizes only a single-local conditional
initializer to a declaration and branch assignments. It is explicitly named
`single-local-if-initializer-v1`, and never replaces the raw score. The alignment
budget is four million token pairs; larger files remain `unknown` in the report.
These are syntax metrics, not a semantic validator.

`source_corpus_v2.json` freezes all 156 surveyed sources plus 15 Rodux sources.
Each repository has its exact commit, production root, source lineage, license
identifier/URL/hash; every source has its SHA-256 and split. No source hashes
overlap. Rodux was reserved before evaluation and is not used to tune these
rewrites. `public_source_roundtrip.py` verifies the manifest, licenses and source
hashes and records every O0/O1/O2 result, including failure and unknown. The
libraries are compiled but **not executed in Roblox**.

`generated_roundtrip.py` adds 12 fixed seeds × O0/O1/O2 × g1/g2. The versioned
grammar combines arithmetic, branch effects, private tables, loop iterations,
copy captures and mutable captures. Runtime vectors include nil/false selections,
signed zero, NaN and infinities. A bounded reducer deletes independent scope units
only while the same failure category persists; it does not claim to preserve a
failure's root cause. Seed, generator/tool hashes, generated source, observations
and reduction attempts are retained. All **72/72** initial configurations pass.

Both source harnesses and the generated suite run in CI. The public report
includes nonexclusive source groups fixed from path/family/type information
before evaluation: buffer/math, UI, promise/event, OOP, loop/capture, module and
type-heavy. Failures and unknowns remain in each group's denominator. The initial
dataflow model still refuses loops and reference capture lifetimes; unknown is
not proof. Allocation, cold-cache and in-memory measurements remain open.

## R1: compiler-recorded names and binding identity

`SourceBinding` stores a name separately from inferred naming hints, together
with `DebugLocal(prototype, register, start_pc, end_pc)`,
`DebugUpvalue(prototype, slot)` or `Function(prototype)`. Invalid identifiers are
rejected rather than repaired and counted as exact recovery. Debug-local
identity outranks upvalue/function hints; conflicting debug identities retain
their evidence and decline a source-name choice.

The lifter maps unique live debug intervals onto entry phis and register writes,
including the reaching initializer immediately before a range begins in the
same block. Parameters/upvalues use their recorded slots. SSA construction,
copy propagation, phi removal/destruction and AST local replacement preserve
that evidence. Optional inline/coalescing passes protect known source locals.
An early parameter-phi mapping prevents `selected` from swallowing `fallback`.
Named functions remain named locals at anonymous return/callback uses; a
same-named field/global definition can inline its function temporary because
the recorded name is already visible there.

The new captured-parameter fixture exposed an existing correctness bug in the
baseline: a closure observed the old parameter after branch reassignment.
Final trivial-phi removal had changed the SSA representative without updating
capture membership. Construction now remaps those membership groups alongside
the local map. Existing close certificates still use their intersection/remap
rules; no certificate is transferred by spelling.

The fixed suite now contains 14 cases × six compiler configurations. It adds
register reuse, same-spelled locals in distinct scopes, shadow captures,
parameter reassignment, identifier edge cases, conditional call/return contexts
and exact literal bytes. All **84/84** configurations pass runtime and repeated
1/4-thread output checks. `invoiceTotal -g2` retains all seven identifiers under
complete binding-aware alignment at O0/O1/O2; the loop naming probe also retains
all seven names. The existing semantic suite passes **45/45**, and all 52 output
size gates pass without a baseline change.

The corpus review also exposed a late-predecessor problem in the previous
capture analysis: a callback created in one branch and a second callback after
the join could receive different cells. The reaching-open analysis now uses a
monotone worklist and unifies overlapping open sites for the same VM register;
`CLOSE` kills the reaching set and separates epochs. A diamond-CFG unit test
checks both the shared-cell and closed-cell cases. The `cell_after_join` runtime
fixture reproduces the conditional event-connection/cleanup pattern at every
optimization/debug level. This fix is necessary for preserving capture identity,
independently of spelling.

With `--emit-upvalue-analysis`, the additive `source_recovery` metadata reports
every recorded name, emitted binding IDs, source origins and unmapped reasons.
It distinguishes recorded names from inferred/generated names and reports exact
spelling separately from a binding that was renamed to avoid collision. An
inlined-away compiler prototype is not counted as recovered source.

`audit_source_names.py` reproduced the original **457** lexical queue using the
frozen baseline binary, then matched prototype identities, formatter occurrence
spans and official parser contexts. The first release audit found **451** mapped
exact names, one non-emitted prototype, and five missing local-binding mappings.
The five remaining occurrences are table-field callbacks whose recorded names
are already present as field keys; parser context records this distinction.
The full 9,594-name denominator is also reported: the validated release mapped
8,120 records and left 1,474 without a proven binding mapping. Neither lexical presence
nor this partial metadata coverage is an overall name-recovery percentage.

The same release passed strict decompilation/recompilation on all 3,936 nonempty
corpus inputs and 513 public-source configurations (468 development, 45 holdout).
The legacy corpus total changed from 2,699 to 2,662 non-equivalent prototypes.
All 29 per-file increases were inspected: conditional statement/constructor
shape, retained short-circuit forms, named functions, and the two Popper variants'
shared capture cells account for the deltas. The original baseline remains
unchanged. [The review](roadmap_v2_acceptance/corpus_review.json) retains every
inspected diff, reason, input/output/rebuilt hash and dataflow status;
[V2 per-file limits](bytecode_roundtrip/baseline_corpus_v2.json) change only those
29 entries. All 3,936 rows satisfy those reviewed limits. This is a normalization
triage decision, not runtime proof for Roblox modules.

## R6: statement policy and literal fidelity

The production pipeline no longer reconstructs general `IfExpression`
initializers. A restricted pass retains short `and/or/not` forms only where
their value semantics follow from boolean/literal facts and existing evaluation
order/arity guards. General branch values stay in statements. Normalization no
longer introduces `IfExpression` from inverted `and/or` idioms. The fixture gate
checks the emitted AST for conditional expressions, including effectful call
arguments and return tuples. This is not a general-purpose lowering pass for
arbitrary externally constructed IR; that roadmap item remains open.

Printable UTF-8 strings with multiple lines use long brackets when exact bytes
can be preserved. A framing LF accounts for Luau's initial-newline rule; the
chosen delimiter avoids both embedded delimiters and an overlap with a trailing
`]`. CR, control bytes and invalid UTF-8 keep quoted escapes. Runtime fixtures
compare every byte, including leading LF, delimiters, CR/LF, NUL and byte 255.
No line-width target is allowed to change string payloads.

## Validation artifacts and cost

[Acceptance summary](roadmap_v2_acceptance/summary.json),
[fixture report](roadmap_v2_acceptance/fixtures.json),
[public-source report](roadmap_v2_acceptance/public.json),
[seeded programs](roadmap_v2_acceptance/generated.json) and
[457-case audit](roadmap_v2_acceptance/name_audit.json) retain per-case results,
tool/source hashes and refusals. There are 862 passing Rust tests and 31 passing
Python tests. The [source gallery](roadmap_v2_gallery.md) distinguishes recovered names from remaining
naming/layout work; no result is an overall source-recovery percentage.

`benchmark_v2.py` measures CLI folder wall time with warm filesystem cache and
reused output directories. It interleaves the frozen and current release at one
and 16 threads, runs seven timed rounds after warm-up, records input/tool/output
hashes, and samples Windows peak working set. No other test/build workload ran
concurrently. [Raw measurements](roadmap_v2_acceptance/benchmark.json):

| Release | Threads | Median | p95 (nearest rank) | Median peak RSS |
|---|---:|---:|---:|---:|
| Frozen baseline | 1 | 22.573 s | 23.066 s | 32.56 MiB |
| Names/capture/statements | 1 | 23.564 s | 23.829 s | 32.15 MiB |
| Frozen baseline | 16 | 1.698 s | 1.793 s | 107.01 MiB |
| Names/capture/statements | 16 | 1.811 s | 2.124 s | 110.95 MiB |

The median cost increases about 4.4% at one thread and 6.7% at 16 threads. This
is a quality/correctness change with measured overhead, **not** an R7 speedup.
For seven samples the reported p95 equals the maximum; it is not a stable tail
estimate. Every output hash is identical across repeated runs and thread counts
within its release. Old/new releases intentionally differ. Cold cache,
allocations, per-pass JSON and in-memory API profiling remain open.

## R6 follow-up: bounded group layout

Call/method argument groups, return tuples and compact arrays use a soft
120-column budget. The width counter stores no rendered output and stops at
120 columns or 256 expression nodes. It preserves existing multiline constructor/callback
shapes when their opening line fits. Single constructor arguments retain the
usual `factory({ ... })` form. Tail `Select` parentheses are unchanged for
calls, arrays and returns, so a one-result adjustment cannot become a spread.
Plain and source-map output now share column tracking; previewing never records
an extra closure occurrence. Literal payloads and indivisible expressions can
still exceed the target.

`layout_audit.py` uses strict equality of the pinned parser's canonical AST,
binding names/identity and type syntax. [The corpus audit](roadmap_v2_acceptance/layout.json)
compares all 3,978 output files (3,936 bytecode inputs plus 42 empty placeholders):
**719 changed text with equal AST, 3,259 identical text, zero changed/unknown**.
Both sides have zero conditional-expression nodes. With tabs expanded to four
columns, lines over 180 columns fall from **368 to 131**:

| Descriptive syntax category | Before | After |
|---|---:|---:|
| Literal | 56 | 48 |
| Expression | 110 | 13 |
| Constructor/callback | 108 | 1 |
| Control flow | 48 | 47 |
| Return expression | 46 | 22 |

The categories describe line syntax, not semantic roles or a source-fidelity
score. Existing literal-byte, arity and control-flow tests remain the relevant
gates for each category. The 84 runtime/name configurations, 45 existing semantic
configurations and 52 size gates pass. The dedicated wide-layout fixture adds
spread/adjusted call, table and return contexts, bringing the release suite to
**90/90** configurations. There are now 865 Rust tests and 32 Python
tests. The release and debug corpus outputs have the same tree hash, including
the release's source-map mode.

The [public layout audit](roadmap_v2_acceptance/layout_public.json) also preserves
AST, bindings and type syntax for all **513** O0/O1/O2 outputs: 86 layout changes
and 427 identical texts, including the held-out family. Long lines drop from
39 to 10, with only literal and control-flow cases remaining in this inventory.

This audit exposed two measurement issues: the AST metric now ignores type
`nameLocation`/`prefixLocation` along with other parser trivia (model
`luau-ast-binding-v2`), while retaining the actual type names. The pinned Windows
CLI receives an exact-byte temporary copy for Unicode/long paths it cannot open;
six corpus filenames now parse instead of becoming unknown. The original v1
acceptance artifacts remain historical records, not silently rescored baselines.
