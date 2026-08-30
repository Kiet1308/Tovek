# Source-like, Semantics-Preserving Luau Loop Structuring

## Scope and repository observations

This is a read-only design review of `D:/Medal/medal-decompiler`. No source code or
configuration is changed by this document. The proposed implementation is intended
for the post-SSA CFG used by the Luau lifter, while preserving the existing
semantics-preserving fallback.

The relevant pipeline is:

1. CFG construction and SSA construction/optimization in `cfg/src/ssa/`.
2. SSA destruction, including block-parameter/edge-copy materialization.
3. Readability-oriented graph restructuring in `restructure/src/`.
4. AST cleanup, local declaration, goto cleanup, and final validation in `ast/` and
   `luau-lifter/src/lib.rs`.

The Luau lifter represents generic-for VM instructions with two internal AST
markers:

- `ast::GenericForInit`, wrapping the parallel setup of generator, state, and the
  hidden iterator-control register.
- `ast::GenericForNext`, carrying result destinations, generator, state, and the
  hidden control `RcLocal`; its CFG terminator has `Then` and `Else` edges.

`cfg::block::BlockEdge` also carries ordered SSA-destruction arguments. Those
arguments are semantically significant and must be emitted as parallel copies
before an edge action.

## CFG reducibility

### Dominators and retreating edges

Treat `N16` as the synthetic entry. A dominance back-edge is an edge `u -> h` for
which `h` dominates `u`; this is stronger and more reliable than treating every
DFS ancestor edge as a loop edge.

The relevant dominance facts are:

- `N8` dominates `N7`, `N10`, `N9`, `N4`, `N5`, `N13`, `N12`, `N6`, and `N14`.
- `N10` dominates `N9`, `N4`, `N5`, `N13`, `N12`, `N6`, and `N14`.
- `N16 -> N2 -> N8` is the only path entering the outer cycle.
- `N7 -> N10` is the only path entering the inner cycle from outside that cycle.

Thus the dominance back-edges are:

```
N4  -> N10
N6  -> N8
N14 -> N8
```

The two outer latches are distinct, but both target the same dominating header.
That is a multi-latch natural loop, not an irreducible loop.

### Natural-loop sets

For a back-edge `latch -> header`, form the natural loop by starting with
`{header, latch}` and walking predecessor edges backwards from the latch, stopping
when the header is reached. Do not walk predecessors of the header itself; that
would incorrectly pull in the preheader.

For `N4 -> N10`:

```
L_inner = {N10, N9, N4}
```

For `N6 -> N8`:

```
L_outer_from_N6 = {N8, N6, N12, N5, N13, N9, N10, N7, N4}
```

For `N14 -> N8`:

```
L_outer_from_N14 = {N8, N14, N12, N5, N13, N9, N10, N7, N4}
```

Merge natural loops with the same header:

```
L_outer = {N8, N7, N10, N9, N4, N5, N13, N12, N6, N14}
```

The nesting is:

```
outer loop, header N8
└── inner loop, header N10
```

`N2 -> N8` is the outer preheader edge. `N7 -> N10` is the inner preheader/body
entry edge. The outer normal exit is `N8 -> N15`; `N16 -> N1` is a guard return
outside both loops.

### Edge interpretation

The graph is naturally represented as follows:

| CFG edge/path | Meaning |
|---|---|
| `N8 -> N7` | outer generic-for has a non-nil iteration result; enter body |
| `N8 -> N15` | outer iterator exhausted normally |
| `N7 -> N10` | initialize inner generic-for, then enter its header |
| `N10 -> N9` | inner iterator produced a non-nil first result; run body |
| `N10 -> N13` | inner iterator exhausted normally |
| `N9 -> N4 -> N10` | inner continue/back-edge when the item does not match |
| `N9 -> N5 -> N12` | matching item; explicit break from inner loop |
| `N13 -> N12` | normal inner exhaustion after explicitly setting `v16 = nil` |
| `N12 -> N6 -> N8` | match was found; continue outer loop |
| `N12 -> N14 -> N8` | no match; delete `petData[key]`, then continue outer loop |
| `N16 -> N1` | initial `v2` guard is false; return |
| `N15` | refresh calls followed by return |

The intended source-like form is therefore:

```luau
for key in pairs(petData) do
    local matchedPet

    for _, pet in ipairs(allPets()) do
        if pet.GUID == key then
            matchedPet = pet
            break
        end
    end

    if not matchedPet then
        petData[key] = nil
    end
end
```

The exact number of generic-for result variables must be retained. If a result is
not used, emit `_` rather than silently shortening the result list, because the
`FORGLOOP` auxiliary count controls the VM result tuple.

### Reducibility proof

The inner cyclic SCC `{N10,N9,N4}` has exactly one incoming edge from outside the
SCC (`N7 -> N10`), and `N10` dominates every node in the SCC. The outer cyclic SCC
`L_outer` has exactly one incoming edge from outside (`N2 -> N8`), and `N8`
dominates every node in it. Collapsing the inner SCC and then the outer SCC gives an
acyclic quotient graph. By the standard single-entry SCC/natural-loop criterion,
the CFG is reducible. The fact that the outer loop has two latches does not change
that conclusion because both retreat to the same dominating header.

## Why the current structurer fails

### Loop discovery is not dominance-based

`restructure/src/lib.rs::find_loop_headers` records `petgraph` DFS `BackEdge`
events. A DFS ancestor edge is not a complete proof of a natural loop, and DFS
classification can be affected by traversal order. The new analysis should compute
dominators and classify `u -> h` only when `h` dominates `u`.

### Loop collapsing assumes a linear body

`restructure/src/loop.rs::try_collapse_loop` has several assumptions that are
incompatible with this graph:

- A `FORGLOOP` body is expected to have at most one successor.
- Body/preheader relationships are checked with `exactly_one()` and direct
  successor tests.
- The ordinary loop-header path expects a direct header back-edge or a very narrow
  shape.
- The post-dominator-based `next`/`breaks` refinement treats exits as one inferred
  target rather than explicit owner-relative escapes.

For the failing function, `N9` has two successors (`N5` for match/break and `N4`
for continue), so it cannot be consumed as a linear body. `N10` has an exhaustion
edge to `N13`, and the outer body contains the entire nested loop and the `N12`
merge. The outer loop therefore also fails the direct one-successor shape.

### Init pairing is heuristic

`find_for_init` scans predecessor blocks backwards for side effects or a marker and
then calls `exactly_one().unwrap()`. It can select the wrong setup when there are
multiple predecessor paths, nested loops, or unrelated side effects, and malformed
graphs can panic. The `GenericForInit` and `GenericForNext` pair must instead carry
stable provenance and be validated structurally.

There is also an inconsistency: `is_for_next` checks the first statement of a block,
while the post-SSA CFG generally puts a marker at the terminator (the last
statement). A region analyzer should use a validated terminator accessor.

### Conditional matching is too narrow

`restructure/src/conditional.rs` recognizes diamonds and triangles. It does not
recursively structure a branch whose arms contain a nested loop, a break, a
continue, or different exits. `N9` is exactly such a branch.

### Last-resort labels are invalid for Luau

When pattern matching stalls, `GraphStructurer::collapse` invokes
`insert_goto_for_edge`. The resulting labels/gotos may cross lexical loop scopes or
have no legal Luau target. `luau-lifter/src/lib.rs` later checks the whole function
tree and reports `control-flow structuring failed: residual goto/label would be
invalid Luau`. Trying to repair a partially mutated graph is unsafe; a failed
structured attempt must restart from the pristine post-SSA clone.

The recent C8 and C12 fixes in the repository demonstrate the same weakness:
break-only bodies previously indexed an empty successor list, and a nested
multi-break shape could lose a middle-loop break when a post-dominator heuristic
collapsed the inferred exit to `None`.

## Proposed region-based structuring algorithm

### Design goals

The structurer should have a readable fast path for reducible regions and a proof
checked fallback for everything else. It should not use a Pet-specific pattern
matcher. A transformation is committed only after all claimed nodes and edges are
accounted for and the emitted AST is free of internal markers and illegal controls.

### Analysis phase

Create an immutable analysis snapshot of the post-SSA CFG:

1. Enumerate nodes reachable from the function entry and build stable predecessor
   and successor arrays. Sort adjacency by branch type and node index for
   deterministic output.
2. Validate every block terminator:
   - zero, one, or two outgoing edges must agree with the terminator kind;
   - conditional edges must be exactly one `Then` and one `Else`;
   - unconditional blocks must have at most one unconditional edge;
   - edge argument destinations and source expressions must be well formed.
3. Compute dominators from the real entry and post-dominators using one synthetic
   exit for all terminal nodes. Do not mutate the graph just to compute analysis.
4. Compute Tarjan SCCs in linear time.
5. Classify dominance back-edges (`header` dominates `latch`) and construct natural
   loops by reverse predecessor closure. Merge same-header latches and establish a
   strict nesting forest.
6. For every SCC, verify the single-entry property. An SCC with multiple external
   entry nodes, or whose candidate header does not dominate every member, is
   irreducible.
7. Index generic-for markers by provenance ID and by node. Reject duplicate,
   missing, or structurally ambiguous IDs before any AST mutation.

The analyses can be computed once for an immutable region tree. If the existing
mutable collapse machinery is retained for simple patterns, recompute only affected
ancestors after each atomic rewrite and never reuse stale dominator/post-dominator
objects.

### Region representation

A useful internal representation is:

```text
LoopInfo {
    id: LoopId,
    header: Node,
    body: BitSet<Node>,
    latches: Vec<Node>,
    normal_exit_edges: Vec<Edge>,
    children: Vec<LoopId>,
    parent: Option<LoopId>,
}

ForPair {
    id: ForId,
    init_node: Node,
    init_statement: usize,
    next_node: Node,
    generator: RValue,
    state: RValue,
    control: RcLocal,
    result_lvalues: Vec<LValue>,
}

Escape {
    Continue(LoopId),
    Break { loop_id: LoopId, values: ExitState },
    Fallthrough(Node),
    Return(Vec<RValue>),
}
```

`LoopId` is an analysis identity, not a source name. Every edge is classified using
the stack of containing loops, so a jump to an enclosing header is never mistaken
for an inner-loop break.

### Recursive sequence builder

Build AST blocks without deleting CFG nodes until the result is proven:

```text
try_structure(function):
    analysis = analyze_and_validate(function)
    if analysis.has_unhandled_irreducible_region():
        return Err(Irreducible)
    candidate = build_sequence(analysis.entry, FunctionExit, Context::root)
    verify_accounting(candidate, analysis)
    verify_semantics(candidate, analysis)
    Ok(candidate.ast)

build_sequence(entry, stop_set, context):
    out = []
    n = entry
    while n not in stop_set:
        assert n is owned by this region and not already consumed
        append ordinary statements from n, excluding a validated terminator

        match terminator(n):
            Return(values):
                append Return(values); mark terminal; return out

            Unconditional(edge):
                append edge_copies(edge)
                action = classify_edge(edge, context)
                if action is Continue/Break/Return:
                    append action; return out
                n = edge.target

            Conditional(condition, then_edge, else_edge):
                join = nearest_valid_common_postdom(then_edge.target,
                                                    else_edge.target,
                                                    stop_set)
                if join is absent:
                    reject CrossRegionOrIrreducible
                then_block = build_sequence(then_edge.target, {join}, context)
                else_block = build_sequence(else_edge.target, {join}, context)
                append If(condition, then_block, else_block)
                n = join

            GenericForNext(next_marker):
                loop = loop_owned_by_header(n)
                append build_loop(loop, context)
                n = loop.normal_exit_join

            malformed/unsupported:
                reject
    return out
```

The actual implementation may use a structured region tree rather than recursive
mutation. The key invariant is that every node is consumed exactly once by one
region, and a branch join is used only if both arms are contained in the same
single-entry region and all incoming edge arguments are represented.

### Loop construction

```text
build_loop(loop L, parent_context):
    require L.header dominates every L.body node
    require all external incoming edges target L.header
    pair = pair_for_loop_header(L.header, L.body)
    if pair is a GenericFor pair:
        validate_generic_protocol(pair, L)

    ctx = Context {
        current_loop: L.id,
        header: L.header,
        body_nodes: L.body,
        normal_exits: computed from marker Else/ordinary exits,
        parent: parent_context,
    }

    body_entry = marker_then_target(L.header)
    body_ast = build_sequence(body_entry,
                               {L.header, *L.normal_exit_join_nodes},
                               ctx)

    verify_all_internal_edges_are_classified(L, ctx)
    verify_no_unclaimed_nodes(L)
    return emit_loop(pair, body_ast, ctx.exit_state_plan)
```

Loop-edge classification is owner-relative:

```text
classify_edge(edge e, context stack):
    target = e.target
    copies = lower_parallel_edge_arguments(e)

    if target == current.header:
        return copies + Continue(current.id)

    if target is a proven current-loop exit:
        return copies + Break(current.id, values_at_edge(e))

    if target == parent.header or target is a parent latch/exit:
        return copies + propagated_escape_to_parent(e)

    if target lies in current region:
        return copies + Fallthrough(target)

    reject CrossRegionEdge
```

An edge to a nested loop's normal exit is not a break from the enclosing loop; it
is the continuation point after the nested loop. Conversely, an edge from an inner
body directly to an enclosing header must propagate an enclosing `Continue`, not
emit an inner `Break`. This explicit owner stack fixes the C12 class of bugs.

For readability, a terminal edge to the current header can be omitted as an
explicit `continue` when it has no edge copies and is the natural end of the loop
body. If it has copies, side effects, or occurs before later body statements, emit
the copies followed by `continue`.

### Conditional construction with escapes

The branch builder must allow each arm to terminate independently:

```text
if condition then
    <ordinary statements>
    break                -- current loop, if the arm targets its exit
else
    <ordinary statements>
    continue             -- current/enclosing loop, if proven
end
```

Do not require either arm to have one successor. A branch with one arm entering an
inner loop and another arm taking a parent escape is valid if its join and ownership
are proven. Preserve a condition expression that can raise even when both arms are
empty; the existing `is_total_pure` safeguard in the jump/SSA structuring code is a
good precedent.

## Generic-for pairing and hidden control

### Current VM/lifter contract

`luau-lifter/src/op_code.rs` documents the register layout:

```text
[generator, state, index/control, result_1, result_2, ...]
```

`FORGPREP*` initializes generator/state/control and jumps to `FORGLOOP`.
`FORGLOOP` calls `generator(state, control)`, copies all auxiliary-count results to
the result registers, tests the first result for nil, and copies that first result
back to the hidden control register only on the non-nil path.

`luau-lifter/src/lifter.rs` currently creates `GenericForInit` around registers
`R[A], R[A+1], R[A+2]` and `GenericForNext` with result registers
`R[A+3..A+3+aux]`, generator `R[A]`, state `R[A+1]`, and control `R[A+2]`.

### Stable pairing

Add a stable `ForId`/provenance field to both markers, preferably derived from the
`FORGPREP` PC and its validated target `FORGLOOP` PC (or an instruction identity
that survives CFG cloning). The ID must be preserved through SSA/local rewrites and
remapped or rejected when a transform duplicates a marker.

The pair validator should require:

1. Exactly one init with the ID reaches the corresponding next header on every
   possible loop-entry path.
2. Exactly one next marker with the ID is at the loop-header terminator.
3. Generator/state/control identities match, or are connected by a proven SSA
   congruence with no intervening observable write.
4. Result destinations are local registers, nonempty, and have the exact auxiliary
   arity.
5. The next marker has exactly one `Then` and one `Else` edge.
6. No unrelated init/next marker is consumed by this loop.

Missing, duplicate, or ambiguous IDs must produce a structured failure, never a
guess. A side-table keyed by marker object identity can work if changing AST structs
is undesirable, but it must obey the same validation rules and cloning contract.

### Source syntax eligibility

Emit `ast::GenericFor` only when the hidden protocol is unobservable outside the
loop, or when all observable values are represented by explicit exports. In
particular, check whether generator/state/control registers are read or written
after the loop, whether result slots are used after exhaustion, and whether body
code mutates a protocol local. If proof fails, lower the loop to explicit protocol
code or use the dispatcher fallback.

Keep the iterator setup exactly once and in order. If the preceding init block
contains side-effecting calls, retain those statements and use the initialized
locals as the generic-for right-hand tuple; do not reconstruct a call in the loop
header unless evaluation equivalence is proven.

## Loop-exit values, exports, and scope

### Why direct loop variables are unsafe

`ast::local_declarations.rs` treats `GenericFor.res_locals` as declared in a child
scope. A result local that is read after the loop cannot simply be placed in
`GenericFor.res_locals`, because the emitted source would either fail to resolve the
name or accidentally bind a different outer name.

For each loop exit, compute an `ExitState` map for locals that are live after the
loop. The map records the exact value reaching the exit, including edge-argument
copies. If all exits have a single proven equivalent value, an existing outer local
may be used. Otherwise synthesize a fresh export local in the nearest enclosing
scope and assign it on each relevant exit.

For a match/break pattern, the safe template is:

```text
declare export before the inner loop, in the outer-loop body scope
set export = nil for this outer iteration
on matching edge: export = exact loop result; break
on normal exhaustion: leave export nil (or assign the proven normal value)
after inner loop: test/use export
```

This is why `matchedPet` must be declared/reset inside every outer iteration. A
function-level declaration would retain a previous outer iteration's match and
change behavior.

Do not assume non-first generic-for result registers become nil on exhaustion. Luau
only tests the first result; other registers can be stale or contain returned
values. If a post-loop use observes one of them and its normal-exit value is not
proven, retain explicit protocol lowering or reject source syntax.

### Fresh names and declarations

Allocate a fresh `RcLocal` and a deterministic textual name. Reserve all local and
global identifiers visible in the enclosing scope (the existing
`ast::rehoist_constants::collect_reserved_identifiers`/`unique_name` utilities are a
useful model), so a synthetic `matchedPet` cannot shadow a global lookup or sibling
binding. Ensure the declaration is emitted before any read and is not moved by
`LocalDeclarer` into a wider scope.

### Closures, `Ref`, and `Close`

Generic-for and explicit dispatcher lowering must preserve Lua cell lifetime:

- A by-value closure capture may require a snapshot per iteration.
- A by-reference capture requires the same cell identity and close lifetime as the
  source bytecode.
- A loop variable captured by a closure may need a fresh per-iteration cell; using
  one shared synthetic variable can make all closures observe the final value.

`cfg/src/ssa/construct.rs::mark_upvalues` currently analyzes `Close` statements and
then removes them from blocks. Once removed, a flat post-SSA CFG lacks enough
information to decide some cell-lifetime questions. Preserve close/ref annotations
before removal, or conservatively reject a source-like loop when a relevant `Ref`
capture is ambiguous. Never hoist such a local across dispatcher iterations unless
its cell is proven function-scoped.

## Parallel assignment and evaluation order

Every edge argument is an SSA-destruction parallel copy. Emit all RHS expressions
before writes, and preserve the order used by `cfg/src/ssa/destruct.rs`. The existing
sequentializer specifically pre-evaluates interfering expressions to handle swaps
and recurrences such as:

```luau
x, y = y, x + y
```

Do not replace a parallel edge copy with a sequence merely because the destination
names look simple. This is the C3 failure mode. Likewise, a generic-for next step
must evaluate `(generator, state, control)` before writing any result or control
destination; the control assignment follows the non-nil test and never occurs on
the nil path.

## Proof obligations and fail-closed conditions

Before committing a source-like region, prove all of the following:

1. **Path correspondence:** each CFG path through the region corresponds to one and
   only one emitted AST path, and every reachable CFG node/edge is represented.
2. **Loop iteration correspondence:** each marker invocation corresponds to exactly
   one source loop test/iteration; every back-edge is represented as a continue or
   natural fallthrough.
3. **Evaluation order:** init expressions, iterator calls, conditions, indexing,
   metamethod-triggering operations, and edge copies execute in source order.
4. **Value semantics:** nil tests, truthiness, multi-result adjustment, varargs,
   aliasing, and first-result control updates are unchanged.
5. **Error semantics:** operations that can raise (comparisons, arithmetic,
   indexing, length, calls, metamethods) are neither dropped nor duplicated.
6. **Scope/cell semantics:** declarations, shadowing, closure captures, `Ref`
   cells, `Close` boundaries, and per-iteration loop variables have equivalent
   lifetimes and identities.
7. **Exit semantics:** returns, multi-return tails, breaks, continues, normal loop
   exhaustion, and parent-loop escapes target the same continuation.
8. **Edge values:** every destination parameter on every incoming edge receives
   exactly one parallel assignment with the original value.
9. **Marker accounting:** every internal marker is consumed exactly once, and no
   malformed marker is printed.
10. **Legality:** generated `break`/`continue` is lexically inside the loop it
    targets; no residual `Goto`, `Label`, or internal marker remains.

Reject the structured candidate when any obligation cannot be established. Specific
rejection conditions include:

- malformed branch arity or branch-type labels;
- unreachable or multiply-owned nodes;
- duplicate/missing/ambiguous generic-for provenance;
- multiple unproven preheaders or external entries;
- cross-region edges with no structured escape action;
- nonlocal/unsupported generic-for result destinations;
- unknown or dynamically ordered edge arguments;
- hidden protocol locals observed outside a source-loop-equivalent scope;
- conflicting exit values or unproven normal-exhaustion values;
- `Ref`/`Close`/closure lifetime ambiguity;
- unsupported terminators, malformed returns, or multret/vararg uncertainty;
- an irreducible SCC for which a local dispatcher cannot model all exits safely.

Failure must return an error/`None` before mutating the caller's function. The caller
then retries using the untouched post-SSA clone.

## Smallest irreducible SCC fallback

After structured reducible regions are collapsed, identify the smallest residual SCC
with multiple external entries or no dominating header. Structure any reducible
children inside it first. Lower only that SCC to a local dispatcher:

1. Assign deterministic dense state IDs to SCC nodes.
2. Emit an entry adapter for each incoming edge, including parallel edge copies.
3. Emit one state body per SCC node. Each body ends in a state assignment plus
   `continue`, or in a modeled return/parent escape.
4. Lower residual `GenericForNext` using the exact explicit iterator protocol.
5. Emit exit adapters that copy values and resume the parent region, break the
   correct enclosing loop, continue it, or return.
6. Hoist only locals proven live across dispatcher iterations; reject ambiguous
   `Ref`/`Close` cells.

This can be factored from `restructure/src/fallback.rs` as `lower_subgraph`. Keep
the existing whole-function `lift` and `lift_with_ignored_locals` as the final
safety net. The current fallback already:

- lowers generic-for init/next with explicit control;
- preserves parallel multi-result assignments;
- computes persistent locals across state transitions;
- refuses unsafe reference captures and unlowered numeric markers.

Do not feed a partially label-mutated graph to the fallback. If a local dispatcher
cannot be proven, rebuild from the pristine function and invoke the whole-function
fallback; if that also returns `None`, report a decompilation failure rather than
emitting plausible but incorrect source.

## Readability strategy

Use a two-tier policy:

- **Readable fast path:** ordinary reducible loops become `GenericFor`, `NumericFor`,
  `While`, `If`, `Break`, and `Continue`; marker/provenance fields are internal and
  invisible to formatting. Introduce export locals only when liveness requires them,
  with deterministic names.
- **Correctness path:** explicit iterator protocol or a small state dispatcher is
  emitted only for shapes that cannot be represented by source syntax with a proof.
  The whole-function dispatcher remains the last resort.

The final AST checks in `luau-lifter/src/lib.rs` and
`ast/src/simplify_gotos.rs` remain hard gates. `simplify_gotos` may clean up legal
internal structures, but it must not be relied upon to infer loop ownership after a
failed region transformation.

## Exact files and functions for a future patch

### `restructure/src/lib.rs`

- Add an analysis object (`LoopInfo`/`LoopForest`) using dominators, backedges, SCCs,
  and post-dominator joins.
- Add a proof-returning `try_structure`/`try_lift` API and an atomic region driver.
- Replace DFS-only loop-header discovery for the new path.
- Keep a compatibility `lift` wrapper if callers still require `ast::Block`.
- Prevent last-resort goto insertion from being used before the proof-based path
  has declined the region.

### `restructure/src/loop.rs`

- Replace `find_for_init` predecessor scanning with marker-indexed pairing.
- Rewrite `try_collapse_loop` around natural-loop regions and owner-relative edge
  classification.
- Support nested conditionals, multiple latches, multiple breaks, normal exits,
  and exit-state/export synthesis.
- Use a consistent validated terminator accessor (`last`, not an accidental `first`).

### `restructure/src/conditional.rs`

- Add recursive branch-region construction using nearest valid common post-dominators.
- Allow arms to end in break/continue/parent escapes and preserve edge copies.
- Retain the existing raising-condition safeguards.

### `restructure/src/jump.rs`

- Guard block merges by region ownership, marker identity, and edge arguments.
- Reject merges that cross a loop scope or alter parallel-copy order.
- Avoid assertions on malformed graph shapes.

### `restructure/src/fallback.rs`

- Factor generic state-machine lowering into a subgraph/SCC API with entry and exit
  adapters.
- Retain whole-function fallback behavior and existing persistent-local/Ref tests.

### `ast/src/for.rs`

- Add optional `ForId`/bytecode provenance to init/next markers, or define a robust
  side-table contract.
- If needed, add internal hidden-control/exit metadata to `GenericFor` while keeping
  display, traversal, local-read/write, and clone behavior correct.
- Update all constructors and transforms that copy markers.

### `luau-lifter/src/lifter.rs`

- Assign provenance IDs from validated `FORGPREP*`/`FORGLOOP` instruction pairs.
- Validate jump targets, auxiliary result counts, and marker placement.
- Replace loop-specific `assert!`/`unwrap` paths with propagated malformed-bytecode
  errors where practical.

### `cfg/src/ssa/construct.rs` and upvalue handling

- Preserve loop-marker provenance and close/ref-lifetime annotations before `Close`
  removal.
- Ensure local-map and phi transformations cannot duplicate or silently merge marker
  identities.

### `cfg/src/ssa/destruct.rs`

- Keep the current parallel-copy correctness machinery.
- Add verification/tests for edge-copy ordering, loop-carried swaps, and marker
  control values rather than changing semantics casually.

### `ast/src/local_declarations.rs`

- Add explicit support for synthetic export locals and dispatcher-persistent locals.
- Ensure declarations inside an outer loop are reset per iteration and loop result
  locals remain child-scoped.

### `luau-lifter/src/lib.rs` and `lua51-lifter/src/main.rs`

- Call the proof-returning structured path on a clone.
- On failure, invoke fallback using the pristine post-SSA CFG.
- Preserve function parameters/upvalues/variadic flags and update both callers if
  the `lift` API changes.
- Keep final whole-function marker/goto validation.

### `ast/src/simplify_gotos.rs`

- Keep the existing conservative cleanup and direct-label dispatcher as a final
  safety net.
- Optionally recognize the new region dispatcher, but do not make it responsible for
  repairing an already-invalid nested loop.

## Regression and differential tests

### Synthetic CFG tests

Add a CFG builder in `restructure` tests for the exact `f22.dot` graph. Assert:

- dominance backedges and natural-loop sets are exactly those listed above;
- the outer and inner loop regions are nested and reducible;
- the emitted AST contains an outer and inner `GenericFor`, the matching `If`, and
  an inner `Break`;
- `matchedPet`/equivalent export is scoped and reset inside the outer body;
- no `GenericForInit`, `GenericForNext`, `Goto`, or `Label` remains.

Add shape variants:

- inner continue only;
- break plus normal exhaustion to a shared join;
- outer continue from both conditional arms;
- multiple latches to one header;
- empty body and break-only body;
- three nested loops with breaks at each depth (C12 shape);
- multiple loop exits and parent-loop escapes;
- nested loops whose edge arguments swap or carry values.

### Marker/protocol tests

- missing, duplicate, or mismatched `ForId` must return a structured failure;
- generator/state/control mismatch must not be guessed;
- init expressions execute once and in source order;
- hidden control updates only after a non-nil first result;
- nil first result leaves control unchanged;
- all auxiliary multi-results and `_` placeholders are retained;
- iterator calls, `__pairs`/`__index` metamethods, and errors occur exactly once.

### Scope and capture tests

- a loop result used after the loop receives a proven export;
- the export resets on each outer iteration;
- conflicting break/exhaustion values use explicit protocol or reject;
- body mutation of result locals is preserved;
- by-value and by-reference closure captures retain their intended snapshots/cells;
- per-iteration closure captures are fresh;
- `Close` boundaries prevent unsafe hoisting.

### Parallel/exit tests

- Fibonacci/swap loop-carried copies (C3);
- iterator multi-return advance and terminating nil behavior;
- branch-specific phi/edge arguments;
- return inside either branch, multi-return return tails, and varargs;
- runtime errors in comparisons, arithmetic, indexing, length, and metamethods;
- no duplicate side effects when a continuation is shared.

### Existing end-to-end harness

Use `_harness` with `luau-compile --binary -O0`, `-O1`, and `-O2`, the lifter, and
the Luau runtime. Compare normalized stdout, error behavior, and observable table/
upvalue state between original and decompiled programs. Include:

- `_harness/_bugs/C8.luau`;
- `_harness/gen2/nestedctrl__nestedctrl_triple_break_label.luau` (C12);
- `_harness/gen2/loopcarry__loopcarry_swap_advance.luau` (C3);
- `_harness/gen/inline-bait__inline-bait-helper-mutate-upval.luau` (C4);
- `_harness/gen/closures-upval__closures-upval-loopvar-while.luau` (C6);
- a SaveInstance reproduction proving `applyData` decompiles without the residual
  goto/label error.

Compile generated source again as a syntax check and assert that the complete AST
tree has no residual internal markers or labels/gotos. Keep all current fallback
tests, especially explicit hidden-control lowering and unsafe `Ref` rejection.

## Summary

The failing graph is a reducible nested, multi-latch natural-loop structure. The
right fix is not a Pet-specific recognizer or a relaxed `exactly_one` check. It is a
dominance/SCC-backed region builder that recursively structures conditionals,
tracks loop ownership for every escape, pairs generic-for markers by provenance,
models loop-exit values and lexical scope explicitly, and commits only after a
path/evaluation/cell-lifetime proof. Reducible regions remain readable; the smallest
unprovable SCC uses an explicit local dispatcher; the existing whole-function
fallback remains the final fail-closed safety net.
