# Late UI tree rebuilding

The C4 pass now restores curried factory/property/children expressions from
their temporary-based lowering. On the final corpus it removes **37 of 59**
SETLIST fallbacks. `FusionPackage/Components/Base/Menu/init` now has one nested
props/children tree instead of separated factory handles, field stores and a
packing loop.

The pass alternates table rebuilding and single-use inlining to a fixed point:

1. A factory result can move into another call's callee position only when it
   has one write, one read, no capture, and crosses no observable evaluation or
   conditional execution. Callee position keeps exactly one result.
2. A local/literal key snapshot can move into a single index assignment only
   when its source remains unchanged. Captured sources require a proved single
   write before they can cross calls; mutable capture snapshots stay in place.
3. Contiguous property assignments keep key evaluation, value evaluation and
   the store in their original order. This permits dynamic/event keys even when
   they call or raise. A captured target table cannot postpone its initialization.
4. A contiguous SETLIST appends only at the exact next list index. Fixed values
   stay single-valued, the final tail stays multret, and an existing expanding
   tail prevents appending. Luau flushes list entries before a keyed field, so
   numeric-key overlap and dynamic-key errors preserve their order.

An initial nil placeholder for a literal key can be removed while its final
value stays at the later position. It cannot be replaced in place across an
effectful constructor suffix. Nonempty discarded tables and effectful old
values remain preserved.

There are **22 remaining fallbacks in 21 files**. They include conditional
property computation (`CollapsedStatLabel`, `StatLabel`), interleaved statements
and captured bindings. These sites retain their source order rather than
duplicating a branch or moving a callback. Their complete locations are recorded
in [ui_setlist_remaining.json](structurer_inventory/ui_setlist_remaining.json).

Fallback evaluates the fixed values and multret tail together with `table.pack`
and writes indices 1 through the recorded count. Unlike a sparse `next` loop,
this overwrites old slots with nil and delays every store until all values have
been evaluated. Generated locals avoid shadowing the target, and naming reserves
the Luau `table` builtin used by this expansion.

`ui_setlist_order.luau` compares actual traces at O0/O1/O2 for factories,
dynamic nil/NaN keys, mutable captures, placeholder order, numeric-key overlap,
multret/nil tails and skipped call sites. AST/CFG tests exercise refused motion,
observable captured initialization and helper-name collisions. Final guard
flattening has an expression budget and refuses to duplicate tables/closures.

## R4 constructor review, 2026-09-11

The later [private property-diamond pass](branch_constructors.md) groups the
stripped `branch_ui` props and children after precomputing its conditional value.
It protects recorded table declarations and callback layouts and does not rerun
UI inlining after ordered initializer snapshots.

The full current scan finds 23 SETLIST sites in 22 files, with unchanged source
bytes. The historical list above contains 22 sites; DragonSecondaryButton was
already present in the accepted baseline but absent from that list. Their
[current locations, hashes and manual shape classification](roadmap_v2_acceptance/branch_ui_fallbacks.json)
separate seven selected-value/branch regions, six ordered call-prefix regions and
ten interleaved constructor/call regions. These labels describe the observed
source shape; they are not automated effect proofs or per-site optimizer refusal
certificates. The diamond pass's per-script refusal counts are recorded
separately.

The selected-value sites are Shine, DamageIndicator, Trait, CollapsedStatLabel,
StatLabel, UtilityFunctions and DragonSecondaryButton. Their conditionals are separated from the table
declaration, select a local used later, or have a missing arm. Two BuildingResources
sites, SelectRecipePrompt, Entry, PayloadSelection and Viewport preserve computed
values before later callee/property lookup. The remaining sites contain
interleaved field construction and calls: SplitTextLabel, QuestBoardMilestones,
both GameUnitView files, SandboxControls/Units, NodeMapButton, Battlepass,
Calendar, Profile and RewardCalendar.

None is made safe merely by recognizing Fusion, Roact or a factory API. General
dependencies across these statements and the SETLIST result boundary remain R4
work; the explicit packing fallback retains nil overwrite and multret behavior.

## R4 bounded constructor regions, 2026-09-11

The later [private constructor-region rule](constructor_regions.md) removes eight
of those 23 packing sites: Shine, DamageIndicator, Trait, CollapsedStatLabel,
StatLabel, UtilityFunctions, DragonSecondaryButton and SplitTextLabel. Their
first unobserved allocation can move past selected local computation and
unrelated stores after checking all local dependencies and both branch arms.
Effectful props initializers and factory handles remain at their evaluation
points. The total is now 15 remaining packing sites in 14 files.

The rule also groups private props/child dictionaries outside these list sites.
It changes 50 private files, with complete thread/mode identity, parser binding
checks, known-source VM observations and per-file review. This does not certify
the remaining sites or provide a general alias/effect dependency graph. Current
locations and exact source hashes are in the [acceptance inventory](roadmap_v2_acceptance/constructor_regions_validation.json).
