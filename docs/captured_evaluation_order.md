# Captured reads at an inline destination

An earlier call must finish before a later read of a captured cell. Previously,
SSA inline protected captured values in the candidate and captured writes
between statements, but it could cross a captured read inside the destination:

```luau
local argument = fetch(replaceCallback)
return callback(argument)
```

Replacing `argument` with `fetch(replaceCallback)` makes an ordinary call read
its callee first. When `fetch` changes the captured callback from `old` to `new`,
the source calls `new` and the previous output calls `old`.

The inliner now checks capture reads while traversing expressions preceding the
replacement position. The current SSA `upvalue_to_group` map identifies both
incoming and passed captured cells, including their SSA versions. A preceding
captured local or closure capture blocks a candidate that may write captured state, alongside the
existing global/index/operator/error barriers. It preserves the temporary and
the original evaluation point. The maps are not inferred from names or types.
Two captured reads commute, so a pure read/allocation candidate does not gain
this barrier. The bounded may-write query stops at the first possible callback
or metamethod; unknown and exhausted queries conservatively require the barrier.
Read-only ordinary locals do not gain this barrier. Existing captured candidate,
intervening write, conditional execution and comparison-order checks remain
necessary; this is an additional destination dependency.

The same dependency applies to an earlier return-tuple element, an earlier call
argument and a captured assignment base/key. Runtime fixtures mutate those cells
inside the candidate callback and cover nil, false, normal values and result
counts. A separate callee fixture distinguishes the invoked closure identity.

## Immutable incoming values

The `luau-v9-val-upval-immutability-v1` input analysis supplies a narrow exception
for incoming values that cannot change. Every constructor of a prototype must
capture that slot by VAL, or forward it by UPVAL from another proven immutable
slot. Any REF source, missing constructor, external main-function slot, direct
SETUPVAL or SETUPVAL in a descendant sharing an UPVAL chain prevents the proof.
The result is a statement about the slot's value/pointer, not the contents of
a table it references. An immutable callee pointer can still perform arbitrary
effects when called. Host C API mutation of closure internals is outside the
ordinary Luau execution contract.

Two bounded worklists propagate possible writes toward parent slots and
established immutability toward children. Every static constructor contributes;
an unreachable REF site still refuses the exception. A dependency cycle cannot
justify its own certificate. Limits are 50,000 prototypes, 200,000 slots,
1,000,000 instructions and 200,000 capture edges. Unsupported bytecode versions
and malformed/over-budget input return no certificates. The supported profile
is v9; REF cells are not inferred immutable just because source usage looks
constant. Type/debug information is not consulted.

The lifter maps these input-slot flags through the exact incoming groups from
the current SSA construction. The exception stores numeric binding IDs only,
without adding AST/local owners. Captured-value protection and other group
checks retain the full capture map. Fresh IDs without a group proof remain
conservative; names, later AST reshaping and copies of an ownership certificate
cannot grant the exception.

Analysis sidecars include `capture_effects`: model, status/refusal, analyzed-slot
count, limits and each proven prototype/slot pair. The existing function/slot
and raw capture records locate the input evidence. The separate Python verifier
parses original bytecode, checks the input hash, every constructor, forwarded
writes and the acyclic derivation from VAL roots. It also rejects malformed
claims, missing ancestors and certificates attached to a refusal. Runtime and
public provenance CI invoke this verifier; cold/warm artifact cache must retain
identical proof metadata. The default source-only path creates no proof JSON.

```powershell
python scripts/capture_effects_audit.py --root ANALYSIS_OUTPUT --input SAVED_BYTECODE --key 203 --report out/capture-effects-audit.json
```

Colon calls have a different pinned compiler order. In Luau commit
`c2ec0d4e5ca50796ba174a7565298f59aa572268`, `compileExprCall` evaluates a nonlocal
receiver expression, emits arguments, then emits `NAMECALL` and `CALL`.
A directly usable receiver register may be reused by `NAMECALL`. Consequently,
`object:consume((fetch()))` already performs `fetch` before the method lookup.
The method fixture checks that lookup/argument order, lookup/fetch errors,
receiver identity, nil/false and multret at O0/O1/O2 and g1/g2. It is a positive
control: the previous release already passes. The fix does not insert a general
method-lookup barrier before colon-call arguments. These facts are pinned to
the compiler, not borrowed from a different Lua implementation.

## Expression effect vocabulary

`ast::effects` exposes nonowning may-effect summaries: throw, table read/write,
allocation, call, yield, captured read/write, global read/write, unknown and
conditional execution. Unknown functions and metamethods conservatively receive
all dynamic-call flags. A closure's body is not evaluated during construction;
its allocation and capture dependencies are separate. Private table construction
reports allocation and possible invalid-key errors; it does not claim a write
to an externally visible table. Type annotations never establish purity.

Primitive-literal equality cannot invoke `__eq`, but its children are still
summarized. Other unknown operators remain barriers. Summary limits are 8,192
nodes/width and depth 128; exhaustion returns unknown dynamic effects. Summaries
refer only to the tree and capture predicate supplied for that invocation and
must be recomputed after mutation. No cross-epoch cache or ownership certificate
is transferred. The inliner consumes the capture-read flag from the intrinsic
node summary; the broader expression summary is available for subsequent work.

Total-pure means safe to discard an unused result under the existing ordinary
runtime contract, which excludes resource exhaustion, native finalizers and
debug hooks. An allocation is recorded even if that allocation alone is
discardable. A captured read can be total-pure yet depend on order relative to
a callback. The vocabulary is not an alias analysis, proof of known API purity,
full statement/store dependency graph or per-pass proof ledger.

## Acceptance

The accepted executable SHA is
`c9e99ed6a5af4859bff5d89c7ffa270c69967965ecb9c57900bf0d9f8695b17c`.
The prior cache release fails nine configurations: three captured-order
fixtures at O0/O1/O2 with g1. All 24 new configurations now pass, including
the six method-order positive controls. The full matrix passes 168 runtime
configurations, nine oracle controls and 513 public compile/decompile/recompile
configurations. Runtime dataflow is 126 proved, 28 unknown and 14 different;
these categories are not replaced by the finite runtime observations.

All 953 primary Rust tests, one child-process repeat, 85 Python tests, 45 legacy
semantic configurations and 52 size gates pass. Runtime/public source and
metadata remain deterministic at one/four threads, including cold/warm artifact
cache. The independent original-bytecode verifier checks 158 immutable slots
across 168 runtime scripts, 4,597 across 513 public scripts and 54,007 across
3,936 nonempty private scripts. No analysis is refused in these datasets;
unproven slots remain unknown. Forty-two empty private inputs are separate.

Six public outputs change: Roact Binding and createReconciler at O0/O1/O2.
All public dataflow classifications and exact-name counts are unchanged. All
657 previously recorded runtime/public binding contracts remain equal. The
3,936 private contracts also match the earlier emission release's detailed
sidecars; that comparison has a separately labelled baseline.

All 35 changed private files were reviewed and recompiled. Their whole-input
comparisons remain unknown before and after. The changes retain evaluation
temporaries; some existing branch/loop cleanup falls back to repeated returns
or a condition temporary inside a while-true loop. Two copies of a texture
script each lose one reconstructed `updateTexture()` call while retaining the
helper definition and input function name. The extra snapshot separates the
helper pattern from its call site. This is a fidelity limitation for later
region normalization, not a helper-recovery improvement. These Roblox modules
were not runtime executed.

Seven interleaved warm-filesystem CLI rounds retain deterministic source trees.
One-thread median changes 18.075 to 18.702 seconds (+3.47%); 16-thread median
changes 1.666061 to 1.666092 seconds (+0.002%). Median peak RSS is unchanged at
33,431,552 bytes for one thread and changes 114,008,064 to 114,216,960 bytes for
16 threads. Seven-sample p95 is the maximum. This records the correctness fix's
cost, not a speedup. The final private source tree is
`0147732a789ec9c6766fb37c97799e8be75adf2781e95cbc9f7f0f94fdf881ec`.

[Validation inventory and per-file reports](roadmap_v2_acceptance/capture_validation.json)
include the prior failing cases, independent slot audits, metadata/cache checks
and benchmark samples. The broader R4 statement/store dependency graph,
ownership analysis and runtime-guard facts remain open.
