# R2 binding preservation and nested-value ancestry

R2 now has an output-producing consumer before SSA destruction. An uncaptured
two-arm scalar selection that is both returned and observed separately gains
a conditional-result role. Selection between two distinct incoming parameters
keeps a separate result binding instead of borrowing either parameter's storage.
The deterministic namer calls this inferred role `selected`, with ordinary
scope collision handling; a compiler-recorded source name takes precedence.
It is not a claim that the author wrote that name or a separate declaration.

The recognizer requires private diamond/triangle arms and distinct scalar phi
inputs. It changes presentation constraints only. Existing SSA liveness and
effect checks still determine which copies may be removed; evaluation order,
capture groups and close certificates are untouched. Captured identities,
recorded source locals, parameter identities, lone returns, self-phi and
shared/extra-predecessor arms are excluded. It declines functions exceeding
512 CFG nodes or 160 observed locals to retain register headroom. Merely
assigning to a parameter does not request a split. Normalizing an optional
argument, such as replacing an absent index with a computed index, does not
force a separate binding. This is a bounded rule,
not a global ban on register reuse or inlining.

`BindingRoles` is separate from optional lineage and `SourceBinding`.
Parameter/result incompatibility is checked across complete coalescing
classes. Phi transport copies inherit the presentation constraint. The SSA
and late AST inliners retain the selected binding, and the final pressure
coalescer does not treat it as an anonymous scratch local. Distinct recorded
debug intervals retain their existing compatibility and protection rules.

## Immutable input graph and final regions

`--emit-binding-provenance` additionally records every visited nested input
RValue occurrence before SSA copy propagation, including assignment receiver
and key expressions. Nodes have function/prototype, initial block/statement,
path, syntax kind, SSA local reference and ordered children. Their instruction
PC and source-line sets come from the containing lifted instruction cluster.
Paths beginning with `0` describe an assignment LHS component; `1` describes a
statement RHS/control/return component. Subsequent indices address ordered
children. A closure's body belongs to its separate function trace.

This immutable graph survives later inlining or deletion. Committed SSA
substitutions record producer and consumer definitions; phi arguments and
generic-for packs have distinct event kinds. Substitution into a return can
have no surviving consumer binding; the installed compound value independently
retains its input-node tag. Definition
records retain original registers and compiler-recorded debug intervals;
local maps preserve their many-to-many storage ancestry through phi elimination.
Parameter transport and edge transport are explicit, bounded map events.

The formatter records exact UTF-8 byte spans of emitted statements and values.
Their binding references join a bounded dependency projection to input sites.
The projection reports **storage/dependency ancestry**, never an exact producer
PC for the particular occurrence. Each region separately contains
`node_ancestry`: direct links to initial statement/value occurrences carried by
retained AST nodes. Calls, method calls, binary/unary/index/conditional values,
tables and closures carry tags, as do assignment, return, branch and SETLIST
statements. Scalar leaves retain their initial graph records but do not carry
persistent AST tags: their final occurrence can remain unknown even when an
enclosing statement or compound value has a mapped instruction cluster.

Moves preserve tags; real AST copies mark `cloned`. Metadata snapshots taken
for emission never set that flag. The shared deep-clone implementation also
preserves tags while detaching mutable blocks. Only committed substitutions
mark an installed node `inlined`. RValue reductions union parent and child
origins, with deterministic truncation. Tags identify derivation ancestry even
when operators/children change; they do not claim value equality or an exact
producer instruction. New constructors default to unknown. Reconstructed calls
have explicit synthesis producers, independently of their retained arguments.
Failed speculative attempts publish no node occurrence. Named functions and
anonymous closures both receive real output regions.

Input graphs and source maps do not authorize
an optimization, recover absent debug names or certify an original callsite.

Classifications distinguish output parameters/iteration bindings, recorded
closure-constructor capture ancestry and incoming captures,
recorded source locals, inferred conditional results, parameter ancestry,
copied local metadata and explicitly recorded emitter synthesis. Copying a
`Local` metadata value records that ancestry; it does not claim that the
surrounding expression was cloned. Compiler loop-control definitions require
a numeric/generic-for preparation marker; missing debug names alone never
classify a compiler temporary. Recorded source definitions take precedence.
The final binding classification calls this **temporary ancestry**, because
mandatory storage maps can merge different roles. Other compiler temporaries
and unrecorded introductions stay unclassified. The lexical binding graph
remains the authority for declarations and actual captures in emitted source.
Repeated spans with the same storage ancestors do not prove cloning.

## Preservation and invalidation contract

Every transform belongs to the following contract; adding a transform does
not grant permission to retain an exact value identity by default.

| Stage/passes | Origin handling | Independent correctness obligation |
|---|---|---|
| Lifting, SSA construction | Record immutable nested occurrences, registers, definitions and debug intervals before positional keys change. | Existing bytecode/capture analysis. |
| Copy propagation, trivial phi elimination, destruction/local maps | Union diagnostic storage ancestry; retain distinct source identities and result presentation constraints. | Existing liveness, capture-cell grouping; close certificates intersect. |
| SSA expression/edge/pack inlining | Keep input graph and record committed substitutions; consumed values may lack final mappings. | Existing ordered effect, arity and capture gates. |
| CFG structuring, deep cloning, factoring, fallback | Moves preserve tags; actual copies mark cloning; newly built control syntax stays unknown. Only surviving nodes produce output records. | Existing region/loop/close proofs and rollback rules. |
| AST inline/copy cleanup, constructor rebuilding, de-inlining, normalization, guard/return cleanup | Retained children preserve tags; reductions merge parent/child input origins; explicit reconstructed calls record synthesis. Unattributed replacements remain unknown. Input paths are never reused as current AST locations. | Each pass's existing alias, order, arity and scope gates. |
| Late local introductions | Publish an explicit producer where instrumented; otherwise keep unknown. | Never copy source/ownership evidence just because names match. |
| Naming and formatting | Keep binding IDs and node origins independently of spelling; emit real syntax spans with separate node and storage ancestry. Previews create no occurrences. | Scope collision rules and independent parser audit. |

The machine-readable policy inventory is `ast::node_origins::PASS_CONTRACTS`
and is exported in the sidecar, including the individual production pass
families. Each policy explicitly disallows transferring effect/lifetime proof.

The input graph/events share the existing 50,000-record per-function budget;
nested traversal stops at depth 256. Projections allow 64 sites per binding or
region, 256 visited dependencies per binding, 2,000,000 dependency visits per
script and 100,000 output regions. Each node retains at most 16 input origins;
merged sets keep the same first 16 ordered entries regardless of merge order.
Exhaustion is explicit. The trace stores
numeric identities and strings rather than extra RcLocal owners. Default
decompilation does not collect this diagnostic data.

## Verification

`scripts/value_provenance.py` independently checks input paths/children,
instruction and line references, region dependency unions, limits, UTF-8
boundaries and explicit unknown/exactness flags. Corruption controls reject
cycles, invented PCs/bindings, changed unions, invalid node/function references,
invented synthesis producers and false exact-producer claims. The compact
annotation audit translates only output positions, including region byte
offsets; input PCs and value paths must remain identical.
It runs inside existing provenance and parser-backed emission audits. Runtime
fixture replay additionally requires the stripped `conditional` result to
remain separate from parameters and the debug version to retain `selected`.
Coverage reports count all output regions, direct node ancestry, storage-only
ancestry, cloning, inlining, multiple origins, synthesis, unknowns and omissions.

The locked release passes 1,018 primary Rust tests plus one child repeat, 122
Python tests, 198 runtime profiles, 513 public profiles, 42 compiler-witness
profiles, 45 legacy semantic profiles and 52 output-size gates. The new binding
fixture detects all 24 compiled mutants across six profiles. The 84-profile
reconstruction study passes in both modes and detects all 84 mutants per mode;
it remains an unblinded regression study. VM agreement does not erase the
runtime dataflow classifications: 119 proved, 27 different and 52 unknown.

Private output changes in 28 of 3,978 files, involving 31 binding renames.
The independent parser finds the same lexical binding/type shape in every
changed file; all recompile at O0/O2, and no specific name becomes generic
`selected`. All 3,936 nonempty private scripts pass parser/binding checks.
Source and complete detailed sidecars agree at 1/16 threads, and default and
detailed sources agree. Runtime/public replay also verifies compact annotation
offsets, capture certificates, previous metadata and cold/warm cache parity.
The persistent-node instrumentation changes none of the source outputs from
the preceding R2 consumer build.

| Dataset | Initial nested values | Output regions | Retained node ancestry | Explicit synthesis | Unknown node ancestry | Omitted output regions |
|---|---:|---:|---:|---:|---:|---:|
| Runtime, 198 profiles | 14,498 | 10,892 | 5,148 | 10 | 5,734 | 0 |
| Public, 513 profiles | 157,680 | 105,908 | 49,921 | 31 | 55,956 | 0 |
| Private, 3,936 scripts | 2,318,516 | 1,358,916 | 645,092 | 680 | 713,144 | 0 |

These are output-region ancestry counts, not source recovery accuracy.
Storage-dependency ancestry covers 5,572 / 67,914 / 850,985 regions respectively
and overlaps the node categories. Inline/copy history also overlaps: the private
set contains 340,245 inlined and 645,623 copied-node regions. A copied AST does
not establish repeated runtime evaluation. Public output includes three
multi-origin regions; a Rust test independently checks bounded multi-origin
merging and deterministic truncation. Input budgets and uninstrumented scalar
leaves retain their explicit unknown/incomplete contract.

The [acceptance inventory](roadmap_v2_acceptance/source_binding_validation.json)
locks the release SHA-256, 146 Rust/Cargo source hashes and compact evidence.
[Owned conditional examples](roadmap_v2_acceptance/source_binding/conditional.json)
show the stripped result separated from parameters and the debug source name
preserved. [Full coverage counts](roadmap_v2_acceptance/source_binding/coverage.json)
include evidence-backed classifications and phi transport. Large raw reports
remain under the ignored `out/v2-r2/verified` tree; the inventory records their
hashes. No general recovery rate for original source variables is claimed.

## Reproduction

Use Rust `nightly-2024-12-15` and the pinned Luau commit
`c2ec0d4e5ca50796ba174a7565298f59aa572268` with `--fflags=false`.
The examples below run from the repository root in PowerShell; `$r2Tools`
points to the existing pinned compiler/VM/parser build. Keep directories must
be fresh. The same fixture, metadata and control checks are wired into CI.

```powershell
$r2Tools = 'D:/Medal/luau-tools-src/build'
cargo +nightly-2024-12-15 test --workspace --all-targets
python -m unittest discover -s scripts -p 'test_*.py'
cargo +nightly-2024-12-15 build --release -p luau-lifter
python scripts/roadmap_v2.py --compiler "$r2Tools/luau-compile.exe" --luau "$r2Tools/luau.exe" --ast "$r2Tools/luau-ast.exe" --lifter target/release/luau-lifter.exe --keep out/r2-reproduction/runtime --report out/r2-reproduction/runtime.json --determinism
python scripts/provenance_fixtures.py --fixtures-report out/r2-reproduction/runtime.json --lifter target/release/luau-lifter.exe --ast "$r2Tools/luau-ast.exe" --keep out/r2-reproduction/traces --report out/r2-reproduction/traces.json --cache --compact-annotations
python scripts/binding_preservation_controls.py --compiler "$r2Tools/luau-compile.exe" --luau "$r2Tools/luau.exe" --keep out/r2-reproduction/controls --report out/r2-reproduction/controls.json
```

For any detailed output tree, `scripts/emission_map_source_audit.py --root TREE
--ast PARSER --report REPORT.json` checks node ancestry together with the
independent lexical binding map. `scripts/source_binding_review.py --before OLD
--after NEW --compiler COMPILER --ast PARSER --report REPORT.json` checks changed
source without exporting its text. The private corpus is not distributed.
