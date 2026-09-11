# Experimental arithmetic loop synthesis

This R5 experiment recognizes one exact family of finite accumulations. It is
**off by default**: equivalent bytecode does not establish that the author wrote
a loop. Enable it with `--synthesize-arithmetic-loops` in single-file,
`decompile-folder` or `validate-folder` mode, or with
`DecompileOptions::synthesize_arithmetic_loops` in the Rust API. Its flag bit is
`SYNTHESIZE_ARITHMETIC_LOOPS` (`1 << 5`); defaults and option union preserve the
explicit opt-in. Existing control-flow policy still governs CFG dispatchers.

For example, an eligible expression becomes:

```luau
-- equivalent fixed-count loop synthesized; original loop unknown
local total = 0
for i = 1, 4 do
    total += value * i
end
return total
```

The expansion is exactly `0 + value*1 + value*2 + value*3 + value*4`, associated
to the left. The pass does not replace it with `10*value`. Its marker classifies
the loop as synthesis, not a recovered original construct. Automatic enabling
requires stronger source-origin evidence and independent precision/recall.

## Eligibility and refusal

The positive-zero seed is checked by floating-point bits. There must be four
through eight consecutive terms, starting at one with unit step. Each term is
a multiplication of the same local by that iteration's exact numeric literal.
Uniformly reversed products (`i * value`) are accepted with their order intact.
Other seeds, mixed orientations, missing indices, reassociation, compound
operands and longer/shorter expansions refuse.

The expansion may occupy a sole scalar return or one nonparallel single-local
declaration. A consecutive chain of declarations may also qualify when each
right-hand side adds the next product to the preceding local. Every result
local must be written once and uncaptured. Discarded intermediates must each
have exactly one read, be distinct, and carry no debug source name conflicting
with the retained final result. No result may also be the multiplicand.
Observed intermediates, captured destinations, ordinary assignments and return
tuples refuse. A chain continuing beyond eight iterations refuses as a whole.

A named arithmetic helper has priority. Before rewriting, bounded patterns
from existing eligible helper declarations are checked for these sums. Any
matching iteration count vetoes loop synthesis throughout the module. This
deliberately conservative veto keeps the helper and its possible call sites
available to the later arithmetic de-inliner, even outside its lexical scope.

## Equivalence argument and limits

The introduced induction values are the exact numbers 1 through N; numeric
loop control performs no user operation on these constants. The accumulator
starts at positive zero. At each iteration the product is evaluated first,
then the next left-associated addition, preserving operand orientation,
rounding, signed zero, NaN, infinity, metamethod calls and exceptions.

The multiplicand is read again for every product. It may be a reference capture
changed by a previous metamethod: the pass never snapshots it as a helper
argument. The accumulator is private, so its intermediate assignments are not
observable through captures or aliases. Fresh locals are allocated only after
all eligibility gates and before final naming, which avoids shadowing existing
source bindings. No existing `break` or `continue` is enclosed by the new loop.

This argument covers program values, arity and the ordered effects in this
family. Stack/debug locations, instruction counts and timing are not preserved.
No purity assumption about arithmetic metamethods or type annotation is made.
No bytecode PC is invented for a fresh induction variable or accumulator.

The usage census and traversal are linear in the module. Each candidate search
examines at most eight terms plus one overflow check; each module can synthesize
at most 64 loops. Helper protection uses the arithmetic family's existing
bounded patterns. Exhausted loop budget leaves remaining source unchanged;
refusal allocates no new local identity. There is no unbounded candidate search.

## Evidence and reproduction

The pinned compiler is `c2ec0d4e5ca50796ba174a7565298f59aa572268`, with
`--fflags=false`, O0/O1/O2 and g1/g2. The runtime matrix includes original-loop
probes, a manually expanded source with no loop, and a named helper that must
remain preferred. `unrolled_capture` checks repeated mutation from `__mul`,
ordered `__add`, an exception at the third multiplication and floating-point
edge cases. It is also a source-origin negative control: synthesis preserves
observations while reducing similarity to the actual expanded source.

Use the pinned executables in place of the tool paths below:

```powershell
python scripts/roadmap_v2.py --compiler luau-compile.exe --luau luau.exe --ast luau-ast.exe --lifter out/lifter.exe --keep out/default-runtime --report out/default-runtime.json --determinism
python scripts/roadmap_v2.py --compiler luau-compile.exe --luau luau.exe --ast luau-ast.exe --lifter out/lifter.exe --lifter-arg=--synthesize-arithmetic-loops --keep out/loop-runtime --report out/loop-runtime.json --determinism
python scripts/reroll_witness.py --before out/default-runtime.json --after out/loop-runtime.json --report out/loop-witness.json
python scripts/provenance_fixtures.py --fixtures-report out/loop-runtime.json --lifter out/lifter.exe --keep out/loop-traces --report out/loop-traces.json
```

The provenance harness replays the recorded feature arguments. Runtime reports
and compiler witnesses retain hashes, options and both source variants. The
[implementation record](roadmap_v2_implementation.md) reports the measured
source-fidelity gains and loss, regression identities and experimental cost.
This experiment does not close general loop recovery or prove original-source
uniqueness. The broader R5 item remains open.
