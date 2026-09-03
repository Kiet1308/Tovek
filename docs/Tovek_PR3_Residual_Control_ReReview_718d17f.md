# PR #3 residual-control re-review: current file matrix

The original PR baseline had 310 files ending with
`control-flow structuring failed: residual goto/label would be invalid Luau`.
That residual-control class is now cleared for the local corpus: the latest
strict folder run decompiled all 3,936 non-empty inputs and failed none. The
42 remaining entries are empty payloads and are intentionally skipped.

The bytecode is local and is not checked into GitHub. These stable paths give a
reviewer who has the RobloxProject export the exact examples to inspect.

## Current matrix

| corpus | entries | decompiled | skipped | failed | parser/compile failures |
| --- | ---: | ---: | ---: | ---: | ---: |
| `D:\Medal\selectedCorpus_pr3_20260831` | 13 | 13 | 0 | 0 | 0 |
| `D:\Medal\examplebytecode\RobloxProject` | 3,978 | 3,936 | 42 | 0 | 0 |

The folder command defaults to `StrictNoSyntheticControl`. Therefore a
successful output is not a certified program-counter state machine: it is
source-like Luau with structured loops/branches. If a future shape cannot be
proved, the command must fail closed with a typed diagnostic instead of hiding
the problem behind a dispatcher.

## Examples fixed in this PR

- `ReplicatedStorage/Shared/CutsceneUtil.lua` (`p6`): a generic-for jump to an
  outer continuation is lowered as a readable `while true` with explicit
  exhaustion handling.
- `ReplicatedStorage/Shared/ForgeVFX/mod/lerp.lua` (`p11`) and
  `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/lerp.lua` (`p11`): nested
  interpolation exits and loop-result resets no longer leave labels.
- `ReplicatedStorage/Shared/Information/GameModifiers.lua` (`p2`): the
  normal-exhaustion result adapter is guarded; a body `break` cannot overwrite
  the break-path value.
- `ReplicatedStorage/FusionPackage/Fusion/State/For/Disassembly.lua` (`p3`):
  the generic-for exit and terminal path compile as ordinary Luau.
- `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Lerps.lua` (`p27`, `p29`),
  `ReplicatedStorage/Part_Icles/Engine.lua` (`p18`), and
  `ReplicatedStorage/Shared/TimeManager/Part_Icles/Engine.lua` (`p18`):
  iterator provenance and prep-kind handling keep `pairs`/`ipairs`-style loops
  readable.
- `ReplicatedStorage/Shared/Network/BufferEncoder/Write.lua` (`p3`): the large
  sibling-branch CFG is emitted without synthetic control markers.
- `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Gamemodes/Expedition.lua`
  (`p64`), `.../Effects/LensFlare/LensFlare.lua` (`p6`), and
  `StarterPlayer/StarterPlayerScripts/Mounts/ShenronDragon/init.lua` (`p19`):
  nested loop/closure paths now pass the same source-like output gate.

The six parser-regression outputs under
`Workspace/Lobby/Summerprops/Piña colada/water/rotating__volt-script-000011`
through `000016.server.luau` also compile. Their receiver begins with a
parenthesized expression; the formatter emits a leading semicolon where Luau
requires statement disambiguation.

## Self-contained reproduction

```powershell
cargo +nightly-2024-12-15 build -p luau-lifter --bin luau-lifter --release `
  --target-dir target/build_release_quality
target/build_release_quality/release/luau-lifter.exe decompile-folder `
  D:\Medal\selectedCorpus_pr3_20260831 target\pr3-recheck `
  --key 203 --threads 1 --verbose
```

Expected result: `Done: 13 decompiled, 0 skipped (no bytecode), 0 failed.`
Compile each output with the pinned official tool:

```powershell
$compiler = 'D:\Medal\luau-tools-src\build\luau-compile.exe'
& $compiler --binary -O0 path\to\file.luau
& $compiler --only-parse path\to\file.luau
```

Committed bytecode fixtures remain in
[`failure_fixtures/residual_control_flow/`](failure_fixtures/residual_control_flow/)
with their own README and expected results.

## Proof boundaries

- Generic-for operand normalization removes only compiler-generated trailing
  nil protocol operands. Explicit single-result `CALL`/`CALLFB` followed by
  `nil, nil` is preserved; `CALLFB` feedback `NOP`s and short instruction
  prefixes are handled with checked arithmetic.
- Re-entry and exhaustion rewrites require CFG dominance/post-dominance,
  private-sentinel, and terminal-transfer proofs. Ordinary source-level
  assignments are not rewritten by the legacy-only adapter heuristic.
- Generated local coalescing is restricted to proven-disjoint live intervals
  and never coalesces protected closure/upvalue locals.
- The final AST invariant rejects every `Goto`, label, and unlowered VM marker
  before formatting.

No current corpus entry has a residual-control failure. The previous 13 paths
are retained above as regression examples so a reviewer can verify that the
fixes apply to the replicated-storage and starter-player code that was absent
from the GitHub checkout.
