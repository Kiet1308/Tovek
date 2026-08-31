# PR #3 residual-control re-review: exact failing files

This is the GitHub-visible companion to the local corpus logs.  It lists every
currently failing input by its stable Roblox project path, the rejected function
ID, and the proof reason.  The source bytecode itself is not checked in, so a
reviewer can use these paths as corpus selectors and use the committed fixtures
for a self-contained reproduction.

## Current matrix (13 files, 14 rejected functions)

| input path | function | diagnostic | evidence / why it is not safe to relax |
| --- | ---: | --- | --- |
| `ReplicatedStorage/FusionPackage/Components/Processors/GameUpgrade.lua` | `p2` | `CapturedLoopResultRef` | loop result is captured; per-iteration cell lifetime is not proven |
| `ReplicatedStorage/FusionPackage/Fusion/State/For/Disassembly.lua` | `p3` | `source_like_unsupported` | `goto` exits a generic-for to a label after it; other transfers cross nested loop/branch regions |
| `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Lerps.lua` | `p29`, `p27` | `ForOriginPrepKindUnsupported` | `INEXT` origin uses a local/upvalue callable alias; no provenance proves it is the builtin `ipairs` protocol |
| `ReplicatedStorage/Part_Icles/Engine.lua` | `p18` | `ForOriginPrepKindUnsupported` | same unproven generic-for prep protocol |
| `ReplicatedStorage/Shared/CutsceneUtil.lua` | `p6` | `source_like_unsupported` | generic-for body jumps to an outer label and has a back-edge to the outer label |
| `ReplicatedStorage/Shared/ForgeVFX/mod/lerp.lua` | `p11` | `source_like_unsupported` | nested interpolation loops have inner and outer exits whose labels cross loop boundaries |
| `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/lerp.lua` | `p11` | `source_like_unsupported` | same cross-loop transfer pattern as `ForgeVFX/mod/lerp.lua` |
| `ReplicatedStorage/Shared/Information/GameModifiers.lua` | `p2` | `source_like_unsupported` | first generic-for body jumps to a post-loop label; remaining labels form a cross-loop state machine |
| `ReplicatedStorage/Shared/Network/BufferEncoder/Write.lua` | `p3` | `source_like_unsupported` | gotos enter sibling `if` regions and later nested blocks; current simplifier cannot legally lower that LCA crossing |
| `ReplicatedStorage/Shared/TimeManager/Part_Icles/Engine.lua` | `p18` | `ForOriginPrepKindUnsupported` | same unresolved prep metadata as `Part_Icles/Engine.lua` |
| `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Effects/LensFlare/LensFlare.lua` | `p6` | `CapturedCellReorder` | iterator preparation could observe a captured mutable cell in a different order |
| `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Gamemodes/Expedition.lua` | `p64` | `source_like_unsupported` | a tail transfer jumps into a nested conditional branch; natural-loop intent is not yet proven |
| `StarterPlayer/StarterPlayerScripts/Mounts/ShenronDragon/init.lua` | `p19` | `source_like_unsupported` | generic-for body exits to an outer label and later jumps back to the outer loop label |

The two functions in `MoonPlayer/.../Lerps.lua` explain the 14-function versus
13-file count.

## Concrete residual-goto shape

The repeated unsupported pattern is structurally equivalent to:

```text
outer loop/header
  -> nested if or generic-for
       -> goto label owned by the outer follow/sibling region
  -> label / back-edge outside the child region
```

The existing simplifier only creates a dispatcher for labels owned directly by
one block.  It intentionally leaves a transfer that crosses the lowest common
ancestor untouched; formatting that AST would produce invalid Luau.  A fix must
plan the transfer at that ancestor (or perform a proven CFG reducibilization),
not merely enable the legacy matcher.

For example, `Fusion/State/For/Disassembly.lua:p3` contains an exit from inside
`_subObjects`' generic-for to `l8` after the loop, plus additional `l13/l17/l14`
transfers across nested loops.  `Shared/Network/BufferEncoder/Write.lua:p3`
contains sibling-`if` crossings (`l87`, `l67`) in a much larger state machine.
`Shared/ForgeVFX/mod/lerp.lua:p11` and its `ForCutscenes` copy contain both inner
and outer interpolation-loop exits (`l30`, `l7`).

## Reproduction and acceptance gates

Run the full corpus command from the companion report and inspect the typed
diagnostics in the generated manifest.  For an isolated path, place its encoded
payload in a one-file input tree and run with `--threads 1 --verbose`; the result
must reproduce the same function ID and diagnostic.  A candidate fix is accepted
only if it:

1. removes the failure without emitting `goto`, labels, or internal loop markers;
2. parses/compiles with the pinned official Luau tool;
3. preserves generic-for prep/step/exhaustion, edge-argument copies, and closure
   cell identity; and
4. gives byte-stable output at one and eight workers.

Until those proofs exist, the typed fail-closed diagnostics above are the correct
behavior; converting them to a weaker legacy fallback would hide a semantic bug.

