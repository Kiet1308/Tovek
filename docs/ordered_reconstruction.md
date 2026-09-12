# R4/R5: ordered expression motion and bounded call reconstruction

The late UI pass now uses an explicit sequence of evaluation events before
moving a single-use field or scalar-call alias. The sequence distinguishes the
callee, receiver, arguments, LHS base/key, RHS and final stores. A dot-call's
lookup precedes its arguments; Luau NAMECALL lookup follows argument evaluation.
Conditional arms remain conditional. Captured reads conflict with possible cell
writes, and observable evaluations cannot cross each other. Scope, intervening
statements, single-use counts and scalar/multret checks remain separate gates.

This extends the existing [curried UI rules](ui_tree_rebuild.md) and
[private constructor regions](constructor_regions.md). No Fusion/Roact/API name
confers purity or ownership. A factory chain and its props/children can join
only while the intermediate objects remain unobserved and uncaptured. Recorded
source bindings and named require/GetService headers remain protected.

For example, `local field = object.field; return field` can become
`return object.field`. A snapshot read before `object.mutate()` stays in place.
An earlier call cannot move past a later LHS base/key lookup. A last-position
scalar call retains parentheses so its additional return values do not escape.
Math/vector constants emitted using environment lookups remain barriers too.

Runtime guard refinement records exact primitive equality facts inside the
appropriate branch: nil, booleans, bounded strings and finite nonzero numeric
literals other than ±pi. Bare truthiness does not distinguish nil from false;
zero equality does not distinguish +0 from -0. Reference-captured cells are
excluded, assignments kill facts, branches intersect facts and loops invalidate
written locals. Type annotations and calls named type/typeof establish no fact.
The emitter retains statement-style conditions and branch returns.

## Reconstruction and its evidence

A small raw-bytecode filter retains prospective named scalar helper binders
through SSA, before parallel child bodies exist. It accepts only fixed 1–8
parameters, no upvalues, at most 128 instructions, scalar returns and a bounded
arithmetic/selection vocabulary. The flag only refuses an early inline. The
later AST pass independently validates the helper and each replacement. An
unused candidate can still be consumed by later ordinary cleanup.

Named arithmetic matching runs before export-table cleanup as well as at the
existing late point. Both helper and caller regions can normalize single-use
lets, branch returns and a private scalar phi result. Lets are substituted once
at a proven evaluation position. A phi result used later remains a declaration
initialized by the equivalent scalar helper call, preserving its binding.
False and nil arms remain distinct. Parameter writes, open return arity,
observable skipped lets, extra branch statements and captured result cells
refuse. There is no reassociation or algebraic simplification.

All de-inline families now independently reject eager arguments that read
reference-captured cells. Written-parameter prefix copies must preserve original
argument order, cannot depend on removed copies, and retain their original
single-result adjustment. Some old reconstructed calls therefore expand back
into explicit statements: upstream SSA behavior is no longer treated as proof
that an indirect captured mutation cannot occur.

Prototype identity, lexical scope and capture identity remain required by their
respective matcher. Shared input source lines produce caller/helper prototype
and PC-region search hints, which prioritize candidates without discarding
rivals. A line match cannot resolve semantic ambiguity. No hint is an original
callsite certificate. The sidecar keeps those ranges separate from call events.

Reconstruction metadata version 2 classifies de-inline events as
`equivalent_call_inference` and generated helpers as `synthesis`. Source comments
say that original callsites are unknown. Historical version-1 reports remain
readable without gaining new evidence. A missing helper prototype or multiple
valid helpers does not justify inventing an original helper/call. Optional loop
synthesis remains separately labeled and disabled by default.

## Resource limits and fallback

The event graph stops at 4,096 nodes, 8,192 events or depth 128. Reconstruction
preflight stops at 200,000 nodes, 8 MiB of string payload or depth 128, including
wide-container checks before allocating traversal lists. Helpers have a 2,048
node limit; statement/expression candidate sets are limited to 256. Statement
fixed points stop after 64 iterations. A shared deterministic fuel counter
allows 20 million charged work units per invocation. Arithmetic matching adds
32 helpers, 64 normalized nodes, eight statements/depth and 8,192 attempts.
Line hints stop at 200,000 PCs, 4,096 prototypes, 8,192 regions and 16 owners/line.

Fuel bounds search work independently of thread scheduling; it is not a hard
wall-clock or RSS limit for the entire decompiler. Validation additionally uses
subprocess timeouts. If an ambiguity scan runs out of fuel, its current candidate
is not committed. Previously completed rewrites retain their evidence; untouched
regions retain the existing lowered output. Missing/excessive line metadata
falls back to structural candidate ordering.

## Validation interpretation

The ordered-reconstruction fixture locks 192 runtime observations per compiler
profile, covering lookup/store order, snapshots, scalar tails, false/nil, signed
zero, metamethod orientation and indirect capture mutation. Seven deliberately
incorrect variants must compile and change observations in all six profiles.
The preceding constructor-region release fails the new fixture at all three
g1 optimization levels: reconstructing the predicate eagerly snapshots a
reference-captured cell before a callback changes it. The revised capture gate
passes all six profiles. Its more conservative private output is therefore
reported alongside an executable counterexample to the old assumption.
Existing UI fixtures additionally cover partial initialization, nil/NaN keys,
overlapping numeric keys, multret, skipped calls and captured constructor state.

The 14-family reconstruction study was locked before its first evaluation with
manifest SHA-256 `bfd98a9fba874e42351ed544d76081923988fcb528f7ec23ff2dbfecb45dafbf`.
That first result recovered no helper calls in its O2 helper cases. Inspection
then motivated the early-binder fix, so subsequent results are development
regressions, **not an independent holdout estimate**. The initial result remains
archived. The separate public repository holdout keeps its original split.

Each study output is recompiled as a complete module with the same pinned
compiler, optimization/debug profile and flags. This is supplementary contextual
validation; compiler remarks, call counts and finite VM observations cannot
promote dataflow `unknown` or `different` to `proved`, or certify an original
source call. Detailed results and limitations belong in the acceptance inventory.

R9 stays paused. AI/model execution remains off by default; this implementation
does not use AI, download models or publish model/profile/binary artifacts.
