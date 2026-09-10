# Tovek — Tình trạng hiện tại & việc cần làm

> Cập nhật: 2026-09-10 — đã hoàn tất A–F trong worktree trên `80c86c2`. Corpus: `D:/Medal/examplebytecode/RobloxProject` (3.978 input). §1.1–1.2 giữ mốc lịch sử 2026-09-03; số liệu cuối ở §1.4 và [structurer_progress.md](structurer_progress.md).

Tick `[x]` khi xong. Mỗi mục có **Đo lường** để biết đã đạt chưa.

---

## 1. Tình trạng hiện tại

### 1.1 Đúng đắn (mốc 2026-09-03)

| Chỉ số | Giá trị |
|---|---:|
| Corpus strict (`--strict-no-synthetic-control`): decompiled / failed | **3.936 / 0** |
| Output chứa `controlFlowState` / `goto` / marker nội bộ | **0** |
| Output binary-compile bằng `luau-compile` chính thức | **3.978 / 3.978** |
| Round-trip semantic (`scripts/semantic_roundtrip.py`, 12 fixture × O0/O1/O2, chạy thật + so stdout) | **36 / 36** |
| Rejection kiểu `Unsafe` trên corpus | 0 |
| Function còn rơi vào legacy structurer (không có proof) | **317 / 26.391 (1,2%)** |
| `cargo test --workspace` | xanh (restructure 102, lifter 44, ast 577, cfg 33…) |
| Oracle bytecode round-trip (`scripts/bytecode_roundtrip.py`, decompile → recompile pin `c2ec0d4` → so proto chuẩn hoá) | **99,13 %** proto tương đương/chấp nhận (26.113 / 26.343); 3.936 / 3.936 input round-trip; lỗi thật (iii) = **0**; sau C1+C5: không tương đương 2.790 → 2.744, file tương đương hoàn toàn 2.693 → 2.721 |

### 1.2 Đẹp (mốc 2026-09-03, còn thiếu)

| Chỉ số | Giá trị | Ghi chú |
|---|---:|---|
| Tổng dòng so với `main` trước PR#3 | +2.157 (+0,4%) (trước C1: +5.352) | `Write.luau` 4.773 → **1.820**: structurer nhân bản khối dispatch ~1.400 dòng ×3, nay `factor_common_tails` gộp lại (không phải do helper inline) |
| Local chưa có tên (`local vN`) | **22.111** (trước B: 34.857) | |
| Lượt dùng param chưa tên (`pN`) | **35.014** (trước B: 67.667; trước type info: 70.301) | |
| Annotation type đã khôi phục | 4.451 trong 1.076 file | 21,9% proto có chữ ký |
| Local có type trong bytecode | 3.045 (vector 512, number 499, string 255, buffer 110…) | đã map sang SSA local (mục B); chỉ tag có tên (vector/buffer/thread/CFrame/Color3/boolean) mới đặt tên |
| De-inline call-site khôi phục (`-- inlined by Luau -O2`) | **588** (trước C1: 559; main trước PR#3: 558) | 5 file mất site đã lấy lại; +29 site mới (written-param, result-alias, arm-return, tail sau `return`) |
| Dòng thụt ≥ 8 tab | 24.550 (trước C1: 26.929, cùng cách đếm) | lồng sâu |
| Khối `if not c then return end` / `else return end` | 1.596 / 871 | |

### 1.3 Đã làm trong đợt 2026-09-02/03

- [x] Root cause `ForInitSuffixOrder`: edge-copy SSA nằm sau marker prep → destructor đặt trước marker (`cfg/src/ssa/destruct.rs::split_edge_transfer_around_for_prep`)
- [x] Alias `pairs`/`ipairs`/`next` là upvalue không bao giờ ghi → chấp nhận (`validate_for_origins` stable_upvalues)
- [x] Loop result bị closure capture trong thân loop → chấp nhận khi loop-owned (`captured_result_is_loop_owned`)
- [x] Shared-tail conditionals: join chung sớm nhất / stop của walk ngoài, validate + rollback + retry cả hàm (`shared_tail_join`, `build_plain_conditional`, `build_inside_join_conditional`)
- [x] Bỏ `continue` thừa cuối thân loop; guard pass muộn `flatten_terminal_tail_guards`
- [x] Bộ fixture round-trip + script + CI pin Luau `c2ec0d4`
- [x] Type info từ bytecode: annotation param + name hint (`parameter_types_from_bytecode`)
- [x] Diagnostics: `MEDAL_DUMP_CFG=1`, `MEDAL_DUMP_TYPES=1`, `MEDAL_NO_SHARED_TAIL=1`, `MEDAL_DEBUG_RESTRUCTURE=1`, `MEDAL_DUMP_PRE_DEINLINE=1`, `MEDAL_TRACE_FCT=1`
- [x] Đặt tên local/param từ type info + cách dùng (mục B): kênh typed-local → SSA, ~60 rule mới trong `name_locals.rs`, `vN` −37 %, `pN` −48 %
- [x] C5 (bảng hằng chết, 2026-09-03): xem mục C bên dưới
- [x] C1 (de-inline hình dạng, 2026-09-03): (1) `factor_common_tails` không hoist marker `-- inlined` (trailing comment) ra `end` của `if`; (2) `deinline_block` coi khối theo sau bởi đúng một `return` rỗng là tail (kể cả trong thân loop) → guard ⇄ nest canon áp dụng được; (3) trần chiều rộng cửa sổ void = `tail_spine_len` (guard-form nở theo nhánh `if` đuôi) thay cho `pat_raw_len + 1`; (4) Gap B dạng arm-return: cửa sổ kết thúc khối mà mọi nhánh `return RET` → `f(args); return RET`; (5) param bị ghi trong callee (`v = v + 1`) match như callee-local qua bản sao `local L = ARG` ở đầu site (`Target::written_params`); (6) result-alias: leaf `RESULT = E; S(RESULT)…` viết lại thành `local T = E; S(T)…; RESULT = T` để khớp `local L = E; …; return L` (`alias_result_leaves`); (7) `factor_common_tails::HoistLeafTails`: `if` ở vị trí tail có else-arm S bị nhân bản ở đuôi các leaf lồng sâu của then-arm → kéo S ra sau `if`, leaf khác thêm `continue`/`return` (no-op tại tail) — chính là dạng `continue`-fallthrough của nguồn mà structurer đã clone. Corpus: site 559 → 588, dòng 511.224 → 508.029, Write.luau 4.773 → 1.820; oracle 2.790 → 2.786 proto không tương đương (lớp xấu không đổi), baseline corpus cập nhật. Diagnostics mới: `MEDAL_DUMP_PRE_DEINLINE=1` (in AST trước de-inline), `MEDAL_TRACE_FCT=1` (in action của factor_common_tails)
- [x] Oracle bytecode round-trip toàn corpus (mục A) + fix lỗi thật nó tìm ra: `{a, b, f()}` bị hạ thành `t[1], t[2], t[3] = a, b, f()` (mất multret của `f()`) — fold-through `local t = {}` xuống sát `SETLIST` (`cfg/src/ssa/inline.rs::movable_table_declaration`) + fallback giữ ngữ nghĩa `for _k, _v in next, { f() } do t[n + _k] = _v end` (`ast/src/set_list.rs`); 5 test mới, corpus `investigate` 247→38

---

### 1.4 Kết quả cuối 2026-09-10

- **A–F hoàn tất**. Legacy đã xoá; còn **0** rejection sau retry trên corpus và semantic fixtures. Danh sách trước/sau và biên proof: [structurer_progress.md](structurer_progress.md).
- Bản release chuẩn (fat LTO): **3.936 decompiled / 42 skipped / 0 failed**; 3.936/3.936 recompile bằng compiler pin `c2ec0d4`. Cả 3.978 output không có marker điều khiển nội bộ.
- Workspace: **854 test** xanh; Python gate: **10 test** xanh. **45/45** lượt semantic O0/O1/O2 chạy trùng kết quả nguồn. Gate bytecode corpus/residual/semantic và gate kích thước **52 file** đều xanh.
- Oracle: proto không tương đương **2.744 → 2.699**, `investigate` **38 → 14**, `suspect` **6 → 5**. Mười tăng cục bộ đã được đối chiếu source/bytecode và ghi [review kèm raw delta](bytecode_roundtrip/review_20260910.md) trước khi cập nhật baseline; không nới luật gate.
- Output **504.400 dòng**; de-inline **660 site** (mục tiêu ≥600); `Write.luau` **959 dòng / 97 site**. C4 giảm fallback SETLIST **59 → 22**, gộp được cây UI tại các vị trí có proof thứ tự/arity.
- Chỉ tiêu cũ của E `<15.000 dòng thụt ≥8 tab` vẫn là giới hạn đã giải thích ở E, không đổi thành cam kết mới. Sau khi gộp cây UI, số này là **30.524**; guard pass đã chặn nhân đôi constructor/closure lớn.

## 2. Việc cần làm (xếp theo thứ tự đề xuất)

### [x] A. Oracle bytecode round-trip cho toàn corpus — *lưới an toàn cho mọi việc sau* (xong 2026-09-03, chi tiết `docs/bytecode_roundtrip.md`)

Mục tiêu: decompile → recompile `luau-compile -O2 --fflags=false` → so sánh với bytecode gốc đã chuẩn hoá, chạy trong CI. Bắt mọi trôi ngữ nghĩa mà parse/compile-check hiện tại bỏ lọt.

- [x] Viết `scripts/bytecode_roundtrip.py`: deserializer Python độc lập (v4–v11, key 203), chuẩn hoá mỗi proto (bỏ register, hằng theo giá trị, jump → nhãn, tách chuỗi import, `LOADN/LOADB`≡`LOADK`, `ADDK`≡`LOADK`+`ADD`, cực nhánh, `DUPTABLE`≡`NEWTABLE`, `CALL` bỏ số kết quả, `BCALL` cho builtin…); ghép proto theo cây + fingerprint (sibling hoán vị, helper de-inline); `--reclassify` chạy lại triage không cần recompile
- [x] Định nghĩa "tương đương": `exact` (chuỗi lệnh chuẩn hoá) / `equiv` (multiset lệnh ngữ nghĩa) / `differ` phân lớp accept·reduced·duplicated·inlined·outlined·dropped-const-table·investigate·suspect; bảng biến đổi được phép + rewrite trung tính (`CFrame.new()`≡`identity`, `Vector3.zero`, `SETLIST`≡`SETTABLEN`, `RETURN` thêm do shared-tail, `BCALL(*)`≡`BCALL(n)`…) trong `docs/bytecode_roundtrip.md`
- [x] Chạy trên 3.936 input có bytecode (42 file rỗng bỏ qua): (i) 26.113/26.343 = **99,13 %**; (ii) `investigate` 38 + `dropped-const-table` 43 + 6 `suspect` đã soi tay đều là artefact inline/outline; (iii) **1 lỗi thật → đã fix** (SETLIST multret, xem 1.3), sau fix = 0
- [x] CI (`.github/workflows/ci.yaml`, job `fixtures`): chạy trên `residual_control_flow` (bytecode) + `semantic_roundtrip` (`--sources`) gate `--baseline` (không file nào tụt status / tăng proto không tương đương); baseline trong `docs/bytecode_roundtrip/`; corpus riêng tư gate cục bộ bằng `baseline_corpus.json`
- [x] Chế độ ground truth `--sources DIR` (compile nguồn `-O2` → cùng pipeline + chỉ số **source likeness** token-ratio, 12 fixture = 0,803). ⚠️ 274 cặp `BytecodeTest`/`RealSourceTest` **không còn trên đĩa** — chạy lại khi khôi phục

**Đo lường:** % proto tương đương ≥ 99% → **99,13 %** ✅; danh sách (iii) = **0** ✅ (`exact+equiv` thuần = 89,4 %).

Phát hiện phụ cho các mục sau: decompiler bỏ hẳn `local t = {...}` không dùng kể cả bảng có closure (`FishingRankBanner`: mất 3 hàm `Formatter`; `TouchJump`/`BaseCamera`: bảng enum chuỗi) → 42 file mất thông tin; 20 constructor `{a, b}` vẫn thành `t[1], t[2] = a, b` khi `NEWTABLE` xa `SETLIST` và entry có side-effect (mục C); compiler pin inline `local function` ở nhiều site hơn Roblox → phình dòng khi đo (mục D/E).

### [x] B. Đặt tên local/param từ type info + cách dùng — *tác động thị giác lớn nhất* (xong 2026-09-03)

- [x] Map typed local (`register` + dải `pc`) sang SSA local: lifter ghi pc cho từng statement (`record_typed_local_hints`), `Function::local_type_hints` keyed `(block, stmt, written)` → `ssa::construct::fresh_local` gắn hint vào `Local.1`, `apply_local_map` giữ hint khi gộp → namer dùng ở tier thấp nhất (20), bỏ qua temp single-use movable. Hint: vector→`vector`, CFrame→`cframe`, Color3→`color`, buffer→`buf` (tránh `buffer2` vì lib `buffer` trong scope), thread→`thread`, boolean→`flag`. Cùng kênh này có thể map debug-locals sau.
- [x] Heuristic theo cách dùng cho param: `for _, x in p`→`items`; `#p`/`p[1]`/`p[i]`/`ipairs`/`table.insert(p)`→`list`; `p.Keypoints`→`sequence`; `p:IsA("X")` xung đột lớp→`instance`; `p[Children]`→`props`; `p.Parent`+property→`instance`; receiver `:Computed`/`:ForPairs`/`:New("X")`→`scope`; `innerScope(p)`→`scope`, `peek(p)`→`state`; `:RegisterType`→`registry`; `:GiveTask`/`:Add(fn)`→`maid`; `:LoadAnimation`→`animator`; `buffer.*`→`buf`/`offset`/`value`; slot API Roblox (`FireClient`→player, `IsDescendantOf`→ancestor, `PivotTo`→cframe, `Instance.new`→className/parent, `error`→message, `require`→moduleScript, `task.spawn`/`pcall`→callback…); hypernym `data`/`state`/`object`. KHÔNG làm `p2 < magnitude`→`maxDistance` (đoán mò).
- [x] Naming local từ callee: noun fallback (`:Computed`→computed, `:NextNumber`→number, `:IsValid`→isValid, verb trần bị từ chối), verb→participle (`merge`→merged, `freeze`→frozen), `KeyOf(t,"K")`→k, `scope:New("Frame")`→frame, `typeof`→typeName, `getmetatable`→metatable, `table.find`→index, `coroutine.running`→thread, `require(local)`→module, `require(script.Parent)`→parentModule, `CFrame.Angles/lookAt/from*`→cframe, wrapper trong suốt (`math.floor(x.Height)`→height, `peek(s.Key)`→key), `setmetatable`→self/object, `#t`→count, `items[i]`→item, accumulator→total
- [x] Annotate `p: number` giữ nguyên (chính xác, không tốn dòng)
- [x] Không làm: naming liên thủ tục (đo lại: callee→arg chỉ ~246 site)
- [x] Guard +lines: temp copy movable single-use không bao giờ bị đặt tên từ usage (`movable_temp_locals`) → còn dọn được 84 dòng copy cũ; `self` chỉ khi không bị capture/không ở root/không trong method candidate (giữ 1.961 colon-method)

**Đo lường:** `local vN` 34.857 → **22.111** (< 25.000 ✅); `pN` 67.667 → **35.014** (< 50.000 ✅); dòng 511.308 → 511.224; A không đổi (oracle baseline 2.790 → 2.790, 0 regression) ✅; ast 590 test xanh.

### [x] C. De-inline chuẩn hoá hình dạng — hoàn tất C4/C6 ngày 2026-09-10

Pass de-inline (`ast/src/deinline.rs`) chỉ khớp hai bản inline khi AST giống hệt.

- [x] Chuẩn hoá trước khi hash: guard ↔ lồng đã có (`unguard`), nay áp dụng được cả khi khối theo sau bởi `return` rỗng / trong thân loop; trần cửa sổ theo `tail_spine_len`; tên local đã là binding-hole. *Không làm* `and`/`or` giao hoán và `x = x + 1` ↔ `x += 1`: cùng một bytecode nên hai bản inline không lệch dạng này (đo: 0 site cần)
- [x] Sửa 5 file mất call-site so với `main` (`ClickToMoveDisplay` ×2, `ClientFishingHandler`, `SaveDiscovery`, `pool`) — 4 nguyên nhân: marker bị `factor_common_tails` hoist, tail sau `return`, trần cửa sổ, Gap B arm-return. Kèm written-param + result-alias (probe `grow`/`put` 8/8 site)
- [x] `Write.luau`: mốc C1 **4.773 → 1.820** dòng nhờ gộp shared-tail; C6 hiện còn **959** dòng và **97** site helper được khôi phục. Shared-tail được factor trước khi đặt phạm vi khai báo; chi tiết C6 bên dưới.
- [x] C4: `rebuild_ui_expression_trees` gộp handle gọi một lần ở vị trí callee, alias khóa `children`, field động liên tiếp và SETLIST muộn. Giữ thứ tự key → value → store, snapshot capture, điều kiện thực thi và arity; không đổi call thành multret ngoài vị trí cho phép. **59 → 22** fallback (21 file còn lại có nhánh/khai báo/capture cần giữ). `Menu` đã thành cây props/children liền mạch. Fallback còn lại dùng `table.pack` có đếm để ghi đúng cả nil; fixture mới bắt và sửa cả lỗi SSA trì hoãn gán captured table. Chi tiết [ui_tree_rebuild.md](ui_tree_rebuild.md).
- [x] Không bỏ `local t = {...}` chết (C5, 2026-09-03): SSA inliner giữ bảng hằng KHÔNG rỗng dù không dùng (`keep_const_table` trong `cfg/src/ssa/inline.rs`), và không forward `{}` vào gốc index-write (`local t = {}; t.k = v` từng thành `({}).k = v` — mất closure `Formatter`). Ra `local _ = {...}`. Oracle `dropped-const-table` 43 → 6 (6 còn lại là khác dạng `SETTABLEN` vs `SETLIST` / field `= nil`, không phải mất mã); proto không tương đương 2.786 → 2.744; +400 dòng (mã khôi phục), 69 file. Fixture mới `semantic_roundtrip/dead_const_tables.luau`

- [x] C6: factor shared-tail trước `LocalDeclarer`; coalescing AST đo áp lực local theo chuỗi phạm vi và chỉ ghép các local có cùng phạm vi nhánh/vòng lặp. Khai báo của nhánh giữ trong nhánh, giúp de-inline thêm **91** site ở `Write` (**6 → 97**). Chính sách SSA destructor được giữ nguyên; thử nghiệm chặn copy-coalescing ở SSA không giải quyết được loop protocol, nên thay đổi cuối nằm ở `factor_common_tails` và `coalesce_locals`.

**Đo lường:** site `-- inlined by Luau -O2` **588 → 660** (mục tiêu ≥600 ✅); `Write.luau` **1.820 → 959** dòng (<2.500 ✅); toàn corpus **504.400** dòng. Oracle **2.744 → 2.699** proto không tương đương; `investigate` **38 → 14**, `suspect` **6 → 5**, `dropped-const-table` vẫn **6**. Cả gate tổng và từng file xanh sau [review baseline](bytecode_roundtrip/review_20260910.md).

### [x] D. Bỏ hẳn legacy structurer — hoàn tất 2026-09-10

Mốc cũ **317** lượt còn `Unsupported` sau retry; hiện **0** trên **26.391** lượt structuring của corpus. Có 9 lượt cần retry bằng builder có proof; cả 9 đều thành công. Bộ semantic cũng không còn rejection sau retry. Legacy matcher và mọi caller đã bị xoá.

- [x] Liệt kê 317 function (`MEDAL_DEBUG_RESTRUCTURE=1`, lọc `retry ... -> Unsupported`), gom theo nhánh từ chối trong `build_path`/`build_loop`: `scripts/structurer_inventory.py`, báo cáo trong `docs/structurer_inventory/`, chi tiết [structurer_progress.md](structurer_progress.md). Diagnostics có node, stop, loại proof và pha retry; script từ chối log chưa chạy xong hoặc không serial.
- [x] Shared-tail, re-entry hữu hạn/vô hạn và join trong cùng iteration có ownership rõ ràng; đường hội tụ chỉ được ghép khi cả hai nhánh tới join trước lần lặp kế tiếp. Numeric/generic loop lồng trong outer cycle có biên riêng; giữ clone budget và helper factoring.
- [x] Numeric-for không backedge, inverted while, header effect/copy, exhaustion adapter, terminal fringe và implicit function exit đều có kiểm tra riêng. Result export giữ outer cell khi parameter/callback dùng sau vòng lặp; loop binding riêng không làm đổi capture. Fixture `loop_shapes` và `loop_result_exports` kiểm chứng các đường continue/break/return/zero-trip.
- [x] Xoá legacy matcher trong `restructure/src/lib.rs`, các module `conditional.rs`, `jump.rs`, `loop.rs`, `may_use_legacy_structurer` và các dependency không còn dùng. Luau và Lua 5.1 lifter gọi builder có proof; trường hợp chưa chứng minh được đi qua chính sách fallback/refusal hiện hành.

**Đo lường:** `retry ... Unsupported` **0**; `Unsafe` **0**; corpus strict **3.936 thành công / 42 rỗng / 0 lỗi**; oracle và toàn bộ fixture gate xanh.

### [x] E. Giảm lồng sâu — *công sức thấp* (xong 2026-09-03 — phần làm được; chỉ tiêu < 15.000 KHÔNG khả thi, xem phân tích)

- [x] Gộp `if a then if b then … end end` (không else, không statement khác) → `if a and b then` (`canonicalize_branches::merge_nested_conjunct_ifs`; đếm trên corpus: chỉ 18 site)
- [x] Kéo `elseif` khi nhánh else chỉ chứa một `if`: formatter đã làm sẵn; 250 chỗ `else` + `if` còn lại đều có statement khác đi kèm (guard + call, marker) nên không phải `elseif`
- [x] Guard `if not c then continue end` cho thân loop: `recover_guard_continue` (§9) đã có; đếm trên corpus chỉ 1 thân loop là một `if` dài duy nhất còn sót

**Đo lường:** dòng thụt ≥ 8 tab: 24.550 (cùng cách đếm, trước C1 26.929). Phân tích thành phần: 11.482 dòng là field của table constructor (cây UI Fusion/React), 11.108 là statement trong closure lồng trong cây UI, chỉ 609 dòng là `if`/`elseif` và 1.351 là `end`. Tức lồng sâu là bản chất nguồn (declarative UI), không phải cấu trúc điều khiển → chỉ tiêu < 15.000 không đạt được bằng biến đổi `if`; A không đổi.

### [x] F. Hardening — hoàn tất 2026-09-10

- [x] `cfg/src/ssa/close_provenance.rs` ghi proof CLOSEUPVALS trước khi SSA xoá marker; proof/obligation đi qua renaming/coalescing. Ref-capture phải đóng đúng register trên mọi cạnh backedge/continue/break; không cho dùng/ghi/recapture sau close. Bytecode fixture thật bị sửa thành NOP hoặc CLOSE sai register phải bị strict từ chối. Có unit test và integration test `iteration_cell`.
- [x] Gate kích thước theo từng file trong CI: `scripts/output_size.py`, baseline **52 file** (7 residual + 45 semantic output), 4 test gate. Ngưỡng `max(25%, 40 dòng)` và `max(25%, 2.048 byte)`; file thiếu/chưa baseline cũng fail. Fixture Transform 470 → 5.195 dòng xác nhận không thể che bằng file khác nhỏ đi.
- [x] Dọn file scratch untracked trong thư mục repo (`tmp_*`, `*.err`, `out_*`, `selectedOut_*`) hoặc thêm vào `.gitignore`: đã ignore các nhóm scratch ở root, không xoá tài liệu hay fixture.

---

## 3. Lệnh đo nhanh

```powershell
# corpus strict
target/release/luau-lifter.exe decompile-folder D:/Medal/examplebytecode/RobloxProject <out> -t 8 -v --emit-upvalue-analysis

# compile toàn bộ output bằng Luau chính thức (script trong scratchpad phiên trước: validate_luau.py)
# round-trip semantic
python scripts/semantic_roundtrip.py --compiler D:/Medal/luau-tools-src/build/luau-compile.exe --luau D:/Medal/luau-tools-src/build/luau.exe --lifter target/release/luau-lifter.exe

# oracle bytecode round-trip toàn corpus (gate: không tăng proto không tương đương)
python scripts/bytecode_roundtrip.py --lifter target/release/luau-lifter.exe --compiler D:/Medal/luau-tools-src/build/luau-compile.exe --corpus D:/Medal/examplebytecode/RobloxProject --key 203 --threads 8 --report out/rt.json --markdown out/rt.md --baseline docs/bytecode_roundtrip/baseline_corpus.json

# kiểm tra rejection cuối cùng (legacy đã bị xoá)
MEDAL_DEBUG_RESTRUCTURE=1 target/release/luau-lifter.exe decompile-folder <corpus> <out> -t 1 2>&1 | grep -c "retry .* Unsupported"

# dump type info
MEDAL_DUMP_TYPES=1 target/release/luau-lifter.exe decompile-folder <corpus> <out> -t 1 2> types.err
```

Lưu ý build: `cargo +nightly-2024-12-15 build --release -p luau-lifter`; compile probe v9: `luau-compile --binary -O2 --fflags=false`.
