# Ví dụ từng bug: code gốc → code decompile → sai ở đâu

Mỗi mục: **code gốc** (compile bằng `luau-compile -O…`), **code decompiler sinh ra** (chạy thật, copy nguyên văn),
**output** thực tế của hai bên, và **chỉ ra dòng sai**. Mọi cặp đều đã chạy lại để xác nhận.
File repro: `_harness/_bugs/<Cx>.luau` (gốc) và `_harness/_bugs/<Cx>.dec.luau` (decompile).

---

## C1 — `not (a < b)` → `a >= b` (sai với NaN) · MEDIUM · `ast/src/unary.rs:88-139` · `-O0`

**Gốc:**
```lua
local n = 0 / 0          -- NaN
print(not (n < 1))       -- not(false) = true
```
**Decompile ra:**
```lua
print(0 / 0 >= 1)
```
**Output:** gốc `true` → decompile `false`.

**❌ Sai ở đâu:** dòng `print(0 / 0 >= 1)`. `not (a < b)` chỉ tương đương `a >= b` khi không có NaN.
Với `n = NaN`: `n < 1` là `false` nên `not(n < 1)` = `true`; nhưng `n >= 1` = `false`. Hàm `Reduce::reduce`
lật cả 4 phép quan hệ dưới `not` (`<,<=,>,>=`) — không an toàn với NaN. (Lật phép `==`/`~=` thì an toàn, giữ nguyên.)

---

## C2 — table: key `[i]=` trộn với positional bị đảo · HIGH · `ast/src/rebuild_table_literals.rs:145` · mọi -O

**Gốc:**
```lua
local u = { [1] = 11, [2] = 22, "a", "b" }
print(u[1], u[2], #u)    -- positional ghi đè: a  b  2
```
**Decompile ra:**
```lua
local v = {
	11,
	22,
	"a",
	"b"
}
print(v[1], v[2], #v)
```
**Output:** gốc `a  b  2` → decompile `11  22  4`.

**❌ Sai ở đâu:** literal `{11, 22, "a", "b"}`. Trong table gốc, hai phần tử positional `"a"`,`"b"` được gán vào
ô `[1]`,`[2]` (SETLIST chạy **sau**), ghi đè `[1]=11`,`[2]=22`. Decompiler biến `[1]=`,`[2]=` thành **2 ô
positional đầu tiên**, đẩy `"a"`,`"b"` xuống ô `[3]`,`[4]`. Hệ quả: `u[1]` ra `11` (đúng phải `"a"`) và
`#u` ra `4` (đúng phải `2`). `insert_table_entry` chỉ khử trùng với `initial_len` phần tử đầu, không nhận ra
key `1` của positional trùng key `1` đã fold.

---

## C2b — key không-dương / phân số bị bỏ (ép kiểu `f64 as usize` bão hoà) · HIGH · `ast/src/formatter.rs:1252` · mọi -O

**Gốc:**
```lua
local t = {}
t[0] = "zero"
print(t[0], t[1], #t)    -- zero  nil  0
```
**Decompile ra:**
```lua
local v = { "zero" }
print(v[0], v[1], #v)
```
**Output:** gốc `zero  nil  0` → decompile `nil  zero  1`.

**❌ Sai ở đâu:** literal `{ "zero" }`. `are_table_keys_sequential` kiểm tra key bằng `(x - 1f64) as usize == i`.
Với key `0`: `(0 - 1)` = `-1`, mà `-1.0 as usize` trong Rust **bão hoà về 0** → `0 == 0` → coi như "tuần tự" →
**bỏ key** và đẩy `"zero"` vào ô `[1]`. Kết quả `t[0]` mất (`nil`), `t[1]="zero"`, `#t=1`. Cùng lỗi với key
`[-1]`, `[0.5]`, `[1.5]`… và áp dụng cho **cả literal viết tay**, không chỉ table được dựng lại.
Sửa: so sánh `*x == (i as f64) + 1.0`, không ép qua `usize`.

---

## C3 — gán song song trong loop bị tuần tự hoá sai (lost-copy / swap) · HIGH · `cfg/src/ssa/destruct.rs:171-221` · mọi -O

**Gốc:**
```lua
local x, y = 0, 1
for _ = 1, 6 do
	x, y = y, x + y      -- đồng thời (Fibonacci)
end
print(x, y)              -- 8  13
```
**Decompile ra:**
```lua
local v = 0
local v2 = 1

for _ = 1, 6 do
	v = v2
	v2 = v + v2
end

print(v, v2)
```
**Output:** gốc `8  13` (Fibonacci) → decompile `32  64` (lũy thừa 2).

**❌ Sai ở đâu:** hai dòng `v = v2` rồi `v2 = v + v2`. Gán song song `x, y = y, x + y` phải tính vế phải
**đồng thời** với `x` cũ: `x_mới = y`, `y_mới = x_cũ + y`. Decompiler tách tuần tự nên `v2 = v + v2` dùng `v`
**vừa bị ghi đè** (= `y`), thành `y + y = 2y`. Phải chèn một biến tạm cho `x` cũ. Đây là bài toán parallel-copy
kinh điển; dãy Fibonacci hoá thành lũy thừa 2.

---

## C4 — đọc upvalue (do closure sửa) ra giá trị CŨ · HIGH · `cfg/src/ssa/upvalues.rs:94` · mọi -O

**Gốc:**
```lua
local state = 0
local function step(delta)
	state = state + delta
	return state
end
local trail = {}
for i = 1, 5 do
	trail[#trail + 1] = step(i)
end
print(table.concat(trail, ","), state)
```
**Decompile ra:**
```lua
local v = 0
local function step(p)
	v += p
	return v
end

local v2 = v               -- ❌ snapshot state = 0 TRƯỚC loop
local v3 = {}
for i = 1, 5 do
	v3[#v3 + 1] = step(i)
end
print(table.concat(v3, ","), v2)
```
**Output:** gốc `1,3,6,10,15  15` → decompile `1,3,6,10,15  0`.

**❌ Sai ở đâu:** dòng `local v2 = v` và `print(..., v2)`. `state` được closure `step` bắt **theo tham chiếu** và
sửa mỗi vòng (đúng → 15). Nhưng decompiler chốt một bản chụp `v2 = v` (=0) **trước** loop rồi in `v2` thay vì
`state` hiện tại. Nguyên nhân: guard `if !visited.contains(&successor)` (upvalues.rs:94) không lan trạng thái
"upvalue đang mở" qua **back-edge** của loop (đỉnh loop đã visited), nên các lần ghi `state` trong loop không
được nhìn thấy ở read sau loop. (Chính tác giả đã ghi TODO nghi ngờ đúng chỗ này, dòng 91-93.)

---

## C5 — ngoặc cắt-1-giá-trị `(f())` bị mất ở vị trí return · HIGH · return/multret lifting + `inline_temps.rs` · mọi -O

**Gốc:**
```lua
local function two() return 1, 2 end
print(two())
print((two()))
local ok, a, b = pcall(function() return (two()) end)
print(ok, a, b)
print(select("#", (two())))
```
**Decompile ra:**
```lua
local function two() return 1, 2 end
print(two())
print((two()))
local success, result, v = pcall(function()
	return two()                 -- ❌ mất ngoặc: return (two())
end)
print(success, result, v)
print(select("#", (two())))
```
**Output:** gốc `… true 1 nil …` → decompile `… true 1 2 …`.

**❌ Sai ở đâu:** dòng `return two()`. Trong gốc `return (two())`, ngoặc cắt multret còn **1** giá trị nên hàm chỉ
trả `1` → `a=1, b=nil`. Decompiler bỏ ngoặc → trả cả `1, 2` → `b=2`. Ở vị trí **argument** (`print((two()))`)
thì ngoặc được giữ; lỗi chỉ ở vị trí return/tail. Cùng lỗi với `return (...)`, `return (table.unpack(t))`,
`return (select(…))`, và qua `inline_temps` (`local x = (select(...)); return x` → `return select(...)`).

---

## C6 — local capture từng-vòng bị gộp về biến loop chung · HIGH · `cfg/src/ssa/upvalues.rs:52` · mọi -O

**Gốc:**
```lua
local fns = {}
local i = 1
while i <= 3 do
	local x = i              -- mỗi vòng một cell mới
	fns[i] = function() return x end
	i += 1
end
print(fns[1](), fns[2](), fns[3]())
```
**Decompile ra:**
```lua
local v = 1
local v2 = {}
while v <= 3 do
	v2[v] = function()
		return v             -- ❌ capture thẳng biến loop v, không phải x từng vòng
	end
	v += 1
end
print(v2[1](), v2[2](), v2[3]())
```
**Output:** gốc `1  2  3` → decompile `4  4  4`.

**❌ Sai ở đâu:** `function() return v end`. Mỗi vòng `local x = i` là một biến **mới** được closure bắt; ba closure
phải giữ ba giá trị 1,2,3. Decompiler loại `x` và cho closure capture thẳng biến loop `v`; vì cùng một cell,
sau loop `v = 4` nên cả ba trả `4`. Nguyên nhân: `UpvaluesOpen::new` chỉ theo dõi `Upvalue::Ref`, bỏ thẳng
`Upvalue::Copy(_) => None` (dòng 52) — capture-theo-giá-trị (snapshot) không được coi là cell riêng.

---

## C7 — `if return a() else return b()` gộp thành `and/or` cắt multret · HIGH · `ast/src/conditional_expressions.rs:121` · `-O2`

**Gốc:**
```lua
local function a() return 1, 2, 3 end
local function b() return 9 end
local function choose(flag, ...)
	if flag then
		return a()
	end
	return b()
end
print("tcount:", select("#", choose(true)))   -- 3
```
**Decompile ra:**
```lua
local function choose(p, ...)
	return not p and 9 or a()    -- ❌ ternary and/or cắt a() còn 1 giá trị
end
```
**Output:** gốc `true: 1 2 3 … tcount: 3` → decompile `true: 1 … tcount: 1`.

**❌ Sai ở đâu:** `return not p and 9 or a()`. Gộp diamond `if … return a() else return b()` thành biểu thức
`and/or` làm `a()` (trả `1,2,3`) bị **cắt còn 1 giá trị** (và biểu thức `and/or` cũng không bao giờ trả nhiều
giá trị). `choose(true)` chỉ ra `1`, `select("#")` = 1 thay vì 3. Không được gộp khi nhánh trả multret.

---

## C8 — `for … do break end` → mất NGUYÊN function · HIGH (mất dữ liệu) · `restructure/src/loop.rs:92-93` · `-O0`

**Gốc:**
```lua
local n = 3
for i = 1, n do break end
print("after")
```
**Decompile ra:**
```lua
-- failed to decompile
```
**Output:** gốc `after` → decompile (rỗng).

**❌ Sai ở đâu:** toàn bộ output. Vòng `for` mà thân chỉ có `break` làm restructure truy cập
`then_successors[0]` ngoài biên → panic; `catch_unwind` mỗi-function nuốt panic và thay bằng comment
`-- failed to decompile`, **mất toàn bộ thân function** (kể cả `print("after")`). Cũng xảy ra với mẫu lồng
tự nhiên `while … do for … do break end end`.

---

## C9 — inliner đảo thứ tự side-effect · HIGH · `cfg/src/ssa/inline.rs:222-229` · `-O0`

**Gốc:**
```lua
local function A() log[#log+1] = "A"; return 1 end
local function B(x) log[#log+1] = "B"..tostring(x); return x end
local function go(a)
	local c1 = A()       -- A chạy trước
	local m = B(a)       -- B chạy sau
	a = 0
	return c1 + m + a
end
print(go(7)); print(table.concat(log, ","))
```
**Decompile ra:**
```lua
print((function(p)
	local v2 = b(p)              -- ❌ B chạy TRƯỚC
	return a() + v2 + 0          --    A chạy sau
end)(7))
```
**Output:** gốc `8` / log `A,B7` → decompile `8` / log `B7,A`.

**❌ Sai ở đâu:** `local v2 = b(p)` rồi `return a() + v2`. Decompiler inline `c1 = A()` vào biểu thức `return`,
đẩy lời gọi `A()` **qua mặt** `m = B(a)`, nên `B` chạy trước `A`. Giá trị tổng vẫn `8` nhưng **thứ tự
side-effect đảo** (log `B7,A` thay vì `A,B7`) — quan sát được. Inline chỉ hợp lệ khi không vượt qua statement
có side-effect.

---

## C10 — snapshot upvalue bị xoá, read bị đẩy SAU lệnh sửa · HIGH · `ast/src/inline_temps.rs` · `-O0` và `-O2`

**Gốc:**
```lua
local source = 1
local function test(flag, bump)
	if flag then
		local captured = source   -- chụp source TRƯỚC bump()
		bump()
		return captured
	end
	return -1
end
-- gọi qua dispatch để chặn inline
… callIt(dispatch, true, setter)   -- setter làm source = 99
```
**Decompile ra:**
```lua
run = function(p, callback)
	if not p then return -1 end
	callback()                    -- ❌ chạy trước
	return v                      -- ❌ đọc source SAU khi nó đã = 99
end
```
**Output:** gốc `1` → decompile `99`.

**❌ Sai ở đâu:** `callback(); return v`. `local captured = source` chụp giá trị `source` (=1) **trước** `bump()`.
Decompiler xoá biến tạm `captured` và để `return source` đọc upvalue **sau** `callback()` (đã đặt `source=99`)
→ trả `99` thay vì `1`. Đây là **ngược** với C4 (đọc ra giá trị quá mới). `collect_usage` của inline_temps
theo từng-block nên không gắn cờ `captured` khi closure bắt biến nằm ở scope bao ngoài.

---

## C11 — `if a < b then end` bỏ luôn phép so sánh có thể raise lỗi · MEDIUM · `restructure/src/jump.rs:33-46` · `-O0`

**Gốc:**
```lua
local ok = pcall(function()
	local a, b = {}, {}
	if a < b then end        -- {} < {} → RAISE lỗi runtime
	return "noerror"
end)
print(ok)                    -- false (vì so sánh lỗi)
```
**Decompile ra:**
```lua
print((pcall(function()
	return "noerror"         -- ❌ cả "if a < b then end" biến mất
end)))
```
**Output:** gốc `false` → decompile `true`.

**❌ Sai ở đâu:** thiếu hẳn `if a < b then end`. Decompiler thấy khối `if` rỗng và điều kiện không-side-effect nên
xoá cả khối; nhưng `{} < {}` (so sánh hai bảng không có `__lt`) **ném lỗi runtime**. Bỏ nó làm `pcall` trả `true`
thay vì `false` — mất lỗi. (Nếu toán hạng có side-effect, ví dụ là lời gọi hàm, thì decompiler giữ lại đúng;
chỉ sai với toán hạng "không-side-effect-nhưng-có-thể-lỗi".)

---

## C12 — `break` bị bỏ khi reconstruct loop lồng nhiều break · HIGH · `restructure/src/loop.rs` · `-O0`

**Gốc (rút gọn phần lỗi):**
```lua
for j = 1, 3 do
	for k = 1, 3 do … end
	if i == 2 and j == 2 then
		trace[#trace + 1] = `break-outer-j @ {i},{j}`
		break                          -- thoát vòng j
	end
end
```
**Decompile ra:**
```lua
for i2 = 1, 3 do
	for i3 = 1, 3 do … end
	if i == 2 and i2 == 2 then
		result[#result + 1] = `break-outer-j @ {i},{i2}`
		-- ❌ THIẾU break ở đây
	end
end
```
**Output:** gốc `… break-outer-j @ 2,2 | 3.1.1 …  count 18`
→ decompile `… break-outer-j @ 2,2 | break-j @ 2,3,1 | 3.1.1 …  count 19`.

**❌ Sai ở đâu:** trong `if i == 2 and i2 == 2 then … end` **thiếu `break`**. Vì không break, vòng `j` chạy tiếp tới
`j=3`, sinh thêm dòng `break-j @ 2,3,1` → `count` = 19 thay vì 18. Lỗi xảy ra khi loop lồng ≥3 cấp và vòng
trong có **nhiều đích break khác nhau** cùng với break của vòng giữa — bộ giải đích-break bỏ sót một break.
(Loop lồng đơn giản với một break mỗi vòng vẫn đúng.)

---

## C13 — gán vào local còn sống bị bỏ thành `local _ = expr` (mất luôn lệnh gán) · HIGH · phân loại "kết quả không dùng" trong SSA · bytecode v9 thật

(Từ report của user, **đã xác nhận trên corpus v9 thật** bằng binary mới. Không tái hiện được bằng `luau-compile`
v11 vì trigger là đặc thù shape bytecode v9.)

**Decompile ra (corpus thật — `Client/HangingPlacement.client.luau`):**
```lua
local v22 = nil                                                    -- dòng 45
…
local _ = localPlayer.AncestryChanged:Connect(function(_, parent)  -- dòng 1449  ❌ đáng lẽ:  v22 = …:Connect(…)
	… end)
…
if v22 then            -- dòng 1507  → LUÔN false vì v22 không bao giờ được gán
	v22:Disconnect()   -- dòng 1508  → không bao giờ chạy → handler AncestryChanged không bị gỡ
	v22 = nil
end
```

**❌ Sai ở đâu:** dòng `local _ = localPlayer.AncestryChanged:Connect(...)`. Connection đáng lẽ phải lưu vào
`v22`, nhưng decompiler coi giá trị "không dùng" (vì `v22` chỉ được đọc trong closure/qua upvalue và ở nhánh
cleanup) nên bỏ luôn lệnh gán → `v22` mãi `nil`. Hệ quả: khối `if v22 then v22:Disconnect() end` thành **code
chết**, connection không bao giờ được gỡ (rò rỉ / khác hành vi).

**Mẫu tối giản (theo report):**
```lua
-- GỐC                                  -- SAI (decompile)
local a = nil                           local a = nil
local b = nil                           local b = nil
a = sig1:Connect(function()             a = sig1:Connect(function() a:Disconnect(); b:Disconnect() end)
  a:Disconnect(); b:Disconnect() end)   local _ = sig2:Connect(function() ... end)  -- ❌ b không được gán
b = sig2:Connect(function()             -- b mãi nil → b:Disconnect() lỗi / không chạy
  a:Disconnect(); b:Disconnect() end)
```

**Facet 2 (counter / default bị mất ghi):** `v23 = v23 + 1` ra `local _ = v23 + 1` (tính nhưng không cập nhật
`v23`). Gợi ý trong corpus: `GiftcodeAdminUI.luau` → `if not tonumber(p.totalRequested) then local _ = #v2 + #v3 end`.

**Khác với C4/C6:** C4 = đọc ra giá trị sai, C6 = capture sai — còn C13 là **mất hẳn lệnh ghi**. Là residual của
fix `fix/closure-captured-local` vừa merge (bắt được vài ca, chưa hết). Hướng sửa (theo reporter, hợp lý):
không phát `local _ = expr` khi (1) op cập nhật một local sẵn có, (2) target được closure/upvalue tham chiếu,
hoặc (3) là self-update `x = x ± …`.

---

> Còn 6 lỗi tầng lifter (L1–L6) chủ yếu cần bytecode obfuscate (xem `CODE_REVIEW_REPORT.md` §4): ví dụ
> `LOADKX`/`COVERAGE` rơi vào `unreachable!` → panic, `LOADB` với offset `C>1` nối nhầm block, STRING const
> index 0 underflow. Đó là lỗi đọc/giải mã opcode, không tiện minh hoạ bằng Luau nguồn vì `luau-compile`
> không sinh ra các mẫu đó.
