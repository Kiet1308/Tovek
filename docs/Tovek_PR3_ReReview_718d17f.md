# PR #3 re-review — current residual-control status

This note is the GitHub-visible status for PR #3. The bytecode corpus is
intentionally local (it is not checked into GitHub); the stable Roblox project
paths below are the selectors a reviewer can use when they have the same
corpus.

## Snapshot

- Repository: `Kiet1308/Tovek`
- PR: [#3](https://github.com/Kiet1308/Tovek/pull/3)
- Branch: `fix/pet-source-like-loop-structuring`
- Local checkout: `D:\Medal\medal-decompiler`
- Full corpus: `D:\Medal\examplebytecode\RobloxProject` (3,978 entries)
- Focused residual-loop corpus: `D:\Medal\selectedCorpus_pr3_20260831` (13 entries)

## Current result

The latest release run uses the folder driver's default strict policy, so a
successful file cannot fall back to a synthetic program-counter dispatcher.
All successful outputs are ordinary source-like Luau (`for`, `while`, `if`,
`break`, `continue`, and `return`); the output gate rejects residual gotos,
labels, and VM loop markers.

| corpus / gate | result |
| --- | ---: |
| focused PR3 files decompiled | 13 / 13 |
| focused PR3 files skipped (empty payload) | 0 |
| focused PR3 files failed | 0 |
| focused PR3 official Luau compile failures | 0 |
| full corpus decompiled | 3,936 / 3,978 |
| full corpus skipped (empty payload) | 42 |
| full corpus failed | 0 |
| full corpus official Luau compile failures | 0 |

The full-corpus compile check was run in batches with the pinned
`luau-compile.exe --binary -O0`; the six files under the non-ASCII `Piña
colada` directory were also compiled from their containing directory because
the Windows command-line encoding cannot open their absolute path reliably.
The official parser-only check (`--only-parse`) passed for all 3,936 emitted
files, including those six paths.

## Reproduction commands

Build the release binary:

```powershell
cargo +nightly-2024-12-15 build -p luau-lifter --bin luau-lifter --release `
  --target-dir target/build_release_quality
```

Focused corpus (the command is deterministic at one worker):

```powershell
target/build_release_quality/release/luau-lifter.exe decompile-folder `
  D:\Medal\selectedCorpus_pr3_20260831 target\pr3-recheck `
  --key 203 --threads 1 --verbose
```

Full corpus:

```powershell
target/build_release_quality/release/luau-lifter.exe decompile-folder `
  D:\Medal\examplebytecode\RobloxProject target\corpus-recheck `
  --key 203 --threads 8 --emit-upvalue-analysis --verbose
```

Compile/parse each emitted `.luau` with the official tool:

```powershell
$compiler = 'D:\Medal\luau-tools-src\build\luau-compile.exe'
& $compiler --binary -O0 path\to\file.luau
& $compiler --only-parse path\to\file.luau
```

For the self-contained committed fixtures, see
[`failure_fixtures/residual_control_flow/README.md`](failure_fixtures/residual_control_flow/README.md).

## Examples now handled

These were representative residual-control paths in the original 310-file
baseline and are now emitted as readable source-like code rather than a
dispatcher or invalid goto/label AST:

- `ReplicatedStorage/Shared/CutsceneUtil.lua` — nested generic-for re-entry
  is represented as a `while true` loop with a guarded exhaustion path.
- `ReplicatedStorage/Shared/ForgeVFX/mod/lerp.lua` and
  `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/lerp.lua` — interpolation
  loop exits and result resets are structured without residual labels.
- `ReplicatedStorage/Shared/Information/GameModifiers.lua` — post-loop
  result handling is guarded by proven normal-exhaustion flow.
- `ReplicatedStorage/FusionPackage/Fusion/State/For/Disassembly.lua` — the
  generic-for exit and terminal path are source-like and compile cleanly.
- `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Lerps.lua` and
  `ReplicatedStorage/Part_Icles/Engine.lua` — iterator protocol metadata keeps
  `pairs`/`ipairs`-style loops readable.
- `ReplicatedStorage/Shared/Network/BufferEncoder/Write.lua` — the large
  nested branch/loop graph no longer leaves control-flow markers.

The six `Workspace/Lobby/Summerprops/Piña colada/water/rotating__volt-script-000011`
through `000016.server.luau` outputs also compile; they are useful parser
regressions because the formatter now disambiguates a call whose receiver
starts with a parenthesized expression.

## Safety and quality gates

- Source-like structuring is attempted first and is fail-closed when its CFG
  proof is unavailable.
- The legacy generic-for matcher is restricted to hidden VM protocol registers
  and is never used for source-like output.
- Generic-for exhaustion adapters are guarded only on legacy output with a
  compiler provenance marker; ordinary source-level AST copies are left alone.
- Explicit single-result calls followed by `nil, nil` retain those operands;
  the detector handles Luau's `CALLFB` feedback `NOP` and short prefixes
  without integer underflow.
- Generated temporary locals are coalesced only when interval and branch
  disjointness proofs hold, preserving captured/upvalue locals.
- The formatter prefixes ambiguous parenthesized method calls with `;`, so
  every emitted file is accepted by the official Luau parser.

No residual `goto`, label, `GenericForInit`, `GenericForNext`, `NumForInit`, or
`NumForNext` marker was found in the focused outputs. The full-corpus search
was additionally checked for the synthetic names (`__pc`, `__state`, and
`controlFlowState`) and found none. Future unsupported bytecode must continue
to fail closed with a typed diagnostic rather than emit invalid or guessed
Luau.
