# Research prompt: source-like, semantics-preserving Luau loop structuring

You are reviewing a Luau decompiler whose primary requirement is semantic
correctness, with readable output as the secondary goal. Do not edit any source
code or run mutating commands. Produce a detailed engineering design and save
your complete response as a Markdown file.

## Problem

The decompiler currently fails on real Luau bytecode after CFG structuring:

```text
DECOMPILE FAILED | ReplicatedStorage.GuiUtils.Pet |
control-flow structuring failed: residual goto/label would be invalid Luau
```

The same family of control flow also appears in nested/batch functions such as
`FishTrainingPopupArea`. The problematic graphs are compiler-generated generic
`for` loops (`FORGPREP`/`FORGLOOP`) with nested conditionals, `break`-like exits,
and values produced by the loop still used after the loop. A naïve rewrite can
make the output look beautiful while changing behavior, especially when a
loop-result register is used on an exhaustion path versus a `break` path.

## Required outcome

Design a general upgrade, not a one-off Pet pattern. It must:

1. Never emit invalid Luau labels/gotos.
2. Never silently change control flow, evaluation order, scope, local lifetime,
   iterator protocol, or values crossing a loop boundary.
3. Preserve the original CFG's semantics even for unsupported or ambiguous
   shapes. A proof failure must select a semantics-preserving fallback (for
   example a state machine), not guess.
4. Produce source-like output for every graph that can be proven to correspond
   to structured Luau: ordinary `for ... in ... do`, nested loops, conditionals,
   `break`, `continue` where legal, and post-loop result values.
5. Be efficient on large graphs. State the asymptotic cost and avoid repeated
   whole-graph work where possible.

## Input and output model

The input is a CFG whose blocks contain an AST-like Luau IR. Edges have explicit
`Then`, `Else`, or `Unconditional` tags and may carry SSA/phi arguments. Generic
loop markers have this shape:

```text
init:  (generator, state, control) = (iterator_expression, ...)
next:  results... = generator(state, control)
       if results[1] ~= nil then body else exhaustion
```

The output should be an AST/pretty-printed Luau program. For a representative
Pet-shaped graph, the desired form is similar to:

```luau
for k in pairs(pets) do
    local found
    for _, pet in ipairs(allPets()) do
        if pet.GUID == k then
            found = pet
            break
        end
    end
    if not found then
        pets[k] = nil
    end
end
```

The exact local names are not important. The important properties are that the
inner result is initialized correctly for zero iterations, copied out on an
early `break`, and is not referenced outside its source lifetime unless an
explicit, proven export is introduced.

## Analysis requested

Research and explain a robust algorithm. Address at least:

- dominance, post-dominance, natural-loop discovery, single-entry checks, and
  nested-loop ownership;
- branch-edge tags versus graph insertion order;
- generic-for iterator identity and hidden control register matching;
- SSA edge arguments/parallel copies and when a source-like pass must refuse;
- modeling `break`, `continue`, exhaustion, and multiple exits;
- live-out/result export with nil initialization and path-sensitive safety;
- local scope and declaration placement;
- irreducible CFGs and a fail-closed fallback;
- validation: AST invariants, bytecode/CFG equivalence, compilation checks,
  regression tests, and adversarial graphs.

Compare candidate strategies, identify unsound transformations, and give a
step-by-step implementation plan with invariants and pseudocode. The plan must
be detailed enough for another engineer to implement without guessing. Do not
implement the plan in this response.
