# SSA inline statement facts

The SSA inliner's backward scans repeatedly ask the same statement which local
groups it reads/writes, whether it writes a captured cell, and whether the
statement or its sole RHS is observable. `cfg/src/ssa/inline/facts.rs` caches
these answers during one block's `inline_rvalues` visit.

The cache changes analysis cost only. It uses the existing effect predicates
and captured-cell map. It does not introduce purity assumptions, change the
order of candidate search, or weaken the global-lookup barrier.

## Lifetime and invalidation

The statement positions and both group maps stay fixed while this cache lives.
Every successful statement substitution invalidates the modified consumer and
the emptied producer. Folding the result pack into a generic-for preparation
invalidates both statements too. Substitution into a CFG edge invalidates the
emptied producer; the edge expression itself has no statement cache entry.
A failed substitution restores the popped RHS and leaves the facts valid.

The cache is discarded at the end of the block visit, before dead-code cleanup,
statement removal, table folding or SETLIST movement. The next inliner visit
starts with empty entries. Local-use counts are mutable and are not cached.
Only integer group IDs and booleans are stored; the cache adds no owners of a
local, closure or block and creates no new local identities.

Debug builds recompute facts on every cache hit and assert equality. This
checks invalidation along the actual pipeline, in addition to focused tests
for mutation, captured reads/writes, emptied statements and the size fallback.
The expensive check is absent from release execution.

## Bounds and counters

Blocks with four through 16,384 statements have one optional entry per statement.
Smaller or larger blocks compute fresh facts in a transient entry. This bounds
the number of cached statement slots; the group-ID vectors still scale with
the reads/writes of those statements. It is not a bound on total process RSS.
Falling back changes cost, not acceptance of an inline candidate.

Opt-in pass profiles attach these counters to `F_SSA_INLINE`:

| Counter | Meaning |
|---|---|
| `ssa_fact_cache_hits` | Reuse of an existing statement summary |
| `ssa_fact_cache_misses` | Computation stored in an empty cache slot |
| `ssa_fact_cache_uncached` | Fresh computation in a block outside the caching size range |
| `ssa_fact_cache_invalidations` | Removal of a populated slot after mutation |
| `ssa_fact_cache_slots` | Sum of allocated statement slots across block visits |

Slots are neither allocation bytes nor peak live entries. Counter aggregation
occurs after each inliner visit; disabled profiling performs no per-query TLS
or report-map access. The profile checker verifies the function context,
nonnegative integer counters and `invalidations <= misses <= slots + invalidations`.
All node/allocation accounting beyond this scope remains separate R7 work.

The [acceptance record](roadmap_v2_implementation.md) contains output-identity,
debug-invariant, profiling and uninstrumented benchmark results.
