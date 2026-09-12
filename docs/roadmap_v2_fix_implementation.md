# Tiến độ sửa output V2

Goal đang chạy: hoàn thành [ROADMAP_V2_FIX.md](ROADMAP_V2_FIX.md), kiểm chứng từng phần và đồng bộ output/HTML khi nghiệm thu. Theo yêu cầu người dùng, commit và push từng phần đã kiểm tra lên `roadmap-v2`. R9 dừng, AI tắt mặc định; model và corpus/output private không được đưa vào commit.

| Mục | Trạng thái |
|---|---|
| F1 import → field và output polish | Xong; commit `c08a908`, đã push |
| F8 nền kiểm tra chất lượng | Đã triển khai và nghiệm thu; gate cuối roadmap vẫn còn |
| F2 biểu thức toán/đối số | Đang triển khai proof số từ cấu trúc chạy, thêm counters giải thích việc từ chối inline |
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

Folder `out/v2-fix-all/private` trong lệnh là đích xuất bản sửa kế tiếp, chưa phải output đã nghiệm thu. Khi hoàn tất toàn roadmap, cần chạy gate, đọc các file ưu tiên, cập nhật folder V2/HTML, kiểm hash và browser trước khi đánh dấu F8 hoàn thành.
