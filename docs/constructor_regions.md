# Private constructor regions across ordered statements

The UI/table fixed point can delay an unobserved private table declaration until
its first foldable property or SETLIST write. This recovers list constructors
when conditional local computation or unrelated field construction separated
the original allocation from its entries. The existing builder then retains
fixed values as scalar entries and the last expanding call as the list tail.
No conditional expression is introduced into emitted source.

## Motion contract

The new region rule requires a fresh single-local table declaration with no
compiler-recorded binding and no capture anywhere in the input function tree.
Initial values are limited to literals, uncaptured local reads and nested tables
whose keys are valid literals. Calls, global/field reads, operators, callbacks,
dynamic or nil/NaN keys, and open result tails in that initializer are refused.
The earlier adjacent-field and local-declaration rules retain their own gates.

For every crossed statement, the rule checks the target's identity at all
expression positions, including nested LHS base/key and closure captures. A
read, alias or write of that target stops the search. Any assignment to a local
used by the initializer also stops it, in either arm of every crossed `if`.
Captured initializer dependencies are refused even if no direct write is
visible, because intervening calls can mutate them through another closure.

Only assignments, calls, method calls, comments, empty statements and structured
`if` regions can be crossed. Their expressions, local declarations, branch
selection, calls and stores are left in the same order and scopes. Loops,
return/break/continue, close markers and other opaque control or SETLIST nodes
are barriers. The destination must be the first eligible field or list write
to this exact target, and it must pass the existing constructor builder's
read/arity/index checks. A direct closure property is a layout barrier so
statement function definitions retain their form. A trailing annotation immediately after the moved
declaration prevents motion so its statement association is retained.

The dependency proof permits stores to unrelated objects: before its first
observation, a fresh uncaptured table has no alias through which these stores
or calls can inspect it. Stable local initializer values retain their exact
values; no field read is delayed. Only the inaccessible allocation moves. As
with existing total-table rebuilding, allocator failure/timing and GC addresses
are outside this source-equivalence contract.

Each proposal searches at most 64 following parent statements with a 4,096-node
proof budget and depth limit 32. Failure leaves that proposal unchanged. The
rule introduces no locals, snapshots, closure clones, cached analysis or new
source/prototype certificates. It does not inspect UI library names or treat
type annotations as effect evidence. The optional pass profiler counts committed
moves as `constructor_regions_sunk`; this is a rewrite count, not time saved.

## Validation contract

Five AST test families cover selected local computation, unrelated nested LHS
evaluation, nil overwrite and scalar/expanding entries, capture and dependency
writes in either arm, recorded bindings, invalid/effectful initializers and
budget fallback, and statement function definitions. The existing UI/SETLIST
ordering tests remain applicable.

The pinned `constructor_regions` fixture has seven source shapes and 1,386
observations at each of O0/O1/O2 and g1/g2. The source observations agree across
all six profiles and are locked before comparing the new executable. They
exercise false/nil values, nil/NaN keys, selected and skipped branches, table
observation, callback capture, a mutated initializer dependency, ordered
base/key/RHS/store evaluations, dynamic API lookup, errors and multret tails.
The runner compiles the actual subject inline at the matrix profile.

`scripts/constructor_region_controls.py` compiles seven deliberate mutants at
all six profiles: eager arm evaluation, truncated tail, lost nil overwrite,
delayed effectful initializer, delayed captured snapshot, delayed observed
table and RHS evaluated before LHS address. All 42 must change a locked runtime
observation. These finite observations establish driver sensitivity; they do
not turn whole-chunk unknown/different dataflow results into proofs.

## Accepted output and results

In the known-source nested fixture, the selected property remains an explicit
`if/else`. Factory lookup and the first child call still occur before it. The
later separated props writes and packing loop become a nested constructor:

```luau
local consume = data.consume
local first = data.make("prefix")
local factory = data.factory()
local selected
if data.condition() then
    selected = data.make("then")
else
    selected = data.make("else")
end
return consume({ first, factory({
    Name = "Panel",
    Selected = selected,
    [data.key()] = data.make("property"),
    Handler = function() return data.make("callback") end
}), data.tail() })
```

The displayed local `first` is explanatory; the exact before/after emitted
fixture files are included with the acceptance artifacts. The final call's
multret tail remains open. The fixture's six output profiles shrink by
191–192 bytes and retain all locked observations; all six dataflow results
remain **unknown**.

The full run passes 988 primary Rust tests plus one child repeat, 112 Python
tests, 186 runtime profiles, 513 public profiles, 42 compiler-witness profiles,
45 legacy semantic profiles and 52 size gates. All 180 previous runtime rows
remain identical after excluding timing. Public holdout outputs (45 profiles)
are unchanged. Five public development profiles change: Spring's state fields
are grouped into a constructor at O0/O1/O2, while table-util's empty module
declaration moves nearer its export assignments at O0/O1 with unchanged source
fidelity metrics. Its source and function assignments are retained.

Spring's raw structural ratio changes 0.6304→0.7310 at O0, 0.7673→0.7388 at O1,
and 0.7704→0.7539 at O2. The lower O1/O2 scores are retained: grouping the
constructor does not establish universal source-similarity improvement. The
aligned exact-name count changes 2→3 without changing identifier spellings,
so this is an alignment change, not newly recovered naming evidence. Existing
dataflow classifications remain unchanged; these Roblox library modules are
not executed by the public harness.

Private output changes in **50/3,978 files**, with 3,928 unchanged. Packing loops
fall from **23 to 15**, across 14 remaining files. Removed sites are Shine,
DamageIndicator, Trait, CollapsedStatLabel, StatLabel, UtilityFunctions,
DragonSecondaryButton and SplitTextLabel. Other changes group props, children
and stable state fields. Every final changed source matches its reviewed
development preview; 14 module/function-layout changes from that preview are
refused by the final rule. Two UI callback fields can still join their related
data fields through the existing builder after the selected value is computed;
the new rule refuses starting a region at a direct closure property.

All 50 changed private files and seven residual fixtures parse and compile at
O0/O2. All 3,936 nonempty private artifacts pass independent parser checks:
495,186 mapped local tokens, 2,182 tokens within declared opaque regions and
zero unexplained tokens. Full source and sidecars agree at 1/16 threads, and
default source matches detailed mode. The 4,574 unaffected prior sidecars
(180 runtime, 508 public and 3,886 private) are byte-identical. Runtime/public
default/compact comments, parser/capture checks and cold/warm artifact cache
also agree. The private corpus has no known original-source or Roblox runtime
claim.

The accepted normal executable is identified by SHA-256 in the
[acceptance inventory](roadmap_v2_acceptance/constructor_regions_validation.json),
alongside all reports, exact fixture outputs and remaining packing locations.
The native Windows private source tree is
`7726b21dc807cf6b421ac7dda1e8e556551cd1198145d5e6a2f866d1f5727475`;
portable UTF-8 path ordering uses a different hash. Export timings were collected
during correctness work with overlapping jobs and are not speed evidence.

Performance experiments remain deferred in favor of R2/R4/R5 output quality;
R9 remains paused and AI remains disabled by default.
