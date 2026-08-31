# PR #3 — Current Control-Flow Failure Status

## Purpose

PR #3 currently improves source-like structuring for several Luau generic-for
shapes. The remaining project goal is broader: eliminate the entire class of
failures reported as:

```text
control-flow structuring failed: residual goto/label would be invalid Luau
```

This document is a status/evidence brief and implementation checkpoint for a
stronger model. It records both the pre-change baseline and the current
proof-backed progress; it does not claim that the entire corpus is solved.

## Repository state

- Repository: `D:\Medal\medal-decompiler`
- Branch: `fix/pet-source-like-loop-structuring`
- PR: https://github.com/Kiet1308/Tovek/pull/3
- Baseline code snapshot: `910a2d6` (`Preserve compiler generic-for break bodies`)
- The current PR patch adds path-sensitive terminal transfer handling, typed
  per-function diagnostics, and an auditable corpus manifest. Generated corpus
  outputs and scratch logs under `target/` are local evidence only.

## Baseline corpus result

Latest release audit at HEAD `910a2d6`, using
`D:\Medal\examplebytecode\RobloxProject` (3,978 `.lua` bytecode entries):

| Result | Count |
|---|---:|
| Decompiled successfully | 3,626 |
| Skipped: empty decoded payload | 42 |
| Explicit decompile failures | 310 |
| Emitted `.luau` files | 3,668 |
| Emitted files failing official Luau syntax parsing | 0 |

All 310 explicit failures in that baseline reported the same public reason:
`control-flow structuring failed: residual goto/label would be invalid Luau`.
There are no observed decode/base64 failures in this run. The 42 skipped files
are genuinely empty payloads, not control-flow failures.

Failure log:

`target/review_corpus_910a2d6_default_t8.err`

The 310 files are concentrated in reusable/framework and generated-controller
code rather than random corruption: `ReplicatedStorage` 252,
`StarterPlayer` 41, `Workspace` 17. Large clusters include `FusionPackage`,
`Shared/Information`, `Shared/TimeManager`, `Part_Icles`, `Animate.client.lua`,
and `PlayerModule`.

### Representative failing examples

Every example below exits with the same final message in the current batch;
the list is intended to show graph/feature diversity, not to claim that the
batch currently records a distinct typed cause for each path.

| Area | Example input | Why it is useful as a planning fixture |
|---|---|---|
| Cmdr server | `ReplicatedStorage/Cmdr/Server commands/Admin/teleportServer.lua` | Small command module that still leaves residual control flow. |
| Cmdr client | `ReplicatedStorage/CmdrClient/Shared/Dispatcher.lua` | Dispatcher-style branching and shared continuation paths. |
| Fusion UI | `ReplicatedStorage/FusionPackage/Components/Base/Container.lua` | Framework component with nested callbacks/branching. |
| Fusion async | `ReplicatedStorage/FusionPackage/Fusion/State/ComputedAsync/Promise/init.spec.lua` | Promise/async state graph; large nested control-flow surface. |
| Data/config | `ReplicatedStorage/Shared/Information/Abilities.lua` | Representative member of the 46-file `Shared/Information` cluster. |
| Time/particles | `ReplicatedStorage/Shared/TimeManager/MeshEmitter/init.lua` | Shared utility with loop-heavy update logic. |
| Particle engine | `ReplicatedStorage/Part_Icles/Emit.lua` | One of the duplicated `Part_Icles`/`TimeManager` utility families. |
| VFX | `ReplicatedStorage/DivergentVFX/LightningCore.lua` | Largest sampled failure (encoded input ~110 KB). |
| Character animation | `StarterPlayer/StarterCharacterScripts/Animate.client.lua` | Generated animation/state-machine shape; many player copies fail too. |
| Player controller | `StarterPlayer/StarterPlayerScripts/PlayerModule/ControlModule/ClickToMoveController.lua` | Large controller with complex branch/loop graph. |
| Client game logic | `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Gamemodes/Expedition.lua` | Nested gameplay control flow and callbacks. |
| Player copy | `Workspace/Players/WGIT05/Animate.client.lua` | Confirms the animation failure is repeated across saved player copies. |

The exact machine-readable evidence is the failure log listed above; it has
310 `FAIL` entries and no alternate decode error for these examples.

## Post-change probe

The current patch was rebuilt and run against the same corpus with:

```powershell
target/debug/luau-lifter.exe decompile-folder `
  D:\Medal\examplebytecode\RobloxProject `
  target\corpus_analysis_20260831_2145 `
  --key 203 --threads 8 --emit-upvalue-analysis --verbose
```

The result is 3,923 successful outputs, 42 empty-payload skips, and 13
failures. Every remaining failure is fail-closed and carries a typed
per-function diagnostic in `.tovek-analysis/manifest.json`:

| Diagnostic class | Files | Meaning |
|---|---:|---|
| `source_like_unsupported` | 8 | No proven source-level region representation yet. |
| `source_like_unsafe_ForOriginPrepKindUnsupported` | 3 | Generic-for prep kind is not source-proven; one file contains two rejected functions. |
| `source_like_unsafe_CapturedLoopResultRef` | 1 | A captured loop-result cell lacks a proven per-iteration lifetime. |
| `source_like_unsafe_CapturedCellReorder` | 1 | Iterator preparation could reorder a captured-cell observation. |

The 13 paths are listed verbatim in `target/corpus_analysis_20260831_2145.err`.
The same counts were reproduced at one and eight workers, and default and
strict policy produced byte-identical source outputs. These 13 cases remain
open implementation work; they are no longer misreported as an unclassified
residual-goto symptom.

For reviewers who only have the GitHub checkout (and not the local `target/`
log), the remaining paths and first rejected function(s) are:

| Path | Diagnostic / function |
|---|---|
| `ReplicatedStorage/FusionPackage/Components/Processors/GameUpgrade.lua` | `source_like_unsafe_CapturedLoopResultRef` / `p2` |
| `ReplicatedStorage/FusionPackage/Fusion/State/For/Disassembly.lua` | `source_like_unsupported` / `p3` |
| `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Lerps.lua` | `source_like_unsafe_ForOriginPrepKindUnsupported` / `p29`, `p27` |
| `ReplicatedStorage/Part_Icles/Engine.lua` | `source_like_unsafe_ForOriginPrepKindUnsupported` / `p18` |
| `ReplicatedStorage/Shared/CutsceneUtil.lua` | `source_like_unsupported` / `p6` |
| `ReplicatedStorage/Shared/ForgeVFX/mod/lerp.lua` | `source_like_unsupported` / `p11` |
| `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/lerp.lua` | `source_like_unsupported` / `p11` |
| `ReplicatedStorage/Shared/Information/GameModifiers.lua` | `source_like_unsupported` / `p2` |
| `ReplicatedStorage/Shared/Network/BufferEncoder/Write.lua` | `source_like_unsupported` / `p3` |
| `ReplicatedStorage/Shared/TimeManager/Part_Icles/Engine.lua` | `source_like_unsafe_ForOriginPrepKindUnsupported` / `p18` |
| `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Effects/LensFlare/LensFlare.lua` | `source_like_unsafe_CapturedCellReorder` / `p6` |
| `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Gamemodes/Expedition.lua` | `source_like_unsupported` / `p64` |
| `StarterPlayer/StarterPlayerScripts/Mounts/ShenronDragon/init.lua` | `source_like_unsupported` / `p19` |

The two `MoonPlayer` functions are counted as one failed file, hence 13 files
but 14 rejected function diagnostics in the JSON evidence.

Actual reproducible payloads for seven representative paths are committed in
[`docs/failure_fixtures/residual_control_flow/`](failure_fixtures/residual_control_flow/),
with a runnable command and expected result in its README. These fixtures are
copied bytecode inputs, not reconstructed source, and cover Cmdr, Fusion,
shared data/time utilities, particle utilities, animation, and PlayerModule
controller families.

## What is already fixed

The following targeted cases pass in both default and
`--strict-no-synthetic-control` modes:

- Official compiler generic-for `break` artifacts at O0/O1/O2 (6/6 runs).
- Terminal `return`, conditional terminal arms, empty-body and nested-break
  fixtures.
- `pairs`, `ipairs`, explicit `next`, `continue`, and debug-level fixtures.
- Exact Pet repro corpus: 3/3 files; deterministic at multiple thread counts;
  no `goto`, label, or `controlFlowState` markers.
- Full workspace tests and release build pass.

These fixes cover specific generic-for regressions; they do **not** solve all
310 corpus failures.

## Current architecture and failure path

Relevant components:

- `luau-lifter/src/lib.rs`
  - Runs SSA/lifting and chooses source-like, legacy, or certified fallback.
  - Enforces the final invariant that no `Goto`, `Label`, or unlowered VM
    marker may reach formatting. Residual control is converted to the public
    failure above.
- `restructure/src/region.rs`
  - Conservative source-like structurer.
  - Requires reducible ownership, valid joins/post-dominators, safe loop
    provenance, supported edge transfers, and safe closure/liveness rewrites.
  - Returns `Unsupported` or a typed `UnsafeStructureReason` when proof is
    unavailable.
- `restructure/src/lib.rs` / legacy matcher
  - Can insert internal labels/gotos while reducing difficult CFGs.
  - Those are only acceptable if later cleanup removes them.
- `restructure/src/fallback.rs`
  - Certified CFG state-machine fallback for some irreducible graphs.
  - Intentionally rejects generic-for VM markers, embedded structured blocks,
    pre-existing break/continue markers, and some reference-capture/lifetime
    shapes because their semantics are not yet proven.
- `ast/src/simplify_gotos.rs`
  - Attempts to remove internal gotos/labels; it cannot always structure an
    arbitrary or irreducible CFG into legal Luau.

The baseline batch API collapsed different internal causes into the same final
message. The current batch API preserves the legacy message for compatibility,
but adds typed per-function evidence and a reproducible audit manifest (corpus
hash, command, key, thread count, policy, tool hash, result hash, and explicit
`parser_status`). The batch binary records `parser_status: "not_run"` because
the official Luau compiler is an external audit tool; `parser_failures: []` is
not presented as a parser pass.

## Concrete problem statement for the planning model

Produce an implementation plan to make the decompiler handle the entire class
of current residual-control failures while preserving semantics and valid
Luau. The plan should first add diagnostics/classification so every failed file
is assigned a precise cause, then propose proof-backed handling for each class.
It must explicitly address:

1. Reducible CFGs that the current region builder rejects unnecessarily.
2. Irreducible or multi-entry CFGs where legacy matching leaves gotos/labels.
3. Generic-for VM protocol shapes in fallback (including generic tables,
   `pairs`, `ipairs`, `next`, `__iter`, `__call`, AUX/result arity, exhaustion,
   `break`, `continue`, and nested loops).
4. SSA edge transfers and parallel copies across loop/branch boundaries.
5. Nested closures, reference captures, upvalue-cell lifetime, and loop-result
   exports.
6. Terminal `return`/`break` paths and shared adapters.
7. A typed failure taxonomy and per-file diagnostics rather than one generic
   residual-goto message.

The plan must include architecture changes, proof obligations, migration/risk
boundaries, concrete regression fixtures, corpus gates, and a staged validation
strategy. It must not recommend emitting invalid Luau or silently guessing when
semantic equivalence cannot be proven.

## Acceptance target

For the current corpus, the target is zero explicit decompile failures caused
by residual control-flow, zero emitted `goto`/label/internal markers, and all
emitted outputs accepted by the official Luau parser. Any remaining unsupported
bytecode must be reported with a precise, actionable reason rather than being
misclassified as a generic residual-goto failure.
