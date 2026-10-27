# Naming, private placeholders và bảng lớn

Ngày: 27/10/2026, UTC+7. Baseline: `7a8a10e`. Nhánh: `v2.7-dev`.

## Thay đổi

- Bỏ class hint chỉ dựa vào `table.__index = table`, đúng binding decision. Các signal metatable/colon và evidence mạnh hơn vẫn có hiệu lực.
- Dùng chung bảng 15 irregular plurals hai chiều cho collection/iterator names: children, people, indices, vertices, matrices, analyses, crises, theses, diagnoses, hypotheses, parentheses, feet, teeth, mice, geese. `axes`/`bases` vẫn bị từ chối vì mơ hồ; không đoán suffix của compound words.
- Placeholder `0` thành `nil` chỉ ở private, uncaptured DUPTABLE có toàn record keys và tối đa 32 fields. Proof phải tìm được store đầu vào đúng key trên mọi đường đi, trước mọi read/escape/rebind hoặc store có thể thêm key gây rehash. Không đi qua loop/return/control transfer; giới hạn 256 statements, depth 16. Không có proof thì giữ `0`.
- Bảng lớn có allocation padding riêng tối đa 4.096 entries. Giới hạn SETLIST gap 255 vẫn giữ nguyên. Hai reproducer cần 300 array slots và/hoặc 256 hash padding entries trước đây bị từ chối ở O1/O2; giờ có output đúng. Không thay layout proof hay chính sách empty hash đã chốt.

## Kiểm chứng

- Workspace: 1.682 passed, 2 ignored, 56 suite groups.
- Fixture and Luau audit: đủ 33 bước, semantic 780/780, deep review 433/433. Hai baseline gates được chạy lại sau khi đọc toàn bộ output delta và ghi baseline mới.
- Probes riêng: 15/15 chạy đúng; binary trước thay đổi đạt 11/15 và từ chối bốn ca bảng lớn O1/O2. Probes còn ghi nhận lỗi naming `Head`/`Tail` để xử lý ở mốc tiếp theo, không coi runtime pass là tên đúng.
- Ba regression sources mới: `naming_irregular_collections`, `table_unobserved_placeholder`, `table_large_allocation`. Placeholder fixture phân biệt table riêng với table đã bị đọc, capture, có key mới và branch chưa chắc store.

## Output và oracle

Đã đọc toàn bộ diff: 4/771 semantic outputs cũ đổi `0` thành `nil`; 9/513 public outputs đổi tên class/iterator và suffix liên quan. Bảy residual outputs giữ nguyên. Số dòng, dòng không trống và rebuilt calls giữ nguyên **ở từng file cũ**, không chỉ tổng.

| Suite cũ | Dòng trước/sau | Không trống trước/sau | Rebuilt calls trước/sau |
|---|---:|---:|---:|
| Semantic 771 | 43.133 / 43.133 | 36.891 / 36.891 | 857 / 857 |
| Public 513 | 40.608 / 40.608 | 31.990 / 31.990 | 72 / 72 |
| Residual 7 | 3.826 / 3.826 | 3.070 / 3.070 | 38 / 38 |

Bounded bytecode oracle: 260 inputs, 1.618 prototypes; exact 1.167, equivalent 140, differ 255, missing 40, extra 16; nonequiv 311. Trong 257 sources cũ, nonequiv 306→308 chỉ tại `template_repeated_key`: compiler bỏ các LOADK 0/SETTABLEKS cho placeholder không thể quan sát trước overwrite. Đã đọc instruction delta và output; runtime O0/O1/O2 đạt. Ba sources mới đóng góp 0/1/2 nonequiv. Capture-excess flags bằng 0. Không coi bounded oracle là chứng minh runtime toàn diện.

## Số đo public

Release A/B, cùng Luau pin/toolchain/flags, CPU affinity 1/4 cores; một warmup và bảy samples mỗi binary, xen kẽ thứ tự. Không instrument; 513 input, warm filesystem, có ghi output. Mỗi lần chạy lại của mỗi binary có cùng output hash.

| Workers | Baseline median | Candidate median | Thay đổi | Peak RSS median KiB trước/sau |
|---|---:|---:|---:|---:|
| 1 | 0,7650 s | 0,7538 s | −1,5% | 32.832 / 32.808 |
| 4 | 0,2218 s | 0,2265 s | +2,1% | 53.480 / 55.636 |

Đây là chi phí đo được của checkpoint trên cloud host; không kết luận speedup hay đạt mục tiêu V2.5.1 từ các chênh lệch nhỏ này.

## Evidence và phần tiếp tục

Evidence giữ tại `/workspace/work/v27`: `audit-10`, `naming-placeholder-{semantic,public,residual}-comparison.json`, các diff tương ứng, `naming-placeholder-oracle.json`, `naming-placeholder-{release,baseline}-probes`, `naming-placeholder-measure/report.json`, workspace/build logs. Lineage JSON lớn được nén lossless bằng Python, xác minh SHA256 trước khi bỏ bản chưa nén; manifest giữ từng đường dẫn/hash.

[Audit M4/M6](audit_existing_m4_m6.md) đối chiếu các phần đã có, tránh viết lại engine. N1/N3 vẫn mở vì local nhận `Head` hoặc `Tail` đang lấy tên của nhánh đầu. L3, hiệu năng và nghiệm thu cuối tiếp tục. Không có frozen private dumps, original generic-for seeds hoặc đặc tả “case (c)”; chưa tuyên bố nghiệm thu các dữ liệu đó. Không release hoặc làm website.
