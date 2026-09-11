# Lexical declarations and historical IR storage

`scripts/binding_graph.py` exports a separate, optional graph for the exact
emitted source. A declaration has a deterministic `dN` identity and an owning
output function `fN`; references are resolved by the pinned Luau parser's
declaration location. These IDs are local to the source hash. They are not
original source IDs, prototype IDs or the decompiler's `bN` storage IDs.

The parser handles local initializer scope, recursive local functions, loop
headers, repeat/until, nested captures and implicit colon-method `self`.
Declaration and reference tokens use half-open UTF-8 byte spans. `typeof`
references in annotations/type syntax remain in the graph with `type_only`;
they do not create runtime captures. Index assignments read their base/key
bindings and do not count as direct writes to those locals. Compound local
assignment has one `read_write` token.

## Evidence and boundaries

| Graph field | Evidence and interpretation |
|---|---|
| `kind` | Current output syntax: local, parameter, local function, iteration binding or implicit self. |
| `captured_in_output` | A runtime reference occurs in a descendant output function. This does not identify REF/VAL mode, mutation, input capture cells or CLOSE behavior. |
| `storage_id` | Exact emitted tokens link this lexical declaration to one final IR ID. All mapped tokens must agree. |
| `storage[].declarations` | All output declarations sharing an IR ID. They remain distinct lexical bindings. |
| `storage[].lineage`, `input_slots`, `has_conditional_result_ancestry` | Historical storage facts copied from the separately validated trace. A parameter can have conditional-result ancestry without being a newly recovered source local. |
| `recorded_identity_status` | Recorded evidence on one unique output binding, ambiguous shared storage, unrecorded, or no storage mapping. Even unique output mapping does not claim one unique author declaration for every origin. |
| `recorded_origins` | On a uniquely mapped output binding, preserves recorded debug PC intervals/upvalue slots/function-name origins. Shared-storage origins stay at the storage level. |
| `protect_recorded_name` | Preserve names with recorded evidence, including ambiguous shared storage. |
| `emitter_introduction` | An explicitly recorded local introduction by an instrumented pass, joined by binding ID. Attached to one compatible local declaration, including a closure snapshot formatted as `local function`; shared storage retains ambiguity. |

An implicit self declaration has an anchor at the method keyword and **no
identifier declaration token**. It cannot be renamed by an ordinary
all-occurrences token edit. Even unused implicit declarations have a graph
row. Being unrecorded does not classify a local as a compiler temporary or a
synthesized local. [Explicit emitter records](emitter_local_origins.md) are
required for synthesis attribution; the exporter does not invent them from
generated names, missing SSA ancestry or comments.

Source, parser executable, analysis sidecar and graph bytes have separate
SHA-256 identities. Every manifest sidecar/source path must resolve inside
the input root, and stored hashes must match. Existing trace and emission-map
validators run before the join. An unmapped token needs an explicitly opaque
output region or a recorded exhausted emitter budget. Missing or conflicting
identity otherwise refuses the graph; no partial graph is published.

This is a diagnostic/consumer layer. It does not modify decompiler source,
split runtime storage or change core coalescing decisions. Arbitrary nested
value origins, earlier clone/synthesis attribution and the complete pass ledger
remain separate R2 tasks. No purity, alias, ownership or capture certificate
is transferred by graph connectivity.

## Commands and budgets

```powershell
python scripts/binding_graph.py --source output.luau --ast PATH/luau-ast.exe --report graph.json

python scripts/binding_graph.py --root DECOMPILED_WITH_PROVENANCE --ast PATH/luau-ast.exe --graphs SEPARATE_GRAPH_DIR --report audit.json --threads 4

python scripts/binding_graph_controls.py --ast PATH/luau-ast.exe --report controls.json
```

The directory command requires `--emit-binding-provenance` sidecars. Graph
files are content-addressed; the per-script report supplies graph hashes.
The single-source command needs no sidecar and has no input-storage evidence.
No model or external service is used.

Per script: source 4 MiB, parser stdout and sidecar JSON 64 MiB each, parser
stderr 1 MiB, parser time 30 seconds, 200,000 visited AST containers, depth
256, 50,000 declarations and 100,000 local tokens. Pipe readers enforce byte
caps during execution and kill an over-budget parser; the entire stdout is
not captured first. The parsed JSON/graph use additional Python object memory,
so 64 MiB is an encoded-byte cap, not a process RSS cap. The thread count is
bounded at 16. Budget exhaustion remains a reported refusal.

## Acceptance

The executable/trace baseline is `9b51e30` / lifter SHA-256
`1d3b9fcfe7f1d2fcad1921be27a133183f78f9a79bbf767c9ab55ce379841f9f`.
The compiler/parser source is pinned at
`c2ec0d4e5ca50796ba174a7565298f59aa572268`.

| Dataset | Scripts | Declarations | Mapped / opaque tokens | Shared storage IDs |
|---|---:|---:|---:|---:|
| Runtime fixtures | 168 | 1,037 | 2,885 / 0 | 0 |
| Public, development + Rodux holdout | 513 | 13,281 | 45,633 / 251 | 3 |
| Private, nonempty | 3,936 | 130,780 | 495,344 / 2,306 | 11 |

All scripts pass; there are no unexplained unmapped tokens or exhausted graph
budgets. The 42 empty private inputs remain separate. All 3,978 private source
files have the accepted byte-identical tree hash
`0147732a789ec9c6766fb37c97799e8be75adf2781e95cbc9f7f0f94fdf881ec`.
No fidelity/behavior change or speedup is claimed for this offline exporter.

The parser finds 194/2,742/31,847 captured output bindings in these datasets.
Recorded evidence on unique output bindings covers 590/614/3,320 declarations.
The 3 public shared-storage cases are Fusion `Disassembly` at O0/O1/O2; the
11 private IDs cover 23 lexical declarations. Unused implicit self and
bindings entirely within opaque regions can lack a storage mapping; they
remain visible (0/21/354 declarations respectively).

Twelve real-parser scope controls have explicit expected kinds, token counts,
capture and direct-write roles, with repeated parsing for deterministic
graphs. Three actual-process controls cover stdout, stderr and timeout caps.
Eight unit tests cover storage ambiguity, wrong same-spelled binding, source
hashes, invalid spans, missing declarations, opaque coverage and five graph
budgets. CI runs both the controls and a graph export of the runtime traces.

Per-script reports and graph-hash determinism evidence are stored in
[`roadmap_v2_acceptance/lexical_validation.json`](roadmap_v2_acceptance/lexical_validation.json).
These diagnostics do not substitute for runtime or dataflow validation of
future source-changing consumers.

All 4,617 graph hashes agree between one and four threads. One-thread diagnostic
wall times were 9.25/34.62/309.93 seconds for runtime/public/private; the initial
four-thread samples were 4.16/14.34/160.89 seconds. These were determinism runs,
not isolated benchmarks: the initial public/runtime runs overlapped, and the
private one-thread run overlapped a later model download. All 93 Python tests
pass; the decompiler binary and its previously accepted Rust/runtime tests
were unchanged by this Python-only consumer.
