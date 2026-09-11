# Nested assignment address evaluation

Late temporary/UI inlining now distinguishes an indexed assignment's terminal
store from the reads used to obtain its base and key. Those address reads happen
before the RHS. The terminal store happens after it.

For example, the pinned compiler preserves this order:

```luau
local handle = factory()
target.Child.Value = handle()
```

The previous late UI pass emitted `target.Child.Value = factory()()` for stripped
local names. This moved `factory()` after the `target.Child` lookup. An `__index`
metamethod could then run, mutate state or raise before the factory. The same
mistake affected `target[keys.Key]` and a nested base and key together.

The corrected pass retains the initializer whenever an index occurs inside an
assignment's base or key. Local/literal children of that index do not prove the
lookup pure. This rule also protects an earlier snapshot of a mutable captured
local. It grants no proof from names, library APIs or type hints, and transfers
no binding/close/ownership certificate to a new local.

The positive case `target.Value = handle()` can still consume an adjacent
`local handle = factory()`: reading an ordinary frame local and literal key has
no observable effect, and the final `__newindex` store still follows both calls.
Existing capture, source-name, conditional-execution and result-arity gates
continue to apply.

This repairs a concrete address/RHS dependency. It does not complete the general
statement/store/alias dependency graph or justify removing BufferWriter snapshots
across unknown global/API lookups.

## Regression contract

The source fixture has nested-base, nested-key, both and plain-leaf functions.
Its 216 vectors combine failures at factory/base/key/invocation/store, scalar
7/false/nil results, nil/NaN/string keys and multi-result factories/callbacks.
Locked observations include ordered events, caught success/failure and the count
and value actually stored. All six runner optimization/debug profiles agree on
the original observations. The roadmap runner also compiles/decompiles/recompiles
the source at O0/O1/O2 with g1/g2 and checks one/four-thread output determinism.

The previous release fails all 162 nested-address vectors at each g1 profile;
its 54 plain-leaf vectors and all g2 vectors pass. Three native AST tests exercise
the nested factory refusal, mutable-capture snapshot and allowed plain-leaf
rewrite independently of compiler lowering.

Fixture: [source](failure_fixtures/roadmap_v2/nested_assignment_order.luau),
[driver](failure_fixtures/roadmap_v2/nested_assignment_order.driver.luau).

## Corpus validation

All 959 primary Rust tests, one child repeat, 93 Python tests, 174 runtime
configurations, nine negative controls, 513 public configurations, 45 legacy
semantic configurations and 52 size gates pass. The six new runtime rows compile
the subject body inline at their matrix profile instead of relying on require's
module compiler settings. Existing fixture execution modes are unchanged.

All 168 prior runtime and 513 public source hashes, complete dataflow results and
fidelity measurements are identical to the accepted scalar-conditional release.
All their prior detailed sidecars are identical after JSON decoding.
Parser-backed emission/provenance checks and cold/warm artifact-cache checks pass
for all 174 runtime and 513 public cases at one/four threads. All 3,978 private
source files remain byte-identical, including 42 empty files; all 3,936 nonempty
scripts retain their recorded-name contracts and capture certificates. No corpus
fidelity gain is claimed for this focused regression fix.

The six new whole-chunk symbolic comparisons remain `different`: the existing
module formatter changes table/closure construction into a named module and
sequential field stores. The focused function audit excludes that module wrapper;
its result must not replace the full-chunk status. See
[validation inventory](roadmap_v2_acceptance/lhs_validation.json).

All 24 uniquely named function/profile comparisons are independently proved by
the bounded symbolic checker. The audit recompiles hash-locked source/output,
checks exact original-bytecode replay and refuses missing/ambiguous function
names. CI runs [the audit](../scripts/assignment_order_audit.py) after the runtime
matrix and retains its result with the fixture artifacts.

Seven interleaved warm CLI rounds retain the same source tree at one/16 threads.
One-thread median changes 25.552 -> 25.191 seconds (-1.42%); 16-thread median
1.935 -> 1.988 seconds (+2.70%). Median peak RSS changes 33,468,416 -> 33,067,008
bytes and 122,179,584 -> 118,030,336 bytes respectively. Seven-sample p95 is the
maximum: 26.303 -> 26.208 seconds and 2.206 -> 2.222 seconds. This is a correctness
fix with measured cost, not a demonstrated speedup. These are same-run
comparisons, not comparisons against absolute timings from earlier sessions.
No cold OS cache or allocation claim is made. [Samples](roadmap_v2_acceptance/lhs_benchmark.json).
