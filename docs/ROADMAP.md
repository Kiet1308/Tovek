# Tovek — Tình trạng hiện tại & việc cần làm

> Cập nhật: 2026-09-03 · `main` @ `df5be58` · corpus đo: `D:/Medal/examplebytecode/RobloxProject` (3.978 input, bytecode v9, types v3)

Tick `[x]` khi xong. Mỗi mục có **Đo lường** để biết đã đạt chưa.

---

## 1. Tình trạng hiện tại

### 1.1 Đúng đắn (đã đạt)

| Chỉ số | Giá trị |
|---|---:|
| Corpus strict (`--strict-no-synthetic-control`): decompiled / failed | **3.936 / 0** |
| Output chứa `controlFlowState` / `goto` / marker nội bộ | **0** |
| Output binary-compile bằng `luau-compile` chính thức | **3.978 / 3.978** |
| Round-trip semantic (`scripts/semantic_roundtrip.py`, 11 fixture × O0/O1/O2, chạy thật + so stdout) | **33 / 33** |
| Rejection kiểu `Unsafe` trên corpus | 0 |
| Function còn rơi vào legacy structurer (không có proof) | **317 / 26.391 (1,2%)** |
| `cargo test --workspace` | xanh (restructure 102, lifter 44, ast 577, cfg 33…) |
| Oracle bytecode round-trip (`scripts/bytecode_roundtrip.py`, decompile → recompile pin `c2ec0d4` → so proto chuẩn hoá) | **99,13 %** proto tương đương/chấp nhận (26.113 / 26.343); 3.936 / 3.936 input round-trip; lỗi thật (iii) = **0** |

### 1.2 Đẹp (còn thiếu)

| Chỉ số | Giá trị | Ghi chú |
|---|---:|---|
| Tổng dòng so với `main` trước PR#3 | +5.352 (+1,0%) | `Write.luau` +2.701 là bytecode tự inline helper 22 lần |
| Local chưa có tên (`local vN`) | **22.111** (trước B: 34.857) | |
| Lượt dùng param chưa tên (`pN`) | **35.014** (trước B: 67.667; trước type info: 70.301) | |
| Annotation type đã khôi phục | 4.451 trong 1.076 file | 21,9% proto có chữ ký |
| Local có type trong bytecode | 3.045 (vector 512, number 499, string 255, buffer 110…) | đã map sang SSA local (mục B); chỉ tag có tên (vector/buffer/thread/CFrame/Color3/boolean) mới đặt tên |
| De-inline call-site khôi phục (`-- inlined by Luau -O2`) | 559 (main: 540) | 5 file mất 1 site, 18 file thêm |
| Dòng thụt ≥ 8 tab | 26.451 | lồng sâu |
| Khối `if not c then return end` / `else return end` | 1.596 / 871 | |

### 1.3 Đã làm trong đợt 2026-09-02/03

- [x] Root cause `ForInitSuffixOrder`: edge-copy SSA nằm sau marker prep → destructor đặt trước marker (`cfg/src/ssa/destruct.rs::split_edge_transfer_around_for_prep`)
- [x] Alias `pairs`/`ipairs`/`next` là upvalue không bao giờ ghi → chấp nhận (`validate_for_origins` stable_upvalues)
- [x] Loop result bị closure capture trong thân loop → chấp nhận khi loop-owned (`captured_result_is_loop_owned`)
- [x] Shared-tail conditionals: join chung sớm nhất / stop của walk ngoài, validate + rollback + retry cả hàm (`shared_tail_join`, `build_plain_conditional`, `build_inside_join_conditional`)
- [x] Bỏ `continue` thừa cuối thân loop; guard pass muộn `flatten_terminal_tail_guards`
- [x] Bộ fixture round-trip + script + CI pin Luau `c2ec0d4`
- [x] Type info từ bytecode: annotation param + name hint (`parameter_types_from_bytecode`)
- [x] Diagnostics: `MEDAL_DUMP_CFG=1`, `MEDAL_DUMP_TYPES=1`, `MEDAL_NO_SHARED_TAIL=1`, `MEDAL_DEBUG_RESTRUCTURE=1`
- [x] Đặt tên local/param từ type info + cách dùng (mục B): kênh typed-local → SSA, ~60 rule mới trong `name_locals.rs`, `vN` −37 %, `pN` −48 %
- [x] Oracle bytecode round-trip toàn corpus (mục A) + fix lỗi thật nó tìm ra: `{a, b, f()}` bị hạ thành `t[1], t[2], t[3] = a, b, f()` (mất multret của `f()`) — fold-through `local t = {}` xuống sát `SETLIST` (`cfg/src/ssa/inline.rs::movable_table_declaration`) + fallback giữ ngữ nghĩa `for _k, _v in next, { f() } do t[n + _k] = _v end` (`ast/src/set_list.rs`); 5 test mới, corpus `investigate` 247→38

---

## 2. Việc cần làm (xếp theo thứ tự đề xuất)

### [x] A. Oracle bytecode round-trip cho toàn corpus — *lưới an toàn cho mọi việc sau* (xong 2026-09-03, chi tiết `docs/bytecode_roundtrip.md`)

Mục tiêu: decompile → recompile `luau-compile -O2 --fflags=false` → so sánh với bytecode gốc đã chuẩn hoá, chạy trong CI. Bắt mọi trôi ngữ nghĩa mà parse/compile-check hiện tại bỏ lọt.

- [x] Viết `scripts/bytecode_roundtrip.py`: deserializer Python độc lập (v4–v11, key 203), chuẩn hoá mỗi proto (bỏ register, hằng theo giá trị, jump → nhãn, tách chuỗi import, `LOADN/LOADB`≡`LOADK`, `ADDK`≡`LOADK`+`ADD`, cực nhánh, `DUPTABLE`≡`NEWTABLE`, `CALL` bỏ số kết quả, `BCALL` cho builtin…); ghép proto theo cây + fingerprint (sibling hoán vị, helper de-inline); `--reclassify` chạy lại triage không cần recompile
- [x] Định nghĩa "tương đương": `exact` (chuỗi lệnh chuẩn hoá) / `equiv` (multiset lệnh ngữ nghĩa) / `differ` phân lớp accept·reduced·duplicated·inlined·outlined·dropped-const-table·investigate·suspect; bảng biến đổi được phép + rewrite trung tính (`CFrame.new()`≡`identity`, `Vector3.zero`, `SETLIST`≡`SETTABLEN`, `RETURN` thêm do shared-tail, `BCALL(*)`≡`BCALL(n)`…) trong `docs/bytecode_roundtrip.md`
- [x] Chạy trên 3.936 input có bytecode (42 file rỗng bỏ qua): (i) 26.113/26.343 = **99,13 %**; (ii) `investigate` 38 + `dropped-const-table` 43 + 6 `suspect` đã soi tay đều là artefact inline/outline; (iii) **1 lỗi thật → đã fix** (SETLIST multret, xem 1.3), sau fix = 0
- [x] CI (`.github/workflows/ci.yaml`, job `fixtures`): chạy trên `residual_control_flow` (bytecode) + `semantic_roundtrip` (`--sources`) gate `--baseline` (không file nào tụt status / tăng proto không tương đương); baseline trong `docs/bytecode_roundtrip/`; corpus riêng tư gate cục bộ bằng `baseline_corpus.json`
- [x] Chế độ ground truth `--sources DIR` (compile nguồn `-O2` → cùng pipeline + chỉ số **source likeness** token-ratio, 11 fixture = 0,798). ⚠️ 274 cặp `BytecodeTest`/`RealSourceTest` **không còn trên đĩa** — chạy lại khi khôi phục

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

### [ ] C. De-inline chuẩn hoá hình dạng — *cắt dòng nhiều nhất*

Pass de-inline (`ast/src/deinline.rs`) chỉ khớp hai bản inline khi AST giống hệt.

- [ ] Chuẩn hoá trước khi hash: guard ↔ lồng (`if not c then return end; A` ≡ `if c then A end`), tên local, thứ tự `and`/`or` giao hoán, `x = x + 1` ↔ `x += 1`
- [ ] Sửa 5 file mất call-site so với `main` (`ClickToMoveDisplay` ×2, `ClientFishingHandler`, `SaveDiscovery`, `pool`)
- [ ] `Write.luau`: helper "ensure capacity + write" inline 22 lần → gom về 1 hàm (kỳ vọng −2.000 dòng)
- [ ] Constructor `{a, b, f()}` có `NEWTABLE` cách xa `SETLIST` vì phần tử cần temporaries có side-effect (Fusion `New "Frame" {...}` lồng): hiện fold-through chỉ khi entry đã có đều thuần; còn 51 file rơi vào fallback `for _k, _v in next, { f() }` và 20 proto `t[1], t[2] = a, b` — cần inline ngược temporaries vào constructor (oracle A: lớp `investigate`)
- [ ] Không bỏ `local t = {...}` chết khi bảng chứa closure/hằng chuỗi (42 file, oracle A lớp `dropped-const-table`) — giữ dưới dạng `local _ = {...}` hoặc comment

**Đo lường:** call-site `-- inlined by Luau -O2` ≥ 600; `Write.luau` < 2.500 dòng; A không đổi.

### [ ] D. Bỏ hẳn legacy structurer — *đóng mảnh code không có proof*

317 function trả `Unsupported` ở cả 2 lần thử của builder source-like.

- [ ] Liệt kê 317 function (`MEDAL_DEBUG_RESTRUCTURE=1`, lọc `retry ... -> Unsupported`), gom theo lý do `return None` trong `build_path`/`build_loop`
- [ ] Shared-tail nhiều điểm vào: cho phép nhân bản có ngân sách nhỏ, hoặc tách thành local function (`synthesize_terminal_helpers` đã có)
- [ ] Các hình dạng hiếm khác (If có body sẵn, block 1 successor có statement không linear, nested loop join ngoài vùng…)
- [ ] Khi = 0: xoá `restructure/src/lib.rs` legacy matcher và `may_use_legacy_structurer`

**Đo lường:** `retry ... Unsupported` = 0; corpus strict vẫn 0 fail; A không đổi.

### [ ] E. Giảm lồng sâu — *công sức thấp*

- [ ] Gộp `if a then if b then … end end` (không else, không statement khác) → `if a and b then`
- [ ] Kéo `elseif` khi nhánh else chỉ chứa một `if`
- [ ] Guard `if not c then continue end` cho thân loop khi phần còn lại dài (đã có cho `return`, mở rộng cho `continue`/`break`)

**Đo lường:** dòng thụt ≥ 8 tab từ 26.451 xuống < 15.000; A không đổi.

### [ ] F. Hardening còn lại (ưu tiên thấp)

- [ ] Giữ provenance `CLOSEUPVALS` qua SSA để chứng minh iteration-cell cho bytecode thủ công (mục §6 của reviewer; không cần cho bytecode compiler sinh ra)
- [ ] Benchmark kích thước output theo file trong CI, chặn regression kiểu Transform 470 → 5.195 dòng
- [ ] Dọn file scratch untracked trong thư mục repo (`tmp_*`, `*.err`, `out_*`, `selectedOut_*`) hoặc thêm vào `.gitignore`

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

# đếm hàm còn dùng legacy
MEDAL_DEBUG_RESTRUCTURE=1 target/release/luau-lifter.exe decompile-folder <corpus> <out> -t 1 2>&1 | grep -c "retry .* Unsupported"

# dump type info
MEDAL_DUMP_TYPES=1 target/release/luau-lifter.exe decompile-folder <corpus> <out> -t 1 2> types.err
```

Lưu ý build: `cargo +nightly-2024-12-15 build --release -p luau-lifter`; compile probe v9: `luau-compile --binary -O2 --fflags=false`.
