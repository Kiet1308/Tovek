# Compound assignment and observable evaluation

The formatter only folds `target = target op rhs` to compound assignment when
the target is the same local, or the same index with local/literal base and key.
Nested field reads, arithmetic and unary expressions cannot establish repeatable
evaluation merely because their children are locals. They may call metamethods,
throw, or produce a different reference on the second evaluation. Recorded type
hints do not discharge these obligations.

For example, `record.branch.value = record.branch.value + 1` reads `branch`
twice. A metatable can return different tables for those reads. Replacing it
with `record.branch.value += 1` removes one observable read. Computed binary
and unary index keys have the same problem. The new fixture also makes the
second key evaluation throw, testing preservation of the error as well as the
final values and event count.

The previous release fails all six O0/O1/O2, g1/g2 configurations of
`compound_effects`: runtime observations differ and the bounded dataflow
checker returns `different`. The corrected release passes all six, now with
`proved` dataflow. The complete suite passes 138 configurations and nine
negative controls. `formatter_effect_controls.py` separately runs the expanded
and compound mutant forms in the pinned VM; all six runs demonstrate the
expected difference. CI runs both suites.

There are 28 changed private corpus outputs and 3,950 identical outputs.
`formatter_corpus_audit.py` checks that every change consists only of expanding
compound assignment, retaining the full parser binding graph, names, types and
operators. This normalization is a shape audit, **not an equivalence proof**:
the expanded form intentionally restores observable evaluations. Across uniquely
named prototypes, all 32 changed table-read counts match the original input
bytecode. Whole-chunk validation remains `unknown` for those 28 files.

In Part_Icles `Emit`, for example, the original `EmitPart` has seven
`GETTABLEKS VisualPart` operations, the previous output recompiles to six, and
the corrected output recompiles to seven. `EmitAttachment` similarly changes
from one to the input's two reads. Counts are supporting witnesses; the runtime
counterexamples establish why collapsing these reads is unsound.

All 513 public source outputs remain byte-identical, including the 45 holdout
configurations. Recorded naming mappings are unchanged on the 138 runtime and
513 public configurations; source and optional lineage metadata remain
deterministic at one and four threads. All 927 primary Rust tests, one child
repeat, 67 Python tests, 45 legacy semantic configurations and 52 per-file size
gates pass. [Validation inventory](roadmap_v2_acceptance/compound_validation.json),
[runtime cases](roadmap_v2_acceptance/compound_runtime.json),
[corpus audit](roadmap_v2_acceptance/compound_corpus.json),
[VM counterexamples](roadmap_v2_acceptance/compound_controls.json).

This is a bounded formatter correctness fix. It does not establish a general
effect model, prove API purity, or authorize moving index evaluations elsewhere
in the pipeline.
