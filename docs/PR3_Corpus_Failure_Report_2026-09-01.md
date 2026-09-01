# PR #3 — Full corpus failure report after re-review

## 1. Mục đích

Báo cáo này liệt kê đầy đủ các input bị từ chối trong lần recheck cuối của PR #3, nguyên nhân chính xác lấy từ manifest, function liên quan và lý do việc từ chối là chủ động (fail-closed). Báo cáo được viết để reviewer có thể kiểm tra lại từng file mà không phải suy luận từ tổng số counter.

## 2. Nguồn dữ liệu và lệnh tái hiện

Nguồn chuẩn (authoritative source):

- Manifest: `target/pr3_corpus_recheck_final_0901/.tovek-analysis/manifest.json`
- Full run log: `target/pr3_corpus_recheck_final_0901.log`
- Output root: `target/pr3_corpus_recheck_final_0901/`
- Corpus input: `D:/Medal/examplebytecode/RobloxProject`
- Lifter commit: `6ef3cf4` (`Harden PR3 loop structuring after re-review`)
- Policy: `StrictNoSyntheticControl` (`--strict-no-synthetic-control`)
- Tool version: `0.9.0-beta`
- Generation id: `sha256:6d79879d75664906d58954671fc929a7f15c7fe513e9239b47584e8940e52a8a`
- Corpus SHA-256: `sha256:82f502bcaabc898e3fa697f80da9d289ab3f1dd871d2e800ed5e542b909eb3fd`

Lệnh đã chạy:

```text
target/release/luau-lifter.exe decompile-folder \
  D:/Medal/examplebytecode/RobloxProject \
  target/pr3_corpus_recheck_final_0901 \
  --key 203 --threads 8 --emit-upvalue-analysis --verbose \
  --strict-no-synthetic-control
```

### Quyền truy cập của reviewer

Các path bắt đầu bằng `ReplicatedStorage/`, `StarterPlayer/` và `Workspace/` là path **tương đối bên trong corpus private** `D:/Medal/examplebytecode/RobloxProject`; chúng không phải thư mục nằm trong GitHub repository. Reviewer không cần truy cập local corpus để đọc báo cáo này: inventory bên dưới đã embed toàn bộ 161 failed paths, function IDs và evidence code/message. Manifest và log local chỉ được trích dẫn như provenance/reproducibility reference.

Các file failed không có source output để reviewer mở (`failed_output_files=0`), vì strict builder cố ý không ghi output khi proof không đạt. Do đó artifact cần review là chính typed diagnostic, không phải một `.luau` hỏng. Nếu cần xác minh CFG/bytecode nội bộ của một file cụ thể, maintainer phải cung cấp corpus/bytecode tương ứng hoặc một sanitized reproducer; không thể suy ra CFG chỉ từ path name.

## 3. Tổng quan kết quả

| Trạng thái | Số lượng | Ý nghĩa |
|---|---:|---|
| Input scripts | 3,978 | Tổng số entry trong corpus |
| Processed | 3,817 | Có payload được xử lý |
| Analyzed / emitted | 3,775 | Có output `.luau` và phân tích hoàn tất |
| Skipped | 42 | Payload rỗng, không phải decompilation failure |
| Failed | 161 | Bị source-like safety gate từ chối; không phát hành output giả mạo |
| Partial / unavailable | 0 / 0 | Không có phân tích dở dang |

161 failed scripts tương ứng với 232 function diagnostics. Một file có thể chứa nhiều function bị từ chối, vì vậy số diagnostic lớn hơn số file. Không file failed nào có `.luau` output trong output root (`failed_output_files=0`).

Điểm quan trọng: đây không phải lỗi parser Luau hay lỗi compile của output. Đây là các trường hợp mà builder không thể chứng minh transform source-like an toàn; policy strict chủ động dừng thay vì phát hành code có thể đổi thứ tự side effect hoặc sai lifetime.

## 4. Phân loại nguyên nhân

| Diagnostic code | Files | Function diagnostics | Nguyên nhân chính | Quyết định an toàn |
|---|---:|---:|---|---|
| `source_like_unsafe_ForInitSuffixOrder` | 159 | 229 | Có executable suffix sau `FORGPREP`; chuyển suffix vào thân `for` có thể đổi thứ tự quan sát được của iterator/limit/step hoặc side effect | Reject typed với `ForInitSuffixOrder`; không reorder mù |
| `source_like_unsafe_ForOriginPrepKindUnsupported` | 1 | 2 | `FORGPREP` origin/prep kind không thuộc tập opcode mà proof source-like hiện hỗ trợ | Reject typed với `ForOriginPrepKindUnsupported` |
| `source_like_unsafe_CapturedLoopResultRef` | 1 | 1 | Generic-for result bị ref-capture; chưa có bằng chứng close/dominance/lifetime đầy đủ | Reject typed với `CapturedLoopResultRef` |

Các code trên đều được ghi trong `evidence[].code` của manifest; function name là tên function decompiler (`p0`, `p1`, …) tại nơi proof thất bại.

Raw evidence message (giống nội dung `evidence[].message` trong manifest):

```text
source_like_unsafe_ForInitSuffixOrder:
  stage=final_invariant
  message=source-like proof rejected: observable FORGPREP suffix reorder

source_like_unsafe_ForOriginPrepKindUnsupported:
  stage=final_invariant
  message=source-like proof rejected: generic-for fast-path prep kind is not source-proven

source_like_unsafe_CapturedLoopResultRef:
  stage=final_invariant
  message=source-like proof rejected: loop result is captured by reference without a proven iteration cell
```

## 4.1. Đối chiếu với các finding của reviewer

- **F1 — branch-local liveness / closure:** unsafe splitter đã được tắt; closure detection đã mở rộng sang indexed LHS. Các zero-trip/nested-loop regression tests pass, nên không còn failure tương ứng trong corpus.
- **F2 — AST-only adapter và stale nil facts:** AST-only guard hiện là no-op; while-carried alias rewrite giữ nguyên assignment hậu loop nếu không có CFG provenance. Không còn failure tương ứng trong corpus.
- **F3 — numeric/generic FOR init suffix:** executable suffix không còn bị di chuyển mù; các trường hợp không chứng minh được thứ tự được liệt kê dưới `ForInitSuffixOrder`.
- **F4 — stable local `FORGPREP_INEXT` alias:** shortcut đã bị loại bỏ; chỉ direct `ipairs` hoặc latest same-block builtin alias được chấp nhận. Không còn shortcut-induced false positive.
- **F5 — captured generic-for result:** không còn chấp nhận mutation-only proof; trường hợp thiếu close/dominance proof được liệt kê dưới `CapturedLoopResultRef`.
- **F6 — generic-loop multi-entry:** external-predecessor check đã đổi sang kiểm tra toàn bộ candidate set. Không còn failure tương ứng trong corpus.
- **F7 — independent validation:** CI đã build official Luau compiler, chạy fixtures ở cả hai mode và parse/binary-compile toàn bộ fixture outputs.

## 5. Danh sách đầy đủ các file failed

### 5.1 `source_like_unsafe_CapturedLoopResultRef`

- `ReplicatedStorage/FusionPackage/Components/Processors/GameUpgrade.lua` — function `p2`

**Lý do:** function này capture result/ref của generic-for. Mutation đơn thuần không đủ để chứng minh lifetime; vì chưa có close provenance và dominance proof chắc chắn nên source-like builder từ chối.

### 5.2 `source_like_unsafe_ForOriginPrepKindUnsupported`

- `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Lerps.lua` — functions `p29`, `p27`

**Lý do:** prep/origin của generic loop không khớp opcode kind mà source-like proof hỗ trợ. Không suy đoán loại loop hoặc tự chèn synthetic control flow.

### 5.3 `source_like_unsafe_ForInitSuffixOrder`

**Lý do chung cho toàn bộ danh sách dưới đây:** function có executable instruction nằm trong suffix của `FORGPREP`. Nếu emit `for ... in ... do` bằng cách di chuyển suffix vào loop body, thứ tự thực thi observable có thể thay đổi. Với policy `StrictNoSyntheticControl`, transform bị reject typed.

- `ReplicatedStorage/CmdrClient/Shared/Argument.lua` — function `p9`
- `ReplicatedStorage/CmdrClient/Shared/Command.lua` — function `p4`
- `ReplicatedStorage/CmdrClient/Shared/Util.lua` — functions `p44`, `p24`, `p16`, `p5`, `p4`, `p2`, `p0`
- `ReplicatedStorage/DivergentVFX/effects/bezier.lua` — function `p7`
- `ReplicatedStorage/DivergentVFX/effects/lightning.lua` — function `p7`
- `ReplicatedStorage/DivergentVFX/effects/mesh.lua` — function `p6`
- `ReplicatedStorage/DivergentVFX/LightningCore.lua` — functions `p111`, `p96`, `p94`, `p79`, `p74`, `p54`, `p50`, `p34`
- `ReplicatedStorage/DivergentVFX/path3d.lua` — function `p4`
- `ReplicatedStorage/DivergentVFX/pool.lua` — function `p7`
- `ReplicatedStorage/FusionPackage/Components/Base/SplitTextLabel/init.lua` — function `p18`
- `ReplicatedStorage/FusionPackage/Components/Base/StyledTextLabel/Bouncing.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Components/Base/VirtualMapScroller/init.lua` — functions `p10`, `p2`
- `ReplicatedStorage/FusionPackage/Components/Base/VirtualMapScroller/ItemPosition.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Components/Base/VirtualScroller/init.lua` — function `p7`
- `ReplicatedStorage/FusionPackage/Components/Effects/Burst.lua` — function `p15`
- `ReplicatedStorage/FusionPackage/Components/Effects/DirectionalBurst.lua` — function `p14`
- `ReplicatedStorage/FusionPackage/Components/Effects/Glitch.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Components/Effects/Sparkles.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Components/Game/UpgradeBar/init.lua` — functions `p2`, `p0`
- `ReplicatedStorage/FusionPackage/Components/Gamemodes/Expedition/NodeMapMenu/init.lua` — function `p5`
- `ReplicatedStorage/FusionPackage/Components/Gamemodes/Expedition/ProgressBar.lua` — function `p2`
- `ReplicatedStorage/FusionPackage/Components/Menu/EventCalendar/init.lua` — function `p6`
- `ReplicatedStorage/FusionPackage/Components/Menu/EventView/Backgrounds/SummerFishingEvent.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Components/Menu/Expedition/Entry.lua` — function `p8`
- `ReplicatedStorage/FusionPackage/Components/Menu/StatTransfer/init.lua` — function `p2`
- `ReplicatedStorage/FusionPackage/Components/Processors/Asset/UnitStats.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Components/Processors/Asset/UnitTotalCost.lua` — functions `p2`, `p0`
- `ReplicatedStorage/FusionPackage/Components/Processors/Battlepass.lua` — function `p1`
- `ReplicatedStorage/FusionPackage/Components/Prompts/ColorPickerPrompt/HuePicker.lua` — function `p3`
- `ReplicatedStorage/FusionPackage/Components/Prompts/FilterSelection/Portal.lua` — function `p4`
- `ReplicatedStorage/FusionPackage/Components/Prompts/ObtainedRewards.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Dependencies/Mock/init.lua` — functions `p50`, `p9`
- `ReplicatedStorage/FusionPackage/Dependencies/Mock/MockPlayerData.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Dependencies/Mock/MockSheetSync.lua` — function `p3`
- `ReplicatedStorage/FusionPackage/Dependencies/Mock/MockUnitLevelInfo.lua` — function `p7`
- `ReplicatedStorage/FusionPackage/Fusion/Animation/Spring.lua` — function `p5`
- `ReplicatedStorage/FusionPackage/Fusion/State/ComputedAsync/Promise/init.spec.lua` — function `p146`
- `ReplicatedStorage/FusionPackage/Stories/EventView.story.lua` — function `p3`
- `ReplicatedStorage/FusionPackage/Stories/Guild.story.lua` — function `p1`
- `ReplicatedStorage/FusionPackage/Stories/LeaderboardDisplay.story.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Stories/LobbyHUD.story.lua` — function `p3`
- `ReplicatedStorage/FusionPackage/Stories/Tournament.story.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Stories/UnitManager.story.lua` — function `p7`
- `ReplicatedStorage/FusionPackage/Utils/formatTime.lua` — function `p0`
- `ReplicatedStorage/FusionPackage/Utils/Promise/init.spec.lua` — function `p146`
- `ReplicatedStorage/MoonPlayer/init.lua` — functions `p54`, `p11`
- `ReplicatedStorage/MoonPlayer/LerpCore/BoatTween/Bezier.lua` — function `p2`
- `ReplicatedStorage/Nodes/ArgumentGuard.lua` — function `p3`
- `ReplicatedStorage/Nodes/init.lua` — functions `p30`, `p27`
- `ReplicatedStorage/Nodes/PacketSerializer.lua` — function `p3`
- `ReplicatedStorage/Nodes/Schema/init.lua` — function `p52`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/concat.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/concatDeep.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/freezeDeep.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/insert.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/pop.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/reverse.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Array/shift.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Dictionary/fromArrays.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Dictionary/merge.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Dictionary/mergeDeep.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Set/intersection.lua` — function `p0`
- `ReplicatedStorage/Nodes/Schema/Sift/Set/merge.lua` — function `p0`
- `ReplicatedStorage/Part_Icles/Emit.lua` — function `p4`
- `ReplicatedStorage/Part_Icles/EmitAnimate.lua` — function `p4`
- `ReplicatedStorage/Part_Icles/EngineReplay.lua` — function `p4`
- `ReplicatedStorage/Part_Icles/Graph.lua` — functions `p6`, `p3`
- `ReplicatedStorage/Part_Icles/ImageEmit.lua` — function `p8`
- `ReplicatedStorage/Part_Icles/Lightning/BoltGen.lua` — functions `p7`, `p6`, `p5`, `p4`
- `ReplicatedStorage/Part_Icles/Lightning/Endpoints.lua` — function `p3`
- `ReplicatedStorage/Part_Icles/Lightning/init.lua` — function `p1`
- `ReplicatedStorage/Part_Icles/PlayHandle.lua` — functions `p5`, `p4`
- `ReplicatedStorage/Part_Icles/PreSimulate.lua` — function `p1`
- `ReplicatedStorage/Part_Icles/Rocks/init.lua` — function `p10`
- `ReplicatedStorage/Part_Icles/TrailEmitter.lua` — function `p2`
- `ReplicatedStorage/Part_Icles/Update.lua` — functions `p7`, `p6`
- `ReplicatedStorage/Part_Icles/UpdateModel.lua` — function `p2`
- `ReplicatedStorage/Shared/EnemyUtils.lua` — functions `p19`, `p18`, `p13`, `p12`, `p11`, `p6`
- `ReplicatedStorage/Shared/ForgeVFX/init.lua` — function `p25`
- `ReplicatedStorage/Shared/ForgeVFX/mod/lerp.lua` — function `p11`
- `ReplicatedStorage/Shared/ForgeVFX/mod/utility.lua` — functions `p35`, `p33`, `p27`
- `ReplicatedStorage/Shared/ForgeVFX/obj/Bezier.lua` — function `p11`
- `ReplicatedStorage/Shared/ForgeVFX/obj/ObjectCache.lua` — function `p2`
- `ReplicatedStorage/Shared/ForgeVFX/shockwave_ring.lua` — function `p9`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/effects/bezier.lua` — function `p21`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/effects/lightning.lua` — functions `p40`, `p39`, `p3`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/effects/shockwave_ring.lua` — function `p9`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/effects/sound.lua` — function `p11`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/emitters.lua` — function `p8`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/common/flipbook.lua` — function `p1`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/lerp.lua` — function `p11`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/mod/utility.lua` — functions `p47`, `p39`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/obj/Bezier.lua` — function `p13`
- `ReplicatedStorage/Shared/ForgeVFXForCutscenes/obj/ObjectCache.lua` — function `p2`
- `ReplicatedStorage/Shared/Information/Ascensions.lua` — functions `p3`, `p2`
- `ReplicatedStorage/Shared/Information/AutoPlayUtils.lua` — functions `p18`, `p12`, `p4`
- `ReplicatedStorage/Shared/Information/BannerInfo/BannerStyling.lua` — function `p1`
- `ReplicatedStorage/Shared/Information/EnemyModifiers.lua` — function `p1`
- `ReplicatedStorage/Shared/Information/Equipment.lua` — function `p3`
- `ReplicatedStorage/Shared/Information/Events/BingoEvent/init.lua` — function `p11`
- `ReplicatedStorage/Shared/Information/Events/BingoEvent/Templates.lua` — function `p0`
- `ReplicatedStorage/Shared/Information/Events/CreatorSpotlight/init.lua` — function `p1`
- `ReplicatedStorage/Shared/Information/Events/Summer2026Event/FusionUtils.lua` — function `p7`
- `ReplicatedStorage/Shared/Information/Events/Summer2026Event/init.lua` — function `p1`
- `ReplicatedStorage/Shared/Information/Evolutions.lua` — function `p5`
- `ReplicatedStorage/Shared/Information/Expeditions/init.lua` — functions `p46`, `p43`, `p7`
- `ReplicatedStorage/Shared/Information/GuildInfo/LevelData.lua` — function `p1`
- `ReplicatedStorage/Shared/Information/Portals.lua` — functions `p9`, `p3`, `p2`, `p1`
- `ReplicatedStorage/Shared/Information/TimeInfo.lua` — functions `p2`, `p1`
- `ReplicatedStorage/Shared/Information/Tournaments/init.lua` — functions `p20`, `p13`, `p12`, `p11`
- `ReplicatedStorage/Shared/LootPlan/init.lua` — function `p16`
- `ReplicatedStorage/Shared/Network/BufferEncoder/Miscellaneous/init.lua` — function `p1`
- `ReplicatedStorage/Shared/Promise/init.spec.lua` — function `p146`
- `ReplicatedStorage/Shared/SchimleFXUtils.lua` — functions `p13`, `p12`, `p11`, `p10`, `p2`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitter/Graph.lua` — functions `p3`, `p2`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitter/init.lua` — functions `p28`, `p26`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitterV2/Emit.lua` — function `p2`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitterV2/EmitAnimate.lua` — function `p2`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitterV2/Graph.lua` — functions `p3`, `p2`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitterV2/Update.lua` — functions `p3`, `p1`
- `ReplicatedStorage/Shared/TimeManager/MeshEmitterV2/UpdateModel.lua` — function `p1`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Emit.lua` — function `p4`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/EmitAnimate.lua` — function `p4`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/EngineReplay.lua` — function `p4`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Graph.lua` — functions `p6`, `p3`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/ImageEmit.lua` — function `p8`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Lightning/BoltGen.lua` — functions `p7`, `p6`, `p5`, `p4`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Lightning/Endpoints.lua` — function `p3`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Lightning/init.lua` — function `p1`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/PlayHandle.lua` — functions `p5`, `p4`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/PreSimulate.lua` — function `p1`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Rocks/init.lua` — function `p10`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/TrailEmitter.lua` — function `p2`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/Update.lua` — functions `p7`, `p6`
- `ReplicatedStorage/Shared/TimeManager/Part_Icles/UpdateModel.lua` — function `p2`
- `ReplicatedStorage/Shared/Utils/Bezier.lua` — functions `p11`, `p7`, `p6`
- `ReplicatedStorage/Shared/Utils/init.lua` — functions `p130`, `p58`, `p9`
- `ReplicatedStorage/Shared/Zone/Geometry/init.lua` — functions `p20`, `p18`
- `ReplicatedStorage/Shared/Zone/Geometry/Vertices.lua` — functions `p4`, `p3`
- `ReplicatedStorage/Shared/Zone/SimpleZone/Utility/SimpleSignal/init.lua` — function `p10`
- `ReplicatedStorage/Shared/Zone/SimpleZone/Utility/t.lua` — function `p10`
- `ReplicatedStorage/SheetSyncedModules/AllScaling/Parser.lua` — functions `p2`, `p1`
- `ReplicatedStorage/SheetSyncedModules/UnitTrials/Parser.lua` — functions `p1`, `p0`
- `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Effects/LensFlare/LensFlare.lua` — function `p9`
- `StarterPlayer/StarterPlayerScripts/ClientMapEffects/Effects/Rain/RainModule.lua` — functions `p34`, `p6`
- `StarterPlayer/StarterPlayerScripts/ClientMapEffects/init.client.lua` — function `p19`
- `StarterPlayer/StarterPlayerScripts/ClientUnitFollow.client.lua` — function `p19`
- `StarterPlayer/StarterPlayerScripts/Mounts/ShenronDragon/init.lua` — function `p23`
- `StarterPlayer/StarterPlayerScripts/PlayerModule/CameraModule/Invisicam.lua` — function `p22`
- `StarterPlayer/StarterPlayerScripts/PlayerModule/CameraModule/VRCameraTeleportDetector.spec.lua` — function `p6`
- `StarterPlayer/StarterPlayerScripts/PlayerModule/CameraModule/ZoomController/Popper.lua` — function `p16`
- `StarterPlayer/StarterPlayerScripts/PlayerModule/ControlModule/ClickToMoveDisplay.lua` — function `p19`
- `StarterPlayer/StarterPlayerScripts/PlayerModule/ControlModule/DynamicThumbstick.lua` — function `p29`
- `StarterPlayer/StarterPlayerScripts/PlayerModule/ControlModule/PathDisplay.lua` — function `p6`
- `StarterPlayer/StarterPlayerScripts/PlayerModule__volt-script-003651/CameraModule/Invisicam.lua` — function `p22`
- `StarterPlayer/StarterPlayerScripts/PlayerModule__volt-script-003651/CameraModule/ZoomController/Popper.lua` — function `p16`
- `StarterPlayer/StarterPlayerScripts/PlayerModule__volt-script-003651/ControlModule/ClickToMoveDisplay.lua` — function `p19`
- `StarterPlayer/StarterPlayerScripts/PlayerModule__volt-script-003651/ControlModule/DynamicThumbstick.lua` — function `p30`
- `StarterPlayer/StarterPlayerScripts/PlayerModule__volt-script-003651/ControlModule/PathDisplay.lua` — function `p6`

## 6. Vì sao các file này không được xuất output

Luồng strict kiểm tra invariant cuối cùng trước khi ghi source-like output. Khi gặp một trong ba proof failure ở trên, decompiler trả về typed rejection và không ghi `.luau`. Điều này bảo đảm:

1. Không biến một CFG có suffix executable thành `for` bằng cách đổi thứ tự side effect.
2. Không gán nhầm opcode prep/origin thành generic `pairs`/`ipairs`.
3. Không làm sai lifetime của iterator result đã bị capture bởi closure/upvalue.

Do đó, các file trong danh sách là “unsupported/unsafe under current source-like proof”, không phải “output đã tạo nhưng không compile”.

## 7. 42 file skipped (không phải failed)

Manifest đánh dấu các file sau là `skipped / empty_bytecode_payload`: entry có raw-bytecode container nhưng không có decoded payload. Chúng không đi vào loop structuring và không phải regression của PR #3.

- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 1/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 1/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002659.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 1/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002660.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 1/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002661.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 2/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 2/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002665.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 2/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002666.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 2/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002667.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 3/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 3/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002671.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 3/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002672.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/RaidSpiritCityAct 3/SceneSpace/Map/Cloud/DragonDrop sky/Model/Script__volt-script-002673.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/SinbadObtainmentCutscene/SceneSpace/Map/ MapCutscene/Cloud/BigDarkener/Script.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/SinbadObtainmentCutscene/SceneSpace/Map/ MapCutscene/Cloud/DarkerOuter/Script.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/SinbadObtainmentCutscene/SceneSpace/Map/ MapCutscene/Cloud/InnerCloud/Script.server.lua`
- `ReplicatedStorage/Assets/Cutscenes/SinbadObtainmentCutscene/SceneSpace/Map/ MapCutscene/Cloud/InnerCloud2/Script.server.lua`
- `ReplicatedStorage/FusionPackage/Utils/HyperText/LICENSE.server.lua`
- `ReplicatedStorage/FusionPackage/Utils/HyperText/READ ME.server.lua`
- `ReplicatedStorage/FusionPackage/Utils/HyperText/UPDATE LOG.server.lua`
- `ReplicatedStorage/MoonAnimatorBackups/==HOW TO RESTORE YOUR ANIMATIONS==.server.lua`
- `ReplicatedStorage/Nodes/Actor/Run/SActorTask.server.lua`
- `ReplicatedStorage/Shared/LootPlan/MultiExample.server.lua`
- `ReplicatedStorage/Shared/LootPlan/SingleExample.server.lua`
- `ReplicatedStorage/Shared/Zone/SimpleZone/Templates/ServerWorker/Worker.server.lua`
- `StarterPlayer/StarterPlayerScripts/RbxCharacterSounds.client.lua`
- `Workspace/Players/Damwibuskibidi/Health.server.lua`
- `Workspace/Players/Luk_Ppanda/Health.server.lua`
- `Workspace/Players/Monderhoa/Health.server.lua`
- `Workspace/Players/OVIS_BL/Health.server.lua`
- `Workspace/Players/Shielaaago/Health.server.lua`
- `Workspace/Players/Shinozzz92/Health.server.lua`
- `Workspace/Players/Superlamtark/Health.server.lua`
- `Workspace/Players/THE_REDMAN1001/Health.server.lua`
- `Workspace/Players/Thelight1606/Health.server.lua`
- `Workspace/Players/WGIT05/Health.server.lua`
- `Workspace/Players/aseon13me/Health.server.lua`
- `Workspace/Players/egodgpops2/Health.server.lua`
- `Workspace/Players/foxyprozed/Health.server.lua`
- `Workspace/Players/longgay91/Health.server.lua`
- `Workspace/Players/miracle_hunter35/Health.server.lua`
- `Workspace/Players/summohay27/Health.server.lua`
- `Workspace/Players/vualucj/Health.server.lua`

## 8. Verification sau khi fix

- Workspace Rust tests: pass (`cargo +nightly-2024-12-15 test --workspace --all-targets`).
- Release build: pass (`cargo +nightly-2024-12-15 build --release -p luau-lifter`).
- Fixture CI ở cả default và strict mode: pass; các rejection được assert là typed và không có residual marker.
- Official Luau audit: 3,817/3,817 emitted source contents parse và binary-compile thành công. Sáu path chứa Unicode được audit bằng ASCII staging copy vì local Windows compiler không mở được narrow-argv path; nội dung không thay đổi.
- Forbidden marker scan trên output corpus: 0 (`goto`, `controlFlowState`, `GenericForInit`, `GenericForNext`).

## 9. Kết luận gửi reviewer

Các failure hiện tại đã được định danh theo từng input/function và đều là kết quả fail-closed có chủ đích của source-like safety proof. Không có failure nào bị che giấu dưới dạng “success”, không có output `.luau` không hợp lệ được phát hành, và toàn bộ output đã phát hành đều qua official Luau parser/compiler audit. Nếu muốn giảm 161 rejection, cần bổ sung proof/CFG model tương ứng cho từng safety boundary; việc nới gate mà không có proof sẽ tái tạo đúng các rủi ro F1–F5 trong re-review.
