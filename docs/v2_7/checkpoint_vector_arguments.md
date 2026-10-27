# Vector constant arguments

The expression matcher now uses the shared exact fold proof after binding all
arguments structurally. A folded component is accepted only when evaluating
the bound literal reproduces its bits, including signed zero. It does not
guess missing arguments or evaluate library calls. Finite bytecode vector
literals are stable values; syntactic constructors remain calls.

The O2 `deinline_vector_arguments` fixture now reconstructs both
`reflect(value, vector constant)` calls. Its mutable constructor counterexample
keeps the lookup and call. Unit controls also refuse incorrect components,
signed-zero mismatches, unknown arguments and constructor calls.

Validation against `927885c`, with the same pinned Rust and Luau toolchains:

- Workspace: 1,655 passed, 0 failed, 2 ignored.
- All 33 Fixture and Luau audit steps passed, including 711/711 semantic
  executions and 295/295 deep-review cases.
- The 513 pinned public outputs are byte-identical.
- Of 711 semantic outputs, only the vector fixture at O2 changes: two helper
  calls restored, two lines removed, one additional line annotation covering
  the two calls. No output loses an inferred-call annotation.
- The semantic oracle remains at 295 non-equivalent prototypes, with no
  per-file increase. Output-size and oracle baselines were regenerated and
  their gates rerun.

The private corpus remains unavailable. Fully constant helper results continue
to follow the existing constant-copy ledger policy; this change proves
structural expression matches with already-bound arguments.
