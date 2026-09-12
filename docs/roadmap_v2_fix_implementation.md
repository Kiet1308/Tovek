# Tiến độ sửa output V2

Goal đang chạy: hoàn thành [ROADMAP_V2_FIX.md](ROADMAP_V2_FIX.md), kiểm chứng từng phần và đồng bộ output/HTML khi nghiệm thu. Theo yêu cầu người dùng, commit và push từng phần đã kiểm tra lên `roadmap-v2`. R9 dừng, AI tắt mặc định; model và corpus/output private không được đưa vào commit.

| Mục | Trạng thái |
|---|---|
| F1 import → field và output polish | Xong; commit `c08a908`, đã push |
| F8 nền kiểm tra chất lượng | Đã triển khai và nghiệm thu, commit `f1059d9` đã push; gate cuối roadmap vẫn còn |
| F2 biểu thức toán/đối số | Bước proof số đã nghiệm thu; các nhóm snapshot còn lại vẫn mở |
| F3 helper điều kiện | Xong và đã nghiệm thu; return cuối nhánh, chuỗi scalar ngắn, giữ arity và binding được bảo vệ |
| F4 constructor trước capture | Xong, commit `e1f1131` đã push; gom init trước lần quan sát đầu tiên |
| F5 tên suy luận | Xong; commit `b66a186` đã push, giữ role/confidence, tên đa kiểu và số lần lặp chuỗi |
| F6 helper/scope/pass cuối | Chưa triển khai |
| F7 annotation/discard | Chưa triển khai |

## F8 nền — 12/09/2026

`scripts/output_quality.py` cung cấp scanner dùng chung, không gắn với đường dẫn máy. Scanner dùng declaration identity của parser để phân biệt tên trùng/shadowing, đếm reference qua closure và tách chỉ số `uncaptured_*`. Các chỉ số `single_use_*` cũ được giữ đúng định nghĩa gồm cả capture để baseline không bị thay đổi ngầm. Ví dụ trên toàn corpus F1: 4.843 Binary tạm tên sinh tự động dùng một lần, nhưng chỉ 4.692 trong số đó không được dùng qua closure.

CLI ghi hash parser/baseline/output, giữ file không parse được trong mẫu số, báo tăng theo từng file kể cả tổng giảm. `--gate-metric` chỉ áp dụng cho chỉ số người chạy chọn, không biến mọi tăng số dòng thành lỗi ngữ nghĩa. Baseline không bị viết lại. Chỉ số có mặt trong mọi row của runtime và public report nhờ tái dùng AST đã parse.

Fixture import đã được đưa vào manifest mặc định: CI hiện tự chạy 204 cấu hình runtime, gồm cả g1/g2 của ca import. Không cần một bước CI riêng để gọi manifest phụ.

Nghiệm thu: **125 test Python**, **204/204 runtime**, **513/513 public**; output của 198 runtime cũ và 513 public giữ nguyên hash bản F1. Lineage/emission/capture audit 204 cấu hình qua, thread 1/4 deterministic. Toàn bộ 3.978 file private đã được scan làm baseline mới. [Bằng chứng](roadmap_v2_acceptance/fix_quality_foundation.json).

Ví dụ chạy tại máy hiện tại:

```powershell
python scripts/output_quality.py --root out/v2-fix-all/private --ast D:/Medal/luau-tools-src/build/luau-ast.exe --baseline out/v2-fix-all/quality-baseline.json --gate-metric sole_use_require_field_relays --report out/v2-fix-all/quality-after.json
```

Folder `out/v2-fix-all/private` hiện chứa bản F2 bước 1 đã nghiệm thu bên dưới. Folder V2/HTML so sánh chính vẫn ở bản F1; khi hoàn tất toàn roadmap cần cập nhật chúng, đọc lại các file ưu tiên, kiểm hash và browser trước khi đánh dấu F8 hoàn thành.

## F2 bước 1 — motion dựa trên giá trị số đã chứng minh

`numeric_facts.rs` lấy bằng chứng từ local chỉ ghi một lần với initializer số và biến đếm của numeric-for đã qua bước kiểm tra khi chạy. Chỉ các phép cộng/trừ/nhân/chia, đổi dấu và so sánh có toán hạng số được xét. Không lấy annotation hoặc tên API làm proof; các literal được formatter phát qua `math.pi`/`math.huge` không phải hằng số môi trường an toàn. Giới hạn độ sâu, số node và số vòng suy luận khiến trường hợp chưa giải được giữ là unknown.

Các gate source binding, ghi lại dependency, capture, vị trí đánh giá, nhánh có thể không chạy và arity vẫn áp dụng. Thêm counters opt-in cho lý do từ chối inline và số ứng viên được nhận nhờ proof số. Counts phản ánh các lần xét trong fixed point, không phải số biến duy nhất.

Kết quả so F1: **13 file private đổi**, giảm **32 dòng, 528 byte, 31 binding p/v**; không phát sinh lỗi parser/compiler. LightningCore giảm 6 dòng, có thể đọc thẳng công thức `list[math.min(i + 1, p)] - list[math.max(i - 1, 0)]`. Public **513 output giữ nguyên hash**; không mô tả bước này là cải thiện toàn bộ toán.

Nghiệm thu: **736 test AST**, **1.025 test workspace chính + 1 lần test con**, **204 runtime hiện có + 6 numeric profiles** qua. Fixture numeric có 210 tổ hợp/profile với counter bị sửa, capture setter, input giả kiểu số, metamethod, global lookup, lỗi ở nhiều vị trí và NaN. Ba profile g1 thay output nên ca runtime thực sự đi qua phép tối ưu; source g2 được bảo vệ. Lineage/emission audits của 204 runtime, 6 numeric và 513 public qua, thread 1/4 deterministic. Fixture numeric được thêm vào manifest mặc định, đưa bộ tiếp theo lên **210 profiles**. [Bằng chứng](roadmap_v2_acceptance/fix_numeric_motion.json).

Profile opt-in trên Geometry, LightningCore, Write và Billboards xác nhận phần lớn ứng viên còn bị chặn ở vị trí đánh giá hoặc bởi statement xen giữa. Ví dụ Geometry ghi nhận 106 lần từ chối vị trí và 146 lần từ chối do statement; LightningCore nhận 4 ứng viên nhờ proof số. Các số này không chứng minh những trường hợp bị từ chối đều có thể rút gọn. F2 còn mở cho các nhóm đã nêu trong roadmap; không đánh dấu xong chỉ bằng 13 file cải thiện.

## F4 — constructor trước capture

Đã tách capture về sau khỏi quan sát trong lúc khởi tạo: một declaration mới, chỉ ghi một lần, chưa bị đọc/capture có thể gom các field liên tiếp vào constructor. Key/value đọc hoặc capture chính bảng, alias, callback xen giữa, reassignment và statement khác vẫn dừng việc gom. Các pass di chuyển constructor qua statement vẫn giữ gate captured cũ. Phân tích có budget và giới hạn độ sâu; không đủ bằng chứng thì giữ nguyên.

Label và nhóm component registry đã có `StyledTextLabel = require(...)` ngay trong bảng. So bước F2: **1.215 file private đổi**, giảm 528 byte, tăng 1.165 dòng chủ yếu do đóng constructor nhiều dòng; số binding sinh tự động và dòng dài không tăng. Đây là cải thiện cách nhóm field, không phải tuyên bố giảm số dòng. Bảng rỗng chỉ có một field chứa callback giữ dạng statement để tránh tăng tầng lồng không có lợi.

Public **513/513** qua; cả **405 profile đo được giữ nguyên điểm cấu trúc**. Năm output thay đổi tại Promise (2) và DataTypeBuffer (3) thuộc nhóm alignment unknown: đã đọc diff, field được gom vào constructor, không lấy chúng làm bằng chứng tăng AST ratio. Bản thử đầu làm Config lồng sâu hơn đã được chỉnh trước nghiệm thu.

Nghiệm thu: **738 test AST**, **1.027 test workspace chính + 1 lần test con**, **210 runtime hiện có + 6 constructor profiles** qua. Fixture mới có **224 tổ hợp/profile**, gồm capture sớm/muộn, quan sát qua alias, self-capture, thay binding, key động/trùng/nil, lỗi theo thứ tự và multret. Cả sáu profile g1/g2 thay output so bản F2, nên VM thực sự kiểm code đã được gom. Symbolic dataflow vẫn unknown. Lineage/emission/capture audits của 210 runtime, 6 constructor và 513 public qua, deterministic thread 1/4, không local token chưa giải thích được. Fixture được thêm vào manifest mặc định, đưa bộ tiếp theo lên **216 profiles**. [Bằng chứng](roadmap_v2_acceptance/fix_constructor_capture.json).

Output F4 và báo cáo chi tiết nằm cục bộ tại `out/v2-fix-all/f4`; folder so sánh chính sẽ đồng bộ khi hoàn tất roadmap.

## F5 — tên suy luận theo vai trò và độ tin cậy

Hint giữ tên, độ tin cậy và loại vai trò qua collection consensus lẫn call-site consensus. Các mô tả kết quả như `serialized`, `formatted`, `frozen` không được pluralize thành danh từ; collection dùng `result` khi là kết quả vòng lặp, hoặc `values` làm gợi ý trung tính. Danh từ yếu như `request` vẫn có thể tạo `requests` nhưng không được tự nâng độ tin cậy để lấn át vai trò có bằng chứng mạnh hơn. Chủ thể getter/factory được phân biệt với tên callee trần; `IsA` và tên child literal vẫn được ưu tiên hơn fallback đó. Pass `refine_names` đã giữ priority giảm dần trên các cạnh lan truyền, không cần đổi cơ chế đó.

Tham số nhận table/string/number không còn bị một nhánh lặp đặt tên thành `items`. `prettyPrint` hiện dùng `value, count: number?`; `count` đến từ số lần lặp chuỗi qua snapshot/default/offset chỉ ghi một lần, không phỏng đoán lại tên nguồn `indentLevel`. Phần tử và phần tử lân cận trong nhánh lặp vẫn có thể tên `item`. Function và callable table có thể cùng giữ vai trò `callback`; nil chỉ thể hiện vai trò tùy chọn. Tên debug/source tiếp tục được bảo vệ.

Vai trò kích thước từ `buffer.create` được truyền ngược qua snapshot chỉ ghi một lần và phép tính với hằng số dương, có giới hạn độ sâu/số vòng. Nhờ đó `expandbuffertosize` trong Write khôi phục tham số `size`; snapshot bị ghi lại hoặc hệ số biến không tạo bằng chứng này. Đây là gợi ý tên, không phải chứng nhận kiểu số hay độ thuần của phép tính.

So F4: **86 file private đổi tên**, **giảm 46 binding p/v**, không file nào tăng binding p/v; cấu trúc, số dòng và dòng dài không đổi, tổng byte tăng 195. Các dạng `serializeds`, `deserializeds`, `formatteds`, `frozens`, `joineds` không còn trong corpus private hiện tại. Public có **17 output thay tên**, toàn bộ **405 profile đo được giữ nguyên điểm cấu trúc**; 3 profile Promise đổi thuộc nhóm alignment unknown đã đọc diff riêng. Ser dùng `result`; Logging dùng `count`; prettyPrint dùng tên đa kiểu đúng vai trò. Đây là cải thiện tên, không tuyên bố đã sửa các snapshot toán/helper còn lại.

Nghiệm thu: **744 test AST**, **1.033 test workspace chính + 1 lần test con**, **222 runtime**, **513 public** qua. Sáu naming profiles nằm trong 222 cấu hình đó, mỗi profile có **42 tổ hợp**; ba profile g1 thay output, ba profile g2 giữ nguyên hash so F4. Các ca tên kết quả width/height và buffer-role giữ nguyên output ở cả 12 cấu hình. Cả 12 output public của Computed/ForKeys/ForPairs/ForValues giữ nguyên hash, bảo vệ `processor`; nhóm ClickToMove giữ nguyên output với `part/parts`; `size` được kiểm trực tiếp trong Write. Unit tests kiểm source/debug, gợi ý mâu thuẫn và snapshot bị ghi lại. Lineage/emission/capture audits và deterministic thread 1/4 đều qua; không local token chưa giải thích được. Symbolic dataflow của fixture mới vẫn unknown. [Bằng chứng](roadmap_v2_acceptance/fix_naming_roles.json).

Output F5 cục bộ: `out/v2-fix-all/f5`, executable SHA `4fb604f528876e2ac0a9e116cda1b29acb45d3db975609fd8aa20fcd7a5c9e8d`. F2/F3/F6/F7 và bước đồng bộ folder V2/HTML tiếp tục mở.

## F3 — return cuối nhánh và guard ngắn

Pass `terminal_returns` chuyển phép ghi cuối nhánh vào biến kết quả suy luận thành return trực tiếp, rồi ghép các guard scalar có giá trị chính xác thành `and/or/not`. Không chuyển statement xen giữa, không đi xuyên loop để đẩy return; các nhánh đã return giữ nguyên arity. Call/method/vararg vốn được gán vào một local được bọc Select khi trả trực tiếp, vì `return (f())` vẫn trả một giá trị còn `return f()` có thể trả nhiều giá trị.

Phải chứng minh có declaration trong cây đang xét; upvalue của hàm ngoài, parameter, binding debug/source và mọi result bị capture đều được giữ. `conditional_result` là vai trò suy luận nên có thể được loại bằng proof terminal này, nhưng không gộp cell đó vào parameter. Chỉ dọn declaration nil vừa mất toàn bộ lần đọc/ghi còn lại; initializer có hiệu ứng không bị bỏ. Phân tích giới hạn độ sâu, số node và số lần viết lại. Node RHS được chuyển giữ ancestry và dấu inline; cú pháp return/wrapper mới không có PC nguồn bịa thêm.

Bản thử đầu tạo một dòng dài ở VRCameraTeleportDetector và làm `_shouldLog` khó đọc hơn. Bản nghiệm thu giới hạn preview khi ghép chuỗi, giữ guard nhiều dòng hoặc quá dài; cặp literal false/nil vẫn dùng nhánh tường minh. Các vị trí đó được kiểm lại, không lấy số dòng giảm làm lý do chấp nhận output khó đọc hơn.

So F5: **79/3.978 file private đổi**, **giảm 259 dòng, 1.702 byte và 16 binding p/v**; không file nào tăng số dòng, binding p/v hay dòng dài. Tất cả file đổi qua parser và compile O0/O2; file không đổi giữ hash baseline. Geometry/HitboxFunctions bỏ local flag trả về, IsEmpty/IsFilled và Registry dùng chain ngắn, Promise bỏ các nhánh true/false dư. Billboards dùng return trực tiếp ở nhánh cuối; các snapshot field/callee vẫn thuộc F2, không tuyên bố đã khôi phục cả công thức.

Public **513/513** qua, **7 output đổi**. Fusion `isSimilar` O0 tăng cả raw/normalized ratio **0,5764 → 0,6089**, so beta là **0,5221**; 404 profile đo được còn lại giữ điểm. Sáu output đổi còn lại thuộc Promise (2), Gamepad (3), TableUtil (1), alignment vẫn unknown và đã đọc diff riêng. Bản `_shouldLog` cuối giữ hash F5. Có báo cáo đối chiếu toàn bộ public với beta tại `out/v2-fix-all/f3/beta-public-review.json`.

Nghiệm thu: **754 test AST**, **1.043 test workspace chính + 1 lần test con**, **222 runtime hiện có + 6 terminal profiles**, **513 public** qua. Fixture mới chạy **200 tình huống/profile**, kiểm false/nil/NaN, tuple/vararg/scalar, branch bị bỏ qua, lỗi ở nhiều event, metamethod, đổi callee trong lookup, parameter và closure quan sát result. Năm profile thay output so F5; O0 g2 giữ nguyên hash. Lineage/emission/capture audits, source/trace và deterministic thread 1/4 đều qua; không local token chưa giải thích được. Symbolic dataflow vẫn unknown. Fixture đã thêm vào manifest mặc định, đưa bộ tiếp theo lên **228 profiles**. [Bằng chứng](roadmap_v2_acceptance/fix_terminal_returns.json).

Output F3 cục bộ: `out/v2-fix-all/f3`, executable SHA `8459de6ce5367646d079868286fa956162020524385d784b0b9570ddc67e9366`. F2/F6/F7 và bước bàn giao V2/HTML tiếp tục mở; folder so sánh chính vẫn ở F1 cho đến lượt đồng bộ cuối.
