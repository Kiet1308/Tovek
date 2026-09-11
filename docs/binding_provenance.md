# Optional SSA and storage lineage

`decompile-folder --emit-binding-provenance` includes `binding_provenance` in
each static-analysis sidecar and implies `--emit-upvalue-analysis`. Ordinary
analysis and source-only decompilation do not collect this trace. Library
artifact APIs use `DecompileOptions::emit_binding_provenance` (flag bit 16).
This diagnostic can generate substantially more metadata than ordinary
upvalue analysis.

The trace answers which input statement, SSA definition and later storage
binding are connected by recorded maps. **Storage ancestry is not value
equality or source-binding identity.** It must not authorize an inline, a
capture-cell merge, an eager evaluation or a close-certificate transfer.
Compiler-recorded `SourceBinding` evidence remains separate and retains its
existing compatibility rules.

## Records and interpretation

| Record | Meaning |
|---|---|
| `function_id`, `prototype` | Static closure instance and original prototype. A prototype can have several lifted instances; unreachable/uninstantiated prototypes need not have a trace. |
| `registers` | Input storage slot, with distinct parameter/incoming-upvalue roles and any recorded debug evidence. A general register is not classified as a compiler temporary. |
| `lifted_statements` | Initial CFG block and statement position, instruction PC set, available line set, coarse instruction role and ordered register reads/writes. These positions refer to the initial lifting snapshot. |
| `definitions` | SSA identity, original register, initial statement/write slot or block parameter, initial read dependencies, debug binding evidence and final storage mappings. Phi dependencies are sets, not ordered expression operands. |
| `local_maps` | Ordered mapping events at SSA construction, cleanup and destruction. This is not a complete ledger of every later AST rewrite. |
| `conditional_results` | A phi supplied by distinct local inputs from a two-arm branch: each arm is direct or one private block leading to a two-predecessor join. Then/else follow CFG edge polarity. Recognition runs after initial SSA construction and again before destruction. |
| `pre_destruct_bindings`, `post_destruct_bindings` | Identity snapshots around SSA destruction, including edge arguments. |
| `final_bindings` | Emitted binding identity/name with a bounded union of recorded storage ancestors. Reverse mappings on origins must agree with this union. |

IDs are strings such as `b1099511627788`, scoped to one script decompilation,
so JSON consumers never round a large integer. The trace retains IDs and
strings, not extra `RcLocal` owners; reference-count-sensitive cleanup must
not change because analysis is enabled.

PCs count instruction words and exclude auxiliary payload slots. A deferred
open call/vararg pack can contribute to a later call, return or `SETLIST`;
`NAMECALL/CALL` and `NEWCLOSURE/CAPTURE` clusters can also have several origins.
Several lifted statements may have the same PC set. Missing line information
stays absent. Generated warnings have no invented instruction origin.

Conditional recognition establishes the incoming scalar binding for each
arm, including when arm computation can throw or have effects. It does not
claim purity, totality, a source `if` expression, or permission to move arm
computation. Shared arms, extra join predecessors, self-phi and equal or
nonlocal inputs are outside this recognizer. The emitter retains statement
style. A final parameter can have both its original parameter and a conditional
result in its storage ancestry without becoming a newly recovered source local.

## Preservation and unknowns

| Stage | Current preservation contract |
|---|---|
| Lift and SSA construct | Record initial instruction clusters and definition/write-slot relations before statement positions change. |
| SSA local map and destruction | Merge lineage through existing source-metadata transfer points. Close and capture compatibility are governed by their existing proofs, independently of lineage. |
| CFG structuring | Freeze the function trace before speculative CFG clones. Cloned local metadata retains ID ancestry; the trace does not keep a mutable CFG alive. |
| AST replacement and cloning | Existing metadata-preserving local replacements and local clones carry ancestry. Inlining a local into an arbitrary nested value does not yet attach provenance to that value. |
| Final naming and formatting | Read remaining ancestry and record exact identifier token spans keyed by final IDs in the optional [emission map](emission_map.md). PC sets retain storage-ancestry meaning; no unique value producer or new source identity is inferred. |

There is a combined limit of 50,000 records per lifted function and 256
ancestors per final binding. Overflow retains the lowest sorted IDs, making
the union deterministic regardless of map order, and marks the lineage
incomplete. `dropped_records` reports trace-record exhaustion separately.

`no_final_binding_mapping` does not distinguish dead code, inlining or a later
untracked rewrite. A local with no ancestry is labelled
`unattributed_or_synthesized_after_ssa`; it is not automatically called a
compiler temporary. `unknown_origins` and `incomplete` remain visible.
`incomplete_lineages` counts partial nonempty ancestry; unlocated final
bindings are a separate summary category.

Arbitrary nested-value provenance, clone/synthesis event attribution and a
complete per-pass preserve/merge/invalidate ledger remain R2 work. Exact final
identifier and annotation locations are available through `output_map`, with
explicit opaque regions for interpolation sub-rendering and display fallbacks.
Consumers must retain unknown cases rather than interpreting absence as proof
of optimization.

## Validation

`scripts/provenance_audit.py` reads only manifest-selected, hash-verified
sidecars. It checks PC bounds, write slots, unique identities, dependency
references, bounded ancestry and both directions of final mappings, and
requires exact source bytes and preservation of all pre-existing sidecar
fields apart from the explicitly changed analysis ID/options.

`scripts/provenance_fixtures.py` replays the bytecode from a passed runtime or
public-source report. It compares ordinary analysis with detailed trace and
requires identical sidecars at one and four threads. Runtime fixtures include
the `selected` phi example with and without local debug information. These
checks test trace consistency; correctness comes from the separate runtime
and unchanged-source gates. See the [acceptance record](roadmap_v2_implementation.md).
