# Equality operand order during SSA inline

Inlining an earlier temporary into the right operand of a comparison can move
its evaluation past the left operand. The SSA inliner previously compensated
by swapping both operands. For `<`, `<=`, `>` and `>=`, reversing the operator
as well preserves the ordered VM comparison. For `==` and `~=`, swapping the
operands changes the arguments passed to `__eq`.

The corrected gate permits equality reversal only when the inserted value is
a nil, boolean, number or string literal. Otherwise the temporary remains when
an earlier observable evaluation blocks ordinary substitution. Type hints do
not prove the absence of a metamethod. The gate does not infer API purity or
authorize arithmetic commutation.

```luau
local right = fetch("right")
return fetch("left") == right
```

The old output evaluated `fetch("right") == fetch("left")`. The call order was
unchanged, but a shared `__eq` received `right, left` instead of `left, right`.
The fixture checks equality and inequality, the call trace, the result, and an
error thrown only for the original operand order. The old binary fails all
three g1 configurations; full debug bindings already preserved the temporary
in the three g2 configurations. The corrected binary passes all six, each with
a bounded dataflow certificate. Rust tests also retain relational inversion
and primitive-literal equality cases.

All 144 runtime configurations and nine negative controls pass: 112 dataflow
results are proved, 18 unknown and 14 different. The last two groups are not
equivalence certificates; their previous classifications are unchanged.

Two of 513 public outputs change: Roact `createSpy` at O1/O2. Its original
`assert(self.values[i] == expected, "value differs")` had been emitted with the
comparison operands reversed. A harness executes the exact source and output
module bodies with a stub for the unused deep-comparison dependency. The source
and corrected output agree on the ordered `__eq` trace and caught error; both
old outputs disagree. This is a focused module test, not a Roblox-wide runtime
claim. Whole-chunk validation remains unknown for both configurations. All
existing public bounded certificates are preserved, including the holdout.

Of 3,978 private files, 3,977 remain byte-identical. The only changed file is
`ReplicatedStorage/FusionPackage/Components/Menu/Summon/init.luau`. The original
prototype 20, PC 21 compares the last call result on the left with the earlier
conditional call result on the right. Recompiled output now keeps that order.
The archived instruction excerpt is a manually reviewed witness; the complete
chunk remains unknown, and the module has not been executed in Roblox.

Optional source/provenance maps pass independent parser checks on all 144
runtime and 513 public configurations and remain deterministic at one/four
threads. Existing semantic, size and workspace test results, exact tool hashes
and the seven-round paired benchmark are recorded in the
[validation inventory](roadmap_v2_acceptance/equality_validation.json),
[runtime matrix](roadmap_v2_acceptance/equality_runtime.json),
[changed-output audit](roadmap_v2_acceptance/equality_corpus.json) and
[benchmark](roadmap_v2_acceptance/equality_benchmark.json).

This closes the equality-reversal defect. The broader R4 effect model and
positioned dependency analysis remain separate work.
