# Open SETLIST: native growth và giá trị snapshot

Ngày: 27/10/2026, UTC+7. Baseline: `33dd44c`.

## Lỗi và phạm vi sửa

Fallback `table.pack` rồi gán từng slot không giữ bulk resize của SETLIST. Reproducer khóa động có `#t = 6` ở bytecode gốc nhưng `8` ở output, đồng thời đổi thứ tự `next`, tại cả O0/O1/O2. Một ca khác ở O2 còn đọc khóa sau khi closure sửa captured cell, thay vì giá trị trước đó.

- SSA table motion so sánh capture group giữa các phiên bản SSA, không chỉ RcLocal. Khóa động có thể lỗi nil/NaN cũng không được chuyển qua callback.
- Một pass late duy nhất giữ thứ tự đánh giá prefix bằng local snapshot, rồi tạo native list constructor với open tail. Nếu key có thể lỗi, constructor ban đầu giữ điểm phát sinh lỗi; các biểu thức còn lại của prefix phải total-pure để không chạy effect trước lỗi đó. Giá trị/key không literal được chụp một lần, giữ identity và node ancestry của biểu thức.
- Proof chỉ áp dụng cho table riêng, không bị đọc/escape/capture giữa hai điểm. Dùng ConstructorMotion và allocation checks hiện hữu; tối đa 64 regions, 32 prefix fields, cửa sổ 64 statements. Local/register limits cuối pipeline vẫn có hiệu lực. Pass mới ghi explicit local-producer ledger và báo cáo metadata; không sao chép source identity vào local mới.
- Reservation cuối từ chối open SETLIST còn chưa chứng minh được, trước khi sửa constructor. Đây là lựa chọn correctness mặc định trong lúc câu hỏi chính sách còn chưa được trả lời, không phải quyết định đã được chủ repo chọn. Ngoại lệ tương thích empty hash đã chốt vẫn giữ nguyên.

Fixture semantic kiểm tra bulk growth, length/next, varargs, captured value/key, shared table identity, thứ tự callback và key lỗi trước call. Nó thay binding global `table` để bắt phụ thuộc vào fallback pack, rồi khôi phục binding đó. Fixture strict riêng có effect sau khóa động: O0/O1 xuất code và chạy đúng; O2/g0,g2 từ chối có lý do, thay vì xuất sai.

## Kiểm chứng

- Workspace: 1.694 passed, 2 ignored. Semantic: 795/795; deep review: 469/469. Đủ 33 Fixture and Luau audit steps đạt trên binary cuối (`audit-21`, 34 entries gồm public baseline).
- 1.476/1.476 table probes đạt; 45 profiles bổ sung được recompile ở O0 và O1, 90/90 đối chiếu VM đạt, không còn output dùng open-tail pack fallback.
- V9 roadmap: 264/264; `constructor_regions` giữ thứ tự initializer/key/callback và lỗi. V12, V14, compact mode, wasm reader, lineage, cache và binding graph đều đạt trong full audit.
- Metadata validator có test cho report/role của snapshot. Negative control chọn role ngoài tập được phép: các pass có thể chia sẻ role nên lấy bất kỳ role của pass khác không còn là mutation sai. Cả 9 real-source controls đạt; không nới validator để bỏ qua metadata lỗi.
- So với baseline, 792 semantic outputs cũ có 4 file đổi: +15 dòng/+15 nonblank, -79 bytes, 875 call annotations giữ nguyên. 513 public và 7 residual outputs byte-identical; public giữ 72 annotations. Ba output mới 153/153/181 dòng, 2.151/2.155/3.120 bytes.
- Oracle 265 inputs/1.649 prototypes: exact 1.183, equiv 143, differ 267, missing 40, extra 16. Mọi per-file status/nonequiv/proto count cũ giữ nguyên. Aggregate nonequiv 317 → 323 chỉ do 6 của fixture mới; không có capture regression. Baselines cập nhật sau review diff và VM checks, rồi full size/oracle gates đạt.

Các lượt trung gian được giữ để đối chiếu: audit-18 phát hiện fixture tự sửa standard table readonly; audit-19 phát hiện constructor_regions chưa được phục hồi; audit-20 phát hiện thiếu metadata integration và hết dung lượng ở public lineage. Audit-21 ban đầu có negative control dùng role hợp lệ, đã sửa và chạy lại bước 29. Không tính những lượt trung gian là nghiệm thu đạt.

Binary `luau-lifter-table-snapshots-metadata`, SHA256 `fc2e5c3c69297c348c1fc01a6dbb47ba699f2b847d566de2bc08f362fbadc31c`. Evidence tại `/workspace/work/v27`: `audit-21`, `table-snapshots-final-*-comparison.json`, `table-snapshots-probes`, `table-snapshots-strict`, `table-snapshots-{layout,capacity,proofs,replacement,remaining,numeric,arrays,padding_keys,empty}`. Metadata-only rebuild giữ từng byte của 795 semantic outputs. Lineage/cache evidence đã nén lossless bằng Python, có SHA256 và allowlist; không xóa source hoặc baseline binaries.

## Phần tiếp tục

Chưa đóng toàn bộ V2.7: broad fuzz cuối và G1 performance còn mở, private dumps/review seeds gốc không có, case (c) chưa đủ đặc tả. Rà ledger còn phát hiện một lỗi sẵn có ở vector constant O2: report trỏ tới binding mà cleanup đã bỏ và validator chưa nhận producer đó; có reproducer riêng `vector-ledger-probe-O2`, tiếp tục sửa sau checkpoint này. Không release, đổi version, làm website hoặc merge main.
