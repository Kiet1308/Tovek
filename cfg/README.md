# CFG analysis and SSA processing

The CFG still stores the existing source AST in block weights. These components
reduce analysis overhead without replacing the rules for expression movement,
captured cells, phi transfers, result arity, source bindings or provenance.

## Adaptive liveness

`ssa/destruct/liveness.rs` computes the same least fixed point with two backends:

```
out[B] = edge_argument_uses[B] union
         union over successor S of (in[S] minus parameters[S])
in[B]  = (statement_uses[B] union out[B]) minus statement_definitions[B]
```

Small functions keep the dense word solver. With `B` blocks and
`W = ceil(local_count / 64)` words per row, it needs six dense matrices at peak.
Dense storage is preferred while `6 * B * W * sizeof(u64)` fits in 8 MiB; the
calculation uses checked arithmetic. A very tall but narrow matrix also stays
dense when its six word rows cost no more than the mandatory sparse row and mask
headers. Crossing the size threshold alone must not replace cheap contiguous
words with more expensive per-block allocations. Functions without local
operands allocate no live-set payload. Terminal single-block destruction
continues to bypass liveness entirely.

For wider matrices above that threshold, the sparse solver seeds statement and edge uses and
propagates only newly live bits backward. Successor parameters and source
definitions apply the two distinct kills in the equations. This also works for
parallel edges, disconnected components, self edges and irreducible cycles.
There is no iteration cap or approximation.

Each live row starts as sorted nonzero words and becomes dense when its sparse
payload or allocation capacity would exceed a dense row. Pending deltas are
coalesced by block and word; at most `B` block IDs are queued, rather than one
event per live bit. Live-in, live-out and pending rows each retain no more than
one dense row's payload per block. Empty rows do not allocate word storage.

The 8 MiB threshold is **not a process memory limit**. Both backends additionally
retain block/local indexes and operand facts. The sparse solver has row headers,
definition/parameter masks, a block worklist and transient storage during row
promotion and draining. Masks scale with input operand occurrences; actual live
sets can still be dense. Telemetry reports the original dense scratch estimate,
the sparse-backend count and the retained result payload; it does not claim to
measure peak process RSS.

Tests compare both solvers with an independent set-based fixed point, exercise
generic-for control and phi transports, and run the full destructor's evaluator
with sparse liveness forced. A 4,096-block chain checks automatic selection,
cross-block uses and the reduction in retained live-set storage. Separate tests
cover row promotion, pending-delta coalescing, tall/narrow matrix selection and
arithmetic overflow.

## Owner-free SSA operand index

`ssa/value_index.rs` builds producer positions and the ordered eligible-use
snapshot in one block visit. It stores stable numeric local IDs instead of
retaining an `RcLocal` for every operand occurrence. The inliner is its production
consumer, including outgoing phi arguments.

The index preserves duplicate operands, first-definition lookup, parallel result
packs and the existing eligibility snapshot. A successful inline consumes a use
slot and can empty a producer, without moving statement positions. The view is
discarded before compaction or permutation and rebuilt for the next block visit.
Parent capture operands are included; child closure bodies are not executed in
that scope. All existing effect, late-read, scalar/multret and source-binding
guards still decide whether substitution is legal.

This is def-use infrastructure, not a complete semantic ValueIR migration.
Expression reduction still runs at its established commit points. There is no
unproven normal-form cache: repeated reduction can affect syntax and provenance.

## Explicit mutation sessions

`analysis::Revisions` distinguishes topology, operands and statement layout.
`inline_with_readonly_captures_report` records edited blocks and operand/layout
changes at existing mutation sites. Its first block identity is stored inline,
so the common one-block case needs no allocation for the report's block list.
No before/after AST fingerprint is built. `luau-lifter` consumes the report and
keys its dominator cache by topology revision.

`structure_jumps_with_changes` separates the legacy outer-round progress flag
from topology changes. Removing a predecessor-free unreachable adapter
invalidates graph analyses without adding a round that the original schedule
did not request. The existing `structure_jumps` wrapper preserves its boolean
behavior.

These revisions belong to an explicitly managed mutation session. They do not
automatically observe public `graph_mut`/`block_mut` access or shared closure
publication. A caller must advance the relevant revision after every reported
edit and begin a new session after untracked mutations. Operand-only edits keep
dominators valid; topology edits conservatively invalidate dependent analyses.

Run the crate's semantic, differential and scaling regressions with:

```sh
cargo test -p cfg
```

Correctness tests and storage bounds are not an end-to-end speedup measurement.
Measure ordinary release execution on fresh scripts separately from diagnostic
instrumentation, and retain the existing output-quality gates.
