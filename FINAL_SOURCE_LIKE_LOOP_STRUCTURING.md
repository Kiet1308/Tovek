# Final design and implementation: source-like Luau loop structuring

## Scope

This document records the final engineering decision for the residual
`goto/label` failure seen in nested generic-for control flow (including
`ReplicatedStorage.GuiUtils.Pet`). It also accounts for the four correctness
findings in [PR #2](https://github.com/Kiet1308/Tovek/pull/2). It combines the
read-only research response
saved by the GPT-5.6 SOL MAX agent with the implementation that is now present
in this repository.

The research agent was instructed to save its response only. Its unmodified
response is [GPT56_SOL_RESPONSE.md](GPT56_SOL_RESPONSE.md). It did not compare
or edit `OtherResponse.md`. No `OtherResponse.md` is present in the current
workspace or Git history, so no comparison is invented here.

## Correctness target

The readable output should use ordinary Luau constructs when the CFG proves
that they are equivalent to the bytecode:

```luau
for key, value in pairs(petData) do
    -- nested loop/conditional body
end
```

If that proof cannot be established, the decompiler must preserve behavior with
the existing explicit state-machine fallback. It must never print an internal
`GenericForInit`, `GenericForNext`, invalid `goto`, or invalid `label`, and it
must never silently discard SSA edge-value transfers.

## Implemented architecture

### Status of the four PR review findings

The current checkout already contains the four correctness fixes requested in
the PR review, independently of the new region pass:

1. Inlining uses the shared `ast::statement_is_observable` / `ast::is_observable`
   barrier, so raising expressions such as computed-key table constructors are
   not crossed as if they were pure.
2. v13 double vectors remain `Literal::VectorD(f64, f64, f64)` through lifting
   and formatting; they are not narrowed to `f32`.
3. v12+ prototype parsing bounds `Function::parse` to the declared `protoSize`,
   rejects an undersized/overrunning slice, and resumes at the exact prototype
   boundary.
4. The harness discovers tools from environment variables or workspace-relative
   candidates and fails immediately when a required executable is missing;
   unexpected compile/runtime/decompile failures are not counted as passes.

These items are verified by the existing unit tests and are preserved by the
new speculative structuring path.

### Proof-driven source-shaped pass

`restructure/src/region.rs` adds an immutable, fail-closed region builder:

- computes reachable nodes, dominators, fixed-point post-dominators, and
  dominance-based natural loops;
- treats non-terminating SCCs as having no usable post-dominator and adds
  explicit straight-line edge-tag checks, so malformed `Then`/`Else` metadata
  cannot be erased as an unconditional transfer;
- merges multiple latches targeting the same dominating header and validates
  single-entry/nested-loop ownership;
- pairs `GenericForInit` and `GenericForNext` structurally, validating the
  generator/state/hidden-control identities and result arity;
- recursively builds nested loops and conditionals using the nearest valid
  common post-dominator;
- classifies loop-header edges as `continue`, loop exits as `break`, and rejects
  cross-owner escapes rather than guessing their meaning;
- tracks loop-result live-outs with per-iteration export locals, initialized to
  `nil`, and rewrites only proven post-loop uses;
- preserves compiler-generated normal-exhaustion adapters even when a body-side
  `break` shares the same adapter node, so a required `result = nil` cannot be
  skipped and leak a stale iterator result;
- validates that every normal-exhaustion adapter hop is explicitly
  `Unconditional`, including the final hop to the join;
- rejects pre-init aliases/captures and all unrecognized writes to exported
  iterator-result locals, preventing a rewritten export from diverging from a
  closure or alias that still observes the original register;
- rejects iterator protocol/result registers that alias function parameters or
  already-linked upvalue cells, because source `for` bindings cannot safely
  shadow those function-scope storage locations;
- rejects a nested loop whose inferred join escapes its enclosing loop, so an
  inner `break` can never accidentally continue the parent after a direct
  outer-exit edge;
- snapshots rewrite state across conditional arms and snapshots iterator RHS
  values before nested-loop rewrites;
- rejects pre-structured blocks whose nested AST bodies are not represented by
  the CFG being analyzed;
- rejects all edge arguments, malformed branch metadata, ambiguous marker
  shapes, unsafe hidden-protocol reads/writes, and unsupported statements;
- recognizes the legal empty-body generic-for shape whose `Then` edge loops
  directly back to its own header;
- treats an ordinary straight-line backedge as loop fall-through (without
  printing a synthetic trailing `continue`), while retaining explicit
  conditional `continue` transfers;
- commits an AST only when every reachable CFG node is consumed exactly once.

Post-dominator propagation uses a predecessor worklist instead of repeatedly
rescanning every CFG node. Speculative export-local allocation is transactional:
all generated IDs are rolled back on a rejected candidate or unwind, while a
successful AST commits them.

Returning `None` is intentional: it is a proof failure, not a license to emit
plausible but potentially wrong source.

### Semantics-preserving fallback

`luau-lifter/src/lib.rs` now runs the read-only source-shaped pass on an
isolated CFG clone while retaining a deep-cloned CFG for the mutating fallback.
If the proof pass declines the graph, graphs with SSA edge arguments go directly
to the fallback that materializes parallel copies; they are not sent through the
legacy matcher that could drop those values. Marker-only graphs retain the
established legacy matcher so ordinary generic-for loops stay source-shaped;
when that matcher leaves residual gotos or VM markers, the pristine CFG is
retried with the certified state-machine fallback. If even that fallback cannot
prove a lowering, the pipeline leaves an explicit internal marker so the final
invariant reports a decompilation error; it never returns a comment-only body
that silently erases the original program. Final AST checks still reject
residual gotos, labels, and internal loop markers.

`cfg/src/function.rs` and `ast/src/simplify_gotos.rs` provide deep cloning of
mutable nested AST block containers for the mutating fallback. The
proof-driven pass is read-only and uses a cheap graph clone; this avoids an
unnecessary recursive clone on the common successful path. Local identities
and closure-function identities remain shared as required by the AST contract.

### Defensive CFG handling

`Function::conditional_edges` now returns `None` for malformed branch tags
instead of asserting. Then/Else ordering is read from explicit edge metadata,
not from `StableDiGraph` insertion order.

## Hidden generic-for protocol

The source-shaped pass treats generator, state, and hidden iterator-control
locals as protocol values. Reads, writes, or captures outside the marker
semantics are rejected because ordinary source `for` syntax cannot reproduce a
stale or externally observed hidden register. The pass also retains exact
iterator setup order and all result slots, including unused slots.

`GenericForNext::LocalRw` intentionally does not report the control update as
a definite block write: Luau performs it only on the non-`nil`/`Then` edge,
while `LocalRw` is not edge-sensitive. The state-machine fallback materializes
that conditional assignment explicitly, and the source-shaped path proves the
protocol local is otherwise unobservable. Treating it as an unconditional write
would make exhaustion-edge liveness less accurate.

This is deliberately conservative: explicit protocol lowering is preferable to
silently changing iteration behavior.

## Verification performed

The following checks pass in the current tree:

- `cargo +nightly test --workspace --all-targets --quiet`
- `cargo +nightly check -p luau-lifter --quiet`
- `cargo +nightly build --release -p luau-lifter --quiet`
- `git diff --check`

The exact fixtures under `D:\Medal\bug` were also run through the release
`decompile-folder` binary (Roblox key `203`) using a mirrored temporary wrapper:

- `Pet.lua.bytecode.b64` (21,081 decoded bytes) decompiled successfully to
  35,187 bytes / 1,681 lines. `luau-compile --only-parse -O2 -g2 -t1` accepts it,
  and the output contains no failure marker, `goto`, label (`::`), or unlowered
  `GenericFor` node.
- The two Fish payloads are byte-identical. Both complete with exit code 0 and
  parse successfully; their output is a single newline because the selected
  root prototype (`p8`) has no instructions. The analysis sidecar records the
  other eight prototypes as orphaned (no closure site), so this is reported as
  an empty root rather than presented as recovered source.
- Re-running the same three files with one worker and with all 24 workers gives
  byte-identical output (Pet SHA-256
  `BC0593EB1B67EBC445AF3212A441EE1E8998695705C6DD8AED0FA638672A1E8C`). The
  Pet fixture also passes the `--no-synth-helpers`, `--assume-no-nan`, and
  `--dont-reuse-var` combinations.

The regression suite includes malformed branch order/tags, edge-argument
rejection, pre-structured-body rejection, top-level diamond continuation,
nested generic-for export scoping, branch-order independence, protocol safety,
deep-clone isolation, and a Pet-shaped nested generic-for whose inner `break`
passes through an adapter owned by the enclosing loop. The last shape proves
that an inner loop can still become readable source without mistaking a valid
break continuation for an illegal parent-loop escape.
It also covers a body-side break that targets the normal-exhaustion adapter and
asserts that the adapter's `result = nil` assignment remains in the emitted
branch, plus a malformed tagged adapter edge that must be rejected.
Exported-result aliasing and post-loop writes are also fail-closed with focused
regressions. A direct nested-to-enclosing-exit edge has its own regression and
is rejected rather than being rendered as a misleading single `break`.

## Decision

Use the source-shaped region builder whenever its proof succeeds, and otherwise
fall back without mutating or guessing. This keeps reducible nested loops close
to the original Luau source while making correctness the hard gate for every
transformation.
