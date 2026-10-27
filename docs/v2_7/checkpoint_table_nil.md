# NEWTABLE: giá trị nil tại runtime

Ngày kiểm chứng: 27/10/2026, UTC+7. Baseline: `68aa748`.

## Lỗi và proof

Constructor được bytecode tạo bằng NEWTABLE có thể nhận nil qua parameter, call hoặc metamethod. Formatter cũ chỉ giữ NEWTABLE nếu nhìn thấy literal nil trong expression; một local nhận nil vẫn được in thành record fields và compiler đổi thành DUPTABLE. DUPTABLE đã đặt mọi key vào template trước các store, trong khi một node nhận nil trong chuỗi NEWTABLE stores có thể bị lần chèn colliding key sau tái sử dụng. Vì vậy `pairs`/`next` đổi thứ tự dù key/value cuối giống nhau.

Formatter dùng lại primitive type proof để nhận biết các giá trị chắc chắn khác nil, bổ sung literal/table/closure. Tên builtin, API và type hint không phải proof. Nếu một field trước field cuối có thể nhận nil, chỉ một key được in dạng bracket để compiler giữ NEWTABLE; các field còn lại giữ record spelling. Nếu mọi field trước đó khác nil, field cuối có thể là nil mà không đổi layout, nên không cần bracket. Template gốc không bị đổi cách in.

Fixture `table_runtime_nil_layout.luau` chạy mỗi vị trí nil trong tám fields, đồng thời kiểm tra nil ở field cuối, `next(t, key)` và một store thêm sau constructor. Unit test kiểm tra local/call/metamethod, literal nil, boolean false, toán tử primitive và template không bị can thiệp. VM/loader của compiler pin được đọc để đối chiếu cách chèn node; không giả định nil store không tạo key.

## Kiểm chứng

- Workspace: 1.666 passed, 2 ignored.
- Semantic round-trip: 723/723; deep review: 319/319.
- Fixture and Luau audit: đủ 33 bước passed, gồm Wasm, metadata, lineage và các runtime/determinism controls; output-size và oracle gates chạy lại với baseline đã review.
- Bộ probe độc lập gồm 17 chương trình × O0/O1/O2: baseline có 6 mismatch, bản sửa đạt 51/51. Đây là public probes, không phải frozen dumps.
- Oracle có 241 inputs, 1.562 prototypes. Một input cũ `deinline_value_sites_without_prefix_limit` giảm nonequiv 6 → 4; 239 input cũ còn lại không đổi compact row. Fixture mới đạt nonequiv 0/4, trong khi cùng source trên baseline là 3/4. Tổng nonequiv 300 → 298; không có per-file regression. Bounded oracle không thay thế runtime proof.

## Review output

720 semantic outputs cũ: 11 file thay đổi, giữ nguyên 41.174 dòng, 35.163 dòng không trống và 813 inferred-call comments. 513 public outputs: 18 file thay đổi, giữ nguyên 40.375 dòng, 31.817 dòng không trống và 72 inferred-call comments. Đã đọc toàn bộ diff; token stream giữ nguyên sau khi chuẩn hóa riêng record-key spelling, và comment count giữ nguyên ở từng file.

Bảy strict residual outputs: chỉ `Part_Icles/Emit.luau` thêm bracket cho `VisualPart` (+4 byte), giữ nguyên 3.786 dòng và 38 call comments. Toàn bộ residual oracle rows giống checkpoint M6.

Candidate đầu bracket cả field cuối chưa rõ giá trị: 36 public files thay đổi và ba output thêm tổng cộng 15 dòng do wrap. Proof cho field cuối loại được phần tăng dòng này; số liệu nghiệm thu bên trên thuộc candidate cuối. Không nới line-width hoặc proof gate để làm đẹp số đo.

Raw evidence và binaries nằm tại `/workspace/work/v27`, audit cuối là `audit-6`. Lượt `audit-5` bị đầy ổ đĩa ở lineage và registry, nên không được tính là passed. Đã nén lossless 33.815 lineage JSON của các audit cũ bằng Python, so SHA-256 bản gốc với dữ liệu giải nén trước khi bỏ bản gốc; manifest `compressed-lineage-evidence.json` giữ đường dẫn và hash. Không xóa source, baseline binaries hoặc mất evidence.

## Còn mở

Size hint khác số stores được fold là lỗi riêng: bộ probe capacity đã tìm 93 mismatch trong 384 ca trên baseline, chưa được đóng bởi thay đổi formatter này. Tiếp tục mô hình layout trong các phép fold/match trước khi đánh dấu hoàn tất table work. Không có private frozen dumps; toàn bộ V2.7 vẫn đang triển khai. Không release hoặc sửa website.
