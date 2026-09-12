# R5 reconstruction study and source-site limits

The study contains fourteen small source families, each compiled at O0/O1/O2
and g1/g2 with Luau commit `c2ec0d4e5ca50796ba174a7565298f59aa572268`
and `--fflags=false`. The source, driver, bytecode hashes, caller operands,
compiler remarks and 162 observations/profile were locked before the first
decompiler run. The manifest remains unchanged at SHA-256
`bfd98a9fba874e42351ed544d76081923988fcb528f7ec23ff2dbfecb45dafbf`.

The first measurement used binary
`82a4cd944a94483b8dda3ed2a153da4fad8d36e3ef013705d8187d52b0f0196f`.
After inspecting its missed helpers, development added early binder retention,
scalar region matching and protection for competing helper definitions. Later
runs explicitly carry `evaluation_use: unblinded-regression`. The historical
manifest name contains “holdout”; that name does not make later measurements
independent. No target was lowered or failing family removed.

## Known-source callsite review

Six families have two returned-result roles. The reviewer follows local binding
identities to each returned result, compares helper arguments by caller parameter
slot/literal, and joins the emitted call's exact byte span to its producer event
and callee prototype. It does not align calls merely by their counts. There are
16 source calls removed by O2 across g1/g2: two each in `helper_let`, `helper_phi`,
`helper_guard` and `ambiguous_helpers`. Eight additional result slots in
`handwritten_shape` and `missing_prototype` are negative source-call examples.

| Measurement | True positive | False positive | False negative | Precision | Recall |
|---|---:|---:|---:|---:|---:|
| Initial holdout | 0 | 2 | 16 | 0% | 0% |
| After unblinding/development | 6 | 4 | 10 | 60% | 37.5% |

The two initial false positives occur in the g2 handwritten family; looking only
at g1 would have missed them. After development, `helper_let` recovers both calls
at g1; g2 retains the recorded product/result declarations. Phi and guard families
recover their dynamic-condition call at both debug levels. Their branch-only
specializations remain lowered. Both missing-prototype profiles and both
ambiguous-helper callers remain unchanged. Competing definitions are preserved.

The four later false positives are the handwritten expressions, which really
have the same computation as the retained helper. They count as false positives
for **predicting original source calls**, even though the replacement has an
equivalence argument and passes runtime observations. Metadata therefore says
`equivalent_call_inference`, never “original call recovered with certainty”.
These small, deliberately difficult cases do not establish general callsite
precision. In particular, 60% is a development result, not a new holdout score.

## Fixed-count loop study

The remaining eight families cover unit stride, stride two, reverse traversal,
zero iterations, last-result observation, per-iteration closure captures,
observable body calls and a mutable captured result. O2 emits an unroll remark
for six families: unit stride, stride two, reverse, zero, last-result and effects.
Capture/mutable-result cases exercise retained-loop boundaries instead.

The optional sum synthesizer accepts only its documented 4–8 term family with
positive-zero seed and consecutive indices 1..N. The unit-stride case is its
positive witness; all other families retain their lowered or original form.
The source loop's multiply/add orientation, scalar result and indices are
compared in the source/output gallery, with runtime traces and a compiled mutant
for each profile. An unused helper parameter cannot form a callable pattern and
therefore cannot veto this optional synthesis.

At O2/g1,g2 the intended eligible-family coverage is 2/2, while coverage of the
six compiler-unrolled source-loop families is only 2/12. Those denominators are
kept separate. Neither number proves a unique source loop: a handwritten sum can
produce the same input. Default mode performs no arithmetic loop synthesis.
Stride/reverse, zero/last and effect/capture examples remain research refusals;
there is no new general-purpose loop re-roller or algebraic simplifier.

## Validation and reproduction

All 84 profiles are checked in default and opt-in synthesis modes, at one/four
threads, against 162 observations each. Each variant's complete emitted module
is recompiled in the same original compiler profile. All 84 deliberately wrong
variants must compile and change observations. The older seven-family,
42-profile compiler witness suite remains an additional development regression.
Dataflow classifications remain separate from these finite VM checks.

The [acceptance inventory](roadmap_v2_acceptance/ordered_reconstruction_validation.json)
contains binary/source/report hashes, initial and later reports, source-site
alignment and selected source/output galleries. The review is replayable with:

```powershell
python scripts/reconstruction_study_review.py `
  --initial docs/roadmap_v2_acceptance/ordered_reconstruction/initial-study.json `
  --initial-work docs/roadmap_v2_acceptance/ordered_reconstruction/initial-gallery `
  --current docs/roadmap_v2_acceptance/ordered_reconstruction/study.json `
  --current-work docs/roadmap_v2_acceptance/ordered_reconstruction/gallery `
  --ast D:/Medal/luau-tools-src/build/luau-ast.exe --report out/study-site-review.json
```

This finishes the bounded implementation and research items in R5. Recovery of
arbitrary specialization, original source uniqueness and broader loop coverage
remain limitations of the approach, not properties established by passing CI.
