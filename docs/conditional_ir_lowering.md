# Lowering existing scalar conditional IR

Disabling a rewrite that creates `IfExpression` is separate from lowering one
already present in the IR. `ast::lower_conditionals` handles the latter in the
v9 pipeline, after expression/guard cleanup and before final naming/formatting.
No later expression pass may inline its snapshots. It introduces literal `not`
guards without reducing/complementing an existing comparison.

Each selected value becomes a fresh local declared before an `if/else`; exactly
one arm assigns it. A single destination adjusts a call/vararg arm to one result,
including nil. Existing destination/source bindings retain their identity and
declaration point. A result read several times is computed once. Fresh locals
have no copied source/debug binding, SSA lineage or ownership certificate.
Their [explicit emitter introductions](emitter_local_origins.md) record selected
results, short-circuit results and evaluation snapshots by binding ID. Records
from a refused tentative rewrite are discarded; input ancestry remains unknown.

The lowerer snapshots preceding ordinary callee, argument and tuple values when
later conditional statements must execute. A last open call/vararg pack remains
in the final position, while `Select` adjustment wrappers remain intact. Nested
`and`/`or` lower their right-hand prefix inside the selected branch, preserving
both skipped evaluation and exact nil/false values.

## Compiler evaluation positions

The target is pinned Luau commit
`c2ec0d4e5ca50796ba174a7565298f59aa572268`, bytecode v9, all fast flags false.
Ordinary calls copy the callee before evaluating arguments. Arithmetic,
comparison and index operands can reuse a local register through
`compileExprAuto`; a colon call can reuse a local receiver register and performs
`NAMECALL` after its arguments. Concatenation prepares consecutive operand
temporaries. The lowerer distinguishes these positions using actual current
function declarations/parameters, not naming or type hints.

O2 inlining can upgrade a captured upvalue into a frame local. Two generated
witnesses demonstrate that the original expression can then observe a later
cell value in arithmetic/indexing than at O0/O1. An unconditional early snapshot
would freeze the wrong profile's behavior. The pass refuses a captured-local
register-reuse position crossed by a conditional that may write captured state.
It does not infer that a function cannot be inlined from its current shape.
Pure arms can still lower; unknown calls/metamethods retain the refusal.

For `while`, condition preparation runs at the start of every iteration,
including after `continue`, followed by an explicit false-condition break.
For `repeat`, it runs after the body in the condition's scope. A continue that
targets this repeat, or a terminal return that must remain last in its block,
refuses condition lowering. A nested loop's continue has its own target.
Numeric and generic loop inputs are prepared once before the loop, preserving
iterator tuple adjustment.

## Bounds and fallback

The initial tree inventory is bounded to 200,000 nodes and depth 128. Identifier
reservation runs only when the immutable inventory found a conditional. A failed
inventory changes nothing and sets `budget_exhausted`; its zero count is not a
claim that the unscanned tree has no conditional expressions. Shared closure
bodies are visited once. Names reserved anywhere in the original tree prevent
new locals from shadowing existing locals or global references, including
inside nested closures.

Per-function admission counts parameters, declarations from all scopes, hidden
loop protocol registers and a conservative expression/capture/destination
scratch bound, including indexed LHS expressions and assignment result slots.
Functions containing internal/opaque control or SETLIST markers refuse new
locals without those markers' separate register contract.
It limits declarations to 192 and the estimated register use to 240, leaving
headroom below the compiler's limits. This deliberately overcounts disjoint
scopes. A statement attempt constructs all replacement expression trees before
publishing them; refusal cannot leave partial branches or unpublished locals.
Nested child statements are independent attempts.

Conditional expressions inside table constructors and assignments to global or
indexed destinations currently refuse. Their allocation/key/store order needs
the broader R4 dependency analysis. Existing production reconstruction keeps
such regions in statement form at the earlier stage; an externally constructed
unsupported IR expression remains valid expression syntax. Refusal is explicit
and is not counted as statement-style completion.

The additive `conditional_lowering` sidecar field records the model, input and
lowered counts, introduced locals, refusal reasons and inventory exhaustion.
Counts describe unique IR function bodies, not necessarily the number of
printed occurrences of a shared body. Source-only operation emits no report
JSON. The artifact cache must preserve the report exactly; the executable hash
invalidates older cached payloads. Other input bytecode versions do not enable
this v9 lowering pass.

## Direct IR validation

The native example creates actual conditional IR at the pass boundary, writes
the original and lowered source, checks report/refusal counts and verifies
idempotence. The independent parser checks conditional-expression counts. The
VM audit compiles and executes every pair at O0/O1/O2 and g1/g2, with all fast
flags false. It embeds each module in a runner so the selected profile applies
without module-cache ambiguity.

The 40 examples cover callee and receiver mutation, frame/captured
operands, concatenation, nested selection, skipped branches, multiple uses,
scalar/open tails, while/continue, repeat, numeric/generic loops, name collisions
and the explicit refusals. Eight examples cover four vararg positions with
either an empty pack or two values ending in nil. The examples contain 42
conditional nodes: 34 lower, while eight keep explicit refusals. All 240
configurations pass 64 vectors each. The bounded dataflow validator proves 174
configurations and reports 66 unknown;
runtime observations do not promote the unknown comparisons. Eighteen compiled
mutant controls detect late callee reads, truncated open tails and eager
evaluation of an unselected arm. CI runs this direct IR audit separately from
the normal decompiler fixtures.

```powershell
cargo +nightly-2024-12-15 run -p luau-lifter --example conditional_lowering -- out/conditional-ir
python scripts/conditional_ir_audit.py --fixtures out/conditional-ir --compiler LUAU_COMPILE --luau LUAU_RUNNER --ast LUAU_AST --report out/conditional-ir.json
```

## Full pipeline acceptance

The final executable SHA is
`1d3b9fcfe7f1d2fcad1921be27a133183f78f9a79bbf767c9ab55ce379841f9f`.
All 956 primary Rust tests, one child-process repeat, 85 Python tests, 168
decompiler runtime configurations, nine oracle controls, 513 public
configurations, 45 legacy semantic configurations and 52 size gates pass.
The direct IR example is also built and executed with all 40 cases.

Every runtime/public source byte, complete dataflow report and source-fidelity
metric equals the accepted captured-read release. Every one of 3,978 private
source files has the same bytes, including 42 empty inputs. The source tree
remains `0147732a789ec9c6766fb37c97799e8be75adf2781e95cbc9f7f0f94fdf881ec`.
All 681 runtime/public and 3,936 private recorded-binding contracts and capture
certificates remain equal. Parser-backed emission/provenance checks and
one/four-thread cold/warm artifact-cache metadata checks pass.

All three corpora report zero remaining conditional IR nodes at this pipeline
boundary, with no inventory exhaustion. This is an implemented IR capability
and a guard for future passes, not a measured corpus fidelity improvement.
`scripts/conditional_corpus_audit.py` reproduces the source, recorded-binding,
capture-certificate and inventory checks using manifest-selected sidecars.
The benchmark measures the inventory path on these corpora; actual lowering
work is exercised by the separate generated IR suite.

Seven interleaved warm-filesystem CLI rounds retain identical source trees.
One-thread median changes 18.202 to 18.690 seconds (+2.68%); 16-thread median
changes 1.697 to 1.664 seconds (-1.91%). Median peak RSS changes 33,763,328 to
33,468,416 bytes and 114,438,144 to 117,493,760 bytes respectively. These are
finite cost observations, not an R7 speedup claim. Seven-sample p95 is the
maximum. [Validation inventory and individual reports](roadmap_v2_acceptance/select_validation.json)
include generated before/after sources, deduplicated VM observations, all
mutants, corpus identity checks, cache/provenance summaries and benchmark samples.
