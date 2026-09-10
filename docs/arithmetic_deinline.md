# Bounded arithmetic helper reconstruction

`ast/src/expr_deinline/arithmetic.rs` adds a separate family to expression
de-inlining. It recovers both `adjust` calls in `helper_loop` at O2/g1 and O2/g2:

```luau
local first = adjust(value, 3)
local value2 = value + 1
local second = adjust(value2, 3)
```

The addition stays at its original evaluation point. The helper definition
keeps its statement `if/else`; selection patterns exist only inside the matcher.
The four-iteration sum still remains unrolled.

## Eligibility and proof

The helper must have a valid compiler-recorded name and bytecode prototype ID,
a fixed arity of one to eight parameters, and a binder written exactly once
across the module, including nested closures. The existing lexical scope and
self-call guards apply. Its body must consist entirely of scalar returns and
return-only branches, including an early return followed by a scalar return.

Allowed expressions contain only parameters, finite number/boolean/nil literals,
arithmetic/comparison/logical operators, unary negation/not, and selections.
There are no free locals, global reads, explicit calls, indexing, constructors,
varargs, or result packs. Type annotations do not establish purity: arithmetic
and comparisons may still invoke metamethods or raise errors.
The exact positive/negative pi constants are also excluded because the current
formatter emits them as `math.pi` lookups. Nonfinite literals are excluded;
the formatter emits infinities through `math.huge`. Moving such syntax into a
different function could change the environment being read. Runtime NaN or
infinity values held in local arguments remain eligible.

The matcher preserves operator kinds, operand order, literal bits, and branch
polarity. Every parameter must bind consistently to a local or literal. A
module-wide capture census rejects arguments captured by reference anywhere,
even if the captured writer appears after the candidate. This prevents an
operator's metamethod from changing a local between its repeated reads while
the reconstructed helper would have snapshotted it at entry. Stable local reads
and literals are total and produce the same identity on every evaluation;
eager argument reads therefore add no effect or exception.

An internal `if c then a else b` can match `c and a or b` only when the actual
middle expression is a number literal or literal `true`. Nil, false, unknown
locals and operator results cannot discharge this truthiness check. Both
branches and the condition must then match exactly. The matcher does not fold
arithmetic, reverse comparisons, reorder sums, or assume that numeric
annotations suppress metamethods. Two matching helpers make a site ambiguous
and leave it unchanged.

These conditions prove a scalar call equivalent within the declared semantics
(excluding debugger/stack observations, resource exhaustion and allocation
timing). They do not prove that the original source used this helper at that
location. Definitions with new calls receive this separate classification:

```luau
-- equivalent arithmetic calls inferred from this bytecode helper; original call sites unknown
```

## Cost and bounded work

Patterns need at least three operator/selection nodes. Replacing a site must
save at least four expression nodes, counting the call and its local callee.
The existing global/string-anchor threshold remains in force for the other
expression family.

Each arithmetic pattern and candidate has at most 64 expression nodes;
statement-return selection depth is limited to four. At most 32 arithmetic
targets are retained. If more qualify, the entire new family is disabled for
that module, since truncating the target set could hide an ambiguous match.
At most 8,192 arithmetic match attempts run per module. Exhausting that budget
refuses the current site, including a tentative hit whose remaining rivals
could not be checked. Earlier proved sites remain valid.

These are deterministic work bounds for the new search, not a wall-time or
whole-program memory cap. The shared lexical traversal and capture census are
linear in the input AST and keep a set of reference-captured locals. Existing
de-inline families retain their existing limits. No line/PC prefilter or
general constant evaluator is added here.

## Reproduction and limits

The runtime matrix pins Luau commit
`c2ec0d4e5ca50796ba174a7565298f59aa572268`, O0/O1/O2, g1/g2 and
`--fflags=false`. `arithmetic_effects` checks scalar results, signed-zero inputs,
NaN/infinity, ordered comparison/multiplication metamethods, mutation of a
table, and a throwing multiplication. `arithmetic_capture` requires refusal:
its comparison changes a reference-captured argument, so the correct result
is 23 with trace `lt:1,mul:10`; snapshotting the argument would produce 5.

After running `scripts/roadmap_v2.py` for both binaries with that manifest:

```powershell
python scripts/arithmetic_witness.py --before out/v2-arithmetic-before-runtime.json --after out/v2-arithmetic-final-runtime.json --report out/v2-arithmetic-witness.json
```

The witness audit verifies tool/source/output hashes, passing runtime reports,
AST call counts, and original compiler remarks. It records both original and
recompiled disassembly in the same compiler context. Original `helper_loop`
remarks confirm two inlines and one four-iteration unroll; output restores the
two calls. `arithmetic_effects` restores one call; its second result has become
statement-level control flow and remains unchanged. The capture fixture gains
no call. These are development fixtures, not an independent precision/recall
estimate. Ordered dataflow may remain `unknown`; compiler remarks and successful
recompilation do not turn that into proof.

Missing prototypes, arbitrary specialization, statement result aliases, loop
re-rolling and source-line candidate discovery remain separate roadmap work.
