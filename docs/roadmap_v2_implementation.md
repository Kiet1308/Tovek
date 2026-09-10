# Roadmap V2 — implementation and acceptance record

This record distinguishes implemented gates from the research roadmap's wider
acceptance criteria. The baseline is not an overall source-recovery percentage.

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
