# Mixed constructor: giữ đúng dung lượng khi fold field

Ngày: 27/10/2026, UTC+7. Baseline: `ba776bd`.

## Lỗi và sửa chung

Fuzz rộng phát hiện `{[1]=11, [true]=22, 33}; t.k=nil` bị từ chối với `constructor fields exceed the original NEWTABLE hash allocation`. Compiler cấp hash 2 và array 1. Khi có list items, `[1]` vẫn là một keyed field trong phép tính hash của cú pháp; chuyển nó thành list item sẽ đổi chỉ số của các store phía sau. Predicate cũ loại `[1]` khỏi phép đếm rồi cho fold `k`, khiến output cần hash 4. Gate cuối phát hiện sự sai khác và từ chối cả output.

Sửa `table_takes_another_key` dùng chung cho SSA và AST: constructor trộn list/key phải tính tất cả explicit keys vào hash census. Store vào array slot tiếp theo cũng phải còn hash headroom khi vẫn in dưới dạng explicit key trong mixed constructor. Trường hợp chưa có list vẫn giữ phép chuyển array keys tuần tự hiện hữu. Không nới gate layout cuối, không thay policy empty-hash, không đưa padding key mới vào table.

Unit test bao phủ mixed/pure indexed constructors, hash đủ/hết chỗ, overwrite và index không tuần tự. Fixture `table_mixed_allocation_growth` kiểm tra numeric/boolean keys, duplicate signed zero, nil stores, hash growth và thứ tự `next`; dùng chung semantic và deep-review suites.

## Kiểm chứng

Lượt fuzz trên baseline dừng sau khi ghi nhận 428 refusal cùng loại trên 51 seeds; không coi đây là một lượt hoàn chỉnh. Tất cả ca lỗi đã giảm và log được giữ. Candidate chạy lại đủ 51 × 9 cấu hình optimization/debug: 459/459 passed.

Code cuối đạt 1.689 workspace tests (2 ignored), 789 semantic cases, 451 deep-review cases và đủ 33 audit steps. Đã chạy lại 1.476/1.476 table probes và 512/512 public structuring probes trên binary mới. Cả 786 semantic, 513 public và 7 residual outputs cũ giống từng byte; số dòng và rebuilt calls không đổi.

Đọc cả ba output mới trước khi cập nhật size baseline: mỗi file 37 dòng/389 bytes theo gate, không đổi baseline file cũ. Oracle thêm một fixture/hai prototypes: exact +1, nonequiv 317 → 318 chỉ do main của fixture mới loại store literal 11 bị ghi đè vào cùng key `-0`; không thêm instruction hay capture excess. Các per-file nonequiv cũ không đổi. Hai gate size/oracle đầu tiên chỉ thiếu baseline fixture mới; giữ logs/results ban đầu và chạy lại hai gate sau review, cả 33 bước có kết quả cuối bằng 0.

Broad fuzz toàn dải sẽ chạy lại sau checkpoint; chưa tuyên bố đã hoàn thiện V2.7 hoặc hết regression hiệu năng.

Evidence tại `/workspace/work/v27`: `fuzz-final-interrupted.json`, `table-mixed-growth`, `table-mixed-recheck.json`, `table-mixed-*/results.json`, `audit-16` và logs. Binary SHA256: `a894fa3666d9250108f0b1198d7c065022326a42161e2165909b52b96c993c18`. Private frozen dumps và các review seeds gốc chưa có trong môi trường này.
