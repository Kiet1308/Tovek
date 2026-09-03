# Tovek vs. lua.expert on the PR3 residual-control corpus

This is a live comparison of the 13 Roblox project paths that previously
exercised PR3's residual-control-flow failures. It is intentionally a
comparison of generated output, not a claim that either decompiler can be
proven semantically correct without the original source and a runtime oracle.

## Reproduction

The comparison was run on 2026-09-01 against the local focused corpus
`D:/Medal/selectedCorpus_pr3_20260831` and Tovek's release output in
`target/corpus_final_quality4` / `selectedOut_final_release`.

The lua.expert request followed its [API documentation](https://lua.expert/docs):

```http
POST https://api.lua.expert/decompile
Content-Type: application/json
```

```json
{"script":"<base64-encoded luauc contents>"}
```

Each request returned plain-text Luau with HTTP 200. The response was compared
with Tovek's output for the same stable Roblox path. No lua.expert output is
committed to the repository.

## Objective checks

Both tools produced source-like Luau for all 13 files. A scan for the exact
synthetic/control-flow artifacts that motivated PR3 found zero in either
output: `goto`, labels, `__pc`, `__state`, `controlFlowState`,
`GenericForInit`, `GenericForNext`, `NumForInit`, and `NumForNext`.

The size figures are useful context, but are not correctness scores. A shorter
file can be the result of more aggressive inlining or less readable naming.

| path (relative Roblox project path) | lua.expert (chars / lines) | Tovek (chars / lines) | shorter |
| --- | ---: | ---: | --- |
| `ReplicatedStorage/FusionPackage/Components/Processors/GameUpgrade.lua` | 2,320 / 74 | 1,963 / 68 | Tovek |
| `ReplicatedStorage/FusionPackage/Fusion/State/For/Disassembly.lua` | 4,245 / 226 | 3,898 / 214 | Tovek |
| `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Lerps.lua` | 14,681 / 572 | 12,203 / 455 | Tovek |
| `ReplicatedStorage/Part_Icles/Engine.lua` | 15,637 / 546 | 13,615 / 539 | Tovek |
| `ReplicatedStorage/Shared/CutsceneUtil.lua` | 6,892 / 304 | 6,417 / 307 | Tovek (chars) |
| `ReplicatedStorage/Shared/ForgeVFX/mod/lerp.lua` | 4,619 / 199 | 4,797 / 206 | lua.expert |
| `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/lerp.lua` | 4,654 / 199 | 4,853 / 207 | lua.expert |
| `ReplicatedStorage/Shared/Information/GameModifiers.lua` | 5,203 / 227 | 5,257 / 240 | lua.expert (chars) |
| `ReplicatedStorage/Shared/Network/BufferEncoder/Write.lua` | 72,206 / 3,264 | 109,220 / 4,775 | lua.expert |
| `ReplicatedStorage/Shared/TimeManager/Part_Icles/Engine.lua` | 15,637 / 546 | 13,615 / 539 | Tovek |
| `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Effects/LensFlare/LensFlare.lua` | 9,980 / 362 | 10,840 / 389 | lua.expert |
| `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Gamemodes/Expedition.lua` | 32,693 / 1,197 | 31,323 / 1,206 | Tovek (chars) |
| `StarterPlayer/StarterPlayerScripts/Mounts/ShenronDragon/init.lua` | 25,110 / 825 | 24,657 / 844 | Tovek (chars) |

The high-level loop counts agree on 12 of 13 files. `BufferEncoder/Write.lua`
is the exception: lua.expert emits 8 visible `for` loops while Tovek emits 10.
That difference is a review signal, not proof that either result is wrong; the
optimized bytecode contains duplicated branches and needs source/runtime
validation before assigning a semantic winner.

## Readability examples

### `CutsceneUtil.lua`

lua.expert keeps useful line/upvalue comments, but uses generic names and an
extra conditional expression:

```luau
local t = {}
--[[ AwaitCutsceneAnimationTracks | Line: 52 ]]
while true do
    local v7 = true
    for v8, v9 in t do
        if not (if v9.Length > 0 then true else false) then
            v7 = false
            break
        end
    end
    if v7 then return t end
end
```

Tovek removes tool metadata and preserves recovered role-oriented names and a
direct condition:

```luau
local tracksByChild = {}
local lastTime = os.clock()
while true do
    local flag = true
    for _, track in tracksByChild do
        if track.Length > 0 then continue end
        flag = false
        break
    end
    if flag then return tracksByChild end
end
```

Both snippets have the same structured loop shape. On this example Tovek is
more source-like; lua.expert's comments are helpful when mapping an instruction
back to a bytecode/source line.

### `ForgeVFX/mod/lerp.lua`

lua.expert is slightly shorter, but still exposes generic temporaries and
splits the equality path into assignments:

```luau
for v3, v4 in p1.Keypoints do
    local v5 = nil
    local v6 = nil
    for v7, v8 in p2.Keypoints do
        if v8.Time == v4.Time then
            v5 = v8
            v6 = v8
            break
        end
        if v8.Time < v4.Time and (...) then
            v6 = v8
            continue
        end
    end
end
```

Tovek keeps the keypoint roles and expresses the same paths with a clearer
`elseif`/exhaustion structure. This is easier to audit even when it costs a few
lines.

### `GameModifiers.lua`

lua.expert is compact and uses `t`, `v2`, `v3`, etc. Tovek retains the recovered
module name (`GameModifiers`) and explicit normal-exhaustion handling. The
extra `flag`/fallback locals are deliberate: PR3's proof says when the legacy
iterator adapter may rewrite an exhaustion path, so the emitted source does not
guess on an unproven path.

## Assessment

For this corpus, the practical verdict is:

* **Prettier / more source-like:** Tovek in most of the formerly failing
  control-flow cases. It avoids watermark and instruction-line comments,
  recovers more meaningful names where the AST permits it, and emits direct
  `for`/`while`/`if`/`continue`/`break` structure. lua.expert is often more
  compact and its line/upvalue comments are valuable for reverse engineering;
  it wins raw compactness on `BufferEncoder/Write.lua` and several small lerp /
  utility files.
* **More defensibly correct:** Tovek for the PR3 failure mode. Its source-like
  path is gated by CFG dominance/post-dominance, private-sentinel, and terminal
  transfer proofs, and the focused output passed the official Luau parser with
  no residual markers. lua.expert's shorter output is not, by itself, evidence
  of semantic correctness, and this comparison has no original-source oracle.
* **Not established by this test:** universal semantic superiority. The
  `BufferEncoder/Write.lua` loop-count difference and any aggressive inlining
  need a targeted behavioral fixture before either tool can be called
  definitively correct for that function.

In short: use lua.expert as a useful independent cross-check and for bytecode
line annotations; for the specific “residual goto/label would be invalid Luau”
class fixed by PR3, Tovek currently gives the cleaner and more auditable result.
