# Tiến độ sửa output V2

**Đã hoàn thành F1–F8 ngày 19/09/2026:** [ROADMAP_V2_FIX.md](ROADMAP_V2_FIX.md) đã tick đủ, kiểm chứng từng phần và đồng bộ output/HTML với executable cuối. Theo yêu cầu người dùng, commit và push từng phần đã kiểm tra lên `roadmap-v2`. R9 dừng, AI tắt mặc định; model và corpus/output private không được đưa vào commit.

| Mục | Trạng thái |
|---|---|
| F1 import → field và output polish | Xong; commit `c08a908`, đã push |
| F8 nền và bàn giao cuối | Xong; nền `f1059d9`, gate cuối 246 runtime/513 public/3.978 private qua; folder V2, HTML và 25 trang đồng bộ; beta giữ nguyên |
| F2 biểu thức toán/đối số | Xong phạm vi sửa; bước cuối `a2048e5` đã push, nghiệm thu đủ năm nhóm; proof hẹp, snapshot chưa đủ proof vẫn giữ với tên rõ hơn khi có ngữ cảnh |
| F3 helper điều kiện | Xong; `401c393` đã push và nghiệm thu; return cuối nhánh, chuỗi scalar ngắn, giữ arity và binding được bảo vệ |
| F4 constructor trước capture | Xong, commit `e1f1131` đã push; gom init trước lần quan sát đầu tiên |
| F5 tên suy luận | Xong; commit `b66a186` đã push, giữ role/confidence, tên đa kiểu và số lần lặp chuỗi |
| F6 helper/scope/pass cuối | Xong, commit `65dc97f` đã push; placement có proof trong constructor đang trộn callback inline và helper riêng |
| F7 annotation/discard | Xong, commit `f5fb317` đã push; bounded discard effects, kiểm compact/fallback/mapping đầy đủ |

Các mục dưới ghi lại từng lần nghiệm thu. Những câu “còn mở/chưa đồng bộ” mô tả thời điểm của mục đó; trạng thái cuối nằm ở bảng trên và mục **F8 bàn giao** cuối tài liệu.

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

## F6 — vị trí helper, 19/09/2026

`MEDAL_DUMP_AST_STAGES=all` (hoặc danh sách tên stage) ghi 11 snapshot AST lên stderr mà không đổi stdout. Trace createSignal/prettyPrint/castToGraph xác nhận các alias đã có từ trước cleanup cuối, không phải được tái tạo ở một pass muộn. Do đó không thêm lượt fixed point chung. Trong createSignal, `fire` bị giữ vì policy tên Function chưa nhận ra field constructor cùng tên; một kiểm tra boolean cũ còn chặn hai capture-read dù bộ kiểm thứ tự chính xác đã cho phép.

Ngoại lệ mới chỉ dùng ở AST đã link closure, với bằng chứng Function cùng tên và constructor vốn đã có callback inline. Danh sách helper riêng đồng nhất hoặc một helper duy nhất giữ nguyên để tránh tăng tầng lồng. DebugLocal/DebugUpvalue, mismatch tên, recursion, nhiều lần dùng, loop/conditional use, callback xen giữa và ghi lại dependency vẫn bị chặn. Không thay exception của SSA. Thử nghiệm đầu đã làm O0 lồng sâu hơn; policy cuối được thu hẹp và cả hai O0 đó giữ nguyên hash baseline.

So F3: **3.978 private giữ nguyên hash**, **4 public output đổi**. createSignal O1/O2 tăng raw và normalized ratio **0,4582 → 0,8920**; 403 profile đo được còn lại giữ điểm. createReconciler O1/O2 gom các helper export vào constructor đang có callback inline, đã đọc diff; alignment vẫn unknown. Không tuyên bố F6 cải thiện diện rộng trên private hoặc đã sửa snapshot toán của F2.

Nghiệm thu: **756 AST tests**, **1.045 workspace tests chính + 1 test con**, **228 runtime hiện có + 6 helper profiles**, **513 public** qua. Fixture mới có **66 tình huống/profile** về identity giữa factory calls, chia sẻ closure qua loop, cell riêng từng iteration, recursion, alias hai field, false/nil/tuple, callback, key lỗi và lỗi theo event. Cả ba g1 đổi output so F3; ba g2 giữ source binding. Lineage/emission/capture audits và deterministic thread 1/4 qua, không local token chưa giải thích được. Manifest mặc định đã thêm helper, lên **234 profiles**. [Bằng chứng](roadmap_v2_acceptance/fix_helper_placement.json).

Output F6: `out/v2-fix-all/f6-final`, SHA `3e567395e15e542e9c748d9db7739ed43eecb5d20fe0c9a8ed4df683d1e13936`. F2/F7 và bàn giao F8 còn mở; V2/HTML chính vẫn ở F1 đến lượt đồng bộ cuối.

## F7 — annotation và discard, 19/09/2026

Cleanup dùng bounded effect summary thay cho kiểm purity cũ: so sánh một giá trị với primitive literal không gọi `__eq` nên có thể bỏ khi kết quả không được đọc. Dynamic equality, field/global lookup, phép toán chưa chứng minh và lời gọi vẫn giữ; literal pi/infinite/vector phát qua lookup môi trường cũng không bị coi là hằng thuần. Closure/constructor có giá trị cấu trúc vẫn được bảo vệ. Không dùng tên API hay annotation làm proof và không di chuyển biểu thức qua callback.

So F6: **7 private file đổi**, bỏ **7 discard**, giảm **8 dòng, 189 byte**; binding p/v và dòng dài không tăng. Các trường hợp cụ thể là so sánh trạng thái đã không còn nhánh sử dụng trong BaseFishingRod/CameraModule và so sánh goal với 1 trong quest registry. Đã đọc toàn bộ 7 diff, compile O0/O2; **513 public output giữ nguyên hash**.

Compact mode vốn có được kiểm đầy đủ, không ghi nhận như tính năng vừa tạo. Trên **232 private file có annotation**, chế độ ngắn rút **849 comment**, giảm **39.064 byte**, giữ nguyên AST/tên/type và đầy đủ text trong sidecar; cả **531 marker call suy luận** vẫn hiện diện. Scanner nay đếm cả nhãn ngắn để việc đổi presentation không bị báo nhầm là đã giảm suy luận. Test formatter kiểm unknown comment, Unicode, nội dung quá dài, không có metadata và map hết budget: những trường hợp thiếu chỗ giữ toàn bộ text đều dùng comment đầy đủ. CLI vẫn yêu cầu `--emit-binding-provenance --compact-annotations`; output mặc định vẫn giữ diagnostic đầy đủ.

Nghiệm thu: **758 AST tests**, **1.047 workspace tests chính + 1 test con**, **126 Python tests**, **234 runtime hiện có + 6 discard profiles**, **513 public** qua. Fixture mới chạy **88 tình huống/profile** với nil/false/string/number/NaN, cùng/khác table, `__eq`, `__index`, `__add`, callback, tuple và lỗi theo event; cả sáu profile thay output và qua VM. Acyclic use-def fingerprint vẫn báo **different** ở fixture này cả trước và sau sửa, nên không coi đó là proof tương đương; runtime hữu hạn là bằng chứng riêng. Lineage/emission/capture, compact AST/span/full-text mapping và thread/cache checks đều qua, không token local chưa giải thích được. Manifest mặc định thêm discard, lên **240 profiles**. [Bằng chứng](roadmap_v2_acceptance/fix_annotation_discard.json).

Output F7: `out/v2-fix-all/f7`, SHA `5d7b18d87a273dc07b2ba5517f8bcd10976f6e52ae7dbdd2e5bd1b7cebe63608`. Chỉ còn F2 và bàn giao F8; V2/HTML chính chưa đồng bộ bản này.

## F2 bước cuối — snapshot hoàn tất và tên biểu thức, 19/09/2026

Một local chỉ ghi một lần từ `#input` có giá trị số sau khi LEN hoàn tất: VM Luau đã pin kiểm cả return của `__len`, sai kiểu thì raise. Proof mới chỉ dùng kết quả đã lưu đó; không cho rằng việc chạy `#input` thuần hoặc được di chuyển. Các phép `//`, `%`, `^` trên toán hạng đã chứng minh là số dùng arithmetic của VM; không lấy annotation, tên math/buffer hoặc giả định môi trường làm proof. Bộ test gồm length callback, return sai kiểu, NaN/infinity/signed zero, số 0 ở mẫu và cell bị setter ghi lại.

Callee đệ quy được xem là ổn định khi có đúng cặp liền nhau `local f` rồi `f = function...`, tổng cộng đúng hai lần ghi và không có goto/label. Tạo closure không chạy thân hàm nên không có quan sát giữa hai bước cài đặt này. Cặp cách nhau bởi statement, init trong nhánh, gán lại hoặc setter trong closure vẫn bị từ chối. Nhờ đó alias callee không cần giữ chỉ vì cell đã có predeclaration nil.

Sau mọi phép biến đổi biểu thức, graph tên bổ sung hint yếu cho `(a+b)/2` và `quantity/2`: `midpoint`, `midpointX/Y/Z`, `halfExtentsSize`, `halfSegCount`. Hint chỉ áp dụng local ghi một lần, không lấn vai trò mạnh/source binding, không lan truyền thành proof kiểu hoặc purity; collision vẫn được resolver scope xử lý. Đặt tên ở cuối tránh làm temp có thể inline bị giữ lại chỉ vì vừa nhận tên đẹp.

So F7: **26 private file đổi** (24 chỉ tên, 2 cấu trúc), **giảm 36 binding p/v, 3 dòng**, tăng 597 byte do tên rõ hơn; không file nào tăng p/v, dòng hay dòng dài. Đã đọc 25 cặp diff nội dung khác nhau. Geometry có halfExtentsSize và midpointX/Y/Z; Timer có midpoint; LightningCore có halfSegCount và vẫn giữ công thức đã gộp ở bước đầu. Write bỏ hai temp `count2 + 1` từ kết quả length, HyperText Util bỏ alias deepCopy.

Public có **2 output đổi**, đều prettyPrint: O1 **0,5933 → 0,5979**, O2 **0,4248 → 0,4281** ở cả raw/normalized ratio; 403 profile đo được còn lại giữ điểm. Các ca này bỏ alias của prettyPrint nhưng vẫn giữ snapshot arithmetic/callee lookup khác. So beta, prettyPrint và Geometry vẫn kém gọn rõ rệt. Việc hoàn tất F2 nghĩa là đã triển khai và kiểm đủ các hướng sửa đã nhận, gồm fallback tên cho snapshot chưa đủ proof; không có nghĩa đã xóa toàn bộ temp hay khôi phục công thức beta.

Nghiệm thu: **761 AST tests**, **1.050 workspace tests chính + 1 test con**, **240 runtime hiện có + 6 completed-snapshot profiles**, **513 public** qua. Fixture mới có **153 tình huống/profile**; ba g1 đổi output, ba g2 giữ hash nguồn. Kiểm recursion, reassign/setter, capture trước init, conditional init, identity/cell trong loop, length/metamethod/error, callback và tuple. Lineage/emission/capture, source spans và deterministic thread 1/4 qua, không token local chưa giải thích được; symbolic dataflow vẫn unknown. Manifest mặc định lên **246 profiles**. [Bằng chứng và hash VM contract](roadmap_v2_acceptance/fix_completed_snapshots.json).

Output F2 cuối: `out/v2-fix-all/f2-final`, SHA `c78895cd804593dfcf35c7bd711af87bf1ea7a4730898a75b4262dcb822bfa77`. F1–F7 đã chốt; còn đồng bộ output/HTML và kiểm bàn giao F8.

## F8 bàn giao — 19/09/2026

Toàn bộ F1–F8 đã nghiệm thu. Code cuối là `a2048e529502e087aaab713da026e845bf2832d1`, executable SHA-256 `c78895cd804593dfcf35c7bd711af87bf1ea7a4730898a75b4262dcb822bfa77`. Bằng chứng cuối nằm trong `out/v2-fix-all/f8`; [bản nghiệm thu gọn](roadmap_v2_acceptance/fix_final_delivery.json) lưu hash source-state, report, executable, 25 trang đối chiếu và các lần nghiệm thu trước, không chứa private source/output.

Đã cập nhật `D:/Medal/V2-vs-beta-v0.9-20260912/index.html`, `BAO_CAO.md`, folder `V2`, diff, CSV và 25 trang đọc song song. So lần bàn giao F1, **1.348 private, 29 public và 1 runtime output đổi**. Đã đọc lại 25 ví dụ ưu tiên: 14 đổi và 11 giữ hash; nhận xét theo output cuối, kể cả điểm còn kém. Mỗi folder beta/V2 có cùng **4.689 output**: 3.978 private, 513 public, 198 runtime chung. 48 runtime mới được kiểm riêng trong bộ 246, không thêm lệch vào một bên so sánh.

| Chỉ số bản cuối | Beta | V2 |
|---|---:|---:|
| Private parse/compile | 3.975/3.978 | 3.978/3.978 |
| Tổng dòng private | 493.350 | 508.870 |
| Tổng byte private, LF | 12.951.033 | 13.622.799 |
| Dòng dài trên 180 ký tự | 382 | 98 |
| Binding p/v, cùng 3.975 file parse được | 54.058 | 36.826 |
| Public raw ratio trung bình, 405 profile chung | 0,8265 | 0,8660 |
| Public normalized ratio trung bình | 0,8260 | 0,8648 |
| Runtime của 198 profile chung | 138 qua, 60 khác hành vi | 198 qua |

Private có 3.047 file đổi và 931 giữ hash so beta. Public raw ratio: **217 tăng / 59 giảm / 129 bằng**; normalized: **215 tăng / 61 giảm / 129 bằng**; **108 profile alignment unknown** vẫn được giữ trong report. Không dùng điểm trung bình để che các ca giảm. F2 cải thiện độ gọn còn nhỏ; Geometry/prettyPrint vẫn dài hơn beta vì còn snapshot lookup/metamethod chưa đủ proof. Tên suy luận rõ hơn không được xem là khôi phục đúng tên tác giả.

Gate cuối chạy toàn bộ **246/246 runtime** bằng executable cuối; **513/513 public** và **3.978/3.978 private** parse/compile qua, có tái sử dụng artifact đã nghiệm thu khi hash trùng. Lineage/emission/capture và deterministic thread 1/4 qua; runtime compact mode giữ AST/span/full-text mapping. Test source gần nhất: **761 AST** trong **1.050 test workspace chính + 1 test con**; **126 Python tests** ở F7, source Python giữ nguyên sau đó. Corpus vẫn **0 relay require→field** đã nhận diện. Đây là kiểm thử hữu hạn, không phải chạy toàn bộ private game trong Roblox.

Bàn giao đã xác minh **9.378 output**, hash 171 source public, 166 link HTML tĩnh và 12.558 link động. Browser kiểm filter đường dẫn Unicode, sort, phân trang, 25 trang source comparison; không lỗi JavaScript. Beta executable và toàn bộ inventory giữ nguyên, digest inventory `fe7c6061350aa867293d408fd4dd7f3d1dbfc17589ffd08e838a1fe8fc974ec8`. Snapshot thống kê lần bàn giao trước lấy đúng từ backup F1, tránh dùng dictionary đã bị refresh thay đổi.

Output chính vẫn dùng diagnostic đầy đủ; lợi ích 39.064 byte của compact mode thuộc phép đo opt-in F7, không cộng vào bản mặc định. R9 tiếp tục dừng, AI tắt mặc định, không tải model và không đưa model/private source/output lên GitHub. R7 performance vẫn để sau theo yêu cầu.
