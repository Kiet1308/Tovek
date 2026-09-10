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

Still open in M0: Luau AST/binding fidelity, independent source-family holdout,
license-enriched public manifest, generated programs/reducer, allocation/RSS
and statistically sampled benchmark. Implementing the initial gate does not
close the whole M0 milestone.
