# V2.8: phạm vi của bộ kiểm chứng bytecode

Goal [public proof recovery](public_proof_recovery.md) giữ nguyên 77 input đã
khóa. Một certificate chỉ chứng minh tương đương trong contract của model đó;
`different` không tự chứng minh lỗi runtime, còn `unknown` không phải pass.
Checker không thay đổi source hay loại input khó khỏi mẫu để tăng tỷ lệ proof.

## Các certificate

| Model | Phạm vi và guard chính |
|---|---|
| `luau-acyclic-use-def-v3` | Symbolic execution có budget; giữ thứ tự effects, branch polarity, result packs, closure identity và cả native/fallback của FASTCALL. |
| `luau-register-cfg-bisimulation-v2` | Giữ opcode, CFG và storage khi có external upvalue, REF hoặc open pack; mismatch vẫn là `unknown`. |
| `luau-private-storage-cfg-v1` | Chỉ bỏ adjacent lookup/MOVE khi nguồn đã chết và có một predecessor; liveness có budget giữ join, backedge và lifetime. |
| `luau-guarded-reference-context-v1` | Giữ nguyên storage của mọi REF owner; từ chối owner có open pack, kiểm tra cell sống dưới frame của các lời gọi và từ chối FASTCALL khi REF đang mở. |
| `luau-private-adjacent-pack-cfg-v1` | Chỉ xử lý producer/consumer kề nhau, không REF/external main upvalue; giữ prefix, arity, opcode và effects. |
| `luau-private-closed-read-cfg-v1` | Trong domain main0/noREF, bỏ một truthy branch chỉ để đọc lại đúng cùng upvalue vào cùng destination; topology và predecessors phải khớp mẫu có budget. |

Graph fallback chỉ chạy sau symbolic `unknown`; không ghi đè symbolic
`different`. Mọi model từ chối khi vượt budget hoặc gặp transition chưa hỗ trợ.
Không suy luận hai builtin khác nhau hay một FASTCALL và ordinary CALL là
tương đương chỉ vì source trông giống nhau.

Forwarded captures, tái sử dụng GETUPVAL, gộp hai truthy arms giống nhau và
private-table normalization cần whole-chunk proof: main không có external
upvalue và không có CAPTURE REF. Table normalization chỉ áp dụng trong một
đoạn kết thúc bằng RETURN; giữ thứ tự template keys, capacity, nil/dead-key
history và mọi store đã escape. Chỉ store dư thừa của cùng một giá trị scalar
khác nil vào slot private đã biết mới được bỏ.

## Các lỗi checker được khóa bằng controls

- NEWCLOSURE ghi destination trước khi đọc self-capture; DUP native sharing
  không được suy diễn từ cùng một thân hàm.
- External main upvalue có thể alias register; symbolic model từ chối domain
  này và exact-storage fallback giữ nguyên vị trí.
- Open result pack không định nghĩa mọi register phía trên nó. Fixed read,
  fixed write vào unresolved tail và fixed GETVARARGS làm mất pending top đều
  bị từ chối.
- Callback qua metamethod có thể ghi đè register hoặc đổi frame top. Pending
  top giữ guard cho những opcode đó; NAMECALL có ngoại lệ riêng khi lookup
  khôi phục dynamic top và receiver/destination đều nằm dưới pending base.
- FASTCALL3 có scratch writes ở pending top; model từ chối trường hợp này.
- Vararg function phải có đúng PREPVARARGS tại entry, đúng parameter count,
  không có đường quay lại entry. Guard áp dụng cả graph fallback.
- ORK dùng register bên trái và constant bên phải; chỉ SUBRK/DIVRK đảo vị trí.

Native counterexamples xác nhận các cặp từng bị chứng minh nhầm có thể in
`7`/`8`, `9`/`7`, `1,7,3`/`1,8,3`, hoặc trả về chính closure thay vì số cũ.
Tests giữ cả positive controls và mutants; sửa guard không đồng nghĩa bỏ toàn
bộ hỗ trợ open packs hay captures.

## Đọc kết quả và tái kiểm

`scripts/dataflow_replay.py` xác nhận inventory, source hashes, compiler
hash/flags, input bytes biên dịch mới và cả input bị typed refusal. Tùy chọn
`--proof-gaps docs/failure_fixtures/public_proof_gaps.json` ghi đủ 77 identities,
initial/current status, output changes và residuals. Manifest giữ tổng số
profiles và phân bố 20 `different` / 57 `unknown`; thiếu, trùng hoặc đổi input
đều bị từ chối. Production replay vẫn fail nếu mất một proof đã có.

Runtime observations, static certificates, typed refusals và compiler
boundaries được báo cáo riêng. Debug/stack inspection, resource exhaustion
và VM allocation timing nằm ngoài contract static. Đây là kiểm chứng có giới
hạn, không phải theorem cho mọi bytecode hay kết luận runtime cho toàn Roblox
corpus. Evidence và số đo cuối của goal nằm trong báo cáo public proof recovery.
