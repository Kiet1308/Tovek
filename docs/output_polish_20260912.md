# Output readability after the V2 / v0.9 beta comparison

The comparison exposed three separate issues: a missing import-declaration guard, query names that included search qualifiers, and generated operator snapshots that the late cleanup refused without trying its existing evaluation-order proof.

The late call-chain pass now keeps `require` and `GetService` declarations when their result is used as a callable. This closes the gap between the callee and scalar-alias paths. Curried UI factories still reconstruct through the existing proof.

Weak get/find result-name hints omit whole PascalCase `From`, `With`, and `By` qualifiers after a nonempty noun; `On` is omitted only when a later `With` filter is present. For example, `FindPartOnRayWithIgnoreList` suggests `part`, and a collection populated with that result can suggest `parts`. Stronger getter hints such as `positionOnGround` and `assetsByType`, literal lookup keys, recorded source identifiers, and transform names retain their existing treatment. A location alone, such as `closestPointOnPath`, is retained too. This is a naming heuristic, not API resolution or type/effect evidence.

Generated single-use binary/unary snapshots can now enter the late cleanup's existing motion, evaluation-position, capture and conditional checks. For example, an unrecorded `local v = source * 2; return v + 4` can become `return source * 2 + 4`. Named/recorded bindings remain protected. Metamethods and exceptions still count as effects; no `math`, vector, or Roblox API is assumed pure.

This does not eliminate every readability regression. Global/member lookups and mutable captured callees still constrain motion. Statement-style conditionals and source-layout differences need separate presentation work. A shorter file or a higher structural similarity score alone does not establish correctness.

Validation results are recorded in `roadmap_v2_acceptance/output_polish_validation.json`. Fresh local outputs and detailed per-file reports are under `out/v2-output-polish`. At the user's request, `D:/Medal/V2-vs-beta-v0.9-20260912/V2`, its offline HTML report, metrics and comparison pages have also been refreshed to this validated build; the beta baseline is unchanged.

Final validation: 1,021 primary Rust tests plus one child repeat; 198 runtime profiles and their metadata replay; 513 public compile profiles. Six of the 405 commonly measured public profiles improved structurally and none regressed; 108 remain unmeasured. The private corpus changed in 60 of 3,978 files, with 83 fewer generated bindings and no increase in lines over 180 characters. All changed files parsed and compiled at O0/O2; unchanged files matched the accepted baseline hashes. Aggregate text size fell only 958 bytes: this is a targeted readability repair, not a broad recovery of the remaining math/conditional regressions.
