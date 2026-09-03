# Residual-control failure fixtures

These are real UniversalSynSaveInstance bytecode payloads copied from the
`examplebytecode/RobloxProject` corpus. They are not hand-written source
examples. Each reproduced the public failure before the current patch:

```text
control-flow structuring failed: residual goto/label would be invalid Luau
```

Run the fixture folder with a release binary:

```powershell
target/release/luau-lifter.exe decompile-folder `
  docs/failure_fixtures/residual_control_flow `
  target/residual_control_flow_fixture_out `
  --key 203 --threads 1 --verbose
```

Expected result is 7 source outputs and 0 rejections in both default and
`--strict-no-synthetic-control` modes. The two inputs that were previously
reported as `source_like_unsafe_ForInitSuffixOrder` are structured now: the
"suffix" after the preparation marker was never bytecode that executed after
`FORGPREP`/`FORNPREP`; it was the lowered SSA init-edge transfer (phi copies
and inliner-folded constant initializers such as `local seen = {}`), which the
destructor now materializes *before* the marker, restoring the original
bytecode order (see `docs/Tovek_PR3_ForInitSuffix_RootCause_and_Fix.md`). No
output contains `goto`, label, `controlFlowState`, or another internal control
marker, and every emitted output passes the official Luau parser.

The sibling directory `../semantic_roundtrip/` holds source fixtures for the
compile -> decompile -> recompile -> execute comparison run by
`scripts/semantic_roundtrip.py` (also part of CI). The fixtures preserve their original `ReplicatedStorage`,
`StarterPlayer`, and `Workspace` path families so a planning/diagnostic tool
can reproduce the same shapes without access to the local corpus.

| Fixture | Original corpus path | Encoded size |
|---|---|---:|
| [teleportServer.lua](ReplicatedStorage/Cmdr/Server%20commands/Admin/teleportServer.lua) | `ReplicatedStorage/Cmdr/Server commands/Admin/teleportServer.lua` | 828 B |
| [Container.lua](ReplicatedStorage/FusionPackage/Components/Base/Container.lua) | `ReplicatedStorage/FusionPackage/Components/Base/Container.lua` | 1,956 B |
| [Abilities.lua](ReplicatedStorage/Shared/Information/Abilities.lua) | `ReplicatedStorage/Shared/Information/Abilities.lua` | 2,648 B |
| [init.lua](ReplicatedStorage/Shared/TimeManager/MeshEmitter/init.lua) | `ReplicatedStorage/Shared/TimeManager/MeshEmitter/init.lua` | 25,292 B |
| [Emit.lua](ReplicatedStorage/Part_Icles/Emit.lua) | `ReplicatedStorage/Part_Icles/Emit.lua` | 32,320 B |
| [Animate.client.lua](StarterPlayer/StarterCharacterScripts/Animate.client.lua) | `StarterPlayer/StarterCharacterScripts/Animate.client.lua` | 16,752 B |
| [ClickToMoveController.lua](StarterPlayer/StarterPlayerScripts/PlayerModule/ControlModule/ClickToMoveController.lua) | `StarterPlayer/StarterPlayerScripts/PlayerModule/ControlModule/ClickToMoveController.lua` | 45,644 B |

The payloads are encoded bytecode, so their usefulness is as reproducible
inputs for instrumentation and CFG/structuring analysis rather than as human-
readable source listings.
