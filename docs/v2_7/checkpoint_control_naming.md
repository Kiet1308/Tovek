# V2.7 control flow, naming and proof checkpoint

This checkpoint is part of the decompiler goal, not completion of V2.7. It is
based on `76212c9`, Rust `nightly-2026-06-15`, and Luau
`c2ec0d4e5ca50796ba174a7565298f59aa572268`.

## Changes

- Recover a `while` header from its first exact break guard, including rebuilt
  calls. Preserve continue behavior, condition evaluation order and NaN.
- Use two atomic arms in default-style assignment/return if-expressions when
  the entire line fits 72 columns, including indentation and rebuilt-call
  comments. Keep call arms as statements to preserve return arity. A declaration
  cannot absorb a condition that reads its own binding.
- Record generated, weak type, evidence and source name tiers. Event-role
  evidence beats `.Parent` stores; consumer evidence can replace weak type
  names. Version the conservative Roblox naming table. Preserve `vector2` and
  `vector3` with scope-local collision suffixes.
- Refuse debugger BREAK, invalid CONCAT/constant operands, missing generic-for
  partners and invalid open-result lists before lifting.
- Add exact vector literal arithmetic to the existing fold proof: recorded
  float precision, signed zero, XYZ access and forward-checked inverse
  proposals. No vector constructor or mutable library call is evaluated.
  This is a prerequisite; expression matching of the reflect example is still
  open and is not claimed fixed here.

Four public semantic fixtures cover the new boundaries. The existing default
style audit now checks the actual D3 rule instead of forbidding all
if-expressions. It still rejects nested or non-atomic arms and lines over 72
columns; compact mode retains its separate policy.

## Validation

- Workspace: 1,652 passed, 0 failed, 2 ignored.
- Every command in the Fixture and Luau audit job passed, including 711/711
  semantic executions, 295/295 deep-review cases, bytecode versions, Wasm,
  binding/capture/lineage controls and 513/513 public-source cases.
- Fixed-seed mutation/reduction fuzz: 206 passed, 23 refused, 11 invalid inputs.
- Output-size baseline: 718 outputs, including 12 new fixture/profile outputs.
- Semantic bytecode oracle: 237 inputs, 295 non-equivalent prototypes; zero
  files exceed the capture tier. The oracle is a bounded diagnostic, not a
  semantic equivalence proof.

Fresh before/after binaries, with the original source inputs frozen:

| workload | outputs | changed | lines before → after | nonblank before → after | rebuilt annotations |
|---|---:|---:|---:|---:|---:|
| original semantic fixtures | 699 | 52 | 41,116 → 40,819 | 34,638 → 34,428 | 802 → 802 |
| pinned public sources | 513 | 36 | 38,831 → 38,629 | 31,955 → 31,808 | 72 → 72 |

No individual output loses a rebuilt-call annotation. Public differences were
reviewed: short value conditionals and vector naming account for the changes.
Public module parsing/dataflow checks do not replace execution in Roblox.

The previous committed oracle baseline had 300 non-equivalent prototypes;
the fresh original binary has 292 on the original 233 sources. The new binary
has 289 on those sources, plus 6 on the four new sources. One existing file,
`locals_past_the_limit_take_if_expressions`, changes from 0 to 1: replacing
`local x; if c then x = a else x = b end` with an if-expression removes one
dead `LOADNIL`. Its normalized instruction delta has no added instruction,
523 common instructions, and no new capture excess. All three runtime profiles
pass. This intentional D3 delta is recorded in the regenerated baseline;
neither the oracle gate nor its comparison rules were relaxed.

## Limits and remaining work

The owner's four frozen dumps (9,987 inputs), local design/review notes and
original-machine timing are unavailable. These results cover the available
public workload only. No private-corpus or performance claim is made.

The remaining roadmap includes table layout/placeholder proofs, unanimous phi
naming, minimized structurer/SSA refusals, vector expression reconstruction,
performance and M6. Release, version bump, website work and merging main remain
outside the requested scope.
