# Vector constructor: ledger khớp binding cuối

Ngày: 27/10/2026, UTC+7. Baseline `6420b4d`.

Một vector constant duy nhất ở O2 tạo constructor snapshot và ghi ledger, rồi `fold_import_callees` xóa binding đó. Sidecar vẫn trỏ tới binding không còn trong final AST; validator cũng chưa nhận producer `materialize_vectors`. Reproducer `return vector.create(1, 2, 3)` cho lỗi `unknown or repeated pass` và `recorded count differs`.

Import cleanup nay chạy trước các late local producers. Constructor snapshot đã ghi nhận được giữ tới emission. Metadata analysis có báo cáo vector spelling cùng ledger; validator nhận chính xác hai model: entry snapshot với role `vector_constructor`, và inline constructor không được có binding introduction. Các kiểm tra lineage, nguồn identity, số record và bidirectional links giữ nguyên.

Regression Rust dùng bytecode pin cho cả single constant và nested vector; kiểm tra source giống nhau khi bật/tắt provenance, mọi record trỏ tới final binding, và local mới không có input ancestry giả. Fixture source tái biên dịch ra bytecode giống từng byte. Python thêm control phân biệt snapshot/inline model. Semantic fixture mới in vector để kiểm tra giá trị runtime.

- Workspace: 1.695 passed, 2 ignored; semantic 798/798; deep review 469/469; đủ 33 Fixture and Luau audit steps (`audit-22`).
- 795 semantic outputs cũ, 513 public và 7 residual giống từng byte; 885/72/38 call annotations tương ứng giữ nguyên.
- Fixture mới O0/O1 có một dòng, O2 có hai dòng do giữ constructor binding. Size baseline chỉ thêm ba outputs; oracle chỉ thêm fixture một prototype với một nonequiv. Mọi per-file status/nonequiv/proto count cũ giữ nguyên, không có capture regression. Aggregate 266 inputs/1.650 prototypes, exact 1.183, equiv 143, differ 268, missing 40, extra 16. Không coi oracle nonequiv là runtime mismatch: VM và semantic controls đều đạt.
- Probe metadata O2 ban đầu đã chạy lại: cả analysis và provenance hợp lệ, source byte-identical giữa hai mode. Tám Python ledger tests và bốn vector integration tests đạt. Size/oracle được chạy lại sau review và cập nhật baseline; initial logs được giữ.

Binary `luau-lifter-vector-ledger`, SHA256 `fe266a6b8af4ba0cb37f27019024a0436cf247b33c5ec9877a7d3f11313ebf02`. Evidence: `/workspace/work/v27/audit-22`, `vector-ledger-verified`, `vector-ledger-*-comparison.json`; probe trước sửa ở `vector-ledger-probe-O2`. Archived lineage/cache/profile JSON được nén lossless bằng Python với SHA256 đối chiếu; source và binary baseline vẫn giữ.

Đây là checkpoint correctness/metadata, chưa phải hoàn tất V2.7. Broad fuzz cuối, benchmark G1 và đặc tả case (c) còn mở. Không có private dumps/review seeds gốc; không release, làm web changelog hoặc merge main.
