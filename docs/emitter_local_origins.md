# Explicit emitter local origins

The final constructor and scalar-conditional passes now record the locals they
actually introduce. A record contains a stable binding ID and the creation
role: constructor property value, constructor initializer snapshot, scalar
select result, short-circuit result or evaluation snapshot. The pass and rewrite
model identify the producer. This distinguishes an emitter-created local from
one whose input provenance simply could not be recovered.

These records are diagnostic. They do not change local identity, emitted code,
name selection, source protection or any rewrite decision. They retain strings
and counts rather than RcLocal owners. No input PC, source/debug binding, SSA
definition, type, close, ownership or effect certificate is copied onto a newly
created local. The existing empty/incomplete storage-ancestry fields remain
unchanged even when its emitter origin is known.

Only successful rewrite attempts publish records. Scalar-conditional lowering
can allocate temporary identities while exploring a statement and later refuse
it; those tentative introductions are discarded with that attempt. Each pass
retains at most 4,096 records, counts every omission and preserves deterministic
creation order. An omitted record grants no classification.

## Sidecar and lexical graph

Each pass report includes `introduced_bindings.records` and `omitted_records`.
Detailed `binding_provenance.local_producers` groups these records by pass,
retains the counts and model, and links each located final binding back to its
record through `emitter_introduction`. The existing output map supplies exact
identifier spans for that binding ID.

The independent Python validator checks the known pass/model/role combinations,
budgets and counts, unique ordered IDs, forward/reverse links, final binding
presence and agreement with the committed pass report. It rejects introductions
carrying input ancestry or recorded source identity. Older sidecars without a
ledger remain readable and gain no new classification.

The parser-backed lexical graph attaches an introduction only when the binding
ID maps to one local declaration. An evaluation snapshot or short-circuit result
initialized with a closure can be formatted as `local function`; the creation
record describes that storage local, not the existing closure prototype.
Same-spelled locals are independent.
Shared storage keeps an ambiguous declaration mapping; the producer is retained
on the storage row without assigning it to one lexical declaration. A parameter,
loop binding cannot be relabeled as a fresh local from these two passes. Other
producer roles cannot claim local-function syntax. Missing debug names and missing SSA ancestry remain insufficient
evidence of either synthesis or a compiler temporary.

## Coverage boundary

This ledger instruments two late passes whose created storage identities remain
stable through final naming and formatting. Earlier rehoisting, de-inline
helpers, local coalescing, CFG reconstruction, clones and formatter-only
temporary names are not covered. Their unknown origin is preserved. The ledger
does not complete arbitrary nested value provenance, per-call-site proof
locations, source-local/storage separation or a pass-wide invalidation policy.

The producer role is an explicit internal creation record, not a reconstruction
of what the original author wrote or an independent semantic proof of the
rewrite. Validation separately checks that the record points to the correct
emitted lexical binding. Existing runtime/dataflow checks remain responsible
for the transformation's behavioral evidence.

## Acceptance

All 972 primary Rust tests plus one child repeat and 102 Python tests pass.
The native conditional generator exports actual producer token spans; the
pinned parser joins all 52 introduced locals to 182 declaration/use tokens
across its 42 cases. The roles are 36 scalar selects, 13 evaluation snapshots
and three short-circuit results. All 252 optimization/debug configurations and
18 compiled runtime mutants pass. The 240 previous configurations keep identical
source, dataflow and observations; 12 new profiles exercise closure snapshots
and short-circuit locals emitted as `local function`.
Tentative locals from refused statements never appear in the ledger.

All 180 runtime and 513 public configurations preserve source bytes, complete
dataflow results and source-fidelity measurements. Uncached and cold/warm
artifact-cache sidecars agree at one/four threads; existing capture and
parser-backed emission checks pass. All 3,978 private source files remain
identical, including 42 empty inputs. All previous sidecar fields in the 4,629
nonempty runtime/public/private scripts are identical after removing only the
new ledgers and final-binding introduction links.

Runtime has 24 explicit introductions: 18 constructor property values and six
initializer snapshots. The runtime lexical graph joins all 24, with identical
graph hashes at one/four threads and no shared-storage ambiguity. Public and
private scripts introduce none through these two passes, and no record budget
is exhausted. No corpus readability improvement is claimed for this diagnostic
addition. Existing `unlocated_final_bindings` still measures missing input
storage ancestry and is intentionally not reduced by knowing an emitter origin.

The real-parser control script accepts the original emitted metadata and
rejects all eight deliberate corruptions: missing identity, copied debug
identity, copied SSA ancestry, wrong pointer, duplicate introduction, wrong
pass role, missing committed report and missing lexical-use token. Unit tests
also cover identical spellings, shared storage, incompatible lexical kinds,
budgets and historical sidecars. CI runs these controls using the runtime trace
directory, where actual constructor introductions are present.

The [acceptance inventory](roadmap_v2_acceptance/producer_validation.json)
records hashes and scope for all reports. Use `scripts/binding_graph.py` on a
directory generated with `--emit-binding-provenance` to inspect declarations;
`scripts/local_producer_controls.py` additionally requires a trace containing
at least one actual constructor introduction, such as the roadmap fixtures.
