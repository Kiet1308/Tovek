# Nghiệm thu phần decompiler V2.7

**Hoàn tất theo phạm vi chủ repo chốt ngày 28/10/2026, UTC+7.** Nhánh
`v2.7-dev`; code nghiệm thu `b798e5b`. Các commit tiếp theo chỉ cập nhật tài
liệu, không thay source engine. Binary ordinary release SHA256:
`f1f019b6564b9447774b622ffac023155fcef3c23bb7b4d3acfe13adf5b5b094`.

## Phạm vi đã chốt

Các phần M4 đã xác định đủ, M5 naming, M6 L1/L2/L4, table allocation/folding,
native open SETLIST, vector matching/producer metadata, capture/SSA correctness
và các malformed-bytecode families đã nhận diện đều có implementation và
kiểm chứng. Các thay đổi, fixture, baseline và output review được liên kết
trong [GOAL](GOAL.md) và [ROADMAP](ROADMAP.md).

Chủ repo đưa ra hai quyết định riêng:

- Chấp nhận số đo hiện tại, chuyển G1 sang việc tiếp theo. Regression V2.5.1
  chưa được khắc phục; mục tiêu đó không còn chặn goal này.
- Chuyển case (c), ca repeat/return O2 gốc và ba seeds generic-for O2/g2 gốc
  sang [backlog chờ dữ liệu](remaining_review_cases.md). Không ghi các ca này
  thành đã sửa hoặc lấy public probes thay original reproducer.

Release, version bump, tag/package phát hành, merge main, website và web
changelog nằm ngoài yêu cầu này. Goal hoàn tất ở phần decompiler trên nhánh dev.

## Kiểm chứng trên code nghiệm thu

| Kiểm tra | Kết quả |
|---|---|
| Workspace, all targets | 1.698 passed, 0 failed, 2 ignored manual scaling probes |
| Fixture and Luau audit | Đủ 33/33 bước; 34 records gồm lượt public baseline |
| Semantic round-trip | 801/801 |
| Deep review | 469/469 |
| Public sources ghim phiên bản | 513/513 |
| Structuring và repeat-inline probes cuối | 1.024/1.024, trong đó 496 inlining witnesses ở bộ repeat-inline |
| Source diff với `d3e64a2` | 801 semantic, 513 public, 7 residual và 1.024 review-probe outputs giữ từng byte |
| Rebuilt-call annotations | Giữ 894 semantic / 72 public / 38 residual |
| Oracle và output-size gates | Đạt; singleton-phi optimization không sửa baseline |

Audit gồm runtime/determinism, source/SSA lineage, capture/producer/binding
metadata, artifact cache, compact style, compiler witnesses và Wasm/Worker
compatibility theo workflow thật. Luau pin
`c2ec0d4e5ca50796ba174a7565298f59aa572268`; Rust nightly-2026-06-15.

Evidence tại `/workspace/work/v27`: `audit-25/results.json`, các log
`audit-25-*.log`, `phi-singleton-workspace.log`,
`phi-singleton-{semantic,public,residual}-comparison.json`,
`review-probes-phi-singleton-summary.json` và runner/log tương ứng. Metadata
lớn được gzip lossless, kiểm tra SHA256 trước khi thay raw; source outputs,
baseline binaries và logs giữ nguyên. Cleanup cache dùng Python với exact
allowlist đã preview.

## Số đo và giới hạn

[Benchmark mới nhất](benchmark_phi_singleton.md), 25 samples mỗi version trên
513 public inputs: median chậm hơn V2.5.1 15,5%/15,4% ở 1/4 workers, so V2.6.1
là 3,3%/4,2%; có nhiễu lớn. Các report giữ paired ratios và toàn bộ samples.
Quyết định chuyển G1 không thay số đo hoặc biến regression thành đã sửa.

[Broad stress trước đó](stress_split_prefix.md) thuộc binary correctness
`d3e64a2`: 135.000 profiles có 133.906 passed, 1.094 invalid, 0 failed/refused;
9.000 mutation profiles có 7.695 passed, 904 typed refusals, 401 invalid,
0 failed. Không gán lượt broad stress ấy cho binary `b798e5b`; các gates và
review probes cuối ở bảng trên đã chạy trên đúng binary nghiệm thu.

Bốn private dumps (9.987 inputs), private review material và Windows timing
không khả dụng; nghiệm thu giới hạn ở public suites. Bounded oracle/probes
không phải proof tương đương cho mọi chương trình. Empty preallocated hash
giữ tương thích theo quyết định chủ repo; `pairs`/`next` qua mức tối ưu khác
vẫn chưa được bảo đảm, như đã ghi trong README. Các open SETLIST chưa có proof
native constructor vẫn bị typed refusal.
