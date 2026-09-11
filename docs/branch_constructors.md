# Private property diamonds

The v9 emitter can group a private table's initial fields, one conditional
property and contiguous following field writes. The conditional value stays in
explicit `if/else` statements. It is evaluated once and assigned to a fresh
scalar local before the table declaration. This handles the stripped-name
`branch_ui` fixture without introducing an `if` expression.

The accepted region is deliberately small: an adjacent single-local table
declaration and an `if` whose arms each assign the same literal key of that
table. A nil/NaN/dynamic branch key, a different arm shape, an observable or
captured table, or a compiler-recorded table binding prevents reconstruction.
Preserving the recorded declaration also preserves its source structure and
parser alignment. Library names and type hints provide no permission to move
calls or reads.

Every initializer that may observe mutable state, call, index, operate on values
or fail is evaluated into a scalar snapshot before the condition, in its
original order. Only literals, uncaptured locals in the same function frame and
recursively inert private tables can be delayed. Incoming captured values are
snapshotted. Capture inventory covers closure occurrences in assignment bases
and keys as well as ordinary RHS expressions. The target cannot occur in the
condition, either value, or its own initializer.

Initializer keys must be total literals. An expanding final array call or
vararg prevents adding fields. Existing entries are kept, including duplicates;
old initializer effects are not discarded when the conditional overwrites a
key. Only following contiguous writes to this exact table are merged. Their
key/value/store order is retained, and a read or capture of the table stops
merging. Existing SETLIST fallback and general UI passes are not rerun after
the snapshots.

Anonymous functions in initial fields or either selected value retain their
table layout. Corpus review found that extracting a numeric dispatch table's
callbacks created several weakly named helper functions and obscured their
field context. The layout refusal is structural, applies to every such table,
and is not an effect or equivalence proof.

## Bounds and provenance

The full input tree is validated before recursive mutation: at most 200,000
nodes and depth 128. A second bounded scan reserves names and collects capture
identities only when a candidate exists. There are at most 256 rebuilt regions
per script. Function budgets count unused parameters, declarations in disjoint
scopes, hidden loop registers and expression scratch space, with conservative
limits of 192 locals and 240 registers. Opaque SETLIST/control lowering prevents
adding locals to that function. All planned snapshots and the selected local
must fit before any statement changes.

The pass runs after expression cleanup and before final scalar-conditional
lowering and naming. It never mutates shared original arm blocks. New locals
receive no copied source/debug, SSA, close or ownership evidence. The optional
`branch_constructors` sidecar reports model
`luau-v9-private-property-diamond-v2`, candidate/rebuilt counts, snapshots, fresh
locals, folded fields, refusal reasons and budget exhaustion. Counts do not
constitute a producer-to-binding provenance ledger; that R2 work remains open.

## Validation and limits

Twelve native tests cover the accepted shape, sequential diamonds, invalid and
dynamic keys, open result tails, captured state, captures in assignment
addresses, source bindings, callback layout, name collisions, shared blocks and
atomic tree/local/register/region budgets. All 971 primary Rust tests plus one
child repeat and 93 Python tests pass. Shared conditional-lowering helpers also
retain all 240 direct IR/profile checks and 18 independently compiled mutants.

The new [fixture](failure_fixtures/roadmap_v2/branch_constructor_order.luau) and
[driver](failure_fixtures/roadmap_v2/branch_constructor_order.driver.luau) lock
1,470 observations per O0/O1/O2, g1/g2 profile. They cover private, exposed and
captured tables, captured initializer mutation, effectful initialization,
dynamic keys and sequential diamonds. Nil/false values, overlapping and nil/NaN
keys, multi-result calls, skipped branches and caught failures remain visible.
Each source body is compiled inline into its runner at the specified profile,
with fast flags disabled.

[Independent controls](../scripts/branch_constructor_controls.py) compile six
intentional mistakes at all six profiles. Late initial evaluation changes 210
vectors, delaying an exposed/captured table changes 180 each, reading captured
state late changes 36, eager branch evaluation changes 210 and truncating the
return pack changes 54. All 36 mutants are detected; the six original controls
match. These are finite behavioral checks, not a whole-program proof.

The full matrix passes 180 runtime configurations and nine existing controls,
513 public configurations, 45 legacy semantic configurations and 52 size gates.
All 3,978 private outputs remain byte-identical, including 42 empty inputs;
3,936 detailed sidecars retain recorded contracts and capture certificates.
The private inventory has 165 candidates: 42 captured tables, 121 incompatible
branch shapes, one local/register refusal and one callback-layout refusal.
None are rebuilt. No public/private fidelity improvement is claimed.

The previously accepted `branch_ui` outputs change only at g1. At g2 their
recorded table declarations remain intact. The g1 whole-chunk symbolic verdict
changes from `proved` to `different`: allocation moves across the condition and
the compiler chooses a different table-construction sequence. That verdict is
retained. No verifier normalization or baseline promotion was added for this
rewrite. The runtime and static checks above have a smaller scope than general
VM equivalence, including table allocation and unspecified iteration layout.

All 171 other prior runtime outputs and all 513 public outputs retain their
source bytes, full dataflow results and fidelity measurements. Their sidecars
are identical after removing the added constructor report; all prior runtime
and public recorded-name/capture contracts remain equal. Parser-backed emission
checks and uncached/cold/warm artifact-cache comparisons agree at one/four
threads for all 180 runtime and 513 public configurations. The six new fixture
whole-chunk dataflow results remain `unknown`.

Seven interleaved warm CLI rounds compare the accepted nested-assignment release
with this binary on the same 3,978 private inputs. One-thread median changes
23.478 -> 23.613 seconds (+0.57%); 16-thread median changes 1.818 -> 1.719 seconds
(-5.48%). Median peak RSS changes 33,132,544 -> 33,153,024 bytes and
118,071,296 -> 116,449,280 bytes respectively. Seven-sample p95 equals the maximum:
24.919 -> 24.023 seconds and 1.920 -> 1.970 seconds. All output trees remain equal.
These measurements record cost and variability; they do not establish an
algorithmic speedup, OS cold-cache behavior or allocation reduction.
[Raw samples](roadmap_v2_acceptance/branch_benchmark.json).

The [acceptance inventory](roadmap_v2_acceptance/branch_validation.json) records
tool hashes and every archived report, including original-profile controls,
runtime/public comparisons, provenance/cache replay, private refusals and the
full current SETLIST scan.

This does not complete a general statement/store/alias dependency graph. In
particular it cannot join constructors separated by selected-local declarations,
arbitrary statements, callback-bearing fields or opaque SETLIST regions. The
remaining UI fallback inventory records those boundaries separately.
