# Partial array keys và native SETLIST

Ngày: 27/10/2026, UTC+7. Baseline: `ac9461e`.

## Lỗi và sửa

Lượt fuzz hoàn chỉnh 15.000 seeds × 9 profiles trên baseline đạt 133.903 passed, 1.094 invalid và 3 failed: seed 8472 ở O0/g0–g2 bị từ chối vì constructor còn `[2]` trước SETLIST. Không có runtime mismatch trong lượt đó. Chỉ cho phép padding constructor chưa đủ: kiểm tra thêm store vào slot 8/9 phát hiện fallback gán từng phần tử có thể tăng mảng khác SETLIST của VM, đổi `#t` và thứ tự `next`, kể cả ở O1/O2.

Shared capacity predicate cho phép giữ array key không tuần tự dưới dạng explicit key khi còn hash headroom. Reservation kiểm tra cả chuỗi trước khi sửa; nếu không thể chuyển thành list tuần tự, đặt nil reservation trước các keyed stores và giữ nguyên chỉ số/thứ tự của chúng. Thiếu hash capacity vẫn từ chối.

Khi chuỗi constructor kết thúc bằng SETLIST, một store lặp vào key nil được append nguyên vị trí nếu capacity proof cho phép, thay vì thay store cũ hoặc dừng fold. Điều này giữ lịch sử collision và cho phép khôi phục native list constructor. Không nới allocation gate cuối, không thay chính sách empty hash hay bỏ capture/evaluation-order checks.

Fixture `table_partial_array_keys` bao phủ cả hai nhánh, duplicate nil keys, varargs có trailing nil, signed zero, store slot 8/9, length và `next`. Unit test thêm reservation không tuần tự, tính idempotent và refusal không sửa dở table.

## Kiểm chứng

- Workspace: 1.690 passed, 2 ignored; semantic: 792/792; deep review: 457/457; đủ 33 audit steps.
- Seed 8472 chạy lại đủ 9 profiles đạt; 18/18 growth probes đạt và đều dùng native constructor.
- Chạy lại đủ 1.476 table probes: tất cả đạt, gồm capacity, collision, numeric keys, array padding và empty-table controls.
- Đọc diff 789 semantic outputs cũ: 5 file đổi, tổng +1 dòng/+1 nonblank line, không mất rebuilt call. 513 public và 7 residual outputs giống từng byte; public giữ 72 call annotations.
- Ba output mới đều 39 dòng/519 bytes theo size gate. Size baseline chỉ đổi 5 file đã review và thêm 3 file mới; lần gate đầu thiếu ba baseline mới được giữ trong `initial-12.log`.
- Oracle: 264 inputs/1.633 prototypes, exact 1.174, equiv 142, differ 261, missing 40, extra 16. Nonequiv 318 → 317 nhờ `table_duplicate_nil_capacity`; fixture mới có 0 nonequiv. Không có per-file hoặc capture regression. Baselines được cập nhật sau review, rồi chạy lại size/oracle.

Đây chưa phải nghiệm thu cuối V2.7. Bốn semantic outputs còn dùng open-tail SETLIST fallback (ba `table_array_dynamic_key`, một `reserved_list_slots_keep_keyed_entries` ở O2); public không có output nào dùng nó. Một probe khóa động riêng đã xác nhận fallback vẫn có thể đổi length 6 → 8 và thứ tự `next`. Phần này đang được xử lý tiếp, không được xem chính sách empty hash là ngoại lệ cho nó. Broad fuzz trên binary mới và mục tiêu hiệu năng vẫn mở; private dumps/review seeds gốc không có.

Evidence: `/workspace/work/v27/audit-17`, `fuzz-v27-acceptance.json`, `fuzz-native-8472.json`, `table-nonsequence/growth-native-results.json`, `dynamic-setlist-growth/results4.json`, `table-native-*-comparison.json`. Binary `luau-lifter-table-native-list` có SHA256 `290dbe8742d5620dddec6ed028855906f2bdc9f69836e69e44500958df580b9e`. Lineage JSON được nén lossless bằng Python với SHA256 kiểm tra từng file, manifest `compressed-lineage-evidence-17.json`.
