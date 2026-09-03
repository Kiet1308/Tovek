# Oracle bytecode round-trip

`scripts/bytecode_roundtrip.py` là lưới an toàn ngữ nghĩa cho toàn corpus: mọi thay
đổi decompiler đều phải giữ **số proto không tương đương không tăng** (gate CI trên
fixture, gate cục bộ trên corpus qua `--baseline`).

## Quy trình

1. `luau-lifter decompile-folder --strict-no-synthetic-control` cả cây input.
2. Recompile từng `.luau` bằng compiler chính thức pin `c2ec0d4` (0.736):
   `luau-compile --binary -O2 -g1 --fflags=false --vector-lib=Vector3 --vector-ctor=new --vector-type=Vector3`
   (bytecode v9 như Roblox; `--vector-*` để `Vector3.new(k,k,k)` fold thành hằng vector
   giống compiler Roblox).
3. Parse cả hai chunk bằng deserializer Python độc lập (v4–v11, types 0–3, key 203/1).
4. Chuẩn hoá từng proto rồi so sánh (mục dưới).
5. Ghép proto gốc ↔ proto recompile: cùng cha + cùng số con → ghép theo fingerprint
   giống hệt trước, rồi theo độ giống (Jaccard ≥ 0,5), rồi theo vị trí; số con khác nhau
   (de-inline sinh helper, closure bị inline mất) → ghép theo độ giống, phần dư là
   `missing`/`extra`.

Chế độ `--sources DIR` (ground truth): compile từng `.luau` nguồn ở `-O2` rồi chạy y
hệt, thêm chỉ số **source likeness** = tỉ lệ khớp chuỗi token (identifier chuẩn hoá
thành `ID`, giữ keyword/literal/toán tử) giữa nguồn thật và output.

## Chuẩn hoá proto

| Thành phần | Chuẩn hoá |
|---|---|
| Register (A/B/C) | bỏ hoàn toàn |
| Hằng (LOADK, GETTABLEKS aux, ADDK C, JUMPXEQK aux…) | thay bằng **giá trị** (chuỗi, số `%.17g`, import path, vector) — không phụ thuộc thứ tự bảng hằng |
| `GETIMPORT(@a.b.c)` | tách thành `GETIMPORT(@a)` + `GETTABLEKS("b")` + `GETTABLEKS("c")` — compiler pin gộp `script.Parent.X` thành 1 import, Roblox không |
| Jump | đích thành nhãn `L<n>` theo thứ tự pc |
| `LOADN` | = `LOADK` (cùng giá trị) |
| `ADDK/SUBK/…/SUBRK/DIVRK` | = `LOADK(k)` + `ADD/SUB/…` |
| `JUMPIF/JUMPIFNOT`, `JUMPIFEQ/NOTEQ`, `LT/NOTLT`, `LE/NOTLE`, `JUMPXEQK*` (bit not) | cùng token `TEST/CMPEQ/CMPLT/CMPLE/CMPK*` — đổi cực nhánh là trung tính |
| `DUPTABLE(template)` | = `NEWTABLE(0)` (+ `SETTABLEKS(k)`/`LOADK(v)` cho entry hằng khác nil của template kiểu 8) |
| `CALL(args, results)` | chỉ giữ số đối số; kết quả không dùng là trung tính; CALL sau `FASTCALL*` thành `BCALL` |
| `FORGLOOP(nvars)` | bỏ số biến (`for k, _ in` = `for k in`) |
| Bỏ khỏi multiset | `NOP BREAK MOVE JUMP JUMPBACK JUMPX COVERAGE CLOSEUPVALS PREPVARARGS FASTCALL* NATIVECALL` |
| Rewrite theo chuỗi (cả hai phía) | `CFrame.new()`≡`CFrame.identity` (kể cả `return` multret→1), `Vector3.zero/one/xAxis/yAxis/zAxis`≡hằng vector, `Vector2.new(0,0)/(1,1)`≡`Vector2.zero/one` |

## Định nghĩa tương đương (từng proto)

| Tier | Điều kiện | Ý nghĩa |
|---|---|---|
| `exact` | chuỗi lệnh chuẩn hoá (có nhãn) giống hệt | cùng bytecode |
| `equiv` | multiset lệnh ngữ nghĩa giống hệt | chỉ khác thứ tự block, cực nhánh, MOVE/JUMP — guard ↔ lồng, `and/or` tái kết hợp, `+=`, đổi tên |
| `differ` | multiset khác | phân lớp bên dưới |
| `missing`/`extra` | không có proto đối ứng | helper de-inline, closure bị inline mất |

Lớp triage cho `differ` (`classify_delta`), sau khi khử các rewrite trung tính đã biết
(`RETURN(0)` thêm/bớt ở cuối, `RETURN(n)` thêm do nhân bản shared-tail,
`SETLIST(n)`≡`SETTABLEN(1..n)`, `BCALL(*)`≡`BCALL(n)` — compiler pin cắt multret ở
builtin có arity cố định, `BCALL(n)`≡`CALL(n)` khi alias `local clamp = math.clamp`,
`{k = nil}` chỉ pre-shape ở Roblox):

| Lớp | Tiêu chí | Bin |
|---|---|---|
| `accept` | chỉ còn họ trung tính (LOADK/LOADNIL/LOADB/NOT/AND/OR/CMP/CAPTURE/GETUPVAL/SETUPVAL/NEWTABLE/DUPTABLE/SETLIST/GETVARARGS/MINUS) hoặc constant folding (`0+7+7+7`→`21`) | (i) |
| `reduced` | mọi lệnh mất vẫn còn trong proto mới, chỉ ít lần hơn | (i) de-inline dedup, gộp shared-tail, `t[k].x = t[k].x + 1`→`+=` |
| `duplicated` | mọi lệnh thêm đã có trong proto gốc | (i) nhân bản shared-tail, helper inline thêm site (phình dòng, xem mục D) |
| `outlined` / `inlined` | phần mất/thêm nằm ở proto khác (bỏ qua hằng/upvalue/CALL vì inline specialise) | (i) de-inline sinh helper / compiler pin inline `local function` mà Roblox không |
| `dropped-const-table` | mất `NEWTABLE`+`SETTABLEKS`+hằng, không thêm gì | (ii) decompiler bỏ `local t = {...}` chết — mất thông tin, không đổi hành vi |
| `investigate` | họ lệnh đổi số lượng hai chiều nhưng mọi hằng/đích gọi vẫn có ở cả hai phía | (ii) |
| `suspect` | có hằng/đích gọi/field/loop/return chỉ tồn tại một phía | (iii) ứng viên lỗi thật, phải soi tay |

Biến đổi được **chấp nhận** (đã kiểm chứng tay trên fixture + corpus): đổi tên,
`x = x + 1`↔`x += 1`, guard `if not c then return end; A`↔`if c then A end`, hoán
vị `and/or`, de-inline (body inline ×N → 1 helper + N call), nhân bản/gộp shared-tail,
`{}`+field store↔constructor, `{a,b,c}`↔`t[1],t[2],t[3]=a,b,c`, materialise boolean
qua nhánh↔`and/or`, `local x` sớm rồi gán (CAPTURE val→ref), bỏ `return` trống cuối
hàm, `CFrame.new()`→`CFrame.identity`, `Vector3.new(0,0,0)`→`Vector3.zero`.

## Kết quả corpus (RobloxProject, 3.936 input có bytecode) — 2026-09-03

Lệnh: `--corpus D:/Medal/examplebytecode/RobloxProject --key 203` (lifter `main` + fix
SETLIST, compiler pin `c2ec0d4`). Chạy ~45 s (decompile 4 s, recompile+compare 38 s, 8 luồng).

| Chỉ số | Giá trị |
|---|---:|
| Input round-trip xong (decompile strict → recompile → parse) | **3.936 / 3.936** |
| Proto so sánh | 26.343 |
| `exact` / `equiv` | 15.694 / 7.859 (**89,4 %**) |
| `differ` → accept / reduced / duplicated / inlined / outlined | 2.210 / 157 / 59 / 94 / 40 |
| **Tương đương + chấp nhận (bin i)** | **26.113 / 26.343 = 99,13 %** ✅ (mục tiêu ≥ 99 %) |
| `dropped-const-table` (bin ii, mất thông tin) | 43 proto / 42 file |
| `investigate` (bin ii) | 38 proto — 20 là constructor `{a, b}`→`t[1], t[2] = a, b` không có tail (đúng ngữ nghĩa, xấu), 2 là `return f(x)` bị compiler pin suy ra 1 kết quả (đã kiểm chứng bằng `--text`: `-O1` giữ multret, `-O2` cắt — do compiler, không phải decompiler) |
| `suspect` (bin iii ứng viên) | 6 proto — **soi tay cả 6: 0 lỗi thật** (bảng dưới) |
| `missing` / `extra` proto | 7 / 136 (136 extra nằm trong 30 file, 16 file có marker de-inline; còn lại là closure bị compiler pin inline mất hoặc helper de-inline không marker) |
| File tương đương hoàn toàn | 2.693 / 3.936 |

Trước khi sửa decompiler (lần chạy 3): `investigate` 247, `suspect` 110 — phần lớn là
`SETLIST` bị hạ thành multi-assign (mất multret). Sau fix: `investigate` 38, `suspect` 6.

6 `suspect` còn lại (đã soi diff + text):

| File | Proto | Kết luận |
|---|---|---|
| `DivergentVFX/effects/impactframe` | 6 | de-inline: 3 body inline → 1 helper (`CALL(4)`), 4 marker trong file |
| `Part_Icles/init`, `Shared/TimeManager/Part_Icles/init` | 6 | compiler pin inline `local function doEmit()` tại call-site trực tiếp (Roblox không, vì closure có CAPTURE ref) |
| `Shared/Network/BufferEncoder/Write` | 0 | proto 9 lệnh; compiler pin inline wrapper `writeu8` → ghép nhầm sibling (header khác) |
| `ClientFishingHandler.client` | 21 | de-inline sinh 2 closure helper (`21.0`, `21.1`) — mọi `FISHING_ACTIVE_CHANGED:FireSelf` vẫn có trong output |
| `ClientGlobalAnnouncement (…).client` | 2.2 | de-inline `escape` (`gsub` ×3) thành helper; 21 `gsub` vẫn có trong output |

Sai khác hệ thống giữa compiler Roblox và compiler pin đã phải chuẩn hoá (không phải lỗi
decompiler): gộp chuỗi import `script.Parent.X`; fold `Vector3.new(k,k,k)`; `DUPTABLE`
template có hằng (tag 8) và field `nil`; cắt multret ở builtin arity cố định
(`math.clamp(a, b, f())`); suy ra số kết quả / inline hàm trong bảng local không đổi
(`v.IsAlive(p)`, `v3.get(t, k)`); fold upvalue hằng làm key (`t[KEY]`→`t.Key`);
`FASTCALL3` (mới) vs `FASTCALL`.

## Lỗi thật đã tìm thấy và sửa nhờ oracle

- **SETLIST multret bị cắt** (`ast/src/set_list.rs`, `cfg/src/ssa/inline.rs`):
  `{a, b, f()}` mà `NEWTABLE` cách xa `SETLIST` (phần tử cần temporaries) được hạ thành
  `t[1], t[2], t[3] = a, b, f()` → chỉ giữ 1 giá trị của `f()`. Sửa: (1) kéo
  `local t = {}` xuống sát `SETLIST` khi không có tham chiếu `t` ở giữa và entry đã có
  đều thuần → fold thành constructor; (2) fallback giữ ngữ nghĩa
  `t[1], t[2] = a, b; for _k, _v in next, { f() } do t[2 + _k] = _v end`.

## Phát hiện phụ (không lệch ngữ nghĩa, ghi vào roadmap)

- Decompiler bỏ hẳn `local t = {...}` không dùng, kể cả bảng chứa closure
  (`FishingRankBanner/init`: mất 3 hàm `Formatter`) — mất thông tin nguồn.
- Nhân bản shared-tail (`ClickToMoveController` `return if humanoid == nil …` ×2) và
  compiler pin inline `local function` tại nhiều site → phình dòng (mục C/D/E).
- Ground truth 274 cặp (`BytecodeTest`/`RealSourceTest`) không còn trên đĩa; chế độ
  `--sources` sẵn sàng khi khôi phục (đã chạy trên 11 fixture `semantic_roundtrip`).

## Lệnh

```powershell
# corpus, ghi báo cáo + baseline gọn
python scripts/bytecode_roundtrip.py --lifter target/release/luau-lifter.exe `
  --compiler D:/Medal/luau-tools-src/build/luau-compile.exe `
  --corpus D:/Medal/examplebytecode/RobloxProject --key 203 --threads 8 `
  --report out/corpus.json --markdown out/corpus.md --write-baseline docs/bytecode_roundtrip/baseline_corpus.json

# gate: không tăng proto không tương đương so với baseline
python scripts/bytecode_roundtrip.py ... --baseline docs/bytecode_roundtrip/baseline_corpus.json

# phân lớp lại từ báo cáo cũ (không decompile/recompile)
python scripts/bytecode_roundtrip.py --lifter x --compiler y --corpus z --reclassify out/corpus.json --markdown out/corpus.md
```
