# Tiến độ sửa output V2

Goal đang chạy: hoàn thành [ROADMAP_V2_FIX.md](ROADMAP_V2_FIX.md), kiểm chứng từng phần và đồng bộ output/HTML khi nghiệm thu. Theo yêu cầu người dùng, commit và push từng phần đã kiểm tra lên `roadmap-v2`. R9 dừng, AI tắt mặc định; model và corpus/output private không được đưa vào commit.

| Mục | Trạng thái |
|---|---|
| F1 import → field và output polish | Xong; commit `c08a908`, đã push |
| F8 nền kiểm tra chất lượng | Đã triển khai và nghiệm thu, commit `f1059d9` đã push; gate cuối roadmap vẫn còn |
| F2 biểu thức toán/đối số | Bước proof số đã nghiệm thu; các nhóm snapshot còn lại vẫn mở |
| F3 helper điều kiện | Chưa triển khai |
| F4 constructor trước capture | Chưa triển khai |
| F5 tên suy luận | Chưa triển khai |
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
